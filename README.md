# AI Brain — Persistent Memory for Claude Code

MCP server providing persistent memory via [mem0](https://github.com/mem0ai/mem0). Store and recall learnings, decisions, and preferences across Claude Code sessions.

## Quick Start

```bash
# 1. Start Qdrant
docker compose up -d

# 2. Install dependencies
cd mcp-server && uv sync && cd ..

# 3. Install plugin in Claude Code
#    Settings → Plugins → enable ai-brain@lklimek
#    Or add to ~/.claude/settings.json:
#      "enabledPlugins": { "ai-brain@lklimek": true }

# 4. Configure environment (in a Claude Code session)
/setup-ai-brain
```

## Install

### Prerequisites

- [uv](https://docs.astral.sh/uv/) — Python package manager
- [Docker](https://docs.docker.com/get-docker/) — for Qdrant
- [Ollama](https://ollama.ai/) — LLM + embeddings (external, not in this compose)

### Plugin Install

Enable `ai-brain@lklimek` in `~/.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "ai-brain@lklimek": true
  }
}
```

The plugin registers the MCP server automatically via `.mcp.json`. No manual `claude mcp add` needed.

### Environment Setup

After enabling the plugin, run `/setup-ai-brain` in a Claude Code session. It will:

1. **Check prerequisites** — uv, Qdrant health, MCP server deps
2. **Configure `.env`** — copy `.env.example`, set your `OLLAMA_URL`
3. **Create user rule** — writes `~/.claude/rules/ai-brain.md` so agents know to use memory

Restart Claude Code after setup to connect the MCP server.

## Architecture

- **mem0** — memory management (add, search, update, delete)
- **Qdrant** — vector similarity search (port 6333)
- **Ollama** — LLM (`qwen2.5:14b`) + embeddings (`nomic-embed-text`)
- **Neo4j** — optional graph store (`docker compose --profile graph up -d`)

## MCP Tools

| Tool | Description |
|------|-------------|
| `add_memory` | Store a memory with optional project scope and metadata |
| `search_memories` | Semantic search across memories |
| `get_memories` | List all memories for a scope |
| `delete_memory` | Remove a memory by ID |
| `update_memory` | Modify existing memory content |

## Memory Scoping

- `project="penny"` → scoped to project (stored as `user_id=project:penny`)
- No project → global scope (stored as `user_id=global`)

## Claude Code Context Persistence

Claude Code loads context into the attention window via several mechanisms. ai-brain leverages them to ensure agents always know to use memory:

| Mechanism | Location | When Loaded | Shared? |
|-----------|----------|-------------|---------|
| **User CLAUDE.md** | `~/.claude/CLAUDE.md` | Every session, all projects | Just you |
| **User rules** | `~/.claude/rules/*.md` | Every session, all projects | Just you |
| **Project CLAUDE.md** | `./CLAUDE.md` or `./.claude/CLAUDE.md` | When in that project | Team (via git) |
| **Project rules** | `./.claude/rules/*.md` | When in that project | Team (via git) |
| **Local CLAUDE.md** | `./CLAUDE.local.md` | When in that project | Just you (gitignored) |
| **Path-scoped rules** | `.claude/rules/*.md` with `paths:` frontmatter | On-demand, when matching files are touched | Team (via git) |
| **Auto memory** | `~/.claude/projects/<project>/memory/` | First 200 lines at session start | Just you |

The user rule created by `/setup-ai-brain` lives in `~/.claude/rules/ai-brain.md` — loaded into every session so agents always know to search and save memories.

### Path-Scoped Rules

For project-specific memory behavior, add rules with `paths:` frontmatter:

```markdown
---
paths:
  - "docker-compose.yml"
  - "Dockerfile*"
---
Before modifying Docker configuration, search ai-brain for Docker-related
lessons learned in this project.
```

## Import Pipeline

Import existing knowledge (lessons learned, agent memory) via triage-based moderation:

```bash
# 1. Generate import report
python3 scripts/generate_import_report.py -o report.json

# 2. Triage in browser (uses claudius triage-findings)
# /triage-findings report.json

# 3. Import approved items
python3 scripts/import_triaged.py report.json

# Dry run (preview without storing)
python3 scripts/import_triaged.py --dry-run report.json
```

## Configuration

All settings via `.env` file (see `.env.example`). Environment variables override `.env`:

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_URL` | `http://localhost:11434` | Ollama API endpoint |
| `OLLAMA_LLM_MODEL` | `qwen2.5:14b` | LLM model for mem0 |
| `OLLAMA_EMBED_MODEL` | `nomic-embed-text` | Embedding model |
| `QDRANT_URL` | `http://localhost:6333` | Qdrant endpoint |
| `QDRANT_COLLECTION` | `ai_brain` | Collection name |
| `QDRANT_EMBED_DIMS` | `768` | Embedding dimensions |
| `NEO4J_ENABLED` | `false` | Enable Neo4j graph store |
| `NEO4J_URL` | `bolt://localhost:7687` | Neo4j bolt endpoint |

## Docker Services

```bash
docker compose up -d              # Qdrant only
docker compose --profile graph up -d  # Qdrant + Neo4j
```

Both services include Traefik labels for reverse proxy with basic auth. Set `QDRANT_DOMAIN`, `NEO4J_DOMAIN`, and `TRAEFIK_AUTH` in `.env`.

<sub>🤖 Co-authored by [Claudius the Magnificent](https://github.com/lklimek/claudius) AI Agent</sub>
