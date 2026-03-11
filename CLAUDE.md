# CLAUDE.md

## Project Overview

**MemCan** — Claude Code plugin for persistent memory via LanceDB + fastembed + genai. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Rust MCP server (rmcp), LanceDB (embedded vectors), genai+Ollama (LLM), fastembed (embeddings).

Architecture: Three-crate workspace — reusable library (`memcan-core`), MCP server binary (`memcan-server`), thin CLI client (`memcan`).

## Structure

```
rs/                              # All Rust source code
  memcan-core/                  # Reusable library
  memcan-server/                # MCP server binary + admin CLI wrappers
  memcan/                       # Thin CLI client (HTTP only, no core dep)
Cargo.toml                       # Workspace root
Dockerfile                       # Multi-stage build for memcan-server
.claude-plugin/                  # Claude Code plugin manifest
hooks/                           # Event hooks (SubagentStop, PreCompact)
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
| `traits` | `VectorStore`, `EmbeddingProvider`, `LlmProvider` abstractions |
| `lancedb_store` | LanceDB implementation of `VectorStore` |
| `embed` | fastembed implementation of `EmbeddingProvider` |
| `llm` | genai implementation of `LlmProvider` |
| `ollama` | Ollama model resolution helpers |
| `pipeline` | Memory add pipeline (LLM fact extraction, dedup, store) |
| `query` | User ID resolution, SQL sanitization helpers |
| `search` | Unified cross-collection search (memories, standards, code) |
| `health` | Dependency circuit breaker (Ollama, LanceDB, Embedding) |
| `indexing::code` | Language-specific symbol extraction, LLM descriptions, incremental code indexing |
| `indexing::standards` | Markdown chunking, LLM metadata extraction |
| `indexing::batch` | Shared batch embedding + upsert helper |
| `config` | `Settings` loading from env/files |
| `init` | `MemcanContext` bootstrap (wires all components) |
| `prompts` | LLM prompt templates |
| `error` | Error types and `Result` aliases |

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
memcan-server index-code <dir> --project <name> [--tech-stack <s>] [--drop]
memcan-server index-standards <file> --standard-id <id> --standard-type <t> [--drop]
memcan-server migrate <file> [--dry-run]
memcan-server import-triaged <file> [--dry-run]
memcan-server test-classification --prompt <f> --model <m>
memcan-server download-model [--model <name>]
memcan-server completions <shell>
```

## CLI Subcommands

```
memcan add <memory> [--project <p>]
memcan search <query> [--project <p>] [--limit <n>]
memcan extract                        # Hook handler: reads stdin, POSTs to server
memcan status [operation_id]
memcan count [--project <p>]
memcan index-standards <file> --standard-id <id> --standard-type <t> [--version <v>] [--lang <l>] [--url <u>] [--wait]
memcan index-standards --drop --standard-id <id>
```

## MCP Tools

Server exposes these MCP tools (via HTTP at `/mcp`):

| Tool | Description |
|---|---|
| `add_memory` | Store a memory (async, returns operation_id) |
| `search_memories` | Semantic search across memories |
| `get_memories` | List memories for a given scope |
| `count_memories` | Count memories |
| `delete_memory` | Delete a memory by ID |
| `update_memory` | Update an existing memory's content |
| `list_collections` | List available collections with point counts |
| `search_standards` | Search indexed standards (CWE, OWASP, etc.) |
| `search_code` | Search indexed code snippets |
| `index_standards` | Index a standards document (async, returns operation_id) |
| `drop_indexed_standards` | Drop all indexed data for a standard_id |
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
| `MEMCAN_API_KEY` | *(none)* | Bearer token auth for MCP API |
| `MEMCAN_URL` | `http://localhost:8190` | Server URL for thin clients (`memcan`) |
| `MEMCAN_LOG_FILE` | *(none = stdout)* | Log file path (renamed from `LOG_FILE`) |
| `LANCEDB_PATH` | `~/.local/share/memcan/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `qwen3.5:9b` | LLM model name (`ollama::` prefix accepted for backward compat) |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`). Passed to genai client explicitly. |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth (sent as `Authorization: Bearer $key`) |

> **Note:** The genai crate does **not** read `OLLAMA_HOST` or `OLLAMA_API_KEY` from environment — MemCan reads them via `Settings` and passes them to the genai client via `ServiceTargetResolver`.
