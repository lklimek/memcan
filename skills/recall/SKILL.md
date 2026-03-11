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

1. **Determine query** -- extract the key topic, error message, or concept to search for.
2. **Search** -- run `search(query=..., project=<repo-name>)` to search across all collections (memories, standards, code, todos) in one call. Set `project` to git remote origin repo name (e.g., `memcan` not `memcan-2`). Use `collections` param to narrow scope when needed (e.g., `collections=["standards"]`).
3. **List all** (optional) -- if search returns few results and broader context is needed, use `get_memories(project=...)` and/or `get_memories()` for full listings.
4. **Opportunistic cleanup** -- if any returned memories are vague, ephemeral, obsolete, or near-duplicates of better memories, fix them on the spot: `update_memory` to improve, or `delete_memory` to remove. Do NOT search deliberately for bad memories — only act on what surfaces during normal recall.
5. **Report** -- present relevant results to the conversation. Include memory IDs for reference.

## When to Invoke

- Session start -- load project context and recent decisions
- Before architecture decisions -- check for prior art or known pitfalls
- On errors -- search for similar past issues and fixes
- During reviews -- recall coding conventions and preferences
- Before planning -- check for relevant past experience

## MCP Tools

| Tool | Use | Example |
|------|-----|---------|
| `search` | Search all collections in one query. | `search(query="docker cache", project="penny")` |
| `get_memories` | List memories by scope. | `get_memories(project="penny", limit=50)` |
| `count_memories` | Count memories. | `count_memories(project="penny")` |
| `update_memory` | Fix a low-quality memory encountered during recall. | `update_memory(memory_id="<uuid>", memory="...")` |
| `delete_memory` | Remove an obsolete or junk memory encountered during recall. | `delete_memory(memory_id="<uuid>")` |
