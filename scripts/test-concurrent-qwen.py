"""Test qwen3.5:9b stability under 5 parallel mem0 add() calls.

Sends 5 concurrent add_memory requests via mem0 AsyncMemory,
same path as the MCP server. Reports success/failure per request.

Usage:
    cd claude-plugin/mcp-server
    uv run python ../../scripts/test-concurrent-qwen.py
"""

from __future__ import annotations

import asyncio
import os
import sys
import time
import uuid

# Allow importing mindojo_mcp from the mcp-server src dir.
sys.path.insert(0, str(__file__).replace("scripts/test-concurrent-qwen.py", "")
    + "claude-plugin/mcp-server/src")

from mindojo_mcp.config import EMBED_DIMS, EMBED_MODEL, settings

# Override LLM to qwen3.5:9b for this test.
TEST_LLM = "qwen3.5:9b"

# Export API key for ollama client.
if settings.ollama_api_key:
    os.environ.setdefault("OLLAMA_API_KEY", settings.ollama_api_key)


def build_config() -> dict:
    """Build mem0 config with qwen3.5:9b as LLM."""
    return {
        "llm": {
            "provider": "ollama",
            "config": {
                "model": TEST_LLM,
                "ollama_base_url": settings.ollama_url,
            },
        },
        "embedder": {
            "provider": "ollama",
            "config": {
                "model": EMBED_MODEL,
                "ollama_base_url": settings.ollama_url,
            },
        },
        "vector_store": {
            "provider": "qdrant",
            "config": {
                "collection_name": "mindojo-memories",
                "url": settings.qdrant_url,
                "embedding_model_dims": EMBED_DIMS,
            },
        },
    }


def extract_entries(result):
    if isinstance(result, dict):
        return result.get("results", result.get("memories", []))
    return result


async def do_add(mem, idx: int, uid: str) -> dict:
    """Single add_memory call, returns result dict."""
    content = f"Concurrency test item {idx}: {TOPICS[idx]}"
    start = time.perf_counter()
    try:
        result = await mem.add(content, user_id=uid)
        elapsed = time.perf_counter() - start
        entries = extract_entries(result) if result else []
        ok = len(entries) > 0
        return {"idx": idx, "ok": ok, "entries": len(entries), "elapsed": elapsed, "error": None}
    except Exception as e:
        elapsed = time.perf_counter() - start
        return {"idx": idx, "ok": False, "entries": 0, "elapsed": elapsed, "error": str(e)}


TOPICS = [
    "Rust borrow checker prevents data races at compile time",
    "Python asyncio.gather runs coroutines concurrently not in parallel",
    "OWASP ASVS V2.1.1 requires passwords of at least 12 characters",
    "Docker layer caching depends on instruction order in Dockerfile",
    "PostgreSQL EXPLAIN ANALYZE shows actual vs estimated row counts",
]


async def main():
    from mem0 import AsyncMemory

    tag = uuid.uuid4().hex[:8]
    uid = f"test-concurrent-{tag}"
    print(f"Testing {TEST_LLM} with 5 parallel mem0 add() calls")
    print(f"User ID: {uid}")
    print(f"Ollama: {settings.ollama_url}")
    print(f"Qdrant: {settings.qdrant_url}")
    print()

    mem = await AsyncMemory.from_config(build_config())

    # Fire all 5 in parallel.
    start = time.perf_counter()
    tasks = [do_add(mem, i, uid) for i in range(5)]
    results = await asyncio.gather(*tasks)
    total = time.perf_counter() - start

    # Report.
    print("=" * 60)
    print("RESULTS")
    print("=" * 60)
    for r in results:
        status = "OK" if r["ok"] else "FAIL"
        err = f" — {r['error']}" if r["error"] else ""
        print(f"  [{status}] item {r['idx']}: {r['entries']} entries in {r['elapsed']:.1f}s{err}")

    ok_count = sum(1 for r in results if r["ok"])
    print(f"\n  {ok_count}/5 succeeded in {total:.1f}s total")

    if ok_count < 5:
        print("\n  ⚠ qwen3.5:9b still has concurrency issues!")
    else:
        print("\n  ✓ qwen3.5:9b handles 5 parallel requests correctly")

    # Cleanup.
    print("\nCleaning up test memories...")
    all_mems = await mem.get_all(user_id=uid)
    entries = extract_entries(all_mems)
    for entry in entries:
        await mem.delete(entry["id"])
    print(f"  Deleted {len(entries)} entries")
    print("=" * 60)

    sys.exit(0 if ok_count == 5 else 1)


if __name__ == "__main__":
    asyncio.run(main())
