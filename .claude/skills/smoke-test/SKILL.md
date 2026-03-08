---
name: smoke-test
description: "End-to-end smoke test of all MindOJO MCP endpoints. Creates, searches, updates, deletes memories and checks logs. Use when verifying MindOJO installation or after upgrades."
user-invocable: true
---

# Smoke Test

End-to-end test of every MindOJO MCP endpoint. Uses project scope `__smoke_test__` to isolate test data. Cleans up all test memories on completion (even if earlier phases fail).

Execute each phase in order. Track pass/fail and timing for the summary table.

## Phase 0: Baseline

1. `list_collections()` -- record available collections.
2. `count_memories(project="__smoke_test__")` -- expect 0. If non-zero, note leftover data and delete it before proceeding.

## Phase 1: Create Memories

Time each `add_memory` call using `date +%s%N` before and after. Flag WARNING if any call completes in under 2 seconds (LLM distillation may be skipped or broken).

Save all returned memory IDs for later phases.

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
  memory="When configuring Ollama for remote access with authentication, several details matter. OLLAMA_HOST must include the protocol and port (e.g., https://ollama.example.com:11434). OLLAMA_API_KEY is sent as a Bearer token in the Authorization header. The genai crate does not read OLLAMA_HOST or OLLAMA_API_KEY from environment variables — MindOJO reads them from Settings and passes them explicitly via ServiceTargetResolver. A common pitfall is omitting the port number: HTTPS defaults to 443, which causes connection-refused errors if Ollama listens on 11434. Another frequent mistake is the model name format — genai requires a provider prefix like ollama::qwen3.5:4b rather than just the bare model name. Without the prefix, genai cannot route the request to the correct backend.",
  project="__smoke_test__",
  metadata={"type": "decision"}
)
```

### 1d. Very Long (~250 words)

```
add_memory(
  memory="Implementing MCP servers in Rust with the rmcp crate involves several key considerations. The #[tool] macro on async methods generates JSON Schema for tool parameters and registers handlers automatically. Each tool method receives typed parameters and returns a CallToolResult. Async operations use tokio — the MCP server runs on a tokio runtime, and tool handlers are async by default. For error handling, prefer thiserror for library error types with structured variants, and anyhow for application-level errors where you need flexible error chaining. The MCP tool handler should catch errors and return them as text content with is_error=true rather than panicking, because a panic kills the entire server process. JSON Schema generation for tool parameters requires that parameter structs derive JsonSchema (from schemars) and Deserialize. Complex nested types work but optional fields should use Option<T> with #[serde(default)] to make them truly optional in the schema. The stdio transport is the standard for Claude Code plugins — the MCP server reads JSON-RPC from stdin and writes responses to stdout. This means you cannot use println! or any stdout logging — it corrupts the MCP protocol stream. Instead, use the tracing crate with a file appender (tracing-appender or tracing-subscriber with fmt::layer().with_writer(file)). Log to a dedicated file like ~/.claude/logs/mindojo-mcp.log. For embedding operations that call fastembed, these are CPU-intensive and block the async runtime. Use tokio::task::spawn_blocking to offload them to the blocking thread pool. Without this, a single embedding computation can starve other MCP requests of executor time, causing timeouts on concurrent tool calls.",
  project="__smoke_test__",
  metadata={"type": "lesson"}
)
```

## Phase 2: Verify Creation

1. `count_memories(project="__smoke_test__")` -- must equal 4.
2. `get_memories(project="__smoke_test__", limit=10)` -- must return all 4. Record their IDs.

## Phase 3: Search

Run three searches and verify the top result matches the expected memory.

1. `search_memories(query="vector embedding dimensions", project="__smoke_test__", limit=3)` -- top result should be the medium memory (1b).
2. `search_memories(query="MCP server Rust rmcp", project="__smoke_test__", limit=3)` -- top result should be the very long memory (1d).
3. `search_memories(query="pre-allocate Vec capacity", project="__smoke_test__", limit=3)` -- top result should be the short memory (1a).

Mark each search PASS if the expected memory ranks first, WARN if it appears but not first, FAIL if absent from results.

## Phase 4: Update

1. Take the short memory ID from Phase 2.
2. Time this call (same `date +%s%N` technique):
   ```
   update_memory(
     memory_id=<short memory ID>,
     memory="Rust Vec::with_capacity pre-allocates heap space but length remains 0. Use resize() to initialize, or push() to grow. reserve() is similar but for additional capacity beyond current length."
   )
   ```
3. Flag WARNING if update completes in under 2 seconds.
4. `get_memories(project="__smoke_test__", limit=10)` -- verify the updated memory contains the new text about `reserve()`.

## Phase 5: Search After Update

1. `search_memories(query="Vec reserve capacity push", project="__smoke_test__", limit=3)` -- should find the updated memory with content mentioning `reserve()`.
2. Mark PASS if found, FAIL if not.

## Phase 6: Cleanup

1. Delete all 4 test memories by ID using `delete_memory(memory_id=<id>)`.
2. `count_memories(project="__smoke_test__")` -- must equal 0.

IMPORTANT: Execute this phase even if earlier phases failed. Use whatever IDs were collected. If IDs are unknown, use `get_memories(project="__smoke_test__", limit=50)` to find and delete any remaining test memories.

## Phase 7: Code and Standards Search

1. `search_code(query="error handling pattern", limit=3)` -- verify it returns without error. Report results or "no data indexed" if empty.
2. `search_standards(query="SQL injection", limit=3)` -- verify it returns without error. Report results or "no data indexed" if empty.

These calls validate that the code and standards collections are accessible. Empty results are acceptable.

## Phase 8: Log Check

1. Run: `tail -100 ~/.claude/logs/mindojo-mcp.log`
2. Look for lines containing `ERROR` or `WARN` that appeared during the test window.
3. Report any issues found, or "no errors in logs" if clean.

## Phase 9: Summary

Print a results table with timing:

```
| Phase | Test                      | Result | Notes           |
|-------|---------------------------|--------|-----------------|
| 0     | list_collections          | PASS   |                 |
| 0     | Baseline count            | PASS   | 0 memories      |
| 1a    | Add short memory          | PASS   | 3.2s            |
| 1b    | Add medium memory         | PASS   | 4.1s            |
| 1c    | Add long memory           | PASS   | 5.0s            |
| 1d    | Add very long memory      | PASS   | 6.3s            |
| 2     | Count after create        | PASS   | 4 memories      |
| 2     | Get all memories          | PASS   | 4 returned      |
| 3a    | Search: embedding dims    | PASS   | Top = 1b        |
| 3b    | Search: MCP rmcp          | PASS   | Top = 1d        |
| 3c    | Search: Vec capacity      | PASS   | Top = 1a        |
| 4     | Update short memory       | PASS   | 3.5s            |
| 4     | Verify update content     | PASS   |                 |
| 5     | Search after update       | PASS   | Found reserve() |
| 6     | Delete all test memories  | PASS   |                 |
| 6     | Final count               | PASS   | 0 memories      |
| 7a    | search_code               | PASS   | 3 results       |
| 7b    | search_standards          | PASS   | No data indexed |
| 8     | Log check                 | PASS   | No errors       |
```

Flag any add/update operation under 2 seconds with WARNING in Notes (e.g., "1.1s WARNING: distillation may be skipped").
