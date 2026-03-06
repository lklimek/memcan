"""Unit tests for memory tools -- direct Qdrant+Ollama layer.

These tests mock Ollama and Qdrant to verify memory tool logic in isolation.
Written TDD-style: tests define the contract BEFORE implementation.
"""

from __future__ import annotations

import hashlib
import json
from datetime import datetime, timezone
from unittest.mock import AsyncMock, MagicMock, patch
from uuid import uuid4

import pytest


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_point(point_id, data, user_id, created_at=None, updated_at=None, **meta):
    """Build a mock Qdrant point with standard payload schema."""
    pt = MagicMock()
    pt.id = point_id
    pt.payload = {
        "data": data,
        "hash": hashlib.md5(data.encode()).hexdigest(),
        "user_id": user_id,
        "created_at": created_at or datetime.now(timezone.utc).isoformat(),
        "updated_at": updated_at,
        **meta,
    }
    pt.vector = [0.1] * 10  # stub vector
    return pt


# ===========================================================================
# TestFactExtraction
# ===========================================================================


class TestFactExtraction:
    """Test LLM call #1 (fact extraction from raw content)."""

    @pytest.mark.asyncio
    async def test_facts_parsed_from_llm_response(self):
        """LLM returns {"facts": ["f1", "f2"]} -- verify parsed correctly."""
        from mindojo_mcp.server import _extract_facts

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {"facts": ["Python 3.14 is preferred", "Use ruff for linting"]}
        )

        with patch("mindojo_mcp.server._get_ollama_async") as mock_get_client:
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client

            facts = await _extract_facts("We use Python 3.14 and ruff for linting")
            assert facts == ["Python 3.14 is preferred", "Use ruff for linting"]

    @pytest.mark.asyncio
    async def test_empty_facts_returns_empty_list(self):
        """LLM returns {"facts": []} -- no facts extracted."""
        from mindojo_mcp.server import _extract_facts

        mock_response = MagicMock()
        mock_response.message.content = json.dumps({"facts": []})

        with patch("mindojo_mcp.server._get_ollama_async") as mock_get_client:
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client

            facts = await _extract_facts("nothing useful here")
            assert facts == []

    @pytest.mark.asyncio
    async def test_malformed_llm_response_returns_none(self):
        """LLM returns non-JSON -- fallback returns None (raw storage)."""
        from mindojo_mcp.server import _extract_facts

        mock_response = MagicMock()
        mock_response.message.content = "I cannot extract facts from this."

        with patch("mindojo_mcp.server._get_ollama_async") as mock_get_client:
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client

            facts = await _extract_facts("some content")
            assert facts is None


# ===========================================================================
# TestMemoryDedup
# ===========================================================================


