# CLAUDE.md

## Project Overview

**MindOJO** — Claude Code plugin for persistent memory via mem0. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Python MCP server (FastMCP + mem0), Qdrant (vectors), Ollama (LLM + embeddings), optional Neo4j (graph).

## Structure

```
claude-plugin/           # Claude Code plugin
  .claude-plugin/        # Manifest
  mcp-server/            # MCP server (Python, uv-managed)
  skills/                # Plugin skills
docker-compose.yml       # Qdrant (+ optional Neo4j)
scripts/                 # Import/migration utilities (see README.md § Scripts)
```

## Versioning

Bump version in `claude-plugin/.claude-plugin/plugin.json` before each commit. Follow [SemVer 2](https://semver.org/).

- **Major** (x.0.0): breaking changes to MCP tools, removed features, incompatible config changes
- **Minor** (0.x.0): new MCP tools, new skills, significant behavior changes
- **Patch** (0.0.x): bug fixes, doc corrections, minor tweaks
