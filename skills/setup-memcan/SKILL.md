---
name: setup-memcan
description: Configure MemCan environment — .env file, Claude Code settings, and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(bash *), Bash(mkdir *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep, mcp__plugin_memcan_brain__search_memories
---

# Setup MemCan

Configures the MemCan environment. The plugin and MCP server are already installed — this sets up the `.env`, injects env vars into Claude Code settings, and creates the user rule. Idempotent.

## Architecture

MemCan uses a two-component architecture:
- **Server** (`memcan-server serve`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service.
- **CLI** (`memcan`) — thin HTTP client for hooks. Installed via `setup.sh` or `cargo install memcan`.

The Claude Code plugin connects to the server via HTTP MCP transport (configured in `.mcp.json`).

## Steps

### 1. Install CLI and Server

Check `command -v memcan`. If missing, install it using the setup script:

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/lklimek/memcan/main/setup.sh)
```

The script handles both CLI binary installation and Docker Compose server setup:
- Downloads and installs the `memcan` CLI binary
- Downloads `docker-compose.yml` and creates `.env` files with generated API keys
- Does NOT auto-start the server — prints instructions for `docker compose up -d`

If Docker is not available and the user only needs the CLI, use `--cli-only`:
```bash
bash <(curl -fsSL https://raw.githubusercontent.com/lklimek/memcan/main/setup.sh) --cli-only
```

Other flags: `--version VERSION`, `--install-dir DIR`, `--server-dir DIR`.

Verify the install succeeded (`command -v memcan`). If it fails, stop and report the error.

Note: The script creates `.env` files (server + CLI) but does NOT start the server. Server connectivity is checked in Step 4.

### 2. Configure Secrets & Settings

Resolve API keys and write configuration. Secrets never appear in conversation context.

Perform these steps:

1. **Resolve MEMCAN_API_KEY**: Check `~/.config/memcan/.env` for existing value, then `$MEMCAN_API_KEY` env var. If neither exists, generate a new key: `openssl rand -hex 32` (or `head -c 32 /dev/urandom | xxd -p -c 64` if openssl unavailable).

2. **Resolve MEMCAN_URL**: Check `~/.config/memcan/.env`, then `$MEMCAN_URL` env var. Default: `http://localhost:8190`.

3. **Resolve OLLAMA_API_KEY**: Check `~/.config/memcan/.env`, then `$OLLAMA_API_KEY` env var. May be empty.

4. **Write `~/.config/memcan/.env`**: Create directory if needed. If the file exists, update `MEMCAN_API_KEY`, `MEMCAN_URL`, and `OLLAMA_API_KEY` (if non-empty) in-place. If creating new, write a template with resolved values and commented defaults for `OLLAMA_HOST`, `LLM_MODEL`, `EMBED_MODEL`, `MEMCAN_LOG_FILE`.

5. **Merge into `~/.claude/settings.json`**: Read existing file (or `{}`), set `.env.MEMCAN_API_KEY` and `.env.MEMCAN_URL`, write back. Use `jq` or `python3` for JSON manipulation.

After completing, report status (without revealing secret values):
- Whether MEMCAN_API_KEY was existing or newly generated
- The MEMCAN_URL value

Review non-secret settings in `~/.config/memcan/.env` with the user:
- `MEMCAN_URL` — Server URL (default: `http://localhost:8190`)
- `OLLAMA_HOST` — Ollama endpoint (default: `http://localhost:11434`)
- `LLM_MODEL` — LLM model with provider prefix (default: `ollama::qwen3.5:4b`)
- `EMBED_MODEL` — fastembed model name (default: `MultilingualE5Large`)
- `MEMCAN_LOG_FILE` — Log file path (default: empty = stdout)

### 3. Create User Rule

Create `~/.claude/rules/memcan.md` (create `~/.claude/rules/` dir if needed).

Write this content:

```markdown
# MemCan — Persistent Memory

Use the MemCan MCP server to store and recall knowledge across sessions.

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
- CLI installed and on PATH (`memcan`)
- `.env` exists at `~/.config/memcan/.env` with `MEMCAN_URL` and `MEMCAN_API_KEY` configured
- Claude Code settings at `~/.claude/settings.json` has `MEMCAN_API_KEY` and `MEMCAN_URL` in `env` block
- User rule exists at `~/.claude/rules/memcan.md`
- MCP server is connected (test: call `search_memories(query="test", limit=1)` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `MEMCAN_API_KEY` is not set: warn that the MCP server has no auth — anyone with network access can read/write memories.
- If `OLLAMA_HOST` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth.
- If `OLLAMA_API_KEY` is set and `OLLAMA_HOST` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext.

If the MCP server check failed, tell the user to ensure the server is running and restart Claude Code to pick up the new env vars from `~/.claude/settings.json`.
