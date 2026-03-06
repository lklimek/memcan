"""Shared memory storage pipeline — extract facts, dedup, store in Qdrant.

Used by both the MCP add_memory tool (server.py) and the SubagentStop
hook (extract_learnings.py).
"""

from __future__ import annotations

import hashlib
import json
import logging
from datetime import datetime, timezone
from string import Template
from uuid import uuid4

from qdrant_client.models import FieldCondition, Filter, MatchValue, PointStruct

from .config import LLM_MODEL, QDRANT_COLLECTION, ensure_models, settings
from .prompts import FACT_EXTRACTION_PROMPT, MEMORY_UPDATE_PROMPT
from .qdrant_utils import _get_ollama_async, aembed, get_qdrant

logger = logging.getLogger(__name__)

RESERVED_KEYS = frozenset({"data", "hash", "user_id", "created_at", "updated_at"})

_models_checked = False


async def ensure_models_once() -> None:
    """Pull Ollama models if not already checked this process."""
    global _models_checked  # noqa: PLW0603
    if not _models_checked:
        await ensure_models()
        _models_checked = True


async def extract_facts(content: str) -> list[str] | None:
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


async def dedup_and_store(facts: list[str], user_id: str, metadata: dict) -> None:
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

        meta = {k: v for k, v in metadata.items() if k not in RESERVED_KEYS}

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


async def store_raw(content: str, user_id: str, metadata: dict) -> None:
    """Store content directly in Qdrant without LLM distillation."""
    vectors = await aembed([content])
    point_id = str(uuid4())
    now = datetime.now(timezone.utc).isoformat()
    meta = {k: v for k, v in metadata.items() if k not in RESERVED_KEYS}
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


async def do_add_memory(content: str, user_id: str, metadata: dict) -> None:
    """Orchestrate memory storage: optionally distill via LLM, then store."""
    await ensure_models_once()

    if not settings.distill_memories:
        await store_raw(content, user_id, metadata)
        return

    facts = await extract_facts(content)
    if facts is None:
        await store_raw(content, user_id, metadata)
        return
    if not facts:
        return

    await dedup_and_store(facts, user_id, metadata)
