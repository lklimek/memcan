---
name: setup-mindojo
description: Configure MindOJO environment — .env file and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(mkdir *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep, mcp__plugin_mindojo_brain__search_memories
---

# Setup MindOJO

Configures the MindOJO environment. The plugin and MCP server are already installed — this sets up the `.env` and user rule. Idempotent.

## Steps

### 1. Check Prerequisites

Verify:
- MindOJO binary exists at `${CLAUDE_PLUGIN_ROOT}/bin/mindojo-mcp` (if not, run `${CLAUDE_PLUGIN_ROOT}/setup.sh`)
- The setup script automatically downloads the configured embedding model. If the model is missing, it will be downloaded on first MCP server start (may cause a delay).
- Ollama is reachable (`curl -sf http://localhost:11434/api/tags`) — needed for LLM chat (fact extraction, distillation). Not needed for embeddings, which run in-process via fastembed.

No external database needed — MindOJO uses embedded LanceDB (data stored locally at `~/.local/share/mindojo/lancedb`). Embeddings are computed locally via fastembed (ONNX), so no embedding service is required.

If any prerequisite fails, tell the user what's missing and how to fix it, then stop.

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

Ask the user for their Ollama host and update `OLLAMA_HOST` in `.env`. The genai crate reads `OLLAMA_HOST` (not `OLLAMA_URL`). Default is `http://localhost:11434`. Ollama is used only for LLM chat — embeddings run locally via fastembed.

Key configuration variables to review with the user:
- `OLLAMA_HOST` — Ollama endpoint (default: `http://localhost:11434`)
- `OLLAMA_API_KEY` — Bearer token for Ollama auth (default: none)
- `LLM_MODEL` — LLM model with provider prefix (default: `ollama::qwen3.5:4b`)
- `EMBED_MODEL` — fastembed model name (default: `MultilingualE5Large`, runs in-process)
- `EMBED_DIMS` — embedding dimensions, must match model (default: `1024`)

Then ask if Ollama requires Bearer token authentication (common when behind a reverse proxy like Traefik, Caddy, or nginx). If yes, ask for the `OLLAMA_API_KEY` value and uncomment/set it in `.env`. If no, leave it commented out (the default).

MCP server logging defaults to `~/.claude/logs/mindojo-mcp.log` (no config needed). If the user wants a custom path, set `LOG_FILE` in `.env` to override.

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
- binaries installed at `${CLAUDE_PLUGIN_ROOT}/bin/`
- `.env` exists at `~/.config/mindojo/.env` with `OLLAMA_HOST` configured
- LanceDB data dir exists at `~/.local/share/mindojo/lancedb`
- User rule exists at `~/.claude/rules/mindojo.md`
- Logging: defaults to `~/.claude/logs/mindojo-mcp.log`; custom path if `LOG_FILE` set in `.env`
- MCP server is connected (test: call `search_memories(query="test", limit=1)` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `OLLAMA_HOST` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth — consider setting `OLLAMA_API_KEY`.
- If `OLLAMA_API_KEY` is set and `OLLAMA_HOST` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext — security risk on untrusted networks. Recommend switching to HTTPS.

If the MCP server check failed, tell the user to restart Claude Code so the plugin's `.mcp.json` gets loaded and the MCP server connects. If all checks passed, no restart needed.
