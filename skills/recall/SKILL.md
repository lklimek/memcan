---
name: recall
description: "Search and retrieve memories. Use when preparing plan, before architecture decisions, on errors, during reviews, before dispatching agents, or when context from past sessions is needed."
allowed-tools:
  - mcp__plugin_memcan_brain__search
  - mcp__plugin_memcan_brain__search_memories
  - mcp__plugin_memcan_brain__get_memories
  - mcp__plugin_memcan_brain__count_memories
---

# Recall

Search and retrieve knowledge from past sessions across all collections.

## Procedure

1. **Determine query** -- extract the key topic, error message, or concept to search for.
2. **Unified search** -- run `search(query=..., project=<repo-name>)` to search across all collections (memories, standards, code) in one call. Set `project` to git remote origin repo name (e.g., `memcan` not `memcan-2`).
3. **Scoped search** (optional) -- if you need only memories or a specific collection, use `search_memories`, `search_standards`, or `search_code` with collection-specific filters.
4. **List all** (optional) -- if search returns few results and broader context is needed, use `get_memories(project=...)` and/or `get_memories()` for full listings.
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
| `search` | **Default.** Searches all collections in one query. | `search(query="docker cache", project="penny")` |
| `search_memories` | Advanced: memories-only with scoped filtering. | `search_memories(query="docker cache", project="penny", limit=5)` |
| `get_memories` | List memories by scope. | `get_memories(project="penny", limit=50)` |
| `count_memories` | Count memories. | `count_memories(project="penny")` |
