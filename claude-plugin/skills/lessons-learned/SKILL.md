---
name: lessons-learned
description: "Load past experience and memory and extract learnings from conversation. Invoke at session start, before presenting plan, after notable events, before decisions, when changing plan, and as the final task when all work is complete. 
---

# Lessons Learned

Extract and persist learnings from the current conversation in three phases.

## Phase 1 — Gather

Scan conversation history for items worth remembering:

- Lessons learned — bugs, pitfalls, failed approaches, workarounds
- Architecture decisions — choices made and their rationale
- User preferences — coding style, tool choices, conventions
- Patterns and anti-patterns — recurring solutions or mistakes
- Configuration quirks — project-specific setup, environment gotchas

Collect as a raw numbered list. Search existing memories (`search_memories`) and drop duplicates. Do NOT save anything yet.

## Phase 2 — Categorize

For each item, assign **scope** and **type**.

Scope (default = global):
- **Global** — language/framework knowledge, tooling tips, general patterns, debugging techniques, workflow preferences. Anything useful across projects.
- **Project-scoped** — project-specific config, architecture unique to this repo, repo-specific conventions that would not apply elsewhere. Set `project` to git repo basename.

Type: `lesson`, `decision`, or `preference`.

Present the categorized list to the user via `AskUserQuestion`. Format each item as:

```
1. <summary> — 🌍 global / lesson
2. <summary> — 📁 project / decision
```

The user may adjust scopes, types, or remove items before proceeding.

## Phase 3 — Save

For each approved item, invoke the `remember` skill with the determined scope and type.

## MCP Tools

| Tool | Example |
|------|---------|
| `search_memories` | `search_memories(query="docker cache", project="penny", limit=5)` |
| `get_memories` | `get_memories(project="penny")` or `get_memories()` for global |
| `add_memory` | `add_memory(content="...", project="penny", metadata={"type": "lesson"})` |
| `delete_memory` | `delete_memory(memory_id="<uuid>")` |
| `update_memory` | `update_memory(memory_id="<uuid>", content="...")` |

## Best Practices

1. **Be specific** — "Axum `.layer()` ordering: last added = outermost" beats "middleware ordering matters"
2. **Include context** — mention framework, language, project when relevant
3. **Tag metadata** — `type` (lesson/decision/preference), `source` (LL-NNN, PR#)
4. **Don't duplicate** — search before adding; update existing memories if needed
