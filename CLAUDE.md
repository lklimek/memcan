# CLAUDE.md

## Project Overview

**MemCan** — Claude Code plugin for persistent memory via LanceDB + fastembed + ollama-rs. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Rust MCP server (rmcp), LanceDB (embedded vectors), ollama-rs (default LLM) / genai (optional), fastembed (embeddings).

Architecture: Three-crate workspace — reusable library (`memcan-core`), MCP server binary (`memcan-server`), thin CLI client (`memcan`).

## Structure

```
rs/                              # All Rust source code
  memcan-core/                  # Reusable library
  memcan-server/                # MCP server binary + admin CLI wrappers
  memcan/                       # Thin CLI client (HTTP only, no core dep, includes walk.rs for directory traversal)
Cargo.toml                       # Workspace root
Dockerfile                       # Multi-stage build for memcan-server
.claude-plugin/                  # Claude Code plugin manifest
skills/                          # Plugin skills
.github/workflows/               # CI + Release workflows
docker-compose.yml               # Traefik + memcan + optional Ollama
scripts/                         # Admin scripts (OWASP indexing, etc.)
```

## Crate Responsibilities

### memcan-core (library)

Reusable library. All domain logic lives here. Must not depend on transport, CLI, or MCP.

| Module | Responsibility |
|---|---|
| `traits` | `VectorStore`, `EmbeddingProvider`, `LlmProvider`, `TableSchema` abstractions |
| `lancedb_store` | LanceDB implementation of `VectorStore` |
| `embed` | fastembed implementation of `EmbeddingProvider` |
| `llm_ollama_rs` | **Default** `LlmProvider` backed by ollama-rs 0.3.5 (feature `ollama-rs-llm`). Token counts from `ChatMessageFinalResponseData.{prompt_eval_count, eval_count}`. |
| `llm` | `LlmProvider` backed by genai 0.5.3 (default feature `genai-llm`) serving the cloud backends: native Anthropic adapter (system-prompt cache breakpoint) or OpenAI adapter with a base-URL override. Token counts from `ChatResponse.usage.{prompt_tokens, completion_tokens, prompt_tokens_details}`. |
| `llm_telemetry` | Shared structured debug log emitter for per-call token counts; called by both LLM backends. |
| `ollama` | Ollama model resolution helpers |
| `pipeline` | Memory add pipeline (LLM fact extraction, dedup, store) |
| `query` | User ID resolution, SQL sanitization helpers |
| `search` | Unified cross-collection search (memories, standards, code, todos) |
| `text` | Shared char-safe text helpers (`truncate_with`) |
| `todo` | Per-project TODO list CRUD operations |
| `record_id` | Full-UUID / unambiguous short-ID prefix resolution shared by the TODO and memory tools (typed `AmbiguousIdPrefix` error) |
| `llm_fallback` | Primary/fallback LLM dispatch with a per-slot circuit breaker; falls back only on availability faults |
| `health` | Dependency circuit breaker (LLM primary/fallback slots, LanceDB, Embedding) |
| `export` | Collection export to JSONL format (paginated scroll, no vectors) |
| `import` | JSONL import with re-embedding (no LLM processing) |
| `indexing::code` | Language-specific symbol extraction, LLM descriptions, incremental code indexing. Input guard: `description_input_budget()` queries the model context window (falling back to the provider's assumed window, see LLM context budget) and truncates oversized symbol text via `truncate_with()` — emits `warn!` when fired. |
| `indexing::code_files` | Batch file-level code indexing from raw source content |
| `indexing::standards` | Markdown chunking, LLM metadata extraction |
| `schema` | Memcan-specific `TableSchema` implementation with filterable columns |
| `typed_table` | Strongly-typed LanceDB table handle (`TypedTable<S>`) |
| `manifest_guard` | Self-heal corrupt/empty LanceDB manifests on open (quarantine to `_corrupt_manifests/`) |
| `config` | `Settings` loading from env/files |
| `init` | `MemcanContext` bootstrap (wires all components) |
| `prompts` | LLM prompt templates |
| `error` | Error types (`MemcanError`, `VectorStoreError`) and `Result` aliases |

### memcan-server (binary)

Transport + concurrency glue. Thin wrappers around core. Must not contain domain logic.

| Component | Responsibility |
|---|---|
| MCP service (`serve.rs`) | Tool handlers (delegate to core), async queue (LRU), LLM semaphore |
| HTTP transport | axum server, Bearer token auth, health endpoint, graceful shutdown |
| stdio transport | MCP over stdin/stdout for backward compat |
| Admin CLI wrappers | Thin entry points for `index-code`, `index-standards`, `migrate`, `import-triaged`, `test-classification` — parse args, call core, report results |

### memcan (thin CLI)

HTTP client only. No `memcan-core` dependency. Communicates exclusively through MCP over HTTP.

## Server Subcommands

```
memcan-server serve [--stdio] [--listen ADDR]   # MCP server (default subcommand)
memcan-server export <collection> [-o <file>] [--filter <sql>]
memcan-server index-code <dir> --project <name> [--tech-stack <s>] [--drop]
memcan-server index-standards <file> --standard-id <id> --standard-type <t> [--drop]
memcan-server migrate <file> [--dry-run]
memcan-server import-triaged <file> [--dry-run]
memcan-server test-classification --prompt <f> --model <m>
memcan-server purge-memories --source <s> [--project <p>] [--dry-run]
memcan-server download-model [--model <name>]
memcan-server completions <shell>
```

## CLI Subcommands

```
memcan add <memory> [--project <p>]
memcan search <query> [--project <p>] [--limit <n>]
memcan extract                        # Hook handler: reads stdin, POSTs to server (NOTE: deprecated — use claudius lessons-learned skill)
memcan status [operation_id]
memcan count [--project <p>]
memcan index-standards <file> --standard-id <id> --standard-type <t> [--version <v>] [--lang <l>] [--url <u>] [--wait]
memcan index-standards --drop --standard-id <id>
memcan export <collection> [-o <file>] [--filter <sql>] [--page-size <n>]
memcan import <file> [--batch-size <n>]
memcan index-code <dir> --project <p> [--tech-stack <s>] [--batch-size <n>] [--wait]
```

## MCP Tools

Server exposes these MCP tools (via HTTP at `/mcp`):

| Tool | Description |
|---|---|
| `add_memory` | Store a memory (async, returns operation_id) |
| `search` | Unified global top-N search across memories, standards, code, and todos; returns candidate-pool `has_more` |
| `search_memories` | Semantic search across memories with `has_more` pagination |
| `get_memories` | List memories for a given scope with `has_more` pagination |
| `count_memories` | Count memories |
| `delete_memory` | Delete a memory by ID; `memory_id` accepts a full UUID (any scope) or unambiguous short-ID prefix (>= 8 chars), matched within optional `project`/`user_id` when given and across all scopes otherwise; ambiguous prefixes fail and ask for the full UUID, unknown IDs fail as not found (both invalid-params errors); IDs are trimmed and matched case-insensitively |
| `update_memory` | Update an existing memory's content (async, returns operation_id); `memory_id` (with optional `project`/`user_id`) resolves like `delete_memory`'s, and an unknown ID fails as not found |
| `list_collections` | List available collections with point counts |
| `search_standards` | Search indexed standards (CWE, OWASP, etc.) with `has_more` pagination |
| `search_code` | Search indexed code snippets with `has_more` pagination |
| `index_standards` | Index a standards document (async, returns operation_id) |
| `drop_indexed_standards` | Drop all indexed data for a standard_id |
| `add_todo` | Add a per-project TODO item, optionally setting `owner` and `blocked_by` |
| `list_todos` | List TODOs by project/owner/status with `has_more` pagination (default 100, max 200) |
| `update_todo` | Update a TODO item's fields, including `owner` and `blocked_by`; statuses are `pending`, `done`, `in_progress`, `blocked`, `postponed`, or `cancelled`; `todo_id` accepts a full UUID or unambiguous short-ID prefix (>= 8 chars), while ambiguous prefixes fail and ask for the full UUID |
| `complete_todo` | Mark a TODO item as done; `todo_id` accepts a full UUID or unambiguous short-ID prefix (>= 8 chars), while ambiguous prefixes fail and ask for the full UUID |
| `delete_todo` | Delete a TODO item; `todo_id` accepts a full UUID or unambiguous short-ID prefix (>= 8 chars), while ambiguous prefixes fail and ask for the full UUID |
| `get_todo` | Fetch a single TODO item; `todo_id` accepts a full UUID or unambiguous short-ID prefix (>= 8 chars), while ambiguous prefixes fail and ask for the full UUID |
| `export_collection` | Export a collection as JSONL (paginated, no vectors) |
| `_import_records` | Import JSONL records: embed + upsert (internal/hidden) |
| `index_code_files` | Index source code files: symbol extraction + LLM + embedding |
| `delete_code_records` | Delete code records for a project, optionally narrowed by a validated `extension` or exact `file_path_exact` (no raw SQL); requires `MEMCAN_API_KEY` |
| `get_queue_status` | Poll async operation progress |

## Versioning

**Only the team coordinator (Claudius) bumps versions.** Subagents must NOT modify `plugin.json` version.

Version lives in two places (keep in sync):
1. `.claude-plugin/plugin.json` — plugin version
2. `Cargo.toml` (workspace) — crate version

Follow [SemVer 2](https://semver.org/).

- **Major** (x.0.0): breaking changes to MCP tools, removed features, incompatible config changes
- **Minor** (0.x.0): new MCP tools, new skills, significant behavior changes
- **Patch** (0.0.x): bug fixes, doc corrections, minor tweaks

### Release Process

All release workflows trigger on **GitHub release creation**, not tag pushes.

1. Bump version in `Cargo.toml` (workspace) and `.claude-plugin/plugin.json`
2. Commit and push to `main`
3. Create a GitHub release via `gh release create vX.Y.Z --generate-notes`
4. This triggers: Release (build binaries + attach to release), Publish (crates.io), Docker (build + push image)
5. Manual test builds: use `workflow_dispatch` on the Release workflow (artifacts only, no release)

## Building

```bash
cargo build --workspace          # debug build (use for development)
cargo build --release --workspace # release build (CI/deploy only — slow)
```

**Always use debug builds during development and testing.** Release builds take 4+ minutes. Only use `--release` for CI, benchmarks, or final verification.

## Testing

```bash
cargo test --workspace           # all unit tests (uses mock Ollama + tempdir LanceDB)
cargo test -p memcan-core       # core library tests only
```

Tests use `mockito` for HTTP mocking and `tempfile` for ephemeral LanceDB directories. No live Ollama or external services required.

### Prompt Testing

After changing any prompt in `rs/memcan-core/src/prompts/`, run the classification test against the fixture vectors:

```bash
cargo build -p memcan-server  # binary location depends on CARGO_TARGET_DIR
memcan-server test-classification \
  --prompt rs/memcan-core/src/prompts/fact-extraction-hook.md \
  --model qwen3.5:9b \
  --data rs/memcan-core/tests/fixtures/hook-test-vectors.jsonl
```

Test vectors live in `rs/memcan-core/tests/fixtures/`:
- `hook-extraction-reject.jsonl` — 82 inputs that MUST produce `{"facts": []}` (junk patterns)
- `hook-extraction-accept.jsonl` — 14 inputs that MUST produce ≥1 fact (valid lessons)
- `hook-test-vectors.jsonl` — combined file in test-classification format (`decision`+`content`)

Target: ≥90% accuracy, ≥85% precision on reject class. If false positives increase, strengthen the rejection examples in `fact-extraction-hook.md`.

## Quality Checks

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Configuration

Environment variables (loaded from `~/.config/memcan/.env` or `.env`):

| Variable | Default | Description |
|---|---|---|
| `MEMCAN_LISTEN` | `127.0.0.1:8191` | Server bind address (Docker overrides to `0.0.0.0:8191`) |
| `MEMCAN_API_KEY` | *(none)* | Bearer token auth for MCP API. Required to use destructive tools (`delete_code_records` is refused when unset). |
| `MEMCAN_WEBUI_USERNAME` | *(none)* | Shared username for the read-only task UI. Empty means unset; `/ui*` is not mounted unless both WebUI credentials are non-empty. |
| `MEMCAN_WEBUI_PASSWORD` | *(none)* | Shared password for the read-only task UI. Empty means unset; the value is masked in `Settings` debug output. |
| `MEMCAN_URL` | `http://localhost:8190` | Server URL for thin clients (`memcan`) |
| `MEMCAN_LOG_FILE` | *(none = stdout)* | Log file path (renamed from `LOG_FILE`) |
| `MEMCAN_BODY_READ_TIMEOUT` | `30` | Seconds the `/mcp` route waits for the next chunk of an incoming request body. The clock resets on every chunk received, so a large body that keeps arriving is never cut off, while a stalled connection is answered `408` and logged at `error` instead of parking silently. Bounds only body ingestion — not handler execution, not the SSE response stream. `0` disables the bound; a non-numeric value logs a `warn!` and falls back to `30`. |
| `LANCEDB_PATH` | `~/.local/share/memcan/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `COMPACT_ON_STARTUP` | `true` | Run full table compaction (`OptimizeAction::All`) on boot before serving; over-fragmented tables only (tables ≤ `COMPACT_FRAGMENT_THRESHOLD` fragments are skipped). Set `false` to avoid boot latency on very large databases. |
| `COMPACT_FRAGMENT_THRESHOLD` | `64` | Auto-compact a table after a write once it reaches this many data fragments; also the startup-compaction skip threshold. `0` disables auto-compaction. |
| `LLM_PROVIDER` | `ollama` | Primary slot backend, case-insensitive: `ollama` \| `anthropic` (alias `claude`; native Messages API) \| `openai` \| `openrouter` \| `openai-compatible` (alias `openai-compat`). Unknown values fail startup listing the accepted ones. Each slot's role, backend, model, and base URL (never the key) are logged at `info` on startup. |
| `LLM_MODEL` | *(per backend)* | Primary slot model. Ollama: defaults to `lklimek/gemma4-text:26b-a4b-it-qat` (`ollama::` prefix accepted; needs ~16GB VRAM — use `qwen3.5:9b` (~6.6GB) on smaller cards, see `docs/memcan-model-guide.html`). Cloud backends: **required**, e.g. `claude-haiku-5-5`; an `ollama::`-prefixed model fails startup. |
| `LLM_API_KEY` | *(per backend)* | Primary slot key (`x-api-key` for `anthropic`, bearer otherwise). Required for cloud backends; Ollama defaults to `OLLAMA_API_KEY` (see `OLLAMA_API_KEY`). Never logged. |
| `LLM_BASE_URL` | *(per backend)* | Primary slot endpoint. Defaults: `anthropic` → `https://api.anthropic.com/v1/`, `openai` → `https://api.openai.com/v1`, `openrouter` → `https://openrouter.ai/api/v1`, `ollama` → `OLLAMA_HOST`; required for `openai-compatible`. Cloud URLs must be HTTPS unless loopback; an `anthropic` URL without a `/v1` segment logs a `warn!`. Cloud clients time out (10s connect, 120s request) and never follow redirects. |
| `LLM_FALLBACK_PROVIDER` | *(unset)* | Optional fallback slot backend, same values; unset keeps single-provider behavior. Any combination is valid, including the same kind twice (e.g. two Anthropic keys/models) and two different clouds. A cloud fallback sends memory, code, and standards content to that third party whenever the primary is unavailable. |
| `LLM_FALLBACK_MODEL` / `LLM_FALLBACK_API_KEY` / `LLM_FALLBACK_BASE_URL` | *(per backend)* | Fallback slot model, key, and endpoint, defaulted and validated exactly like the primary's `LLM_*` counterparts. Errors name the exact missing variable. |
| `OPENROUTER_API_KEY` / `OPENROUTER_MODEL` / `OPENROUTER_BASE_URL` | *(none)* | **Deprecated** (startup `warn!`). Mapped onto the first `openrouter` / `openai-compatible` slot, where a set slot variable wins; pre-slot `.env` files keep their meaning. `OPENROUTER_API_KEY` is only sent to `OPENROUTER_BASE_URL` or the default endpoint, and `OPENROUTER_BASE_URL` only receives `OPENROUTER_API_KEY` — a mixed pairing fails startup. Full rule: `apply_legacy_openrouter_env` in `config.rs` (operator copy in `.env.example`). |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`); default base URL of every Ollama slot (a slot's `*_BASE_URL` wins). Injected into the LLM client (ollama-rs default; or genai via `ServiceTargetResolver`). |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth (sent as `Authorization: Bearer $key`); default key of every Ollama slot aimed at `OLLAMA_HOST`'s origin (or the default host). A slot on another host gets no key (logged at `info`) unless it sets its own `*_API_KEY`, which always wins. |

> **Note:** Each slot builds its own LLM client with its own credentials and has its own circuit breaker. For Ollama-primary with a Claude fallback, set e.g. `LLM_PROVIDER=ollama`, `LLM_FALLBACK_PROVIDER=anthropic`, `LLM_FALLBACK_API_KEY=…`, and `LLM_FALLBACK_MODEL=claude-haiku-5-5`. When the local primary is unavailable, memory, code, and standards content will leave the local environment and be sent to the third-party cloud. This confidentiality/data-egress trade-off accompanies the availability benefit. Cloud backends never receive `temperature` (current Claude and OpenAI reasoning models reject non-default values). The `anthropic` backend marks the system prompt as an ephemeral prompt-cache breakpoint, sends no `response_format`/`thinking` parameters, and defaults `max_tokens` to 4096. **Do not point `openai`/`openai-compatible`/`openrouter` at `api.anthropic.com`:** Anthropic's OpenAI-compatible endpoint has been observed to reject `response_format: json_object` with a 400 (vendor docs say the field is ignored; 400 is a hard error, no fallback), which breaks JSON mode — use `anthropic` instead. Startup logs a `warn!` for such a slot. Base URLs in logs, `Debug` output, and configuration errors drop userinfo, query, and fragment.

> **Credential/endpoint pairing:** a key is only sent to the endpoint configured with it. A `./.env` in the working directory may set an endpoint (`*_BASE_URL`, `OPENROUTER_BASE_URL`, `OLLAMA_HOST`) only for a key it also sets; an endpoint from `./.env` paired with a key from the environment or `~/.config/memcan/.env` fails startup naming both variables.

> **Fault classes:** only availability faults (transport errors, timeouts, 3xx, 401, 402, 408, 429, 5xx) trip a slot's breaker and reach the fallback, on every backend; other 4xx and empty replies are content faults that stay local and count as proof the backend is reachable. A prompt that crashes the model runner (5xx), outlasts the timeout, or arrives in a 429 burst can still open the breaker and divert traffic to the fallback.

> **Breaking (`/health`):** dependencies are reported per LLM slot — `llm_primary` and `llm_fallback` (the latter only when configured), each with its `backend` and `model`. The former `ollama` and `openrouter` keys are gone; monitors keyed on `dependencies.ollama` must switch to `dependencies.llm_primary`. An LLM slot's `error` is a fixed note, not the upstream error text; details are in the server log.

### Tasks web UI deployment

The read-only task browser is available at `/ui/tasks` only when both
`MEMCAN_WEBUI_USERNAME` and `MEMCAN_WEBUI_PASSWORD` are non-empty. The bundled Docker Compose
entrypoint is plain HTTP and does not provide TLS out of the box. Its dedicated `/ui*` router bypasses
the API's Bearer middleware so the application's Basic Auth can work, but it remains restricted by
`TRAEFIK_IP_ALLOWLIST`.

Keep the allowlist at its default localhost-only setting (`127.0.0.1/32,::1/128`) until TLS is
configured. Exposing `/ui*` beyond localhost requires the operator to add a Traefik TLS entrypoint
and certificate resolver, or place another TLS-terminating reverse proxy upstream. Plain-HTTP
Basic Auth is insecure because its credentials are merely base64-encoded.

Application-level login throttling is deferred for this trusted-network MVP. Rate limiting belongs
at the proxy or network layer, where the deployment has an established client-IP trust model: the
bundled `docker-compose.yml` enforces it on the `memcan-ui` router through the Traefik `ratelimit`
middleware (`average=10` req/s, `burst=20`). Revisit those values, and the decision to keep
throttling out of the application, before exposing the UI beyond trusted networks. Basic Auth also
has no application logout operation: browsers retain the shared credentials for their session.

## LLM Context Budget

Prompt budgets derive from the model's context window: fact extraction and standards chunking use 40% of it (`resolve_context_budget`), code descriptions use it minus 520 reserved tokens (`description_input_budget`). Ollama slots discover the window via `/api/show`; when it is unknown they assume 4096 tokens (`DEFAULT_CONTEXT_WINDOW`). Cloud slots cannot report one and assume 32768 (`CLOUD_DEFAULT_CONTEXT_WINDOW`, 8x the local default). With a fallback slot, the budget is sized for the smaller known window of both slots; an unknown primary window counts as its assumed one, and with neither window known the assumption is the smaller of both slots' (a cloud primary with an undiscoverable Ollama fallback assumes 4096). There is no per-model discovery and no override variable.

## LLM Token Telemetry

Both LLM backends emit a structured `debug` log line after each chat call:

```
RUST_LOG=memcan::llm::telemetry=debug
```

Example output:
```
DEBUG memcan::llm::telemetry: LLM token usage op="fact_extraction" model="qwen3.5:9b" prompt_tokens=599 completion_tokens=78 total_tokens=677
```

Fields: `op` (call-site label: `fact_extraction`, `dedup`, `code_description`, `standards_metadata`), `model`, `prompt_tokens`, `completion_tokens`, `total_tokens`, plus `cache_creation_tokens` / `cache_read_tokens` when the provider reports prompt-cache usage (e.g. Anthropic, OpenAI, OpenRouter; already included in `prompt_tokens`). When the provider returns no counts, logs "LLM token usage unavailable" instead.

## Deprecated Features

### Hooks (removed)

All plugin hooks have been removed. The `hooks/` directory no longer exists. The `SubagentStop` and `PreCompact` auto-hooks were removed in v0.35 due to quality issues, and the remaining `SessionStart` check hook was removed as it provided no real value. The `lessons-learned` skill has been moved to the [claudius](https://github.com/lklimek/claudius) plugin, which owns classification logic. MemCan skills (`remember`, `recall`) are pure executors.
