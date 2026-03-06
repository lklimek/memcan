"""Unit tests for search_standards and search_code MCP tools."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, Mock, patch

import pytest

from mindojo_mcp.config import CODE_COLLECTION, QDRANT_COLLECTION, STANDARDS_COLLECTION


@pytest.fixture()
def _mock_asearch():
    """Patch asearch_collection in server module to return sample results."""
    sample = [{"score": 0.95, "title": "test doc", "content": "hello"}]
    with patch(
        "mindojo_mcp.server.asearch_collection",
        new_callable=AsyncMock,
        return_value=sample,
    ) as mock:
        yield mock


class TestSearchStandards:
    """search_standards tool tests."""

    @pytest.mark.asyncio
    async def test_returns_json_from_mock(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        result = await search_standards(query="buffer overflow")
        parsed = json.loads(result)
        assert isinstance(parsed, list)
        assert parsed[0]["score"] == 0.95

    @pytest.mark.asyncio
    async def test_passes_standards_collection(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss")
        call_kwargs = _mock_asearch.call_args
        assert call_kwargs.kwargs["collection"] == STANDARDS_COLLECTION

    @pytest.mark.asyncio
    async def test_builds_filter_dict(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(
            query="xss",
            standard_type="security",
            standard_id="owasp-asvs",
            tech_stack="python",
            lang="en",
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-asvs"
        assert filters["tech_stack"] == "python"
        assert filters["lang"] == "en"

    @pytest.mark.asyncio
    async def test_case_insensitive_filters(self, _mock_asearch):
        """Uppercase filter values are normalized to lowercase."""
        from mindojo_mcp.server import search_standards

        await search_standards(
            query="xss", standard_type="SECURITY", standard_id="OWASP-ASVS"
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["standard_type"] == "security"
        assert filters["standard_id"] == "owasp-asvs"

    @pytest.mark.asyncio
    async def test_ref_id_wraps_to_ref_ids_list(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", ref_id="CWE-79")
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["ref_ids"] == ["CWE-79"]

    @pytest.mark.asyncio
    async def test_ref_id_none_passes_none(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", ref_id=None)
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["ref_ids"] is None

    @pytest.mark.asyncio
    async def test_limit_clamped_low(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", limit=0)
        assert _mock_asearch.call_args.kwargs["limit"] == 1

    @pytest.mark.asyncio
    async def test_limit_clamped_high(self, _mock_asearch):
        from mindojo_mcp.server import search_standards

        await search_standards(query="xss", limit=999)
        assert _mock_asearch.call_args.kwargs["limit"] == 100


class TestSearchCode:
    """search_code tool tests."""

    @pytest.mark.asyncio
    async def test_returns_json_from_mock(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        result = await search_code(query="error handling")
        parsed = json.loads(result)
        assert isinstance(parsed, list)
        assert parsed[0]["score"] == 0.95

    @pytest.mark.asyncio
    async def test_passes_code_collection(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="retry logic")
        assert _mock_asearch.call_args.kwargs["collection"] == CODE_COLLECTION

    @pytest.mark.asyncio
    async def test_builds_filter_dict(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(
            query="retry",
            project="myrepo",
            tech_stack="rust",
            file_path="src/main.rs",
        )
        filters = _mock_asearch.call_args.kwargs["filters"]
        assert filters["project"] == "myrepo"
        assert filters["tech_stack"] == "rust"
        assert filters["file_path"] == "src/main.rs"

    @pytest.mark.asyncio
    async def test_limit_clamped_low(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="test", limit=-5)
        assert _mock_asearch.call_args.kwargs["limit"] == 1

    @pytest.mark.asyncio
    async def test_limit_clamped_high(self, _mock_asearch):
        from mindojo_mcp.server import search_code

        await search_code(query="test", limit=200)
        assert _mock_asearch.call_args.kwargs["limit"] == 100


class TestListCollections:
    """list_collections tool tests."""

    @pytest.fixture()
    def _mock_qdrant(self):
        """Patch get_qdrant in server module to return a mock QdrantClient."""
        mock_qd = Mock()

        # Default: all collections exist with count 42
        mock_qd.count.return_value = Mock(count=42)

        # Default: facet returns one hit
        mock_qd.facet.return_value = Mock(hits=[Mock(value="security", count=42)])

        with patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd):
            yield mock_qd

    @pytest.mark.asyncio
    async def test_returns_collections_with_counts(self, _mock_qdrant):
        from mindojo_mcp.server import list_collections

        result = json.loads(await list_collections())
        assert "collections" in result
        names = [c["name"] for c in result["collections"]]
        assert STANDARDS_COLLECTION in names
        assert QDRANT_COLLECTION in names

        standards = next(
            c for c in result["collections"] if c["name"] == STANDARDS_COLLECTION
        )
        assert standards["count"] == 42
        assert "standard_type" in standards["filters"]

    @pytest.mark.asyncio
    async def test_skips_nonexistent_collections(self, _mock_qdrant):
        from mindojo_mcp.server import list_collections

        def count_side_effect(collection_name, exact=True):
            if collection_name == STANDARDS_COLLECTION:
                raise Exception("Not found")
            return Mock(count=42)

        _mock_qdrant.count.side_effect = count_side_effect

        result = json.loads(await list_collections())
        names = [c["name"] for c in result["collections"]]
        assert STANDARDS_COLLECTION not in names
        assert QDRANT_COLLECTION in names

    @pytest.mark.asyncio
    async def test_handles_unindexed_facet_fields(self, _mock_qdrant):
        from mindojo_mcp.server import list_collections

        def facet_side_effect(collection_name, key, limit=10):
            if key == "standard_type":
                return Mock(hits=[Mock(value="security", count=42)])
            raise Exception("Field not indexed")

        _mock_qdrant.facet.side_effect = facet_side_effect

        result = json.loads(await list_collections())
        standards = next(
            c for c in result["collections"] if c["name"] == STANDARDS_COLLECTION
        )
        assert "standard_type" in standards["filters"]
        assert standards["filters"]["standard_type"] == [
            {"value": "security", "count": 42}
        ]
        # Fields that failed faceting should be absent
        for field in ["standard_id", "version", "tech_stack", "lang"]:
            assert field not in standards["filters"]
