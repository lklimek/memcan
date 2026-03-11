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

Collect as a raw numbered list. Search existing knowledge (`search`) and drop duplicates.

## Quality Gate — apply before Phase 2

Every candidate memory must pass ALL of these criteria. Drop items that fail any one:

- **Self-contained**: makes sense without surrounding conversation context
- **Specific**: names the tool, library, setting, or API involved
- **Actionable**: a future session can use this to avoid a mistake or make a decision
- **Not ephemeral**: won't be invalidated by the next commit, test run, or deploy

Good memories:

- "ollama-rs parse_host_port() drops URL path component — OLLAMA_HOST with /v1 base path silently loses it"
- "LanceDB compact_files() requires write lock — concurrent compaction causes silent data loss"
- "Do not use `git add -A` in hooks — it stages unrelated files in monorepo worktrees"

Bad memories (reject these):

- "All 79 tests pass" — ephemeral status, not a lesson
- "Well-structured error handling" — vague praise, no actionable detail
- "File created: /path/to/foo.rs" — file notification, not a lesson
- "Commit 810b83c" — opaque reference, useless without context
- "serde = ^1.0" — manifest fact, already in Cargo.toml

Tone: factual, third-person, present tense. Pattern: "[Subject]: [what happened/what to do] — [why/context]". No first person, no vague qualifiers.

## Opportunistic Cleanup

During Phase 1 dedup searches, you will encounter existing memories. If any fail the Quality Gate above, fix them on the spot:

- **Vague or context-dependent** → `update_memory` to make self-contained and specific
- **Ephemeral or obsolete** (superseded by code changes, no longer relevant) → `delete_memory`
- **Near-duplicate of a better memory** → `delete_memory` the weaker one

Do NOT search deliberately for bad memories. Only act on what surfaces during normal dedup checks.

Log each cleanup action briefly: `updated <id> (reason)` or `deleted <id> (reason)`.

## Phase 2 — Categorize and Save

For each item:

1. Assign **scope**: global (useful across projects) or project-scoped (set `project` to git remote origin repo name (e.g., `memcan` not `memcan-2`))
2. Assign **type**: `lesson`, `decision`, or `preference`
3. Save via `add_memory` with `metadata={"type": "<type>", "source": "lessons-learned"}`

Log each save briefly: scope, type, one-line summary.

## MCP Tools

| Tool | Use | Example |
|------|-----|---------|
| `search` | **Default.** Dedup check across all collections. | `search(query="docker cache", project="penny")` |
| `search_memories` | Advanced: memories-only scoped search. | `search_memories(query="docker cache", project="penny", limit=5)` |
| `add_memory` | Save a new memory. | `add_memory(memory="...", project="penny", metadata={"type": "lesson"})` |
| `update_memory` | Update an existing memory. | `update_memory(memory_id="<uuid>", memory="...")` |
| `delete_memory` | Delete a low-quality memory. | `delete_memory(memory_id="<uuid>")` |
| `count_memories` | Count memories. | `count_memories(project="penny")` |

## Best Practices

1. **Be specific** — "Axum `.layer()` ordering: last added = outermost" beats "middleware ordering matters"
2. **Include context** — mention framework, language, project when relevant
3. **Tag metadata** — `type` (lesson/decision/preference), `source` (lessons-learned)
4. **Don't duplicate** — search before adding; update existing memories if needed
