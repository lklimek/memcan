---
name: list-collections
description: "Discover available data collections and valid filter values. Use before searching when unsure what data exists or what filters to use."
allowed-tools:
  - mcp__plugin_mindojo_brain__list_collections
---

# List Collections

Discover what data is indexed and what filter values are valid.

## When to Use

- Before first search in a session -- discover what's available
- When a search returns empty results -- check if the filter values are valid
- When unsure which collection has the data you need

## Procedure

1. Call `list_collections()` -- no parameters needed.
2. Review the response: each collection lists its point count and available filter values with counts.
3. Use the discovered values in `search_standards`, `search_code`, or `search_memories`.
