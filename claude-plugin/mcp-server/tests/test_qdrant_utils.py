"""Unit tests for qdrant_utils — all external calls mocked."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest


class TestGetQdrant:
    """get_qdrant() returns a lazy singleton QdrantClient."""

    def teardown_method(self):
        # Reset singleton between tests
        import mindojo_mcp.qdrant_utils as qu

        qu._qdrant = None

    @patch("mindojo_mcp.qdrant_utils.QdrantClient")
    def test_returns_singleton(self, mock_cls):
        from mindojo_mcp.qdrant_utils import get_qdrant

        first = get_qdrant()
        second = get_qdrant()
        assert first is second
        mock_cls.assert_called_once()

    @patch("mindojo_mcp.qdrant_utils.QdrantClient")
    def test_passes_qdrant_url(self, mock_cls):
        from mindojo_mcp.qdrant_utils import get_qdrant

        get_qdrant()
        mock_cls.assert_called_once()
        # Should use settings.qdrant_url
        call_kwargs = mock_cls.call_args
        assert "url" in call_kwargs.kwargs or (call_kwargs.args and call_kwargs.args[0])


class TestEnsureCollection:
    """ensure_collection() creates collection only when missing."""

    def teardown_method(self):
        import mindojo_mcp.qdrant_utils as qu

        qu._qdrant = None

    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    def test_creates_when_missing(self, mock_get_qd):
        from mindojo_mcp.qdrant_utils import ensure_collection

        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        # Simulate empty collections list
        mock_qd.get_collections.return_value.collections = []

        ensure_collection("test-collection", dims=128)

        mock_qd.create_collection.assert_called_once()
        call_kwargs = mock_qd.create_collection.call_args.kwargs
        assert call_kwargs["collection_name"] == "test-collection"

    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    def test_skips_when_exists(self, mock_get_qd):
        from mindojo_mcp.qdrant_utils import ensure_collection

        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        existing = MagicMock()
        existing.name = "test-collection"
        mock_qd.get_collections.return_value.collections = [existing]

        ensure_collection("test-collection")

        mock_qd.create_collection.assert_not_called()


class TestDropByFilter:
    """drop_by_filter() counts then deletes matching points."""

    def teardown_method(self):
        import mindojo_mcp.qdrant_utils as qu

        qu._qdrant = None

    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    def test_deletes_matching_points(self, mock_get_qd):
        from qdrant_client.models import FieldCondition, Filter, MatchValue

        from mindojo_mcp.qdrant_utils import drop_by_filter

        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        mock_qd.count.return_value.count = 5

        f = Filter(must=[FieldCondition(key="source", match=MatchValue(value="x"))])
        deleted = drop_by_filter("my-col", f)

        assert deleted == 5
        mock_qd.delete.assert_called_once_with(
            collection_name="my-col", points_selector=f
        )

    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    def test_skips_delete_when_zero(self, mock_get_qd):
        from qdrant_client.models import Filter

        from mindojo_mcp.qdrant_utils import drop_by_filter

        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        mock_qd.count.return_value.count = 0

        deleted = drop_by_filter("my-col", Filter(must=[]))

        assert deleted == 0
        mock_qd.delete.assert_not_called()


class TestEmbed:
    """embed() calls Ollama synchronously and returns vectors."""

    @patch("mindojo_mcp.qdrant_utils._get_ollama_sync")
    def test_embed_single(self, mock_get):
        from mindojo_mcp.qdrant_utils import embed

        mock_client = MagicMock()
        mock_get.return_value = mock_client
        mock_resp = MagicMock()
        mock_resp.embeddings = [[0.1, 0.2, 0.3]]
        mock_client.embed.return_value = mock_resp

        result = embed(["hello"])

        assert result == [[0.1, 0.2, 0.3]]
        mock_client.embed.assert_called_once()

    @patch("mindojo_mcp.qdrant_utils._get_ollama_sync")
    def test_embed_multiple(self, mock_get):
        from mindojo_mcp.qdrant_utils import embed

        mock_client = MagicMock()
        mock_get.return_value = mock_client
        mock_resp1 = MagicMock()
        mock_resp1.embeddings = [[1.0, 2.0]]
        mock_resp2 = MagicMock()
        mock_resp2.embeddings = [[3.0, 4.0]]
        mock_client.embed.side_effect = [mock_resp1, mock_resp2]

        result = embed(["a", "b"])

        assert result == [[1.0, 2.0], [3.0, 4.0]]
        assert mock_client.embed.call_count == 2


class TestAembed:
    """aembed() calls Ollama asynchronously."""

    @pytest.mark.asyncio
    @patch("mindojo_mcp.qdrant_utils._get_ollama_async")
    async def test_aembed(self, mock_get):
        from mindojo_mcp.qdrant_utils import aembed

        mock_client = AsyncMock()
        mock_get.return_value = mock_client
        mock_resp = MagicMock()
        mock_resp.embeddings = [[0.5, 0.6]]
        mock_client.embed.return_value = mock_resp

        result = await aembed(["test"])

        assert result == [[0.5, 0.6]]
        mock_client.embed.assert_awaited_once()


class TestAsearchCollection:
    """asearch_collection() builds filters and returns results."""

    @pytest.mark.asyncio
    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    @patch("mindojo_mcp.qdrant_utils.aembed")
    async def test_str_filter_uses_match_value(self, mock_aembed, mock_get_qd):
        from qdrant_client.models import FieldCondition, MatchValue

        from mindojo_mcp.qdrant_utils import asearch_collection

        mock_aembed.return_value = [[0.1, 0.2]]
        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd

        scored = MagicMock()
        scored.score = 0.95
        scored.payload = {"text": "hello", "source": "docs"}
        mock_qd.query_points.return_value.points = [scored]

        results = await asearch_collection("col", "query", filters={"source": "docs"})

        assert len(results) == 1
        assert results[0]["score"] == 0.95
        assert results[0]["text"] == "hello"

        # Verify filter construction
        call_kwargs = mock_qd.query_points.call_args.kwargs
        qf = call_kwargs["query_filter"]
        assert len(qf.must) == 1
        cond = qf.must[0]
        assert isinstance(cond, FieldCondition)
        assert isinstance(cond.match, MatchValue)
        assert cond.match.value == "docs"

    @pytest.mark.asyncio
    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    @patch("mindojo_mcp.qdrant_utils.aembed")
    async def test_list_filter_uses_match_any(self, mock_aembed, mock_get_qd):
        from qdrant_client.models import FieldCondition, MatchAny

        from mindojo_mcp.qdrant_utils import asearch_collection

        mock_aembed.return_value = [[0.1, 0.2]]
        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        mock_qd.query_points.return_value.points = []

        await asearch_collection("col", "query", filters={"tags": ["a", "b"]})

        call_kwargs = mock_qd.query_points.call_args.kwargs
        qf = call_kwargs["query_filter"]
        cond = qf.must[0]
        assert isinstance(cond, FieldCondition)
        assert isinstance(cond.match, MatchAny)
        assert cond.match.any == ["a", "b"]

    @pytest.mark.asyncio
    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    @patch("mindojo_mcp.qdrant_utils.aembed")
    async def test_none_values_skipped(self, mock_aembed, mock_get_qd):
        from mindojo_mcp.qdrant_utils import asearch_collection

        mock_aembed.return_value = [[0.1, 0.2]]
        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        mock_qd.query_points.return_value.points = []

        await asearch_collection("col", "query", filters={"a": "val", "b": None})

        call_kwargs = mock_qd.query_points.call_args.kwargs
        qf = call_kwargs["query_filter"]
        assert len(qf.must) == 1
        assert qf.must[0].key == "a"

    @pytest.mark.asyncio
    @patch("mindojo_mcp.qdrant_utils.get_qdrant")
    @patch("mindojo_mcp.qdrant_utils.aembed")
    async def test_no_filters_passes_none(self, mock_aembed, mock_get_qd):
        from mindojo_mcp.qdrant_utils import asearch_collection

        mock_aembed.return_value = [[0.1, 0.2]]
        mock_qd = MagicMock()
        mock_get_qd.return_value = mock_qd
        mock_qd.query_points.return_value.points = []

        await asearch_collection("col", "query")

        call_kwargs = mock_qd.query_points.call_args.kwargs
        assert call_kwargs["query_filter"] is None
