# Prompt Templates

LLM prompts used by MemCan's pipeline and indexing modules. Loaded at compile time via `include_str!` in `prompts.rs`.

## Files

| Prompt | Constant | Used by | Purpose |
|---|---|---|---|
| `fact-extraction.md` | `FACT_EXTRACTION_PROMPT` | `pipeline.rs` (default) | Split user input into individual facts. Permissive — assumes input is pre-approved for storage. Used for explicit `add_memory` MCP tool calls. |
| `fact-extraction-hook.md` | `FACT_EXTRACTION_HOOK_PROMPT` | `serve.rs` (MCP server) | Strict variant for automatic extraction from Claude Code hook output. Whitelist-based — only 7 patterns pass. Rejects session narration, changelogs, status messages, etc. |
| `memory-update.md` | `MEMORY_UPDATE_PROMPT` | `pipeline.rs` | Deduplication engine. Compares new facts against existing memories and emits ADD/UPDATE/DELETE/NONE operations. |
| `metadata-extraction.md` | `METADATA_EXTRACTION_PROMPT` | `indexing/standards.rs` | Extract structured metadata (section ID, title, chapter, ref IDs, code patterns) from standards document chunks (OWASP, CWE, ASVS, etc.). |

## Template Variables

| Variable | Used in | Source |
|---|---|---|
| `$today` | `fact-extraction.md`, `fact-extraction-hook.md` | `render_prompt()` |
| `$existing_memories` | `memory-update.md` | `pipeline.rs` dedup stage |
| `$new_facts` | `memory-update.md` | `pipeline.rs` dedup stage |
| `$document_title` | `metadata-extraction.md` | `indexing/standards.rs` |
| `$chunk_text` | `metadata-extraction.md` | `indexing/standards.rs` |

## Testing

After modifying `fact-extraction-hook.md`, run the classification test:

```bash
memcan-server test-classification \
  --prompt rs/memcan-core/src/prompts/fact-extraction-hook.md \
  --model qwen3.5:9b \
  --data rs/memcan-core/tests/fixtures/hook-test-vectors.jsonl
```

Target: >= 90% accuracy, >= 85% precision on reject class.
