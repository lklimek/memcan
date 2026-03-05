"""Direct Qdrant + Ollama embedding utilities (no mem0 dependency).

Used by search MCP tools and indexing scripts.
"""

from __future__ import annotations

import logging
from typing import Any

from ollama import AsyncClient as AsyncOllamaClient
from ollama import Client as OllamaClient
from qdrant_client import QdrantClient
from qdrant_client.models import (
    Distance,
    FieldCondition,
    Filter,
    MatchAny,
    MatchValue,
    PointStruct,  # noqa: F401 — re-exported for consumers
    VectorParams,
)

from .config import EMBED_DIMS, EMBED_MODEL, settings

logger = logging.getLogger(__name__)

# --- Lazy singleton QdrantClient ---
_qdrant: QdrantClient | None = None


def get_qdrant() -> QdrantClient:
    """Return a shared QdrantClient (lazy singleton)."""
    global _qdrant  # noqa: PLW0603
    if _qdrant is None:
        _qdrant = QdrantClient(url=settings.qdrant_url)
    return _qdrant


# --- Lazy singleton Ollama clients ---
_ollama_sync: OllamaClient | None = None
_ollama_async: AsyncOllamaClient | None = None


def _get_ollama_sync() -> OllamaClient:
    global _ollama_sync  # noqa: PLW0603
    if _ollama_sync is None:
        _ollama_sync = OllamaClient(host=settings.ollama_url)
    return _ollama_sync


def _get_ollama_async() -> AsyncOllamaClient:
    global _ollama_async  # noqa: PLW0603
    if _ollama_async is None:
        _ollama_async = AsyncOllamaClient(host=settings.ollama_url)
    return _ollama_async


# --- Embedding ---


def embed(texts: list[str]) -> list[list[float]]:
    """Embed texts via Ollama (synchronous)."""
    client = _get_ollama_sync()
    results: list[list[float]] = []
    for text in texts:
        resp = client.embed(model=EMBED_MODEL, input=text)
        results.append(list(resp.embeddings[0]))
    return results


async def aembed(texts: list[str]) -> list[list[float]]:
    """Embed texts via Ollama (async)."""
    client = _get_ollama_async()
    results: list[list[float]] = []
    for text in texts:
        resp = await client.embed(model=EMBED_MODEL, input=text)
        results.append(list(resp.embeddings[0]))
    return results


# --- Collection Management ---


def ensure_collection(name: str, dims: int = EMBED_DIMS) -> None:
    """Create Qdrant collection if it doesn't exist."""
    qd = get_qdrant()
    collections = [c.name for c in qd.get_collections().collections]
    if name not in collections:
        qd.create_collection(
            collection_name=name,
            vectors_config=VectorParams(size=dims, distance=Distance.COSINE),
        )
        logger.info("Created collection %s (dims=%d)", name, dims)


def drop_by_filter(collection: str, filter: Filter) -> int:
    """Delete points matching filter. Returns count deleted."""
    qd = get_qdrant()
    before = qd.count(collection_name=collection, count_filter=filter, exact=True).count
    if before > 0:
        qd.delete(collection_name=collection, points_selector=filter)
        logger.info("Deleted %d points from %s", before, collection)
    return before


# --- Search ---


async def asearch_collection(
    collection: str,
    query: str,
    filters: dict[str, Any] | None = None,
    limit: int = 10,
) -> list[dict]:
    """Semantic search with optional payload filters.

    Args:
        collection: Qdrant collection name.
        query: Text to embed and search for.
        filters: Dict of field_name -> value for exact match filters.
            Values can be str (MatchValue) or list[str] (MatchAny).
            None values are skipped.
        limit: Max results.

    Returns:
        List of dicts with 'score' and all payload fields.
    """
    vectors = await aembed([query])
    query_vector = vectors[0]

    must_conditions: list[FieldCondition] = []
    if filters:
        for key, value in filters.items():
            if value is None:
                continue
            if isinstance(value, list):
                must_conditions.append(
                    FieldCondition(key=key, match=MatchAny(any=value))
                )
            else:
                must_conditions.append(
                    FieldCondition(key=key, match=MatchValue(value=value))
                )

    qd_filter = Filter(must=must_conditions) if must_conditions else None

    qd = get_qdrant()
    results = qd.query_points(
        collection_name=collection,
        query=query_vector,
        query_filter=qd_filter,
        limit=limit,
        with_payload=True,
    )

    return [{"score": point.score, **(point.payload or {})} for point in results.points]
