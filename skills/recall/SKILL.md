---
name: recall
description: "Search and retrieve memories. Use when preparing plan, before architecture decisions, on errors, during reviews, before dispatching agents, or when context from past sessions is needed."
allowed-tools:
  - mcp__plugin_memcan_brain__search
  - mcp__plugin_memcan_brain__search_memories
  - mcp__plugin_memcan_brain__search_standards
  - mcp__plugin_memcan_brain__search_code
  - mcp__plugin_memcan_brain__get_memories
  - mcp__plugin_memcan_brain__count_memories
  - mcp__plugin_memcan_brain__update_memory
  - mcp__plugin_memcan_brain__delete_memory
---

# Recall

Search and retrieve knowledge from past sessions across all collections.

## Procedure

1. **Formulate query** -- extract 2-4 keywords from the current task or user message. Build a specific search query using domain terms, error messages, API names, or tool names. If context is unclear, use keywords from the first user message.
   - Good: `"SPV bloom filter identity addresses"`, `"LanceDB compaction write lock"`, `"axum middleware auth bearer"`
   - Bad: `"project context"`, `"recent decisions"`, `"help"`, single words

2. **Task-specific search** -- run `search(query=..., project=<repo-name>)` with the query from step 1. Set `project` to git remote origin repo name (e.g., `memcan` not `memcan-2`). Use `collections` param to narrow scope when needed (e.g., `collections=["standards"]`).

3. **Broaden if needed** -- only if step 2 returned fewer than 3 results, run a second search with a broader architectural query (e.g., `"error handling conventions"`, `"module structure patterns"`). Skip this step if step 2 already returned useful results.

4. **List all** (optional) -- if both searches returned few results and broader context is needed, use `get_memories(project=...)` and/or `get_memories()` for full listings.

5. **Opportunistic cleanup** -- if any returned memories are vague, ephemeral, obsolete, or near-duplicates of better memories, fix them on the spot: `update_memory` to improve, or `delete_memory` to remove. Do NOT search deliberately for bad memories -- only act on what surfaces during normal recall.

6. **Report** -- present relevant results to the conversation. Include memory IDs for reference.

## When to Invoke

- Session start -- load project context and recent decisions
- Before architecture decisions -- check for prior art or known pitfalls
- On errors -- search for similar past issues and fixes
- During reviews -- recall coding conventions and preferences
- Before planning -- check for relevant past experience

## MCP Tools

| Tool | Params | Use |
|------|--------|-----|
| `search` | `query: str, project?: str, collections?: [str], limit?: int` | Search all collections in one query. `limit` is a **global top-N** cap across collections (merged by relevance), not per-collection. |
| `search_memories` | `query: str, project?: str, limit?: int` | Memories-only search. |
| `search_standards` | `query: str, standard_id?: str, standard_type?: str, limit?: int` | Search indexed standards. |
| `search_code` | `query: str, file_path?: str, tech_stack?: str, limit?: int` | Search indexed code. |
| `get_memories` | `project?: str, limit?: int` | List memories by scope. |
| `count_memories` | `project?: str` | Count memories. |
| `update_memory` | `memory_id: str, memory: str` | Fix a memory encountered during recall. |
| `delete_memory` | `memory_id: str` | Remove obsolete memory. |

## `search` Response Shape

Each result exposes a top-level `data` excerpt capped at ~500 characters (trailing `…` when truncated); the duplicated `metadata.data` is no longer returned, and other free-text `metadata` strings are capped at the same limit. For full untruncated content, fetch by id via `get_memories`.
