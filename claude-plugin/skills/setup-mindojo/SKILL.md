---
name: setup-mindojo
description: Configure MindOJO environment — .env file and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(mkdir *), Bash(uv *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep, mcp__plugin_mindojo_brain__search_memories
---

# Setup MindOJO

Configures the MindOJO environment. The plugin and MCP server are already installed — this sets up the `.env` and user rule. Idempotent.

## Steps

### 1. Check Prerequisites

Verify:
- `uv` is installed (`which uv` or `~/.local/bin/uv --version`)
- Qdrant is running (`curl -sf http://localhost:6333/healthz`)
- MCP server deps are installed (`cd ${CLAUDE_PLUGIN_ROOT}/mcp-server && uv sync`)

If any fails, tell the user what's missing and how to fix it, then stop.

### 2. Configure .env File

The MCP server searches for `.env` in order:
1. **Platform config dir**: `~/.config/mindojo/.env` (Linux), `~/Library/Application Support/mindojo/.env` (macOS)
2. **CWD**: `./.env` (dev fallback)

Env vars always override `.env` values.

Check if `.env` exists at `~/.config/mindojo/.env` (Linux) or equivalent.

If not, create the config dir and copy from `.env.example`:
```bash
mkdir -p ~/.config/mindojo
cp ${CLAUDE_PLUGIN_ROOT}/../.env.example ~/.config/mindojo/.env
```

Ask the user for their Ollama URL and update `OLLAMA_URL` in `.env`. Verify `QDRANT_URL` — defaults to `http://localhost:6333` which is usually correct.

Then ask if Ollama requires Bearer token authentication (common when behind a reverse proxy like Traefik, Caddy, or nginx). If yes, ask for the `OLLAMA_API_KEY` value and uncomment/set it in `.env`. If no, leave it commented out (the default). Refer to README "Ollama Authentication" for details.

MCP server logging now defaults to `~/.claude/logs/mindojo-mcp.log` (no config needed). If the user wants a custom path, set `LOG_FILE` in `.env` to override.

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
- ✅/❌ `.env` exists at `~/.config/mindojo/.env` with `OLLAMA_URL` configured
- ✅/❌ Qdrant is healthy
- ✅/❌ User rule exists at `~/.claude/rules/mindojo.md`
- ✅/❌ Logging: defaults to `~/.claude/logs/mindojo-mcp.log`; custom path if `LOG_FILE` set in `.env`
- ✅/❌ MCP server is connected (test: call `search_memories(query="test", limit=1)` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `OLLAMA_URL` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth — consider setting `OLLAMA_API_KEY`.
- If `OLLAMA_API_KEY` is set and `OLLAMA_URL` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext — security risk on untrusted networks. Recommend switching to HTTPS.

If the MCP server check failed, tell the user to restart Claude Code so the plugin's `.mcp.json` gets loaded and the MCP server connects. If all checks passed, no restart needed.
