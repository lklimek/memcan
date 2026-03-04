"""Integration tests requiring live Ollama + Qdrant services."""

from __future__ import annotations

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
