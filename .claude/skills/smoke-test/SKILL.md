---
name: smoke-test
description: "End-to-end smoke test of all MemCan MCP endpoints. Creates, searches, updates, deletes memories and checks logs. Use when verifying MemCan installation or after upgrades."
user-invocable: true
---

# Smoke Test

End-to-end test of every MemCan MCP endpoint. Uses project scope `__smoke_test__` to isolate test data. Cleans up all test memories on completion (even if earlier phases fail).

Execute each phase in order. Track pass/fail for the summary table.

## Architecture Note

MemCan uses async processing: `add_memory` and `update_memory` return immediately with `status: queued`. Background tasks handle LLM distillation. Use `get_queue_status` to monitor completion, and add short delays before verification phases.

## Phase 0: Baseline

1. `list_collections()` -- record available collections.
2. `count_memories(project="__smoke_test__")` -- expect 0. If non-zero, note leftover data and delete it before proceeding.

## Phase 1: Create Memories

Call `add_memory` for each test memory. All should return `status: queued` near-instantly (async processing). Save returned operation IDs for status tracking.

### 1a. Short (~20 words)

```
add_memory(
  memory="Rust Vec::with_capacity pre-allocates but does not initialize elements. Use resize() to also initialize.",
  project="__smoke_test__",
  metadata={"type": "lesson"}
)
```

### 1b. Medium (~60 words)

```
add_memory(
  memory="When using LanceDB with fastembed in Rust, always derive embedding dimensions from the model at startup rather than hardcoding. Different models produce different vector sizes (e.g., AllMiniLmL6V2 = 384, MultilingualE5Large = 1024). Hardcoding causes dimension mismatch errors that are cryptic and only appear at query time, not at insert time.",
  project="__smoke_test__",
  metadata={"type": "lesson"}
)
```

### 1c. Long (~120 words)

```
add_memory(
  memory="When configuring Ollama for remote access with authentication, several details matter. OLLAMA_HOST must include the protocol and port (e.g., https://ollama.example.com:11434). OLLAMA_API_KEY is sent as a Bearer token in the Authorization header. The genai crate does not read OLLAMA_HOST or OLLAMA_API_KEY from environment variables — MemCan reads them from Settings and passes them explicitly via ServiceTargetResolver. A common pitfall is omitting the port number: HTTPS defaults to 443, which causes connection-refused errors if Ollama listens on 11434. Another frequent mistake is the model name format — genai requires a provider prefix like ollama::qwen3.5:4b rather than just the bare model name. Without the prefix, genai cannot route the request to the correct backend.",
  project="__smoke_test__",
  metadata={"type": "decision"}
)
```

### 1d. Very Long (~250 words)

```
add_memory(
  memory="Implementing MCP servers in Rust with the rmcp crate involves several key considerations. The #[tool] macro on async methods generates JSON Schema for tool parameters and registers handlers automatically. Each tool method receives typed parameters and returns a CallToolResult. Async operations use tokio — the MCP server runs on a tokio runtime, and tool handlers are async by default. For error handling, prefer thiserror for library error types with structured variants, and anyhow for application-level errors where you need flexible error chaining. The MCP tool handler should catch errors and return them as text content with is_error=true rather than panicking, because a panic kills the entire server process. JSON Schema generation for tool parameters requires that parameter structs derive JsonSchema (from schemars) and Deserialize. Complex nested types work but optional fields should use Option<T> with #[serde(default)] to make them truly optional in the schema. The HTTP transport uses Streamable HTTP via axum — the MCP server mounts StreamableHttpService on a /mcp route with Bearer token auth. This means the server can handle multiple concurrent clients, unlike stdio which is single-session. For embedding operations that call fastembed, these are CPU-intensive and block the async runtime. Use tokio::task::spawn_blocking to offload them to the blocking thread pool. Without this, a single embedding computation can starve other MCP requests of executor time, causing timeouts on concurrent tool calls.",
  project="__smoke_test__",
  metadata={"type": "lesson"}
)
```

## Phase 2: Wait and Verify Creation

