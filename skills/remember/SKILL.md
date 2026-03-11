---
name: remember
description: "Save a memory. Use when user says 'remember X' or after notable discoveries. Handles dedup and reports collection size. Used by `lessons-learned` skill for saving."
allowed-tools:
  - mcp__plugin_memcan_brain__search
  - mcp__plugin_memcan_brain__search_memories
  - mcp__plugin_memcan_brain__add_memory
  - mcp__plugin_memcan_brain__update_memory
  - mcp__plugin_memcan_brain__count_memories
---

# Remember

Quick-save a memory with automatic dedup and size reporting.

## Procedure

1. **Decide scope** — if content is clearly project-specific, set `project` to git remote origin repo name (e.g., `memcan` not `memcan-2`); otherwise omit (global).
2. **Check for duplicates** — `search(query=<content summary>, project=<if scoped>)`. If a similar memory exists, use `update_memory` instead of creating a new one.
3. **Count before** — `count_memories(project=<if scoped>)`, note the count.
4. **Save** — `add_memory(memory=<text>, project=<if scoped>, metadata={"type": "<lesson|decision|preference>", ...})`. Or `update_memory` if updating.
5. **Count after** — `count_memories(...)` again, note the new count.
6. **Report** — tell the user: what was saved/updated, scope (global or project name), memory count before and after (e.g., "5 -> 6").

## Content Guidelines

- Be specific and self-contained — the memory should make sense months later without surrounding context.
- Follow the pattern: "[Subject]: [what happened/what to do] — [why/context]"
  - Example: "ollama-rs parse_host_port(): drops URL path component — OLLAMA_HOST with base path /v1 silently loses it"
  - Example: "LanceDB FTS index: must call optimize after batch inserts — otherwise new rows are invisible to search"
- Tone: factual, third-person, present tense. No first person ("I found"), no vague qualifiers ("interesting", "well-structured").
- Always name the specific tool, library, or setting involved.
- Include "why" or "when" context, not just "what".
- Include framework/language/project when relevant.
- Tag `metadata.type`: `lesson`, `decision`, or `preference`.
