---
name: search-standards
description: "Search coding and security standards. Use during code reviews, security audits, architecture decisions, or when checking compliance."
allowed-tools:
  - mcp__plugin_mindojo_brain__search_standards
---

# Search Standards

Query indexed coding and security standards (OWASP, CWE, style guides, etc.).

## Procedure

1. **Determine scope** -- infer from context what kind of standard the user needs.
2. **Search** -- call `search_standards(query=<topic>)` with optional filters:
   - `standard_type` -- category filter: `"security"`, `"coding"`, `"cve"`, `"guideline"`
   - `standard_id` -- specific standard identifier (e.g. `"owasp-asvs"`, `"owasp-cheatsheets"`)
   - `ref_id` -- cross-reference ID (e.g. `"CWE-89"`, `"V5.3.4"`)
   - `tech_stack` -- technology filter (e.g. `"python"`, `"rust"`)
   - `lang` -- language filter (e.g. `"en"`)

   Use `list_collections()` to discover available filter values.
3. **Present** -- show relevant sections with their `standard_id` and `ref_id` for traceability.
