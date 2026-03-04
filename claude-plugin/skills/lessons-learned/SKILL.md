---
name: lessons-learned
description: "Extract and recall learnings from conversation history — bugs, decisions, preferences, patterns. Invoke at session start, after notable events, before decisions, and as the final task when all work is complete."
---

# Lessons Learned

Monitor conversations and recall knowledge across sessions. When something worth remembering surfaces, delegate saving to the `remember` skill.

## What to Watch For

Continuously scan conversation history, user comments, error outputs, and agent reports for:

- **Lessons learned** — bugs, pitfalls, failed approaches, workarounds
- **Architecture decisions** — choices made and their rationale
- **User preferences** — coding style, tool choices, conventions
- **Patterns and anti-patterns** — recurring solutions or recurring mistakes
- **Configuration quirks** — project-specific setup details, environment gotchas

When you spot any of these, invoke the `remember` skill to save it.

## When to Search

- **Session start** — recall project context, recent decisions
- **Before architecture decisions** — check for prior art or known pitfalls
- **On errors** — search for similar past issues and fixes
- **During reviews** — recall coding conventions and preferences

## Scoping

- `project` param = git repo basename (e.g., `"penny"`, `"claudius"`)
- Omit `project` for global memories (cross-project learnings)

## MCP Tools

### `search_memories`

```
search_memories(query="docker build cache issues", project="penny", limit=5)
```

### `get_memories`

```
get_memories(project="penny")       # all penny memories
get_memories()                       # all global memories
```

### `add_memory`

```
add_memory(content="Docker COPY doesn't resolve symlinks in build context", project="penny", metadata={"type": "lesson", "source": "LL-002"})
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
