---
name: setup-mindojo
description: Configure MindOJO environment — .env file and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(mkdir *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep, mcp__plugin_mindojo_brain__search_memories
---

# Setup MindOJO

Configures the MindOJO environment. The plugin and MCP server are already installed — this sets up the `.env` and user rule. Idempotent.

## Architecture

MindOJO uses a two-component architecture:
- **Server** (`mindojo serve`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service.
- **CLI** (`mindojo-cli`) — thin HTTP client for hooks. Downloaded by `setup.sh`, lives at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli`.

The Claude Code plugin connects to the server via HTTP MCP transport (configured in `.mcp.json`).

## Steps

### 1. Check Prerequisites

Verify:
- MindOJO CLI binary exists at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli` (if not, run `${CLAUDE_PLUGIN_ROOT}/setup.sh`)
- MindOJO server is reachable — the CLI and plugin connect to it via `MINDOJO_URL` (default: `http://localhost:8190`). Test: `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli count`
- Ollama is reachable — needed for LLM chat (fact extraction, distillation). Not needed for embeddings, which run in-process on the server via fastembed. Read `OLLAMA_HOST` and `OLLAMA_API_KEY` from `~/.config/mindojo/.env` (if it exists). Use `OLLAMA_HOST` as the base URL (default `http://localhost:11434`). If `OLLAMA_API_KEY` is set, include `-H "Authorization: Bearer $key"`. Check: `curl -sf [-H "Authorization: Bearer $key"] $host/api/tags`.

No external database needed — MindOJO uses embedded LanceDB on the server (data stored at `LANCEDB_PATH`, default `~/.local/share/mindojo/lancedb`). Embeddings are computed on the server via fastembed (ONNX), so no embedding service is required.

If any prerequisite fails, tell the user what's missing and how to fix it, then stop.

### 2. Configure .env File

The `.env` file configures both the server and CLI. Search order:
1. **Platform config dir**: `~/.config/mindojo/.env` (Linux), `~/Library/Application Support/mindojo/.env` (macOS)
2. **CWD**: `./.env` (dev fallback)

Env vars always override `.env` values.

Check if `.env` exists at `~/.config/mindojo/.env` (Linux) or equivalent.

If not, create the config dir and copy from `.env.example`:
```bash
mkdir -p ~/.config/mindojo
cp ${CLAUDE_PLUGIN_ROOT}/../.env.example ~/.config/mindojo/.env
```

Key configuration variables to review with the user:

**Server connection (for CLI and plugin):**
- `MINDOJO_URL` — Server URL (default: `http://localhost:8190`)
- `MINDOJO_API_KEY` — Bearer token auth for MCP API (must match server config)

**Ollama (for LLM on the server):**
- `OLLAMA_HOST` — Ollama endpoint (default: `http://localhost:11434`)
- `OLLAMA_API_KEY` — Bearer token for Ollama auth (default: none)
- `LLM_MODEL` — LLM model with provider prefix (default: `ollama::qwen3.5:4b`)

**Embeddings:**
- `EMBED_MODEL` — fastembed model name (default: `MultilingualE5Large`, runs in-process on server; dimensions derived automatically)

**Logging:**
- `MINDOJO_LOG_FILE` — Log file path (default: `~/.claude/logs/mindojo-mcp.log`; set empty for stdout)

Ask if Ollama requires Bearer token authentication (common when behind a reverse proxy like Traefik, Caddy, or nginx). If yes, ask for the `OLLAMA_API_KEY` value and set it in `.env`. If no, leave it commented out.

### 3. Create User Rule

Create `~/.claude/rules/mindojo.md` (create `~/.claude/rules/` dir if needed).

Write this content:

```markdown
# MindOJO — Persistent Memory

Use the MindOJO MCP server to store and recall knowledge across sessions.

## Session Start
- Search memories for the current project: `search_memories(query="project context", project="<repo-name>")`
- Also search global memories for cross-project learnings

## When to Save (add_memory)
- Lessons learned from bugs, failed approaches, workarounds
- Architecture decisions with rationale
- User preferences and coding conventions
- Discovered patterns and anti-patterns
- Project-specific configuration quirks

## When to Search (search_memories)
- Before architectural decisions — check for prior art or known pitfalls
- On errors — search for similar past issues and their fixes
- During code reviews — recall project conventions
- When starting work on an unfamiliar area

## Scoping
- Set `project` param to git remote origin repo name — NOT the directory name (e.g., `dash-evo-tool` not `dash-evo-tool-2`)
- Omit `project` for global memories (universal learnings)

## Quality Guidelines
- Be specific: "Axum .layer() last = outermost" > "middleware ordering matters"
- Include context: project, framework, language
- Tag with metadata: type (lesson/decision/preference), source (LL-NNN, PR#)
- Search before adding to avoid duplicates
```

### 4. Verify

Print a summary:
- CLI binary installed at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli`
- `.env` exists at `~/.config/mindojo/.env` with `MINDOJO_URL` and `MINDOJO_API_KEY` configured
- User rule exists at `~/.claude/rules/mindojo.md`
- MCP server is connected (test: call `search_memories(query="test", limit=1)` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `MINDOJO_API_KEY` is not set: warn that the MCP server has no auth — anyone with network access can read/write memories.
- If `OLLAMA_HOST` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth.
- If `OLLAMA_API_KEY` is set and `OLLAMA_HOST` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext.

If the MCP server check failed, tell the user to ensure the server is running (`docker compose up -d` or `mindojo serve`) and restart Claude Code so the plugin's `.mcp.json` gets loaded.