1. Wait 10 seconds for async processing to complete.
2. `get_queue_status()` -- check that operations are `completed` or `stored`.
3. `count_memories(project="__smoke_test__")` -- must equal 4.
4. `get_memories(project="__smoke_test__", limit=10)` -- must return all 4. Record their IDs.

If count is less than 4, wait another 10 seconds and retry. The LLM distillation step can take time depending on model speed.

## Phase 3: Search

Run three searches and verify the top result matches the expected memory.

1. `search_memories(query="vector embedding dimensions", project="__smoke_test__", limit=3)` -- top result should be the medium memory (1b).
2. `search_memories(query="MCP server Rust rmcp", project="__smoke_test__", limit=3)` -- top result should be the very long memory (1d).
3. `search_memories(query="pre-allocate Vec capacity", project="__smoke_test__", limit=3)` -- top result should be the short memory (1a).

Mark each search PASS if the expected memory ranks first, WARN if it appears but not first, FAIL if absent from results.

## Phase 4: Update

1. Take the short memory ID from Phase 2.
2. Call:
   ```
   update_memory(
     memory_id=<short memory ID>,
     memory="Rust Vec::with_capacity pre-allocates heap space but length remains 0. Use resize() to initialize, or push() to grow. reserve() is similar but for additional capacity beyond current length."
   )
   ```
3. Should return `status: queued`.
4. Wait 10 seconds, then `get_memories(project="__smoke_test__", limit=10)` -- verify the updated memory contains the new text about `reserve()`.

## Phase 5: Search After Update

1. `search_memories(query="Vec reserve capacity push", project="__smoke_test__", limit=3)` -- should find the updated memory with content mentioning `reserve()`.
2. Mark PASS if found, FAIL if not.

## Phase 6: Cleanup

1. Delete all test memories by ID using `delete_memory(memory_id=<id>)`.
2. `count_memories(project="__smoke_test__")` -- must equal 0.

IMPORTANT: Execute this phase even if earlier phases failed. Use whatever IDs were collected. If IDs are unknown, use `get_memories(project="__smoke_test__", limit=50)` to find and delete any remaining test memories.

## Phase 7: Code and Standards Search

1. `search_code(query="error handling pattern", limit=3)` -- verify it returns without error. Report results or "no data indexed" if empty.
2. `search_standards(query="SQL injection", limit=3)` -- verify it returns without error. Report results or "no data indexed" if empty.

These calls validate that the code and standards collections are accessible. Empty results are acceptable.

## Phase 8: TODO CRUD

Test the per-project TODO list tools using project `__smoke_test__`.

1. **Create** two TODOs:
   ```
   add_todo(
     title="Fix embedding dimension mismatch",
     description="Derive dimensions from model at startup instead of hardcoding",
     project="__smoke_test__",
     priority="high"
   )
   ```
   ```
   add_todo(
     title="Add retry logic for Ollama timeouts",
     project="__smoke_test__",
     priority="low"
   )
   ```
   Save returned IDs. Both should return a `TodoItem` with `status: "pending"`.

2. **List** TODOs:
   ```
   list_todos(project="__smoke_test__")
   ```
   Must return 2 items, sorted by priority (high first).

3. **Update** the low-priority TODO:
   ```
   update_todo(
     todo_id=<low priority ID>,
     priority="medium",
     description="Add exponential backoff with jitter"
   )
   ```
   Verify returned item has updated priority and description.

4. **Complete** the high-priority TODO:
   ```
   complete_todo(todo_id=<high priority ID>)
   ```
   Verify returned item has `status: "done"` and `completed_at` is set.

5. **List with filter**:
   ```
   list_todos(project="__smoke_test__", status="pending")
   ```
   Must return only 1 item (the medium-priority one).

6. **Delete** both TODOs by ID using `delete_todo(todo_id=<id>)`.

7. **Verify cleanup**:
   ```
   list_todos(project="__smoke_test__")
   ```
   Must return 0 items.

IMPORTANT: Execute this phase even if earlier phases failed. Always delete any TODOs created during testing. If IDs are unknown, use `list_todos(project="__smoke_test__")` to find and delete any remaining test TODOs.

