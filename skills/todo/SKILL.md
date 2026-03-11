---
name: todo
description: Manage per-project TODO lists. Use when postponing work, tracking small tasks, or managing backlogs across sessions. Triggers on "add todo", "show todos", "mark done", "what's pending".
allowed-tools:
  - mcp__plugin_memcan_brain__add_todo
  - mcp__plugin_memcan_brain__list_todos
  - mcp__plugin_memcan_brain__update_todo
  - mcp__plugin_memcan_brain__complete_todo
  - mcp__plugin_memcan_brain__delete_todo
  - mcp__plugin_memcan_brain__search
---

# TODO Management

Per-project TODO lists that persist across sessions.

## Adding TODOs

```
add_todo(title="Fix auth timeout", project="backend", priority="high")
add_todo(title="Update docs", description="Add API examples", project="mylib")
```

Priority: `high`, `medium` (default), `low`.

## Listing

```
list_todos(project="backend")                    # all
list_todos(project="backend", status="pending")  # only open
list_todos(project="backend", status="done")     # completed
```

Results sorted: high priority first, then by creation date.

## Completing

```
complete_todo(todo_id="<uuid>")
```

## Updating

```
update_todo(todo_id="<uuid>", priority="high")
update_todo(todo_id="<uuid>", title="New title", description="New desc")
```

## Searching

TODOs are searchable via unified search:

```
search(query="auth timeout", collections=["todos"], project="backend")
```
