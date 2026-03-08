# CLAUDE.md

## Project Overview

**MemCan** — Claude Code plugin for persistent memory via LanceDB + fastembed + genai. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Rust MCP server (rmcp), LanceDB (embedded vectors), genai+Ollama (LLM), fastembed (embeddings).

Architecture: Long-lived HTTP MCP server (`memcan-server`) with thin CLI client (`memcan`). Server handles all heavy operations (embedding, LLM, storage). CLI is a lightweight HTTP client with no fastembed/LanceDB deps.

## Structure

```
rs/                              # All Rust source code
  memcan-core/                  # Shared library (traits, LanceDB, genai, fastembed, pipeline, config)
  memcan-server/                # Fat server binary (MCP HTTP/stdio server + admin subcommands)
  memcan/                       # Thin CLI client (binary: memcan)
Cargo.toml                       # Workspace root
Dockerfile                       # Multi-stage build for memcan-server
.claude-plugin/                  # Claude Code plugin manifest
hooks/                           # Event hooks (SubagentStop, PreCompact)
skills/                          # Plugin skills
.github/workflows/               # CI + Release workflows
docker-compose.yml               # Traefik + memcan + optional Ollama
```

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
```

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
cargo build --workspace          # debug build
cargo build --release --workspace # release build
```

## Testing

```bash
cargo test --workspace           # all unit tests (uses mock Ollama + tempdir LanceDB)
cargo test -p memcan-core       # core library tests only
```

Tests use `mockito` for HTTP mocking and `tempfile` for ephemeral LanceDB directories. No live Ollama or external services required.

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
| `LLM_MODEL` | `ollama::qwen3.5:4b` | LLM model (genai format with provider prefix) |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`). Passed to genai client explicitly. |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth (sent as `Authorization: Bearer $key`) |

> **Note:** The genai crate does **not** read `OLLAMA_HOST` or `OLLAMA_API_KEY` from environment — MemCan reads them via `Settings` and passes them to the genai client via `ServiceTargetResolver`.