class TestMemoryDedup:
    """Test LLM call #2 (dedup/merge against existing memories)."""

    @pytest.mark.asyncio
    async def test_add_event_upserts_new_point(self):
        """Dedup returns ADD event -- new point upserted with uuid4 ID."""
        from mindojo_mcp.server import _dedup_and_store

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {"events": [{"type": "ADD", "data": "New fact about testing"}]}
        )

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = []
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.1] * 10]

            await _dedup_and_store(
                facts=["New fact about testing"],
                user_id="test-user",
                metadata={},
            )

            mock_qd.upsert.assert_called_once()
            call_kwargs = mock_qd.upsert.call_args
            points = call_kwargs.kwargs.get("points") or call_kwargs[1].get(
                "points", call_kwargs[0][1] if len(call_kwargs[0]) > 1 else None
            )
            assert points is not None
            assert len(points) == 1
            assert points[0].payload["data"] == "New fact about testing"

    @pytest.mark.asyncio
    async def test_update_event_upserts_with_updated_at(self):
        """Dedup returns UPDATE -- existing point re-embedded with updated_at."""
        from mindojo_mcp.server import _dedup_and_store

        existing_point = _make_point(
            str(uuid4()),
            "Old fact",
            "test-user",
            created_at="2025-01-01T00:00:00+00:00",
        )

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {
                "events": [
                    {
                        "type": "UPDATE",
                        "data": "Updated fact",
                        "memory_id": existing_point.id,
                    }
                ]
            }
        )

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = [existing_point]
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.2] * 10]

            await _dedup_and_store(
                facts=["Updated fact"],
                user_id="test-user",
                metadata={},
            )

            mock_qd.upsert.assert_called_once()
            call_kwargs = mock_qd.upsert.call_args
            points = call_kwargs.kwargs.get("points") or call_kwargs[1].get(
                "points", call_kwargs[0][1] if len(call_kwargs[0]) > 1 else None
            )
            assert points[0].payload["updated_at"] is not None
            assert points[0].payload["created_at"] == "2025-01-01T00:00:00+00:00"

    @pytest.mark.asyncio
    async def test_delete_event_deletes_point(self):
        """Dedup returns DELETE -- existing point deleted."""
        from mindojo_mcp.server import _dedup_and_store

        existing_id = str(uuid4())

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {"events": [{"type": "DELETE", "memory_id": existing_id}]}
        )

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = [_make_point(existing_id, "old", "test-user")]
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.1] * 10]

            await _dedup_and_store(
                facts=["old fact"],
                user_id="test-user",
                metadata={},
            )

            mock_qd.delete.assert_called_once()

    @pytest.mark.asyncio
    async def test_none_event_no_changes(self):
        """Dedup returns NONE -- no upsert or delete."""
        from mindojo_mcp.server import _dedup_and_store

        mock_response = MagicMock()
        mock_response.message.content = json.dumps({"events": [{"type": "NONE"}]})

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = [_make_point(str(uuid4()), "existing", "test-user")]
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.1] * 10]

            await _dedup_and_store(
                facts=["existing fact"],
                user_id="test-user",
                metadata={},
            )

            mock_qd.upsert.assert_not_called()
            mock_qd.delete.assert_not_called()

    def test_hash_dedup_same_content_same_hash(self):
        """Same content produces same md5 hash."""
        content = "Always use ruff for Python linting"
        h1 = hashlib.md5(content.encode()).hexdigest()
        h2 = hashlib.md5(content.encode()).hexdigest()
        assert h1 == h2


# ===========================================================================
# TestDistillationFallback
# ===========================================================================


class TestDistillationFallback:
    """Test fallback behavior when LLM calls fail or distillation is off."""

    @pytest.mark.asyncio
    async def test_extraction_failure_stores_raw_content(self):
        """If LLM call #1 (extraction) fails, raw content is stored directly."""
        from mindojo_mcp.server import _extract_facts

        with patch("mindojo_mcp.server._get_ollama_async") as mock_get_client:
            mock_client = AsyncMock()
            mock_client.chat.side_effect = Exception("LLM unavailable")
            mock_get_client.return_value = mock_client

            facts = await _extract_facts("important content")
            # On exception, should return None to signal raw storage fallback
            assert facts is None

    @pytest.mark.asyncio
    async def test_dedup_failure_stores_facts_as_add(self):
        """If LLM call #2 (dedup) fails, each fact stored as ADD."""
        from mindojo_mcp.server import _dedup_and_store

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = []
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.side_effect = Exception("LLM unavailable")
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.1] * 10]

            await _dedup_and_store(
                facts=["fact one", "fact two"],
                user_id="test-user",
                metadata={},
            )

            # Both facts should be upserted even though dedup LLM failed
            assert mock_qd.upsert.call_count == 2

    @pytest.mark.asyncio
    async def test_distill_disabled_stores_raw(self):
        """If distill_memories=False, skip LLM calls, store raw content."""
        from mindojo_mcp.server import _do_add_memory

        mock_qd = MagicMock()

        with (
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
            patch("mindojo_mcp.server.settings") as mock_settings,
            patch("mindojo_mcp.server._extract_facts") as mock_extract,
        ):
            mock_settings.distill_memories = False
            mock_aembed.return_value = [[0.1] * 10]

            await _do_add_memory(
                content="raw content here",
                user_id="test-user",
                metadata={},
            )

            # LLM extraction should NOT be called
            mock_extract.assert_not_called()
            # Content should be stored directly
            mock_qd.upsert.assert_called_once()


