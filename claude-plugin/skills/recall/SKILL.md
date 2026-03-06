---
name: recall
description: "Search and retrieve memories. Use when preparing plan, before architecture decisions, on errors, during reviews, before dispatching agents, or when context from past sessions is needed."
allowed-tools:
  - mcp__plugin_mindojo_brain__search_memories
  - mcp__plugin_mindojo_brain__get_memories
  - mcp__plugin_mindojo_brain__count_memories
---

# Recall

Search and retrieve memories from past sessions.

## Procedure

1. **Determine query** -- extract the key topic, error message, or concept to search for.
2. **Search both scopes** -- run `search_memories` twice: once with `project` set to git remote origin repo name (e.g., `mindojo` not `mindojo-2`), once without (global). Use `limit=5` each.
3. **List all** (optional) -- if search returns few results and broader context is needed, use `get_memories(project=...)` and/or `get_memories()` for full listings.
4. **Report** -- present relevant memories to the conversation. Include memory IDs for reference.

## When to Invoke

- Session start -- load project context and recent decisions
- Before architecture decisions -- check for prior art or known pitfalls
- On errors -- search for similar past issues and fixes
- During reviews -- recall coding conventions and preferences
- Before planning -- check for relevant past experience

## MCP Tools

| Tool | Example |
|------|---------|
| `search_memories` | `search_memories(query="docker cache", project="penny", limit=5)` |
| `get_memories` | `get_memories(project="penny", limit=50)` or `get_memories()` for global |
| `count_memories` | `count_memories(project="penny")` or `count_memories()` for global |
