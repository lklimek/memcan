"""Integration tests requiring live Ollama + Qdrant services."""

from __future__ import annotations

import asyncio
import time
import uuid

import pytest

pytestmark = pytest.mark.integration


@pytest.fixture()
def _settings():
    """Load settings with OLLAMA_API_KEY propagated."""
    import os

    from mindojo_mcp.config import Settings

    s = Settings()
    if s.ollama_api_key:
        os.environ.setdefault("OLLAMA_API_KEY", s.ollama_api_key)
    return s


@pytest.fixture()
def _ollama_client(_settings):
    """Return a configured httpx client for Ollama API calls."""
    import httpx

    headers = {}
    if _settings.ollama_api_key:
        headers["Authorization"] = f"Bearer {_settings.ollama_api_key}"
    return httpx.Client(base_url=_settings.ollama_url, headers=headers, timeout=120)


def test_ollama_reachable(_ollama_client):
    """Ollama responds to health check."""
    resp = _ollama_client.get("/")
    assert resp.status_code == 200


def test_ollama_models_available(_settings, _ollama_client):
    """Configured LLM and embedding models are available."""
    from mindojo_mcp.config import EMBED_MODEL, LLM_MODEL

    resp = _ollama_client.get("/api/tags")
    assert resp.status_code == 200
    model_names = {m["name"] for m in resp.json()["models"]}
    assert LLM_MODEL in model_names, f"{LLM_MODEL} not found in {model_names}"
    assert EMBED_MODEL in model_names, f"{EMBED_MODEL} not found in {model_names}"


def test_embedding_dimension_matches_config(_settings, _ollama_client):
    """Embedding vector length matches EMBED_DIMS constant."""
    from mindojo_mcp.config import EMBED_DIMS, EMBED_MODEL

    resp = _ollama_client.post(
        "/api/embed",
        json={"model": EMBED_MODEL, "input": "dimension check"},
    )
    assert resp.status_code == 200
    embeddings = resp.json()["embeddings"]
    assert len(embeddings) > 0
    assert len(embeddings[0]) == EMBED_DIMS


def test_qdrant_connection_and_collection(_settings):
    """get_qdrant() connects and the memories collection exists."""
    from mindojo_mcp.config import QDRANT_COLLECTION
    from mindojo_mcp.qdrant_utils import ensure_collection, get_qdrant

    qd = get_qdrant()
    ensure_collection(QDRANT_COLLECTION)
    collections = [c.name for c in qd.get_collections().collections]
    assert QDRANT_COLLECTION in collections


def test_memory_add_and_search_roundtrip(_settings):
    """Add a point via embed+upsert, search for it, verify match, clean up."""
    from qdrant_client.models import (
        FieldCondition,
        Filter,
        MatchValue,
        PointStruct,
    )

    from mindojo_mcp.config import QDRANT_COLLECTION
    from mindojo_mcp.qdrant_utils import embed, ensure_collection, get_qdrant

    ensure_collection(QDRANT_COLLECTION)
    qd = get_qdrant()

    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-roundtrip-{tag}"
    point_id = str(uuid.uuid4())
    content = f"Bilby integration test marker {tag}: always check the beer fridge"

    try:
        vectors = embed([content])
        payload = {
            "data": content,
            "hash": __import__("hashlib").md5(content.encode()).hexdigest(),
            "user_id": test_uid,
            "created_at": __import__("datetime")
            .datetime.now(__import__("datetime").timezone.utc)
            .isoformat(),
            "updated_at": None,
        }
        qd.upsert(
            collection_name=QDRANT_COLLECTION,
            points=[PointStruct(id=point_id, vector=vectors[0], payload=payload)],
        )

        # Search for the point
        results = qd.query_points(
            collection_name=QDRANT_COLLECTION,
            query=vectors[0],
            query_filter=Filter(
                must=[FieldCondition(key="user_id", match=MatchValue(value=test_uid))]
            ),
            limit=5,
            with_payload=True,
        )

        assert len(results.points) > 0
        top = results.points[0]
        assert top.payload["user_id"] == test_uid
        assert top.payload["data"] == content
    finally:
        qd.delete(
            collection_name=QDRANT_COLLECTION,
            points_selector=[point_id],
        )