# ===========================================================================
# TestPayloadSchema
# ===========================================================================


class TestPayloadSchema:
    """Verify Qdrant payload matches backward-compatible schema."""

    @pytest.mark.asyncio
    async def test_add_creates_correct_payload(self):
        """ADD creates payload with data, hash, user_id, created_at, updated_at=None."""
        from mindojo_mcp.server import _dedup_and_store

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {"events": [{"type": "ADD", "data": "Test payload schema"}]}
        )

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = []
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.1] * 10]

            await _dedup_and_store(
                facts=["Test payload schema"],
                user_id="test-user",
                metadata={"type": "lesson", "source": "LL-001"},
            )

            call_kwargs = mock_qd.upsert.call_args
            points = call_kwargs.kwargs.get("points") or call_kwargs[1].get(
                "points", call_kwargs[0][1] if len(call_kwargs[0]) > 1 else None
            )
            payload = points[0].payload

            assert payload["data"] == "Test payload schema"
            assert payload["hash"] == hashlib.md5(b"Test payload schema").hexdigest()
            assert payload["user_id"] == "test-user"
            assert "created_at" in payload
            # Verify ISO format
            datetime.fromisoformat(payload["created_at"])
            assert payload["updated_at"] is None
            assert payload["type"] == "lesson"
            assert payload["source"] == "LL-001"

    @pytest.mark.asyncio
    async def test_update_preserves_created_at_sets_updated_at(self):
        """UPDATE preserves original created_at, sets updated_at to now."""
        from mindojo_mcp.server import _dedup_and_store

        original_created = "2025-01-01T00:00:00+00:00"
        existing_id = str(uuid4())
        existing_point = _make_point(
            existing_id, "Old data", "test-user", created_at=original_created
        )

        mock_response = MagicMock()
        mock_response.message.content = json.dumps(
            {
                "events": [
                    {
                        "type": "UPDATE",
                        "data": "Updated data",
                        "memory_id": existing_id,
                    }
                ]
            }
        )

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = [existing_point]
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server._get_ollama_async") as mock_get_client,
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_client = AsyncMock()
            mock_client.chat.return_value = mock_response
            mock_get_client.return_value = mock_client
            mock_aembed.return_value = [[0.2] * 10]

            before = datetime.now(timezone.utc)
            await _dedup_and_store(
                facts=["Updated data"],
                user_id="test-user",
                metadata={},
            )
            after = datetime.now(timezone.utc)

            call_kwargs = mock_qd.upsert.call_args
            points = call_kwargs.kwargs.get("points") or call_kwargs[1].get(
                "points", call_kwargs[0][1] if len(call_kwargs[0]) > 1 else None
            )
            payload = points[0].payload

            assert payload["created_at"] == original_created
            updated_at = datetime.fromisoformat(payload["updated_at"])
            assert before <= updated_at <= after


# ===========================================================================
# TestSearchMemories
# ===========================================================================


