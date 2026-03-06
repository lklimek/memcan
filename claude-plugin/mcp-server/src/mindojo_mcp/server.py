"""MindOJO MCP Server — persistent memory via Qdrant + Ollama.

Transport: stdio (launched by Claude Code).
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP
from qdrant_client.models import (
    FieldCondition,
    Filter,
    MatchValue,
    PointStruct,
)

from .config import (
    CODE_COLLECTION,
    QDRANT_COLLECTION,
    STANDARDS_COLLECTION,
    settings,
)
from .memory_pipeline import do_add_memory, ensure_models_once
from .qdrant_utils import aembed, asearch_collection, get_qdrant

logger = logging.getLogger(__name__)

mcp = FastMCP(
    "mindojo",
    instructions="Persistent memory for Claude Code — store and recall learnings, decisions, preferences across sessions.",
)


_background_tasks: set[asyncio.Task] = set()  # prevent GC of fire-and-forget tasks


def _resolve_user_id(project: str | None, user_id: str | None) -> str:
    """Determine the user_id for scoping.

    Priority: explicit user_id > project:<name> > settings.default_user_id.
    """
    if user_id:
        return user_id
    if project:
        return f"project:{project}"
    return settings.default_user_id


# ---------------------------------------------------------------------------
# MCP Tools
# ---------------------------------------------------------------------------


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
    uid = _resolve_user_id(project, user_id)
    meta = metadata or {}
    logger.info("add_memory: queued for user_id=%s, len=%d", uid, len(memory))

    async def _do_add() -> None:
        try:
            await do_add_memory(content=memory, user_id=uid, metadata=meta)
            logger.info("add_memory: persisted for user_id=%s", uid)
        except Exception:
            logger.exception("add_memory: failed for user_id=%s", uid)

    task = asyncio.create_task(_do_add())
    _background_tasks.add(task)
    task.add_done_callback(_background_tasks.discard)
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
    uid = _resolve_user_id(project, user_id)
    logger.info("search_memories: query=%r user_id=%s limit=%d", query, uid, limit)

    await ensure_models_once()
    vectors = await aembed([query])
    qfilter = Filter(must=[FieldCondition(key="user_id", match=MatchValue(value=uid))])
    qd = get_qdrant()
    results = qd.query_points(
        collection_name=QDRANT_COLLECTION,
        query=vectors[0],
        query_filter=qfilter,
        limit=limit,
        with_payload=True,
    )

    output = []
    for point in results.points:
        payload = point.payload or {}
        entry = {
            "id": point.id,
            "memory": payload.get("data", ""),
            "hash": payload.get("hash", ""),
            "score": point.score,
            "created_at": payload.get("created_at"),
            "updated_at": payload.get("updated_at"),
            "user_id": payload.get("user_id", ""),
        }
        # Include extra metadata fields
        for k, v in payload.items():
            if k not in ("data", "hash", "created_at", "updated_at", "user_id"):
                entry[k] = v
        output.append(entry)

    return json.dumps(output, default=str)


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
    uid = _resolve_user_id(project, user_id)
    logger.info("get_memories: user_id=%s limit=%d", uid, limit)

    qfilter = Filter(must=[FieldCondition(key="user_id", match=MatchValue(value=uid))])
    qd = get_qdrant()
    points, _ = qd.scroll(
        collection_name=QDRANT_COLLECTION,
        scroll_filter=qfilter,
        limit=limit,
        with_payload=True,
    )

    output = []
    for point in points:
        payload = point.payload or {}
        entry = {
            "id": point.id,
            "memory": payload.get("data", ""),
            "hash": payload.get("hash", ""),
            "created_at": payload.get("created_at"),
            "updated_at": payload.get("updated_at"),
            "user_id": payload.get("user_id", ""),
        }
        for k, v in payload.items():
            if k not in ("data", "hash", "created_at", "updated_at", "user_id"):
                entry[k] = v
        output.append(entry)

    return json.dumps(output, default=str)


@mcp.tool()
async def count_memories(
    project: str | None = None,
    user_id: str | None = None,
) -> str:
    """Count total memories for a given scope.

    Args:
        project: Git repo name for project-scoped count. Omit for global.
        user_id: Explicit user ID override.

    Returns:
        JSON string with count and user_id.
    """
    uid = _resolve_user_id(project, user_id)
    logger.info("count_memories: user_id=%s", uid)

    qfilter = Filter(must=[FieldCondition(key="user_id", match=MatchValue(value=uid))])
    qd = get_qdrant()
    result = qd.count(
        collection_name=QDRANT_COLLECTION,
        count_filter=qfilter,
        exact=True,
    )
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
    qd = get_qdrant()
    qd.delete(
        collection_name=QDRANT_COLLECTION,
        points_selector=[memory_id],
    )
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
    qd = get_qdrant()

    existing_points = qd.retrieve(
        collection_name=QDRANT_COLLECTION,
        ids=[memory_id],
        with_payload=True,
    )

    if not existing_points:
        return json.dumps({"error": "memory not found", "memory_id": memory_id})

    existing = existing_points[0]
    old_payload = existing.payload or {}

    vectors = await aembed([memory])
    payload = {
        "data": memory,
        "hash": hashlib.md5(memory.encode()).hexdigest(),
        "user_id": old_payload.get("user_id", ""),
        "created_at": old_payload.get(
            "created_at", datetime.now(timezone.utc).isoformat()
        ),
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    # Preserve extra metadata
    for k, v in old_payload.items():
        if k not in ("data", "hash", "user_id", "created_at", "updated_at"):
            payload[k] = v

    qd.upsert(
        collection_name=QDRANT_COLLECTION,
        points=[PointStruct(id=memory_id, vector=vectors[0], payload=payload)],
    )
    return json.dumps({"status": "updated", "memory_id": memory_id}, default=str)


_COLLECTION_FACETS: dict[str, list[str]] = {
    STANDARDS_COLLECTION: [
        "standard_type",
        "standard_id",
        "version",
        "tech_stack",
        "lang",
    ],
    CODE_COLLECTION: ["project", "tech_stack"],
    QDRANT_COLLECTION: ["user_id"],
}


@mcp.tool()
async def list_collections() -> str:
    """List available data collections with point counts and filterable field values.

    Call this to discover what data is indexed and what filter values are valid
    for search_standards, search_code, and search_memories.

    Returns:
        JSON object with per-collection info: name, count, and available
        filter values (via Qdrant facet API). Collections that don't exist
        are omitted.
    """
    qd = get_qdrant()
    collections: list[dict] = []

    for name, facet_fields in _COLLECTION_FACETS.items():
        try:
            count = qd.count(collection_name=name, exact=True).count
        except Exception:
            logger.debug("list_collections: skipping %s (not found)", name)
            continue

        filters: dict[str, list[dict]] = {}
        for field in facet_fields:
            try:
                facet_resp = qd.facet(collection_name=name, key=field, limit=10)
                filters[field] = [
                    {"value": hit.value, "count": hit.count} for hit in facet_resp.hits
                ]
            except Exception:
                logger.debug("list_collections: facet %s.%s failed", name, field)

        collections.append({"name": name, "count": count, "filters": filters})

    return json.dumps({"collections": collections}, default=str)


def _empty_hint(filters: dict[str, Any]) -> dict:
    """Build a diagnostic hint object for empty search results."""
    active = {k: v for k, v in filters.items() if v is not None}
    if active:
        summary = ", ".join(f"{k}='{v}'" for k, v in active.items())
        hint = (
            f"No matches found. Applied filters: {summary}. "
            "Use list_collections() to discover valid filter values."
        )
    else:
        hint = "No semantic matches found. Try broadening your query."
    return {"results": [], "hint": hint}


@mcp.tool()
async def search_standards(
    query: str,
    standard_type: str | None = None,
    standard_id: str | None = None,
    ref_id: str | None = None,
    tech_stack: str | None = None,
    lang: str | None = None,
    limit: int = 10,
) -> str:
    """Search indexed standards (CWE, OWASP, etc.) by semantic similarity.

    Args:
        query: Natural language search query.
        standard_type: Filter by category ("security", "coding", "cve", "guideline").
        standard_id: Filter by standard ID. Use list_collections() to discover
            available values.
        ref_id: Filter by a cross-reference ID (e.g. "CWE-89", "V5.3.4").
            Matches against the ref_ids list stored on each document.
        tech_stack: Filter by technology stack (e.g. "python", "rust").
        lang: Filter by language code (e.g. "en").
        limit: Max results (1-100, default 10).

    Returns:
        JSON array of matching standards, each with:
        - score (float): semantic similarity 0-1
        - data (str): section content text
        - standard_id (str): e.g. "owasp-asvs"
        - standard_type (str): e.g. "security"
        - section_id (str): e.g. "V2.1.1"
        - section_title (str): section heading
        - chapter (str): parent chapter
        - ref_ids (list[str]): cross-reference IDs
        - version (str): standard version
        - tech_stack (str): technology if applicable
        - lang (str): language code
        - url (str): source URL
        Returns empty object with hint when no results found.
    """
    limit = max(1, min(limit, 100))

    # Case-insensitive normalization
    if standard_type:
        standard_type = standard_type.lower()
    if standard_id:
        standard_id = standard_id.lower()

    filters: dict[str, Any] = {
        "standard_type": standard_type,
        "standard_id": standard_id,
        "ref_ids": [ref_id] if ref_id else None,
        "tech_stack": tech_stack,
        "lang": lang,
    }
    logger.info("search_standards: query=%r limit=%d", query, limit)
    results = await asearch_collection(
        collection=STANDARDS_COLLECTION,
        query=query,
        filters=filters,
        limit=limit,
    )
    if results:
        return json.dumps(results, default=str)
    return json.dumps(_empty_hint(filters), default=str)


@mcp.tool()
async def search_code(
    query: str,
    project: str | None = None,
    tech_stack: str | None = None,
    file_path: str | None = None,
    limit: int = 10,
) -> str:
    """Search indexed code snippets by semantic similarity.

    Args:
        query: Natural language search query.
        project: Filter by project name.
        tech_stack: Filter by technology stack (e.g. "python", "rust").
        file_path: Filter by source file path (substring match).
        limit: Max results (1-100, default 10).

    Returns:
        JSON array of matching code snippets, each with:
        - score (float): semantic similarity 0-1
        - data (str): code snippet content
        - project (str): project/repo name
        - tech_stack (str): technology stack
        - file_path (str): source file path
        - line_start (int): starting line number
        - line_end (int): ending line number
        Returns empty object with hint when no results found.
    """
    limit = max(1, min(limit, 100))

    if tech_stack:
        tech_stack = tech_stack.lower()
    if project:
        project = project.lower()

    filters: dict[str, Any] = {
        "project": project,
        "tech_stack": tech_stack,
        "file_path": file_path,
    }
    logger.info("search_code: query=%r limit=%d", query, limit)
    results = await asearch_collection(
        collection=CODE_COLLECTION,
        query=query,
        filters=filters,
        limit=limit,
        prefix_fields={"file_path"},
    )
    if results:
        return json.dumps(results, default=str)
    return json.dumps(_empty_hint(filters), default=str)


def _setup_logging() -> None:
    """Configure file logging if LOG_FILE is set."""
    if not settings.log_file:
        return
    log_path = Path(settings.log_file)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    handler = logging.FileHandler(str(log_path))
    handler.setLevel(logging.DEBUG)
    handler.setFormatter(
        logging.Formatter(
            "%(asctime)s %(levelname)s %(name)s %(message)s",
            datefmt="%Y-%m-%d %H:%M:%S",
        )
    )
    logging.root.addHandler(handler)
    logging.root.setLevel(logging.DEBUG)
    logger.info("MindOJO MCP server starting, log_file=%s", settings.log_file)


def main() -> None:
    """Entry point — run MCP server over stdio."""
    _setup_logging()
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