@pytest.mark.benchmark
def test_performance_10_writes_10_reads(_settings):
    """Benchmark 10 writes and 10 reads using direct Qdrant+Ollama.

    Run with: pytest -v -s -m benchmark
    """
    import hashlib
    from datetime import datetime, timezone

    from qdrant_client.models import (
        FieldCondition,
        Filter,
        MatchValue,
        PointStruct,
    )

    from mindojo_mcp.config import QDRANT_COLLECTION
    from mindojo_mcp.qdrant_utils import embed, ensure_collection, get_qdrant

    ensure_collection(QDRANT_COLLECTION)
    qd = get_qdrant()

    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-perf-{tag}"
    point_ids: list[str] = []

    write_contents = [
        f"Performance test {tag} item {i}: {topic}"
        for i, topic in enumerate(
            [
                "quantum computing breakthroughs",
                "best practices for distributed systems",
                "memory-efficient data structures",
                "effective code review techniques",
                "container orchestration patterns",
                "real-time stream processing",
                "zero-trust security architecture",
                "observability and tracing strategies",
                "database indexing optimization",
                "API versioning conventions",
            ]
        )
    ]

    search_queries = [
        "quantum computing",
        "distributed systems",
        "data structures",
        "code review",
        "container orchestration",
        "stream processing",
        "security architecture",
        "observability",
        "database indexing",
        "API versioning",
    ]

    write_times: list[float] = []
    read_times: list[float] = []

    try:
        for content in write_contents:
            start = time.perf_counter()
            vectors = embed([content])
            pid = str(uuid.uuid4())
            point_ids.append(pid)
            payload = {
                "data": content,
                "hash": hashlib.md5(content.encode()).hexdigest(),
                "user_id": test_uid,
                "created_at": datetime.now(timezone.utc).isoformat(),
                "updated_at": None,
            }
            qd.upsert(
                collection_name=QDRANT_COLLECTION,
                points=[PointStruct(id=pid, vector=vectors[0], payload=payload)],
            )
            elapsed = time.perf_counter() - start
            write_times.append(elapsed)

        for query in search_queries:
            start = time.perf_counter()
            vectors = embed([query])
            qd.query_points(
                collection_name=QDRANT_COLLECTION,
                query=vectors[0],
                query_filter=Filter(
                    must=[
                        FieldCondition(key="user_id", match=MatchValue(value=test_uid))
                    ]
                ),
                limit=5,
                with_payload=True,
            )
            elapsed = time.perf_counter() - start
            read_times.append(elapsed)

        avg_write = sum(write_times) / len(write_times)
        avg_read = sum(read_times) / len(read_times)
        total = sum(write_times) + sum(read_times)

        print("\n" + "=" * 60)
        print("PERFORMANCE REPORT (direct Qdrant)")
        print("=" * 60)
        print("\nWrites:")
        for i, t in enumerate(write_times):
            print(f"  write[{i}] = {t:.3f}s")
        print(f"  avg write = {avg_write:.3f}s")
        print("\nReads:")
        for i, t in enumerate(read_times):
            print(f"  read[{i}] = {t:.3f}s")
        print(f"  avg read  = {avg_read:.3f}s")
        print(f"\nTotal time  = {total:.3f}s")
        print("=" * 60)

    finally:
        if point_ids:
            qd.delete(
                collection_name=QDRANT_COLLECTION,
                points_selector=point_ids,
            )


@pytest.mark.mcp_roundtrip
@pytest.mark.asyncio
async def test_mcp_async_add_memory_roundtrip(_settings):
    """Verify async fire-and-forget add_memory persists end-to-end.

    Calls the add_memory MCP tool directly, waits for background task,
    verifies the point exists in Qdrant via count with user_id filter.

    Run with: pytest -v -s -m mcp_roundtrip
    """
    from qdrant_client.models import FieldCondition, Filter, MatchValue

    from mindojo_mcp.config import QDRANT_COLLECTION
    from mindojo_mcp.qdrant_utils import ensure_collection, get_qdrant
    from mindojo_mcp.server import add_memory

    ensure_collection(QDRANT_COLLECTION)
    qd = get_qdrant()

    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-mcp-roundtrip-{tag}"
    content = f"The preferred Python version for project {tag} is 3.14"

    qfilter = Filter(
        must=[FieldCondition(key="user_id", match=MatchValue(value=test_uid))]
    )

    try:
        count_before = qd.count(
            collection_name=QDRANT_COLLECTION, count_filter=qfilter, exact=True
        ).count

        tasks_before = asyncio.all_tasks()

        result_json = await add_memory(memory=content, user_id=test_uid)
        assert '"queued"' in result_json

        bg_tasks = asyncio.all_tasks() - tasks_before - {asyncio.current_task()}
        assert len(bg_tasks) == 1, f"Expected 1 background task, got {len(bg_tasks)}"

        start = time.perf_counter()
        timeout = 120.0

        done, pending = await asyncio.wait(bg_tasks, timeout=timeout)
        if pending:
            pytest.fail(
                f"Background add_memory task still running after {timeout:.0f}s"
            )
        for t in done:
            if t.exception():
                pytest.fail(f"Background add_memory task failed: {t.exception()}")

        count_after = qd.count(
            collection_name=QDRANT_COLLECTION, count_filter=qfilter, exact=True
        ).count
        elapsed = time.perf_counter() - start

        assert count_after > count_before, (
            f"Expected count to increase: before={count_before}, after={count_after}"
        )
        print(f"\nMCP async roundtrip: memory persisted in {elapsed:.1f}s")
    finally:
        # Clean up test points
        count = qd.count(
            collection_name=QDRANT_COLLECTION, count_filter=qfilter, exact=True
        ).count
        if count > 0:
            qd.delete(collection_name=QDRANT_COLLECTION, points_selector=qfilter)