class TestSearchMemories:
    """Test search_memories MCP tool output format."""

    @pytest.mark.asyncio
    async def test_search_returns_correct_format(self):
        """search_memories returns list of dicts with expected fields."""
        from mindojo_mcp.server import search_memories

        point = _make_point(
            str(uuid4()),
            "Always use type hints",
            "test-user",
            type="lesson",
        )
        point.score = 0.95

        mock_qd = MagicMock()
        mock_search_result = MagicMock()
        mock_search_result.points = [point]
        mock_qd.query_points.return_value = mock_search_result

        with (
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_aembed.return_value = [[0.1] * 10]

            result_json = await search_memories(query="type hints", user_id="test-user")
            results = json.loads(result_json)

            assert isinstance(results, list)
            assert len(results) == 1
            r = results[0]
            assert r["id"] == point.id
            assert r["memory"] == "Always use type hints"
            assert r["hash"] == point.payload["hash"]
            assert r["score"] == 0.95
            assert "created_at" in r
            assert "updated_at" in r
            assert r["user_id"] == "test-user"


# ===========================================================================
# TestGetMemories
# ===========================================================================


class TestGetMemories:
    """Test get_memories MCP tool output format."""

    @pytest.mark.asyncio
    async def test_get_returns_correct_format(self):
        """get_memories returns list of dicts (no score field)."""
        from mindojo_mcp.server import get_memories

        point = _make_point(str(uuid4()), "Use pytest", "test-user")

        mock_qd = MagicMock()
        mock_qd.scroll.return_value = ([point], None)

        with patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd):
            result_json = await get_memories(user_id="test-user")
            results = json.loads(result_json)

            assert isinstance(results, list)
            assert len(results) == 1
            r = results[0]
            assert r["id"] == point.id
            assert r["memory"] == "Use pytest"
            assert "score" not in r
            assert "created_at" in r
            assert r["user_id"] == "test-user"


# ===========================================================================
# TestCountMemories
# ===========================================================================


class TestCountMemories:
    """Test count_memories MCP tool."""

    @pytest.mark.asyncio
    async def test_count_returns_count_and_user_id(self):
        """count_memories returns {"count": N, "user_id": "..."}."""
        from mindojo_mcp.server import count_memories

        mock_qd = MagicMock()
        mock_count_result = MagicMock()
        mock_count_result.count = 42
        mock_qd.count.return_value = mock_count_result

        with patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd):
            result_json = await count_memories(user_id="test-user")
            result = json.loads(result_json)

            assert result["count"] == 42
            assert result["user_id"] == "test-user"


# ===========================================================================
# TestDeleteMemory
# ===========================================================================


class TestDeleteMemory:
    """Test delete_memory MCP tool."""

    @pytest.mark.asyncio
    async def test_delete_returns_status_and_id(self):
        """delete_memory returns {"status": "deleted", "memory_id": "..."}."""
        from mindojo_mcp.server import delete_memory

        memory_id = str(uuid4())
        mock_qd = MagicMock()

        with patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd):
            result_json = await delete_memory(memory_id=memory_id)
            result = json.loads(result_json)

            assert result["status"] == "deleted"
            assert result["memory_id"] == memory_id
            mock_qd.delete.assert_called_once()


# ===========================================================================
# TestUpdateMemory
# ===========================================================================


class TestUpdateMemory:
    """Test update_memory MCP tool."""

    @pytest.mark.asyncio
    async def test_update_preserves_created_at_sets_updated_at(self):
        """update_memory re-embeds, preserves created_at, sets updated_at."""
        from mindojo_mcp.server import update_memory

        memory_id = str(uuid4())
        original_created = "2025-06-01T12:00:00+00:00"
        existing_point = _make_point(
            memory_id, "Old content", "test-user", created_at=original_created
        )

        mock_qd = MagicMock()
        mock_qd.retrieve.return_value = [existing_point]

        with (
            patch("mindojo_mcp.server.get_qdrant", return_value=mock_qd),
            patch("mindojo_mcp.server.aembed", new_callable=AsyncMock) as mock_aembed,
        ):
            mock_aembed.return_value = [[0.3] * 10]

            result_json = await update_memory(memory_id=memory_id, memory="New content")
            json.loads(result_json)  # verify valid JSON

            mock_qd.upsert.assert_called_once()
            call_kwargs = mock_qd.upsert.call_args
            points = call_kwargs.kwargs.get("points") or call_kwargs[1].get(
                "points", call_kwargs[0][1] if len(call_kwargs[0]) > 1 else None
            )
            payload = points[0].payload

            assert payload["data"] == "New content"
            assert payload["created_at"] == original_created
            assert payload["updated_at"] is not None
            datetime.fromisoformat(payload["updated_at"])
