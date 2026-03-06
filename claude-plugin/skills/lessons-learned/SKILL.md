---
name: lessons-learned
description: "Extract and save learnings from conversation. Invoke at session start, before presenting plan, after notable events, before decisions, when changing plan, and as the final task when all work is complete."
---

# Lessons Learned

Extract and persist learnings from the current conversation. Runs unattended — no user approval needed.

## Phase 1 — Gather

Scan conversation history for items worth remembering:

- Lessons learned — bugs, pitfalls, failed approaches, workarounds
- Architecture decisions — choices made and their rationale
- Direction changes — when the user corrected course, rejected an approach, or redirected priorities (record both the original direction and why it changed)
- User preferences — coding style, tool choices, conventions
- Patterns and anti-patterns — recurring solutions or mistakes
- Configuration quirks — project-specific setup, environment gotchas

Collect as a raw numbered list. Search existing memories (`search_memories`) and drop duplicates.

## Phase 2 — Categorize and Save

For each item:

1. Assign **scope**: global (useful across projects) or project-scoped (set `project` to git remote origin repo name (e.g., `mindojo` not `mindojo-2`))
2. Assign **type**: `lesson`, `decision`, or `preference`
3. Save via `add_memory` with `metadata={"type": "<type>", "source": "lessons-learned"}`

Log each save briefly: scope, type, one-line summary.

## MCP Tools

| Tool | Example |
|------|---------|
| `search_memories` | `search_memories(query="docker cache", project="penny", limit=5)` |
| `add_memory` | `add_memory(memory="...", project="penny", metadata={"type": "lesson"})` |
| `update_memory` | `update_memory(memory_id="<uuid>", memory="...")` |
| `count_memories` | `count_memories(project="penny")` or `count_memories()` for global |

## Best Practices

1. **Be specific** — "Axum `.layer()` ordering: last added = outermost" beats "middleware ordering matters"
2. **Include context** — mention framework, language, project when relevant
3. **Tag metadata** — `type` (lesson/decision/preference), `source` (lessons-learned)
4. **Don't duplicate** — search before adding; update existing memories if needed
