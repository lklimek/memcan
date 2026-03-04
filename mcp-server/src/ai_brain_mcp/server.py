"""AI Brain MCP Server — 5 tools for persistent memory via mem0.

Transport: stdio (launched by Claude Code).
"""

from __future__ import annotations

import json
import logging
from typing import Any

from mcp.server.fastmcp import FastMCP
from mem0 import Memory

from .config import settings

logger = logging.getLogger(__name__)

mcp = FastMCP(
    "ai-brain",
    instructions="Persistent memory for Claude Code — store and recall learnings, decisions, preferences across sessions.",
)

_memory: Memory | None = None


def _get_memory() -> Memory:
    """Lazy-init mem0 Memory instance."""
    global _memory  # noqa: PLW0603
    if _memory is None:
        _memory = Memory.from_config(settings.to_mem0_config())
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
def add_memory(
    content: str,
    project: str | None = None,
    user_id: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> str:
    """Store a memory — lesson learned, decision, preference, or pattern.

    Args:
        content: The memory content to store.
        project: Git repo name for project-scoped memory. Omit for global.
        user_id: Explicit user ID override.
        metadata: Optional metadata dict (e.g., {"source": "penny", "type": "lesson"}).

    Returns:
        JSON string with the stored memory result.
    """
    mem = _get_memory()
    resolved_uid = _resolve_user_id(project, user_id)
    result = mem.add(content, user_id=resolved_uid, metadata=metadata or {})
    return json.dumps(result, default=str)


@mcp.tool()
def search_memories(
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
    mem = _get_memory()
    resolved_uid = _resolve_user_id(project, user_id)
    results = mem.search(query, user_id=resolved_uid, limit=limit)
    return json.dumps(results, default=str)


@mcp.tool()
def get_memories(
    project: str | None = None,
    user_id: str | None = None,
) -> str:
    """List all memories for a given scope.

    Args:
        project: Git repo name for project-scoped listing. Omit for global.
        user_id: Explicit user ID override.

    Returns:
        JSON array of all memories in the scope.
    """
    mem = _get_memory()
    resolved_uid = _resolve_user_id(project, user_id)
    results = mem.get_all(user_id=resolved_uid)
    return json.dumps(results, default=str)


@mcp.tool()
def delete_memory(memory_id: str) -> str:
    """Delete a specific memory by ID.

    Args:
        memory_id: The ID of the memory to delete.

    Returns:
        JSON confirmation of deletion.
    """
    mem = _get_memory()
    mem.delete(memory_id)
    return json.dumps({"status": "deleted", "memory_id": memory_id})


@mcp.tool()
def update_memory(memory_id: str, content: str) -> str:
    """Update an existing memory's content.

    Args:
        memory_id: The ID of the memory to update.
        content: New content for the memory.

    Returns:
        JSON string with the update result.
    """
    mem = _get_memory()
    result = mem.update(memory_id, content)
    return json.dumps(result, default=str)


def main() -> None:
    """Entry point — run MCP server over stdio."""
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
