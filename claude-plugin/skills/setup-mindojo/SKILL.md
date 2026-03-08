---
name: setup-mindojo
description: Configure MindOJO environment — .env file, Claude Code settings, and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(bash *), Bash(mkdir *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep, mcp__plugin_mindojo_brain__search_memories
---

# Setup MindOJO

Configures the MindOJO environment. The plugin and MCP server are already installed — this sets up the `.env`, injects env vars into Claude Code settings, and creates the user rule. Idempotent.

## Architecture

MindOJO uses a two-component architecture:
- **Server** (`mindojo serve`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service.
- **CLI** (`mindojo-cli`) — thin HTTP client for hooks. Downloaded by `setup.sh`, lives at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli`.

The Claude Code plugin connects to the server via HTTP MCP transport (configured in `.mcp.json`).

## Steps

### 1. Check Prerequisites

Verify:
- MindOJO CLI binary exists at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-cli` (if not, run `${CLAUDE_PLUGIN_ROOT}/setup.sh`)

Note: Server connectivity is checked in Step 4. Don't require it here.

If any prerequisite fails, tell the user what's missing and how to fix it, then stop.

### 2. Configure Secrets & Settings

Run the secret configuration script. It resolves API keys (from existing `.env` → env var → auto-generate), writes `~/.config/mindojo/.env`, and merges `MINDOJO_API_KEY` + `MINDOJO_URL` into `~/.claude/settings.json`. Secrets never appear in conversation context.

```bash
bash ${CLAUDE_PLUGIN_ROOT}/bin/configure-secrets.sh
```

Review the script's status output. If it reports `MINDOJO_API_KEY=<generated>`, a new key was created — the server will need restarting to pick it up.

After the script completes, review non-secret settings in `~/.config/mindojo/.env` with the user:
- `MINDOJO_URL` — Server URL (default: `http://localhost:8190`)
- `OLLAMA_HOST` — Ollama endpoint (default: `http://localhost:11434`)
- `LLM_MODEL` — LLM model with provider prefix (default: `ollama::qwen3.5:4b`)
- `EMBED_MODEL` — fastembed model name (default: `MultilingualE5Large`)
- `MINDOJO_LOG_FILE` — Log file path (default: empty = stdout)

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
- Claude Code settings at `~/.claude/settings.json` has `MINDOJO_API_KEY` and `MINDOJO_URL` in `env` block
- User rule exists at `~/.claude/rules/mindojo.md`
- MCP server is connected (test: call `search_memories(query="test", limit=1)` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `MINDOJO_API_KEY` is not set: warn that the MCP server has no auth — anyone with network access can read/write memories.
- If `OLLAMA_HOST` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth.
- If `OLLAMA_API_KEY` is set and `OLLAMA_HOST` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext.

If the MCP server check failed, tell the user to ensure the server is running and restart Claude Code to pick up the new env vars from `~/.claude/settings.json`.
