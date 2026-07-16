---
name: todo
description: Manage per-project TODO lists. Use when postponing work, tracking small tasks, or managing backlogs across sessions. Triggers on "add todo", "show todos", "mark done", "what's pending".
allowed-tools:
  - mcp__plugin_memcan_brain__add_todo
  - mcp__plugin_memcan_brain__get_todo
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
add_todo(title="Ship v2", project="backend", owner="alice", blocked_by=["<uuid>"])
```

Priority: `high`, `medium` (default), `low`.
`owner`: free-text assignee. `blocked_by`: TODO IDs this item waits on.

## Status model

Six statuses: `pending` (default), `in_progress`, `blocked`, `postponed`, `done`, `cancelled`.
Terminal: `done`, `cancelled`. Open = everything else.

## Listing

```
list_todos(project="backend")                       # all
list_todos(project="backend", status="in_progress") # exact status match
list_todos(project="backend", owner="alice")        # by assignee
```

`status` matches ONE status exactly — there is no "all open" filter. `status="pending"` omits
`in_progress`/`blocked`/`postponed`; omit `status` and filter client-side to list every open item.

Results sorted: high priority first, then by creation date.

## Reading one

```
get_todo(todo_id="<uuid>")
```

## Completing

```
complete_todo(todo_id="<uuid>")
```

## Updating

```
update_todo(todo_id="<uuid>", priority="high")
update_todo(todo_id="<uuid>", title="New title", description="New desc")
update_todo(todo_id="<uuid>", status="blocked", owner="bob", blocked_by=["<uuid>"])
```

## Searching

TODOs are searchable via unified search:

```
search(query="auth timeout", collections=["todos"], project="backend")
```
