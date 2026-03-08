# CLAUDE.md

## Project Overview

**MindOJO** — Claude Code plugin for persistent memory via LanceDB + fastembed + genai. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Rust MCP server (rmcp), LanceDB (embedded vectors), genai+Ollama (LLM), fastembed (embeddings).

## Structure

```
rs/                              # All Rust source code
  mindojo-core/                  # Shared library (traits, LanceDB, genai, fastembed, pipeline, config)
  mindojo-mcp/                   # MCP server binary (stdio transport)
  mindojo-extract/               # Hook binary — extracts learnings from conversations
  mindojo-import-triaged/        # CLI — imports triaged memories from JSON
  mindojo-index-code/            # CLI — indexes source code files
  mindojo-index-standards/       # CLI — indexes technical standards documents
  mindojo-migrate/               # CLI — migrates/imports legacy JSON data
  mindojo-test-classification/   # CLI — tests prompt classification accuracy
Cargo.toml                       # Workspace root
claude-plugin/                   # Claude Code plugin
  .claude-plugin/                # Manifest
  hooks/                         # Event hooks (SubagentStop)
  skills/                        # Plugin skills
  setup.sh                       # Downloads release binaries
  bin/                           # Downloaded binaries (gitignored)
.github/workflows/               # CI + Release workflows
docker-compose.yml               # Optional Ollama container
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
| `LANCEDB_PATH` | `~/.local/share/mindojo/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `TECH_STACK` | *(none)* | Default tech stack filter |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `ollama::qwen3.5:4b` | LLM model (genai format with provider prefix) |
| `EMBED_MODEL` | `AllMiniLML6V2` | Fastembed model for in-process embeddings |
| `EMBED_DIMS` | `384` | Embedding vector dimensions (must match embed model) |
| `LOG_FILE` | `~/.claude/logs/mindojo-mcp.log` | Log file path |

> **Note:** genai reads `OLLAMA_HOST` (default `http://localhost:11434`) for the Ollama endpoint. `OLLAMA_URL` and `OLLAMA_API_KEY` are no longer used.
