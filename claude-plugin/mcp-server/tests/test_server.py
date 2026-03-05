"""Unit tests for MCP server async tools."""

from __future__ import annotations

import asyncio
import json
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from qdrant_client.models import FieldCondition, Filter, MatchValue


@pytest.fixture()
def _mock_memory():
    """Patch _get_memory to return an AsyncMock of AsyncMemory."""
    mem = AsyncMock()
    mem.add.return_value = {"results": [{"id": "abc123", "memory": "test"}]}
    mem.search.return_value = {
        "results": [{"id": "abc123", "memory": "test", "score": 0.9}]
    }
    mem.get_all.return_value = {"results": [{"id": "abc123", "memory": "test"}]}
    mem.delete.return_value = None
    mem.update.return_value = {"id": "abc123", "memory": "updated"}

    with patch(
        "mindojo_mcp.server._get_memory", new_callable=AsyncMock, return_value=mem
    ):
        yield mem


class TestAddMemoryFireAndForget:
    """add_memory should return immediately with queued status."""

    @pytest.mark.asyncio
    async def test_returns_queued_status(self, _mock_memory):
        from mindojo_mcp.server import add_memory

        result = await add_memory(memory="test memory")
        parsed = json.loads(result)
        assert parsed["status"] == "queued"
        assert "user_id" in parsed

    @pytest.mark.asyncio
    async def test_does_not_await_mem_add_inline(self, _mock_memory):
        from mindojo_mcp.server import add_memory

        await add_memory(memory="test memory")

        # mem.add should not have been called yet (it's in a background task)
        _mock_memory.add.assert_not_called()

        # Let the event loop process background tasks
        await asyncio.sleep(0)

        _mock_memory.add.assert_called_once()

    @pytest.mark.asyncio
    async def test_background_task_passes_correct_args(self, _mock_memory):
        from mindojo_mcp.server import add_memory

        await add_memory(
            memory="important lesson",
            project="myrepo",
            metadata={"type": "lesson"},
        )
        await asyncio.sleep(0)

        _mock_memory.add.assert_called_once_with(
            "important lesson",
            user_id="project:myrepo",
            metadata={"type": "lesson"},
        )


class TestSearchMemories:
    """search_memories should await and return results."""

    @pytest.mark.asyncio
    async def test_returns_search_results(self, _mock_memory):
        from mindojo_mcp.server import search_memories

        result = await search_memories(query="test query")
        parsed = json.loads(result)
        assert "results" in parsed
        _mock_memory.search.assert_called_once()


class TestGetMemories:
    """get_memories should await and return results."""

    @pytest.mark.asyncio
    async def test_returns_all_memories(self, _mock_memory):
        from mindojo_mcp.server import get_memories

        result = await get_memories()
        parsed = json.loads(result)
        assert "results" in parsed
        _mock_memory.get_all.assert_called_once()

    @pytest.mark.asyncio
    async def test_default_limit(self, _mock_memory):
        from mindojo_mcp.server import get_memories

        await get_memories()
        _mock_memory.get_all.assert_called_once_with(user_id="global", limit=100)

    @pytest.mark.asyncio
    async def test_passes_limit_to_get_all(self, _mock_memory):
        from mindojo_mcp.server import get_memories

        await get_memories(limit=5)
        _mock_memory.get_all.assert_called_once_with(user_id="global", limit=5)


class TestDeleteMemory:
    """delete_memory should await and return confirmation."""

    @pytest.mark.asyncio
    async def test_returns_deleted_status(self, _mock_memory):
        from mindojo_mcp.server import delete_memory

        result = await delete_memory(memory_id="abc123")
        parsed = json.loads(result)
        assert parsed["status"] == "deleted"
        _mock_memory.delete.assert_called_once_with("abc123")


class TestUpdateMemory:
    """update_memory should await and return result."""

    @pytest.mark.asyncio
    async def test_returns_update_result(self, _mock_memory):
        from mindojo_mcp.server import update_memory

        result = await update_memory(memory_id="abc123", memory="new content")
        parsed = json.loads(result)
        assert parsed["id"] == "abc123"
        _mock_memory.update.assert_called_once_with("abc123", "new content")


class TestCountMemories:
    """count_memories should return count from Qdrant."""

    @pytest.mark.asyncio
    async def test_returns_count(self, _mock_memory):
        from mindojo_mcp.server import count_memories

        # Use MagicMock for vector_store so client.count is sync (called via to_thread)
        vs = MagicMock()
        count_result = MagicMock()
        count_result.count = 42
        vs.client.count.return_value = count_result
        vs.collection_name = "mindojo"
        _mock_memory.vector_store = vs

        result = await count_memories()
        parsed = json.loads(result)
        assert parsed["count"] == 42
        assert "user_id" in parsed
        vs.client.count.assert_called_once_with(
            collection_name="mindojo",
            count_filter=Filter(
                must=[FieldCondition(key="user_id", match=MatchValue(value="global"))]
            ),
            exact=True,
        )

    @pytest.mark.asyncio
    async def test_scoped_count(self, _mock_memory):
        from mindojo_mcp.server import count_memories

        vs = MagicMock()
        count_result = MagicMock()
        count_result.count = 7
        vs.client.count.return_value = count_result
        vs.collection_name = "mindojo"
        _mock_memory.vector_store = vs

        result = await count_memories(project="myrepo")
        parsed = json.loads(result)
        assert parsed["count"] == 7
        assert parsed["user_id"] == "project:myrepo"
        vs.client.count.assert_called_once_with(
            collection_name="mindojo",
            count_filter=Filter(
                must=[
                    FieldCondition(
                        key="user_id", match=MatchValue(value="project:myrepo")
                    )
                ]
            ),
            exact=True,
        )

    @pytest.mark.asyncio
    async def test_returns_error_on_missing_vector_store(self, _mock_memory):
        from mindojo_mcp.server import count_memories

        _mock_memory.vector_store = None

        result = await count_memories()
        parsed = json.loads(result)
        assert "error" in parsed
