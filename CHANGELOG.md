# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `delete_code_records` MCP tool — project-scoped delete over the code table, optionally narrowed by a validated `extension` or exact `file_path_exact` (no caller-supplied SQL reaches the predicate), returning the number of rows removed; the mandatory project scope prevents an unscoped wipe, and the tool is refused when the server runs without `MEMCAN_API_KEY` (#20)
- LanceDB manifest self-healing on table open — a crash during a commit can leave a zero-byte/truncated latest manifest that makes LanceDB crash-loop on open (it picks the newest version by filename without checking its contents). A filesystem preflight now quarantines such manifests aside (moved to a sibling `_corrupt_manifests/` directory, never deleted) and falls back to the last good version. Manifests are validated by their trailing `LANC` magic footer; the guard only ever touches files under `_versions/` (never data files) and never empties `_versions/` — if the sole remaining manifest is corrupt it is left in place and logged at ERROR for manual recovery (ee0d45a)
- Startup full compaction — `COMPACT_ON_STARTUP` (default `true`): on boot, before serving (the safe single-writer window), every table is fully compacted (`OptimizeAction::All`: merge data fragments + optimize indices + prune old versions). A compaction error is logged and skipped, never blocking boot. Durably collapses fragment backlogs that can otherwise exhaust file descriptors (d34941e)
- Fragment-count auto-compaction — `COMPACT_FRAGMENT_THRESHOLD` (default `64`; `0` disables): after each write on the store, a table at or above the threshold triggers a single-flight background compaction. Triggering on the persistent on-disk fragment count (rather than an in-memory per-session counter that reset every restart and never fired in production) makes it robust across restarts; a process-wide guard ensures only one compaction runs at a time (d34941e)

### Changed

- `memcan index-code` (thin CLI): an explicit `--tech-stack` now restricts walked extensions to that stack and fails with a nonzero exit on unrecognized values (previously a free-form label); names are matched case-insensitively and stored canonically lowercase. Omitting it preserves auto-detection. The `memcan-server index-code` admin path is unchanged — it indexes all supported languages and treats `--tech-stack` as a metadata label (#20)
- `LanceDbStore::compact_table` now runs full compaction (`OptimizeAction::All`) and returns `CompactionOutcome { fragments_before, fragments_after }` instead of `Result<()>`; startup compaction previously ran `OptimizeAction::Prune` only, which pruned old version manifests but never compacted data fragments (d34941e)

### Fixed

- `index-code`: exclude `.claude` worktree directories in the file walker (CLI and core) — prevents indexing stray worktree clones (#20)
- `index-code`: pace batch submission under the server queue cap and retry on "server busy" instead of silently dropping rejected batches — prevents data loss on large repos (#20)
- `index-code`: file extensions are matched case-insensitively (`Foo.RS` is indexed as Rust, not as an unknown-language fallback) (#20)
- `index_code_files`: storage-layer guard skips paths under skip-dirs (e.g. `.claude`, `target`) — counted in `skipped` and logged — even if a client sends them; absolute paths are skipped too (#20)

### Security

- Bump `rustls-webpki` to 0.103.13 — clears RUSTSEC-2026-0098, RUSTSEC-2026-0099, RUSTSEC-2026-0104 (#20)

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
