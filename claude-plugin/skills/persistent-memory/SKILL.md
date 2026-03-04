---
name: persistent-memory
description: "Use to save and recall learnings, decisions, preferences across sessions. Invoke at session start and after notable discoveries."
---

# Persistent Memory

Store and retrieve knowledge across Claude Code sessions via the MindAJO MCP server.

## When to Save

- Lessons learned (bugs, pitfalls, workarounds)
- Architecture decisions and rationale
- User preferences and conventions
- Discovered patterns and anti-patterns
- Project-specific configuration details

## When to Search

- **Session start** — recall project context, recent decisions
- **Before architecture decisions** — check for prior art or known pitfalls
- **On errors** — search for similar past issues and fixes
- **During reviews** — recall coding conventions and preferences

## Scoping

- `project` param = git repo basename (e.g., `"penny"`, `"claudius"`)
- Omit `project` for global memories (cross-project learnings)

## MCP Tools

### `add_memory`

```
add_memory(content="Docker COPY doesn't resolve symlinks in build context", project="penny", metadata={"type": "lesson", "source": "LL-002"})
```

### `search_memories`

```
search_memories(query="docker build cache issues", project="penny", limit=5)
```

### `get_memories`

```
get_memories(project="penny")       # all penny memories
get_memories()                       # all global memories
```

### `delete_memory`

```
delete_memory(memory_id="<uuid>")
```

### `update_memory`

```
update_memory(memory_id="<uuid>", content="Updated: use 0.0.0.0 not 127.0.0.1 in containers")
```

## Best Practices

1. **Be specific** — "Axum `.layer()` ordering: last added = outermost" beats "middleware ordering matters"
2. **Include context** — mention the project, framework, language
3. **Tag with metadata** — `type` (lesson/decision/preference), `source` (LL-NNN, PR#, etc.)
4. **Don't duplicate** — search before adding; update existing memories if needed
5. **Scope correctly** — project-specific stays scoped; universal patterns go global
