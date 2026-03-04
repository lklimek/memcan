---
name: setup-mindajo
description: Configure MindAJO environment — .env file and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(mkdir *), Bash(uv *), Bash(curl *), Bash(which *), Bash(cp *), Glob, Grep
---

# Setup MindAJO

Configures the MindAJO environment. The plugin and MCP server are already installed — this sets up the `.env` and user rule. Idempotent.

## Steps

### 1. Check Prerequisites

Verify:
- `uv` is installed (`which uv` or `~/.local/bin/uv --version`)
- Qdrant is running (`curl -sf http://localhost:6333/healthz`)
- MCP server deps are installed (`cd ${CLAUDE_PLUGIN_ROOT}/../mcp-server && uv sync`)

If any fails, tell the user what's missing and how to fix it, then stop.

### 2. Configure .env File

The MCP server reads `<repo-root>/.env` via Pydantic Settings. Env vars override `.env` values.

Check if `.env` exists at `${CLAUDE_PLUGIN_ROOT}/../.env`.

If not, copy from `.env.example`:
```bash
cp ${CLAUDE_PLUGIN_ROOT}/../.env.example ${CLAUDE_PLUGIN_ROOT}/../.env
```

Ask the user for their Ollama URL and update `OLLAMA_URL` in `.env`. Verify `QDRANT_URL` — defaults to `http://localhost:6333` which is usually correct.

### 3. Create User Rule

Create `~/.claude/rules/mindajo.md` (create `~/.claude/rules/` dir if needed).

Write this content:

```markdown
# MindAJO — Persistent Memory

Use the MindAJO MCP server to store and recall knowledge across sessions.

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
- Set `project` param to git repo basename (e.g., "penny", "claudius")
- Omit `project` for global memories (universal learnings)

## Quality Guidelines
- Be specific: "Axum .layer() last = outermost" > "middleware ordering matters"
- Include context: project, framework, language
- Tag with metadata: type (lesson/decision/preference), source (LL-NNN, PR#)
- Search before adding to avoid duplicates
```

### 4. Verify

Print a summary:
- ✅/❌ `.env` exists with `OLLAMA_URL` configured
- ✅/❌ Qdrant is healthy
- ✅/❌ User rule exists at `~/.claude/rules/mindajo.md`

Tell the user to restart Claude Code for the MCP server to connect.