## Phase 9: Unified Search

Test the unified `search` tool that searches across all collections simultaneously.

1. First, create a temporary test memory for search:
   ```
   add_memory(
     memory="Circuit breaker pattern: track consecutive failures per dependency. After threshold, open the breaker and skip calls for a cooldown period. Use AtomicU32 for lock-free failure counting.",
     project="__smoke_test__",
     metadata={"type": "lesson"}
   )
   ```
   Wait 10 seconds for async processing.

2. **Search all collections** (no collection filter):
   ```
   search(query="circuit breaker failure tracking", project="__smoke_test__", limit=3)
   ```
   Verify it returns results and includes the test memory from the `memories` collection. Results should have `collection` field indicating source.

3. **Search with collection filter**:
   ```
   search(query="circuit breaker failure tracking", collections=["memories"], project="__smoke_test__", limit=3)
   ```
   Verify results only come from the `memories` collection.

4. **Search standards collection** (no project filter needed):
   ```
   search(query="input validation OWASP", collections=["standards"], limit=3)
   ```
   Verify it returns results from standards or "no data indexed" gracefully.

5. **Cleanup**: Delete the test memory created in step 1. Use `get_memories(project="__smoke_test__", limit=10)` to find it, then `delete_memory`. Verify `count_memories(project="__smoke_test__")` returns 0.

IMPORTANT: Execute this phase even if earlier phases failed. Always delete any memories created during testing. If IDs are unknown, use `get_memories(project="__smoke_test__", limit=50)` to find and delete any remaining test memories.

## Phase 10: Log Check

Check server logs for errors during the test window:
- If running via Docker: `docker compose logs memcan --tail 100`
- If running locally: `tail -100 ${MEMCAN_LOG_FILE:-~/.claude/logs/memcan-mcp.log}`

Look for lines containing `ERROR` or `WARN`. Report any issues found, or "no errors in logs" if clean.

## Phase 11: Summary

Print a results table:

```
| Phase | Test                      | Result | Notes             |
|-------|---------------------------|--------|-------------------|
| 0     | list_collections          | PASS   |                   |
| 0     | Baseline count            | PASS   | 0 memories        |
| 1a    | Add short memory          | PASS   | queued            |
| 1b    | Add medium memory         | PASS   | queued            |
| 1c    | Add long memory           | PASS   | queued            |
| 1d    | Add very long memory      | PASS   | queued            |
| 2     | Queue status              | PASS   | 4/4 completed     |
| 2     | Count after create        | PASS   | 4 memories        |
| 2     | Get all memories          | PASS   | 4 returned        |
| 3a    | Search: embedding dims    | PASS   | Top = 1b          |
| 3b    | Search: MCP rmcp          | PASS   | Top = 1d          |
| 3c    | Search: Vec capacity      | PASS   | Top = 1a          |
| 4     | Update short memory       | PASS   | queued            |
| 4     | Verify update content     | PASS   |                   |
| 5     | Search after update       | PASS   | Found reserve()   |
| 6     | Delete all test memories  | PASS   |                   |
| 6     | Final count               | PASS   | 0 memories        |
| 7a    | search_code               | PASS   | 3 results         |
| 7b    | search_standards          | PASS   | No data indexed   |
| 8a    | Add 2 TODOs              | PASS   |                   |
| 8b    | List TODOs (priority)     | PASS   | High first        |
| 8c    | Update TODO               | PASS   |                   |
| 8d    | Complete TODO             | PASS   | completed_at set  |
| 8e    | List pending only         | PASS   | 1 item            |
| 8f    | Delete TODOs              | PASS   |                   |
| 8g    | Verify TODO cleanup       | PASS   | 0 items           |
| 9a    | Unified search (all)      | PASS   | Multi-collection  |
| 9b    | Unified search (filtered) | PASS   | Memories only     |
| 9c    | Unified search (standards)| PASS   |                   |
| 9d    | Unified search cleanup    | PASS   | 0 memories        |
| 10    | Log check                 | PASS   | No errors         |
```
