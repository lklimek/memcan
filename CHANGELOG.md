# Changelog

All notable changes to this project are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/). This project follows [Semantic Versioning](https://semver.org/).

## [2.0.1] - 2026-07-27

### Added

- `MEMCAN_BODY_READ_TIMEOUT` (default 30s) bounds how long `/mcp` waits for the next chunk of an incoming request body. The MCP transport collects a POST body in full before any session lookup or log line, with no timer of its own, so a client or proxy that opens a request and then stalls mid-body parks the call indefinitely and leaves no trace in the server log — the request simply vanishes from the session. Such a stall is now answered `408` and logged at `error` with the session ID. The bound is per-chunk and resets on every chunk received, so it never truncates a large body that keeps arriving, never limits handler execution once the body is complete (LLM-backed calls legitimately run for tens of seconds), and never touches the long-lived SSE response stream. This is hardening against the leading hypothesis for the silent-drop reports rather than a confirmed root-cause fix, and it is not a general resource bound: a client dribbling bytes indefinitely stays unbounded, since no request body size limit exists. `0` disables it.
- A `debug` line on every `/mcp` request arrival (method, session ID, content length), emitted before the body is read, so a request that reaches the process but produces no further output is distinguishable from one that never arrived. The bundled Docker Compose already runs at `memcan_server=debug`.

### Changed

- A non-numeric `MEMCAN_BODY_READ_TIMEOUT` logs a `warn!` naming the offending value instead of silently reverting to the default.

## [2.0.0] - 2026-07-27

### BREAKING

- `list_todos`, `search`, `search_memories`, `search_code`, `search_standards`, and `get_memories` MCP tools now return a wrapped object carrying a `has_more` flag instead of a bare JSON array — any consumer that assumed a top-level array from these tools must be updated. `list_todos`' default limit also changes from 50 to 100; every other tool's default is unchanged.

### Added

- `get_todo`/`update_todo`/`complete_todo`/`delete_todo` accept an unambiguous short-ID prefix (>=8 hex characters) of a TODO's UUID, not just the full UUID. Exact match is tried first (no behavior change for full IDs); zero matches keeps the existing not-found error; an overly ambiguous prefix fails with an actionable error listing the first 5 candidates, indicating with "at least 5+" wording when the true match count exceeds that cap rather than silently under-reporting it. `blocked_by` references on `add_todo`/`update_todo` are resolved and canonicalized to full UUIDs at write time, avoiding future ambiguity drift.
- `has_more` pagination indicator on `list_todos`, `search`, `search_memories`, `search_code`, `search_standards`, and `get_memories` (see BREAKING above), computed via a limit+1 over-fetch so it's correct exactly at the limit boundary, unlike a naive `returned_count == limit` check.
- Tasks web UI: light-only "paper" theme, multi-select status-filter checkboxes (defaults to hiding `done`/`cancelled`), and filter-preserving navigation between the list and detail views.

### Fixed

- `list_todos` now scans up to a 10,000-row hard cap and sorts before truncating to the caller's limit, matching `list_all_todos`'s existing pattern — previously a high-priority TODO outside the first `limit` rows in storage scan order could be silently invisible, breaking the documented priority-sorted contract.
- Traefik now emits JSON access logs (`--accesslog`), giving request outcome (status, duration) visibility end-to-end for diagnosing dropped MCP requests.
- `setup-memcan` skill's Step 5 verify pass now also checks the CLI's own version against the plugin baseline (previously only the server was checked, so the CLI could silently drift for many releases), and requires user approval before applying any fix — all detected version drift is collected and presented together via a Before → After table (mirroring Step 1's install-choice pattern) instead of auto-applying fixes as they're found.

## [1.1.0] - 2026-07-16

### Added

- `get_todo` MCP tool for fetching a single TODO item by ID.
- `owner` and `blocked_by` fields on TODO items, accepted by both `add_todo` and `update_todo`.
- TODO statuses `in_progress`, `blocked`, `postponed`, and `cancelled`, expanding the status set to six values alongside `pending` and `done`.
- Read-only Tasks web UI (`memcan-server`) — `GET /ui` (redirects to `/ui/tasks`), `GET /ui/tasks` (filterable/sortable list), `GET /ui/tasks/{id}` (detail, with `blocked_by` cross-links). Server-rendered, auto-escaping. Mounted only when both `MEMCAN_WEBUI_USERNAME` and `MEMCAN_WEBUI_PASSWORD` are set (opt-in, fail-closed single shared HTTP Basic-Auth account); Docker Compose gives it a dedicated Traefik router that excludes the Bearer-token API middleware.
- OpenRouter LLM backend with a runtime-selectable primary/fallback provider — `LLM_PROVIDER` (`ollama` | `openrouter`) and `LLM_FALLBACK_PROVIDER` select which backend serves each call and which one takes over on an availability fault (connection failure, timeout, 5xx); `OPENROUTER_API_KEY`, `OPENROUTER_MODEL`, and `OPENROUTER_BASE_URL` configure the OpenRouter side. `/health` reports `openrouter` as a dependency separately from `ollama`. **Note:** this adds `MemcanError::LlmUnavailable`, a new variant on the public, non-`#[non_exhaustive]` `MemcanError` enum — an external consumer with an exhaustive `match` (no wildcard arm) on `MemcanError` will need to add a arm for it.

### Changed

- **Default LLM model is now `lklimek/gemma4-text:26b-a4b-it-qat`** (was `gemma4:26b-a4b-it-qat`) — a projector-free build of the same Google QAT `Q4_0` checkpoint with the unused vision projector stripped at the manifest level. MemCan is text-only and never sends images, so the projector was pure overhead: on a 16GB card it forced partial CPU offload of the text model itself (~87% GPU residency, 56–75s cold load). The projector-free build achieves 100% GPU residency and ~7.6s cold load at identical text quality. See `docs/memcan-model-guide.html` and `ollama-models/gemma4-text-26b-a4b-it-qat/`.

## [1.0.0] - 2026-07-07

### BREAKING

- `LlmOptions` (public struct, `memcan-core`) gains a new `op: &'static str` field for per-call telemetry labeling. Source-breaking for external consumers constructing it via exhaustive struct-literal syntax instead of `..Default::default()`.

### Added

- Per-call LLM token telemetry — every LLM chat call now emits a structured `debug` line at target `memcan::llm::telemetry`, tagged with an `op` label (`fact_extraction`, `dedup`, `code_description`, `standards_metadata`). Covers both the default ollama-rs backend and the optional genai backend; the genai backend logs the `ollama::`-stripped model name so it matches what's actually sent to the provider.
- Input-budget guard for code-symbol descriptions — `description_input_budget()` queries the model's context window and caps symbol text sent to the LLM accordingly (floored at 256 chars so a tiny `num_ctx` can never zero out the budget), truncating with a marker and a single `warn!` per indexing run instead of silently overflowing the context.
- `docs/memcan-model-guide.html` — plain-language comparison guide for picking the LLM model by available VRAM.

### Changed

- **Default LLM model is now `gemma4:26b-a4b-it-qat`** (was `qwen3.5:9b`). Benchmarked against real engineering conversations through the production fact-extraction and dedup code paths: materially better junk-filtering precision and zero malformed responses, at the cost of needing a full 16GB-VRAM card (vs. qwen's ~6.6GB). Set `LLM_MODEL=qwen3.5:9b` to keep the previous, lighter model — recommended on 8GB cards. See `docs/memcan-model-guide.html` for the full comparison.
- `fact-extraction.md` / `memory-update.md` prompts sharpened for distillation quality: self-containment, don't-fragment, collapse-parallel-facts, and timeless-present-tense rules; dedup overlap detection now explicitly covers sibling-symbol claims and same-batch near-duplicates.
- Fact batches are pre-filtered with an O(n) exact-duplicate removal pass (`dedup_facts_exact`) before the LLM-backed dedup call.

### Fixed

- LLM JSON responses wrapped in a markdown code fence (observed on qwen3.5:9b, despite `format: json`) were silently dropped as a failed parse instead of being read — a live memory or dedup decision could be lost with only a `warn!` log. Fixed via a shared fence-stripping helper (`strip_code_fence`) applied at every LLM-JSON-parse call site (fact extraction, dedup, standards metadata extraction). A follow-up fix closed a real gap in that same helper: a fence whose body starts on the same line as the language tag but closes on a later line returned an empty string instead of the JSON content.
- `test-classification` CLI tool's LLM call options didn't match the production `extract_facts` path (missing `think: false`, had a stray `max_tokens` cap) — classification benchmarks run through it could silently reflect the tool's own contract bug rather than real model behavior. Now matches production exactly.
- `fact-extraction.md` collapse-parallel-facts rule was contradicted by one of its own worked examples, which still split two sibling symbols into separate facts — the rule text alone never fixed the actual splitting behavior. Removed the contradicting example and sharpened the rule with an explicit wrong/right contrast.
- `generate_description()`'s truncation cap was content-only, not content-plus-marker — output could exceed the computed budget by the marker's length. Now a genuine hard cap, with a marker-less fallback for the (production-unreachable) case where the budget can't fit the marker at all.

### Security

- Bump transitive `crossbeam-epoch` 0.9.18 → 0.9.20 (RUSTSEC-2026-0204 — invalid pointer dereference in `fmt::Pointer` for `Atomic`/`Shared`).

## [0.39.0] - 2026-06-26

### Added

- `delete_code_records` MCP tool — project-scoped delete over the code table, optionally narrowed by a validated `extension` or exact `file_path_exact` (no caller-supplied SQL reaches the predicate), returning the number of rows removed; the mandatory project scope prevents an unscoped wipe, and the tool is refused when the server runs without `MEMCAN_API_KEY` (#20)
- LanceDB manifest self-healing on table open — a crash during a commit can leave a zero-byte/truncated latest manifest that makes LanceDB crash-loop on open (it picks the newest version by filename without checking its contents). A filesystem preflight now quarantines such manifests aside (moved to a sibling `_corrupt_manifests/` directory, never deleted) and falls back to the last good version. Manifests are validated by their trailing `LANC` magic footer; the guard only ever touches files under `_versions/` (never data files) and never empties `_versions/` — if the sole remaining manifest is corrupt it is left in place and logged at ERROR for manual recovery (ee0d45a)
- Startup full compaction — `COMPACT_ON_STARTUP` (default `true`): on boot, before serving (the safe single-writer window), each over-fragmented table is fully compacted (`OptimizeAction::All`: merge data fragments + optimize indices + prune old versions). Tables already at or below `COMPACT_FRAGMENT_THRESHOLD` fragments are skipped, so an already-compact database doesn't pay a full rewrite on every restart. A compaction error is logged and skipped, never blocking boot. Durably collapses fragment backlogs that can otherwise exhaust file descriptors (d34941e)
- Fragment-count auto-compaction — `COMPACT_FRAGMENT_THRESHOLD` (default `64`; `0` disables): after each write on the store, a table at or above the threshold triggers a single-flight background compaction. Triggering on the persistent on-disk fragment count (rather than an in-memory per-session counter that reset every restart and never fired in production) makes it robust across restarts; a per-table single-flight guard, shared across all compaction entry points, ensures each table is compacted by at most one task at a time within the process (d34941e)

### Changed

- **MCP `search` tool**: `limit` parameter is now a **global cap** on total merged results across all collections (was per-collection). Results are merged by relevance score before the limit is applied.
- **`ollama_rs` API**: Replaced the deprecated `Ollama::new()` constructor (deprecated since ollama-rs 0.3.5) with `Ollama::builder().host().port().build()` on the no-API-key path; the authenticated path still uses `Ollama::new_with_request_headers` to inject the Bearer header.
- **Dependencies**: Refreshed all workspace dependencies via `cargo update`. Notable updates: `ollama-rs` 0.3.4 → 0.3.5, `zerocopy` 0.8.42 → 0.8.52, `zeroize` 1.8.2 → 1.9.0, `tower-http` 0.6.8 → 0.6.11.
- **`audit.toml`**: Removed stale `number_prefix` ignore entry (dep no longer in tree); added `proc-macro-error2` RUSTSEC-2026-0173 (unmaintained, transitive via lance → jsonb → jiff → defmt-macros).
- `memcan index-code` (thin CLI): an explicit `--tech-stack` now restricts walked extensions to that stack and fails with a nonzero exit on unrecognized values (previously a free-form label); names are matched case-insensitively and stored canonically lowercase. Omitting it preserves auto-detection. The `memcan-server index-code` admin path is unchanged — it indexes all supported languages and treats `--tech-stack` as a metadata label (#20)
- **BREAKING** (`memcan-core`): `LanceDbStore::compact_table` now runs full compaction (`OptimizeAction::All`) and returns `CompactionOutcome { fragments_before, fragments_after }` instead of `Result<()>` — a source-breaking change to the published crate, so the next release needs the matching SemVer bump. Startup compaction previously ran `OptimizeAction::Prune` only, which pruned old version manifests but never compacted data fragments (d34941e)

### Fixed

- **MCP `search` tool**: `metadata` field no longer duplicates the `data` key — `data` is available only at the top level of each result.
- **MCP `search` tool**: Result `data` excerpts are capped at 500 content characters (a trailing `…` is appended when truncated). Truncation is character-safe (no mid-codepoint cuts).
- **MCP `search` tool**: Every free-text string field in `metadata` (not just `description`) is now subject to the same 500-character cap, keeping the compact-response guarantee consistent across the full payload.
- **Ingestion pipeline**: Fact truncation in `validate_facts` was byte-slicing (`f[..2000]`), which panics on multibyte UTF-8 input. Now uses the shared character-safe `truncate_with` helper (`char_indices()`-based, appends `...`).
- **Docker**: Raised container file-descriptor limits (`nofile` soft 65536 / hard 262144) to prevent `EMFILE` errors during large LanceDB table scans.
- `index-code`: exclude `.claude` worktree directories in the file walker (CLI and core) — prevents indexing stray worktree clones (#20)
- `index-code`: pace batch submission under the server queue cap and retry on "server busy" instead of silently dropping rejected batches — prevents data loss on large repos (#20)
- `index-code`: file extensions are matched case-insensitively (`Foo.RS` is indexed as Rust, not as an unknown-language fallback) (#20)
- `index_code_files`: storage-layer guard skips paths under skip-dirs (e.g. `.claude`, `target`) — counted in `skipped` and logged — even if a client sends them; absolute paths are skipped too (#20)

### Security

- Bump transitive `rustls-webpki` 0.103.9 → 0.103.13 to clear three advisories: RUSTSEC-2026-0098 (URI name constraints), RUSTSEC-2026-0099 (wildcard name constraints), RUSTSEC-2026-0104 (reachable CRL-parsing panic) (#20)
- Bump `quinn-proto` to 0.11.15 — clears RUSTSEC-2026-0185 (high-severity remote memory exhaustion from unbounded out-of-order QUIC stream reassembly; transitive via `reqwest`) (#22)

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
