# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). This project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **MCP `search` tool**: `metadata` field no longer duplicates the `data` key — `data` is available only at the top level of each result.
- **MCP `search` tool**: Result `data` excerpts are capped at 500 content characters (a trailing `…` is appended when truncated). Truncation is character-safe (no mid-codepoint cuts).
- **Ingestion pipeline**: Fact truncation in `validate_facts` was byte-slicing (`f[..2000]`), which panics on multibyte UTF-8 input. Now uses character-safe iteration (`chars().take(2000)`).
- **Security**: Bumped transitive `rustls-webpki` 0.103.10 → 0.103.13 to clear three advisories: RUSTSEC-2026-0098 (URI name constraints), RUSTSEC-2026-0099 (wildcard name constraints), RUSTSEC-2026-0104 (reachable CRL-parsing panic).
- **`ollama_rs` API**: Replaced deprecated `Ollama::new()` with `Ollama::builder().host().port().build()` (deprecated since ollama-rs 0.3.5).

### Changed

- **MCP `search` tool**: `limit` parameter is now a **global cap** on total merged results across all collections (was per-collection). Results are merged by relevance score before the limit is applied.
- **Dependencies**: Refreshed all workspace dependencies via `cargo update`. Notable updates: `ollama-rs` 0.3.3 → 0.3.5, `zerocopy` 0.8.47 → 0.8.52, `zeroize` 1.8.2 → 1.9.0, `tower-http` 0.6.x → 0.6.11.
- **`audit.toml`**: Removed stale `number_prefix` ignore entry (dep no longer in tree); added `proc-macro-error2` RUSTSEC-2026-0173 (unmaintained, transitive via lance → jsonb → jiff → defmt-macros).

## [0.38.0] - 2026-03-17

### Added

- Export, import, and remote code indexing — new MCP tools: `export_collection`, `_import_records`, `index_code_files` (b61c4d2)
- New CLI commands on thin client: `export`, `import`, `index-code`, `index-standards` (b61c4d2)
- Tool param signatures in skill MCP tables; `allowed-tools` to lessons-learned (4238338)

### Removed

- `lessons-learned` skill — moved to claudius plugin, which owns classification logic (937f7d4)

### Changed

- Strip classification logic — memcan becomes pure execution layer (937f7d4)
- `remember` and `recall` skills simplified to pure executors
- Native ARM64 Docker + conditional crates.io publish (1095a40)
- Set traefik v3 and ollama latest in docker compose (de0872b)

## [0.36.0] - 2026-03-16

### Added

- Per-project TODO lists — `add_todo`, `list_todos`, `update_todo`, `complete_todo`, `delete_todo` MCP tools
- `todo` skill for managing TODO items across sessions
- Collection export to JSONL format (paginated scroll, no vectors)
- JSONL import with re-embedding (no LLM processing)

### Changed

- Native ARM64 Docker + conditional crates.io publish
- Set traefik v3 and ollama latest in docker compose
