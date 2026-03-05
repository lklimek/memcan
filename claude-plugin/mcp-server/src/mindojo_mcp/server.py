"""MindOJO MCP Server — 6 tools for persistent memory via mem0.

Transport: stdio (launched by Claude Code).
"""

from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP
from mem0 import AsyncMemory

from .config import ensure_nothink_model, settings

logger = logging.getLogger(__name__)

mcp = FastMCP(
    "mindojo",
    instructions="Persistent memory for Claude Code — store and recall learnings, decisions, preferences across sessions.",
)

_memory: AsyncMemory | None = None


async def _get_memory() -> AsyncMemory:
    """Lazy-init mem0 AsyncMemory instance."""
    global _memory  # noqa: PLW0603
    if _memory is None:
        await ensure_nothink_model()
        _memory = await AsyncMemory.from_config(settings.to_mem0_config())
    return _memory


def _resolve_user_id(project: str | None, user_id: str | None) -> str:
    """Determine the user_id for mem0 scoping.

    Priority: explicit user_id > project:<name> > settings.default_user_id.
    """
    if user_id:
        return user_id
    if project:
        return f"project:{project}"
    return settings.default_user_id


@mcp.tool()
async def add_memory(
    memory: str,
    project: str | None = None,
    user_id: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> str:
    """Store a memory — lesson learned, decision, preference, or pattern.

    Args:
        memory: The memory content to store.
        project: Git repo name for project-scoped memory. Omit for global.
        user_id: Explicit user ID override.
        metadata: Optional metadata dict (e.g., {"source": "penny", "type": "lesson"}).

    Returns:
        JSON string with queued status (write happens in background).
    """
    mem = await _get_memory()
    uid = _resolve_user_id(project, user_id)
    meta = metadata or {}
    logger.info("add_memory: queued for user_id=%s, len=%d", uid, len(memory))

    async def _do_add() -> None:
        max_attempts = 1  # bump to 3 if nothink model proves insufficient
        for attempt in range(1, max_attempts + 1):
            try:
                result = await mem.add(memory, user_id=uid, metadata=meta)
                entries = (result or {}).get("results", [])
                if entries:
                    logger.info(
                        "add_memory: persisted for user_id=%s (attempt %d/%d)",
                        uid,
                        attempt,
                        max_attempts,
                    )
                    return
                logger.warning(
                    "add_memory: empty result for user_id=%s (attempt %d/%d)",
                    uid,
                    attempt,
                    max_attempts,
                )
            except Exception:
                logger.exception(
                    "add_memory: failed for user_id=%s (attempt %d/%d)",
                    uid,
                    attempt,
                    max_attempts,
                )
        logger.error(
            "add_memory: gave up after %d attempts for user_id=%s", max_attempts, uid
        )

    asyncio.create_task(_do_add())
    return json.dumps({"status": "queued", "user_id": uid})


@mcp.tool()
async def search_memories(
    query: str,
    project: str | None = None,
    user_id: str | None = None,
    limit: int = 10,
) -> str:
    """Semantic search across stored memories.

    Args:
        query: Natural language search query.
        project: Git repo name to scope search. Omit for global.
        user_id: Explicit user ID override.
        limit: Max results to return (default 10).

    Returns:
        JSON array of matching memories with scores.
    """
    limit = max(1, min(limit, 1000))
    mem = await _get_memory()
    uid = _resolve_user_id(project, user_id)
    logger.info("search_memories: query=%r user_id=%s limit=%d", query, uid, limit)
    results = await mem.search(query, user_id=uid, limit=limit)
    return json.dumps(results, default=str)


@mcp.tool()
async def get_memories(
    project: str | None = None,
    user_id: str | None = None,
    limit: int = 100,
) -> str:
    """List memories for a given scope (up to limit).

    Args:
        project: Git repo name for project-scoped listing. Omit for global.
        user_id: Explicit user ID override.
        limit: Max memories to return (default 100).

    Returns:
        JSON array of memories in the scope (capped by limit).
    """
    limit = max(1, min(limit, 1000))
    mem = await _get_memory()
    uid = _resolve_user_id(project, user_id)
    logger.info("get_memories: user_id=%s limit=%d", uid, limit)
    results = await mem.get_all(user_id=uid, limit=limit)
    return json.dumps(results, default=str)


@mcp.tool()
async def count_memories(
    project: str | None = None,
    user_id: str | None = None,
) -> str:
    """Count total memories for a given scope.

    Efficient count without fetching memory content. Useful for reporting
    collection size without the overhead of get_memories.

    Args:
        project: Git repo name for project-scoped count. Omit for global.
        user_id: Explicit user ID override.

    Returns:
        JSON string with count and user_id.
    """
    mem = await _get_memory()
    uid = _resolve_user_id(project, user_id)
    logger.info("count_memories: user_id=%s", uid)

    # NOTE: Reaches into mem0 internals (vector_store.client) for efficient
    # counting. May break on mem0 upgrades — pin mem0 version.
    try:
        from qdrant_client.models import FieldCondition, Filter, MatchValue

        vs = mem.vector_store
        qfilter = Filter(
            must=[FieldCondition(key="user_id", match=MatchValue(value=uid))]
        )
        result = await asyncio.wait_for(
            asyncio.to_thread(
                vs.client.count,
                collection_name=vs.collection_name,
                count_filter=qfilter,
                exact=True,
            ),
            timeout=30.0,
        )
    except AttributeError:
        logger.exception("count_memories failed — mem0 internals may have changed")
        return json.dumps(
            {"error": "count_memories unavailable — mem0 internal API changed"}
        )
    except TimeoutError:
        logger.warning("count_memories timed out after 30s")
        return json.dumps({"error": "count_memories timed out"})
    logger.info("count_memories: user_id=%s count=%d", uid, result.count)
    return json.dumps({"count": result.count, "user_id": uid})


@mcp.tool()
async def delete_memory(memory_id: str) -> str:
    """Delete a specific memory by ID.

    Args:
        memory_id: The ID of the memory to delete.

    Returns:
        JSON confirmation of deletion.
    """
    logger.info("delete_memory: id=%s", memory_id)
    mem = await _get_memory()
    await mem.delete(memory_id)
    return json.dumps({"status": "deleted", "memory_id": memory_id})


@mcp.tool()
async def update_memory(memory_id: str, memory: str) -> str:
    """Update an existing memory's content.

    Args:
        memory_id: The ID of the memory to update.
        memory: New content for the memory.

    Returns:
        JSON string with the update result.
    """
    logger.info("update_memory: id=%s", memory_id)
    mem = await _get_memory()
    result = await mem.update(memory_id, memory)
    return json.dumps(result, default=str)


def _setup_logging() -> None:
    """Configure file logging if LOG_FILE is set."""
    if not settings.log_file:
        return
    log_path = Path(settings.log_file)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    logging.basicConfig(
        filename=str(log_path),
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )
    logger.info("MindOJO MCP server starting, log_file=%s", settings.log_file)


def main() -> None:
    """Entry point — run MCP server over stdio."""
    _setup_logging()
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
