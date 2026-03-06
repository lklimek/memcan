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
from string import Template
from typing import Any
from uuid import uuid4

from mcp.server.fastmcp import FastMCP
from qdrant_client.models import (
    FieldCondition,
    Filter,
    MatchValue,
    PointStruct,
)

from .config import (
    CODE_COLLECTION,
    LLM_MODEL,
    QDRANT_COLLECTION,
    STANDARDS_COLLECTION,
    ensure_models,
    settings,
)
from .prompts import FACT_EXTRACTION_PROMPT, MEMORY_UPDATE_PROMPT
from .qdrant_utils import _get_ollama_async, aembed, asearch_collection, get_qdrant

logger = logging.getLogger(__name__)

mcp = FastMCP(
    "mindojo",
    instructions="Persistent memory for Claude Code — store and recall learnings, decisions, preferences across sessions.",
)


_RESERVED_KEYS = frozenset({"data", "hash", "user_id", "created_at", "updated_at"})

_background_tasks: set[asyncio.Task] = set()  # prevent GC of fire-and-forget tasks

_models_checked = False


async def _ensure_models_once() -> None:
    global _models_checked
    if not _models_checked:
        await ensure_models()
        _models_checked = True


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
# Internal: LLM-based fact extraction and dedup
# ---------------------------------------------------------------------------


async def _extract_facts(content: str) -> list[str] | None:
    """LLM call #1: extract facts from content. Returns None on failure."""
    try:
        client = _get_ollama_async()
        response = await client.chat(
            model=LLM_MODEL,
            messages=[
                {"role": "system", "content": FACT_EXTRACTION_PROMPT},
                {"role": "user", "content": content},
            ],
        )
        parsed = json.loads(response.message.content)
        return parsed.get("facts", [])
    except Exception:
        logger.exception("Fact extraction failed")
        return None


async def _dedup_and_store(facts: list[str], user_id: str, metadata: dict) -> None:
    """LLM call #2: dedup against existing memories, then execute ADD/UPDATE/DELETE."""
    qd = get_qdrant()

    for fact in facts:
        vectors = await aembed([fact])
        vector = vectors[0]

        qfilter = Filter(
            must=[FieldCondition(key="user_id", match=MatchValue(value=user_id))]
        )
        search_results = qd.query_points(
            collection_name=QDRANT_COLLECTION,
            query=vector,
            query_filter=qfilter,
            limit=5,
            with_payload=True,
        )

        existing_memories = [
            {"id": str(p.id), "text": p.payload.get("data", "")}
            for p in search_results.points
        ]

        try:
            client = _get_ollama_async()
            prompt = Template(MEMORY_UPDATE_PROMPT).safe_substitute(
                existing_memories=json.dumps(existing_memories),
                new_facts=json.dumps([fact]),
            )
            response = await client.chat(
                model=LLM_MODEL,
                messages=[{"role": "user", "content": prompt}],
            )
            parsed = json.loads(response.message.content)
            events = parsed.get("events", [])
        except Exception:
            logger.exception("Dedup LLM failed, falling back to ADD")
            events = [{"type": "ADD", "data": fact}]

        meta = {k: v for k, v in metadata.items() if k not in _RESERVED_KEYS}

        for event in events:
            event_type = event.get("type", "NONE")
            if event_type == "ADD":
                point_id = str(uuid4())
                now = datetime.now(timezone.utc).isoformat()
                data = event.get("data", fact)
                payload = {
                    "data": data,
                    "hash": hashlib.md5(data.encode()).hexdigest(),
                    "user_id": user_id,
                    "created_at": now,
                    "updated_at": None,
                    **meta,
                }
                qd.upsert(
                    collection_name=QDRANT_COLLECTION,
                    points=[PointStruct(id=point_id, vector=vector, payload=payload)],
                )
            elif event_type == "UPDATE":
                memory_id = event.get("memory_id")
                existing = next(
                    (p for p in search_results.points if str(p.id) == memory_id),
                    None,
                )
                if existing is None:
                    logger.warning(
                        "UPDATE memory_id %s not in search results, skipping",
                        memory_id,
                    )
                    continue
                new_data = event.get("data", fact)
                new_vectors = await aembed([new_data])
                payload = {
                    "data": new_data,
                    "hash": hashlib.md5(new_data.encode()).hexdigest(),
                    "user_id": user_id,
                    "created_at": existing.payload.get("created_at"),
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                    **meta,
                }
                qd.upsert(
                    collection_name=QDRANT_COLLECTION,
                    points=[
                        PointStruct(
                            id=memory_id, vector=new_vectors[0], payload=payload
                        )
                    ],
                )
            elif event_type == "DELETE":
                memory_id = event.get("memory_id")
                if not any(str(p.id) == memory_id for p in search_results.points):
                    logger.warning(
                        "DELETE memory_id %s not in search results, skipping",
                        memory_id,
                    )
                    continue
                qd.delete(
                    collection_name=QDRANT_COLLECTION,
                    points_selector=[memory_id],
                )
            # NONE: no-op


async def _store_raw(content: str, user_id: str, metadata: dict) -> None:
    """Store content directly in Qdrant without LLM distillation."""
    vectors = await aembed([content])
    point_id = str(uuid4())
    now = datetime.now(timezone.utc).isoformat()
    meta = {k: v for k, v in metadata.items() if k not in _RESERVED_KEYS}
    payload = {
        "data": content,
        "hash": hashlib.md5(content.encode()).hexdigest(),
        "user_id": user_id,
        "created_at": now,
        "updated_at": None,
        **meta,
    }
    qd = get_qdrant()
    qd.upsert(
        collection_name=QDRANT_COLLECTION,
        points=[PointStruct(id=point_id, vector=vectors[0], payload=payload)],
    )


async def _do_add_memory(content: str, user_id: str, metadata: dict) -> None:
    """Orchestrate memory storage: optionally distill via LLM, then store."""
    await _ensure_models_once()

    if not settings.distill_memories:
        await _store_raw(content, user_id, metadata)
        return

    facts = await _extract_facts(content)
    if facts is None:
        await _store_raw(content, user_id, metadata)
        return
    if not facts:
        return

    await _dedup_and_store(facts, user_id, metadata)


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
            await _do_add_memory(content=memory, user_id=uid, metadata=meta)
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

    await _ensure_models_once()
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
        standard_type: Filter by standard type (e.g. "cwe", "owasp").
        standard_id: Filter by standard ID (e.g. "CWE-79").
        ref_id: Filter by referenced ID — matched against ref_ids list.
        tech_stack: Filter by technology stack (e.g. "python", "rust").
        lang: Filter by language code (e.g. "en").
        limit: Max results (1-100, default 10).

    Returns:
        JSON array of matching standards with scores.
    """
    limit = max(1, min(limit, 100))
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
    return json.dumps(results, default=str)


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
        file_path: Filter by source file path.
        limit: Max results (1-100, default 10).

    Returns:
        JSON array of matching code snippets with scores.
    """
    limit = max(1, min(limit, 100))
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
    )
    return json.dumps(results, default=str)


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
