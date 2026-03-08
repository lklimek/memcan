# CLAUDE.md

## Project Overview

**MindOJO** — Claude Code plugin for persistent memory via LanceDB + fastembed + genai. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Rust MCP server (rmcp), LanceDB (embedded vectors), genai+Ollama (LLM), fastembed (embeddings).

Architecture: Long-lived HTTP MCP server (`mindojo serve`) with thin CLI client (`mindojo-cli`). Server handles all heavy operations (embedding, LLM, storage). CLI is a lightweight HTTP client with no fastembed/LanceDB deps.

## Structure

```
rs/                              # All Rust source code
  mindojo-core/                  # Shared library (traits, LanceDB, genai, fastembed, pipeline, config)
  mindojo-server/                # HTTP MCP server binary + admin subcommands (mindojo)
  mindojo-cli/                   # Thin HTTP client binary (mindojo-cli, no core deps)
  mindojo-mcp/                   # [legacy] MCP server binary (stdio transport)
  mindojo-extract/               # [legacy] Hook binary — extracts learnings from conversations
  mindojo-import-triaged/        # [legacy] CLI — imports triaged memories from JSON
  mindojo-index-code/            # [legacy] CLI — indexes source code files
  mindojo-index-standards/       # [legacy] CLI — indexes technical standards documents
  mindojo-migrate/               # [legacy] CLI — migrates/imports legacy JSON data
  mindojo-test-classification/   # [legacy] CLI — tests prompt classification accuracy
Cargo.toml                       # Workspace root
Dockerfile                       # Multi-stage build for mindojo server
claude-plugin/                   # Claude Code plugin
  .claude-plugin/                # Manifest
  hooks/                         # Event hooks (SubagentStop, PreCompact)
  skills/                        # Plugin skills
  setup.sh                       # Downloads mindojo-cli binary
  bin/                           # Downloaded binaries (gitignored)
.github/workflows/               # CI + Release workflows
docker-compose.yml               # Traefik + mindojo + optional Ollama
```

## Server Subcommands

```
mindojo serve [--stdio] [--listen ADDR]   # MCP server (default subcommand)
mindojo index-code <dir> --project <name> [--tech-stack <s>] [--drop]
mindojo index-standards <file> --standard-id <id> --standard-type <t> [--drop]
mindojo migrate <file> [--dry-run]
mindojo import-triaged <file> [--dry-run]
mindojo test-classification --prompt <f> --model <m>
mindojo download-model [--model <name>]
mindojo completions <shell>
```

## CLI Subcommands

```
mindojo-cli add <memory> [--project <p>]
mindojo-cli search <query> [--project <p>] [--limit <n>]
mindojo-cli extract                        # Hook handler: reads stdin, POSTs to server
mindojo-cli status [operation_id]
mindojo-cli count [--project <p>]
```

## Versioning

**Only the team coordinator (Claudius) bumps versions.** Subagents must NOT modify `plugin.json` version.

Version lives in `claude-plugin/.claude-plugin/plugin.json`. Follow [SemVer 2](https://semver.org/).

- **Major** (x.0.0): breaking changes to MCP tools, removed features, incompatible config changes
- **Minor** (0.x.0): new MCP tools, new skills, significant behavior changes
- **Patch** (0.0.x): bug fixes, doc corrections, minor tweaks

## Building

```bash
cargo build --workspace          # debug build
cargo build --release --workspace # release build
```

## Testing

```bash
cargo test --workspace           # all unit tests (uses mock Ollama + tempdir LanceDB)
cargo test -p mindojo-core       # core library tests only
```

Tests use `mockito` for HTTP mocking and `tempfile` for ephemeral LanceDB directories. No live Ollama or external services required.

## Quality Checks

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Configuration

Environment variables (loaded from `~/.config/mindojo/.env` or `.env`):

| Variable | Default | Description |
|---|---|---|
| `MINDOJO_LISTEN` | `127.0.0.1:8191` | Server bind address (Docker overrides to `0.0.0.0:8191`) |
| `MINDOJO_API_KEY` | *(none)* | Bearer token auth for MCP API |
| `MINDOJO_URL` | `http://localhost:8190` | Server URL for thin clients (`mindojo-cli`) |
| `MINDOJO_LOG_FILE` | *(none = stdout)* | Log file path (renamed from `LOG_FILE`) |
| `LANCEDB_PATH` | `~/.local/share/mindojo/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `ollama::qwen3.5:4b` | LLM model (genai format with provider prefix) |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`). Passed to genai client explicitly. |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth (sent as `Authorization: Bearer $key`) |

> **Note:** The genai crate does **not** read `OLLAMA_HOST` or `OLLAMA_API_KEY` from environment — MindOJO reads them via `Settings` and passes them to the genai client via `ServiceTargetResolver`.
