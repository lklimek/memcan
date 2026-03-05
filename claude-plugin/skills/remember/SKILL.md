---
name: remember
description: "Save a memory. Use when user says 'remember X' or after notable discoveries. Handles dedup and reports collection size. Used by `lessons-learned` skill for saving."
allowed-tools:
  - mcp__plugin_mindojo_brain__search_memories
  - mcp__plugin_mindojo_brain__add_memory
  - mcp__plugin_mindojo_brain__update_memory
  - mcp__plugin_mindojo_brain__count_memories
---

# Remember

Quick-save a memory with automatic dedup and size reporting.

## Procedure

1. **Decide scope** — if content is clearly project-specific, set `project` to git repo basename; otherwise omit (global).
2. **Check for duplicates** — `search_memories(query=<content summary>, project=<if scoped>, limit=3)`. If a similar memory exists, use `update_memory` instead of creating a new one.
3. **Count before** — `count_memories(project=<if scoped>)`, note the count.
4. **Save** — `add_memory(memory=<text>, project=<if scoped>, metadata={"type": "<lesson|decision|preference>", ...})`. Or `update_memory` if updating.
5. **Count after** — `count_memories(...)` again, note the new count.
6. **Report** — tell the user: what was saved/updated, scope (global or project name), memory count before and after (e.g., "5 -> 6").

## Content Guidelines

- Be specific and self-contained — the memory should make sense months later without surrounding context.
- Include framework/language/project when relevant.
- Tag `metadata.type`: `lesson`, `decision`, or `preference`.
