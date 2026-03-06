# CLAUDE.md

## Project Overview

**MindOJO** — Claude Code plugin for persistent memory via Qdrant + Ollama. Stores and recalls learnings, decisions, preferences across sessions. MIT license.

Stack: Python MCP server (FastMCP), Qdrant (vectors), Ollama (LLM + embeddings).

## Structure

```
claude-plugin/           # Claude Code plugin
  .claude-plugin/        # Manifest
  mcp-server/            # MCP server (Python, uv-managed)
  skills/                # Plugin skills
docker-compose.yml       # Qdrant
scripts/                 # Import/migration utilities (see README.md § Scripts)
```

## Versioning

Bump version in `claude-plugin/.claude-plugin/plugin.json` before each commit. Follow [SemVer 2](https://semver.org/).

- **Major** (x.0.0): breaking changes to MCP tools, removed features, incompatible config changes
- **Minor** (0.x.0): new MCP tools, new skills, significant behavior changes
- **Patch** (0.0.x): bug fixes, doc corrections, minor tweaks

## Testing

Tests live in `claude-plugin/mcp-server/tests/`. Run from `claude-plugin/mcp-server/`:

```bash
uv run pytest                    # unit + integration (default)
uv run pytest -m benchmark -v -s # 10-write/10-read perf report (~3 min)
uv run pytest -m mcp_roundtrip -v -s  # async fire-and-forget roundtrip (~10s)
uv run pytest -o "addopts=" -v -s     # everything (~5 min)
```

| Marker | Default | Requires | Notes |
|---|---|---|---|
| *(none)* | ✅ runs | — | Unit tests (config, server) |
| `integration` | ✅ runs | Live Ollama + Qdrant | Connectivity, model availability, sync roundtrip |
| `benchmark` | ❌ excluded | Live Ollama + Qdrant | 10 writes + 10 reads, prints timing report |
| `mcp_roundtrip` | ❌ excluded | Live Ollama + Qdrant | Async `add_memory` fire-and-forget end-to-end |

Exclusions configured via `addopts` in `pyproject.toml`.
