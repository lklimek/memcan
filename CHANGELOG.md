# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `index-code`: `--tech-stack` now restricts which file extensions are walked (e.g. `--tech-stack rust` indexes only `.rs`), enabling single-language indexing of mixed-language repos (59f9008)
- `delete_code_records` MCP tool — project-scoped, optionally filtered delete over the code table (e.g. `file_path NOT LIKE '%.rs'`), returning the number of rows removed; mandatory project scope prevents an unscoped wipe

### Fixed

- `index-code`: exclude `.claude` worktree directories in the file walker (CLI and core) — prevents indexing stray worktree clones (9cf944c)
- `index-code`: pace batch submission under the server queue cap and retry on "server busy" instead of silently dropping rejected batches — prevents data loss on large repos (5776d18)
- `index-code`: an explicit but unrecognized `--tech-stack` now fails loudly with a nonzero exit instead of silently indexing every language
- `index-code`: tech-stack names and file extensions are matched case-insensitively (`--tech-stack Rust`, `Foo.RS`)
- `index_code_files`: storage-layer guard rejects paths under skip-dirs (e.g. `.claude`, `target`) even if a client sends them

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
