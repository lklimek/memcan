---
name: search-code
description: "Search indexed source code. Use when looking for implementations, function signatures, patterns, or understanding how something is built across projects."
allowed-tools:
  - mcp__plugin_mindojo_brain__search_code
---

# Search Code

Query indexed source code across projects.

## Procedure

1. **Extract intent** -- identify what to search for: function name, pattern, concept, or API usage.
2. **Search** -- call `search_code(query=<description>)` with optional filters:
   - `project` -- limit to a specific repository
   - `tech_stack` -- technology filter (e.g. `"rust"`, `"python"`, `"react"`)
   - `file_path` -- filter by source file path

   Use `list_collections()` to discover available filter values.
3. **Present** -- show results with file paths and line numbers for easy navigation.
