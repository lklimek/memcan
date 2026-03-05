---
name: search-standards
description: "Search coding and security standards. Use during code reviews, security audits, architecture decisions, or when checking compliance."
allowed-tools:
  - mcp__plugin_mindojo_brain__search_standards
---

# Search Standards

Query indexed coding and security standards (OWASP, CWE, style guides, etc.).

## Procedure

1. **Determine standard type** -- infer from context: `security`, `style`, `architecture`, `testing`, etc.
2. **Search** -- call `search_standards(query=<topic>)` with optional filters:
   - `standard_type` -- e.g. "OWASP", "CWE", "PEP"
   - `standard_id` -- specific standard identifier (e.g. "OWASP-ASVS-4.0")
   - `ref_id` -- section/rule reference (e.g. "V5.3.4", "CWE-89")
   - `tech_stack` -- technology filter (e.g. "python", "rust", "kubernetes")
   - `lang` -- language filter
3. **Present** -- show relevant sections with their `standard_id` and `ref_id` for traceability.
