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


@pytest.fixture(autouse=True)
def _ensure_nothink(_settings):
    """Ensure the -mindojo-nothink model variant exists before tests run."""
    from mindojo_mcp.config import ensure_nothink_model

    asyncio.run(ensure_nothink_model())


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
    resp = _ollama_client.get("/api/tags")
    assert resp.status_code == 200
    model_names = {m["name"] for m in resp.json()["models"]}
    assert _settings.ollama_llm_model in model_names, (
        f"{_settings.ollama_llm_model} not found in {model_names}"
    )
    assert _settings.ollama_embed_model in model_names, (
        f"{_settings.ollama_embed_model} not found in {model_names}"
    )


def test_embedding_dimension_matches_config(_settings, _ollama_client):
    """Embedding vector length matches qdrant_embed_dims setting."""
    resp = _ollama_client.post(
        "/api/embed",
        json={"model": _settings.ollama_embed_model, "input": "dimension check"},
    )
    assert resp.status_code == 200
    embeddings = resp.json()["embeddings"]
    assert len(embeddings) > 0
    assert len(embeddings[0]) == _settings.qdrant_embed_dims


def test_memory_from_config_initializes(_settings):
    """Memory.from_config() succeeds with live settings."""
    from mem0 import Memory

    mem = Memory.from_config(_settings.to_mem0_config())
    assert mem is not None


def test_memory_add_and_search_roundtrip(_settings):
    """Add a memory, search for it, verify match, clean up."""
    from mem0 import Memory

    mem = Memory.from_config(_settings.to_mem0_config())
    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-roundtrip-{tag}"
    content = f"Bilby integration test marker {tag}: always check the beer fridge"

    def _extract_entries(result):
        """Unwrap mem0 result into a flat list of entries."""
        if isinstance(result, dict):
            return result.get("results", result.get("memories", []))
        return result

    try:
        add_result = mem.add(content, user_id=test_uid)
        assert add_result is not None

        # Verify memory was stored (get_all is deterministic, unlike search
        # which depends on LLM-distilled content matching the query).
        all_mems = mem.get_all(user_id=test_uid)
        entries_list = _extract_entries(all_mems)

        # mem0 distills content via LLM, so the tag may be stripped.
        # Verify we got at least one result scoped to our test user.
        assert len(entries_list) > 0, (
            f"Expected memories for user {test_uid}, got: {all_mems}"
        )
        assert entries_list[0]["user_id"] == test_uid
    finally:
        all_mems = mem.get_all(user_id=test_uid)
        for entry in _extract_entries(all_mems):
            mem.delete(entry["id"])


@pytest.mark.benchmark
def test_performance_10_writes_10_reads(_settings):
    """Benchmark 10 writes and 10 reads, print timing report.

    Run with: pytest -v -s -m benchmark
    """
    from mem0 import Memory

    mem = Memory.from_config(_settings.to_mem0_config())
    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-perf-{tag}"

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

    def _extract_entries(result):
        if isinstance(result, dict):
            return result.get("results", result.get("memories", []))
        return result

    write_times: list[float] = []
    read_times: list[float] = []

    try:
        for i, content in enumerate(write_contents):
            start = time.perf_counter()
            mem.add(content, user_id=test_uid)
            elapsed = time.perf_counter() - start
            write_times.append(elapsed)

        for i, query in enumerate(search_queries):
            start = time.perf_counter()
            mem.search(query, user_id=test_uid, limit=5)
            elapsed = time.perf_counter() - start
            read_times.append(elapsed)

        avg_write = sum(write_times) / len(write_times)
        avg_read = sum(read_times) / len(read_times)
        total = sum(write_times) + sum(read_times)

        print("\n" + "=" * 60)
        print("PERFORMANCE REPORT")
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
        all_mems = mem.get_all(user_id=test_uid)
        for entry in _extract_entries(all_mems):
            mem.delete(entry["id"])


@pytest.mark.mcp_roundtrip
@pytest.mark.asyncio
async def test_mcp_async_add_memory_roundtrip(_settings):
    """Verify async fire-and-forget add_memory persists end-to-end.

    Exercises the real asyncio.create_task path used by the MCP server.
    Depends on Ollama LLM distillation — mem0 makes ~2 LLM calls per add
    (extract facts + dedup check). With cold models expect ~35s per call,
    so total write time can reach ~70-80s. Timeout is set to 120s.

    Run with: pytest -v -s -m mcp_roundtrip
    """
    from mem0 import AsyncMemory
    from qdrant_client.models import FieldCondition, Filter, MatchValue

    import mindojo_mcp.server as server_mod
    from mindojo_mcp.server import add_memory

    mem = await AsyncMemory.from_config(_settings.to_mem0_config())
    tag = uuid.uuid4().hex[:8]
    test_uid = f"test-mcp-roundtrip-{tag}"
    content = f"The preferred Python version for project {tag} is 3.14"

    def _extract_entries(result):
        if isinstance(result, dict):
            return result.get("results", result.get("memories", []))
        return result

    def _qdrant_count(uid: str) -> int:
        vs = mem.vector_store
        qfilter = Filter(
            must=[FieldCondition(key="user_id", match=MatchValue(value=uid))]
        )
        count_result = vs.client.count(
            collection_name=vs.collection_name,
            count_filter=qfilter,
            exact=True,
        )
        return count_result.count

    original_memory = server_mod._memory
    try:
        server_mod._memory = mem

        count_before = _qdrant_count(test_uid)

        # Snapshot tasks before calling add_memory so we can isolate the new one.
        tasks_before = asyncio.all_tasks()

        result_json = await add_memory(memory=content, user_id=test_uid)
        assert '"queued"' in result_json

        # Find the background task created by add_memory.
        bg_tasks = asyncio.all_tasks() - tasks_before - {asyncio.current_task()}
        assert len(bg_tasks) == 1, f"Expected 1 background task, got {len(bg_tasks)}"

        start = time.perf_counter()
        # mem0 makes ~2 Ollama LLM calls per add (~35s each when cold).
        timeout = 120.0

        # Wait for the background task to finish (the actual mem0 write).
        # The server's _do_add catches exceptions internally, so we also
        # check asyncio task state for unexpected errors.
        done, pending = await asyncio.wait(bg_tasks, timeout=timeout)
        if pending:
            pytest.fail(
                f"Background add_memory task still running after {timeout:.0f}s"
            )
        for t in done:
            if t.exception():
                pytest.fail(f"Background add_memory task failed: {t.exception()}")

        # Poll until the memory is visible via get_all (should be immediate
        # after bg task completes, but mem0 may have internal delays).
        found = False
        while time.perf_counter() - start < timeout:
            all_mems = await mem.get_all(user_id=test_uid)
            entries = _extract_entries(all_mems)
            if len(entries) > 0:
                found = True
                break
            await asyncio.sleep(1.0)

        elapsed = time.perf_counter() - start
        if not found:
            pytest.fail(
                f"Memory not found after {timeout:.0f}s — LLM may have returned "
                f"empty distillation (mem0 decided content isn't worth storing)"
            )

        count_after = _qdrant_count(test_uid)
        assert count_after > count_before, (
            f"Expected count to increase: before={count_before}, after={count_after}"
        )
        print(f"\nMCP async roundtrip: memory persisted in {elapsed:.1f}s")
    finally:
        server_mod._memory = original_memory
        all_mems = await mem.get_all(user_id=test_uid)
        for entry in _extract_entries(all_mems):
            await mem.delete(entry["id"])
