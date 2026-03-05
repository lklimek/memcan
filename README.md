# MindOJO — Persistent Memory for Claude Code

MCP server providing persistent memory via [mem0](https://github.com/mem0ai/mem0). Store and recall learnings, decisions, and preferences across Claude Code sessions.

## Quick Start

```bash
# 1. Start Qdrant
docker compose up -d

# 2. Install dependencies
cd claude-plugin/mcp-server && uv sync && cd ../..

# 3. Install plugin in Claude Code
#    Settings → Plugins → enable mindojo@lklimek
#    Or add to ~/.claude/settings.json:
#      "enabledPlugins": { "mindojo@lklimek": true }

# 4. Configure environment (in a Claude Code session)
/setup-mindojo
```

## Install

### Prerequisites

- [uv](https://docs.astral.sh/uv/) — Python package manager
- [Docker](https://docs.docker.com/get-docker/) — for Qdrant
- [Ollama](https://ollama.ai/) — LLM + embeddings (external, not in this compose)

### Plugin Install

Enable `mindojo@lklimek` in `~/.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "mindojo@lklimek": true
  }
}
```

The plugin registers the MCP server automatically via `.mcp.json`. No manual `claude mcp add` needed.

### Environment Setup

After enabling the plugin, run `/setup-mindojo` in a Claude Code session. It will:

1. **Check prerequisites** — uv, Qdrant health, MCP server deps
2. **Configure `.env`** — copy `.env.example`, set your `OLLAMA_URL`
3. **Create user rule** — writes `~/.claude/rules/mindojo.md` so agents know to use memory

Restart Claude Code after setup to connect the MCP server.

## Architecture

- **mem0** — memory management (add, search, update, delete)
- **Qdrant** — vector similarity search (port 6333)
- **Ollama** — LLM (`qwen3.5:9b`, auto-derived as `qwen3.5:9b-mindojo-nothink`) + embeddings (`qwen3-embedding:8b`)
- **Neo4j** — optional graph store (`docker compose --profile graph up -d`)

## MCP Tools

| Tool | Description |
|------|-------------|
| `add_memory` | Store a memory with optional project scope and metadata |
| `search_memories` | Semantic search across memories |
| `get_memories` | List all memories for a scope |
| `delete_memory` | Remove a memory by ID |
| `update_memory` | Modify existing memory content |
| `count_memories` | Count memories for a scope (without fetching content) |

## Memory Scoping

- `project="penny"` → scoped to project (stored as `user_id=project:penny`)
- No project → global scope (stored as `user_id=global`)

## Claude Code Context Persistence

Claude Code loads context into the attention window via several mechanisms. MindOJO leverages them to ensure agents always know to use memory:

| Mechanism | Location | When Loaded | Shared? |
|-----------|----------|-------------|---------|
| **User CLAUDE.md** | `~/.claude/CLAUDE.md` | Every session, all projects | Just you |
| **User rules** | `~/.claude/rules/*.md` | Every session, all projects | Just you |
| **Project CLAUDE.md** | `./CLAUDE.md` or `./.claude/CLAUDE.md` | When in that project | Team (via git) |
| **Project rules** | `./.claude/rules/*.md` | When in that project | Team (via git) |
| **Local CLAUDE.md** | `./CLAUDE.local.md` | When in that project | Just you (gitignored) |
| **Path-scoped rules** | `.claude/rules/*.md` with `paths:` frontmatter | On-demand, when matching files are touched | Team (via git) |
| **Auto memory** | `~/.claude/projects/<project>/memory/` | First 200 lines at session start | Just you |

The user rule created by `/setup-mindojo` lives in `~/.claude/rules/mindojo.md` — loaded into every session so agents always know to search and save memories.

### Path-Scoped Rules

For project-specific memory behavior, add rules with `paths:` frontmatter:

```markdown
---
paths:
  - "docker-compose.yml"
  - "Dockerfile*"
---
Before modifying Docker configuration, search MindOJO for Docker-related
lessons learned in this project.
```

## Scripts

Utility scripts in `scripts/` for importing existing knowledge into MindOJO.

### `import_triaged.py`

Reads a triage-annotated report (produced by `triage-findings`), filters for findings with action `fix`, and stores them in mem0. Determines memory scope from each finding's recommendation field (`project:<name>` → project-scoped, otherwise global).

```bash
# Import approved items
python3 scripts/import_triaged.py report.json

# Preview without storing
python3 scripts/import_triaged.py --dry-run report.json
```

| Flag | Description |
|------|-------------|
| `report` | Path to triaged `report.json` (required) |
| `--dry-run` | Show what would be imported without storing |

## Configuration

The MCP server searches for `.env` in order:

| Priority | Location | Use case |
|----------|----------|----------|
| 1 | `~/.config/mindojo/.env` (Linux) / `~/Library/Application Support/mindojo/.env` (macOS) | Production — survives plugin updates |
| 2 | `./.env` in CWD | Development — running from source checkout |
| 3 | Defaults | Fallback (localhost Ollama + Qdrant) |

Environment variables always override `.env` values. Run `/setup-mindojo` to create the config file, or copy `.env.example` manually:

```bash
mkdir -p ~/.config/mindojo
cp .env.example ~/.config/mindojo/.env
```

**Settings reference** (see `.env.example`):

**Application:**

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_URL` | — | Ollama API endpoint (e.g. `http://host:11434`) |
| `OLLAMA_API_KEY` | — | Bearer token for Ollama auth (see [Ollama Authentication](#ollama-authentication)) |
| `OLLAMA_LLM_MODEL` | `qwen3.5:9b` | Base LLM model — server auto-derives `-mindojo-nothink` variant |
| `OLLAMA_EMBED_MODEL` | `qwen3-embedding:8b` | Embedding model |
| `QDRANT_URL` | `http://localhost:6333` | Qdrant endpoint |
| `QDRANT_COLLECTION` | `mindojo` | Collection name |
| `QDRANT_EMBED_DIMS` | `4096` | Embedding dimensions |
| `NEO4J_ENABLED` | `false` | Enable Neo4j graph store |
| `NEO4J_URL` | `bolt://localhost:7687` | Neo4j bolt endpoint |
| `NEO4J_USER` | `neo4j` | Neo4j username |
| `NEO4J_PASSWORD` | — | Neo4j password (required if Neo4j enabled) |
| `DEFAULT_USER_ID` | `global` | Default user ID for memory scoping |

**Infrastructure (Docker / Traefik):**

| Variable | Default | Description |
|----------|---------|-------------|
| `QDRANT_DOMAIN` | — | Domain for Qdrant via Traefik reverse proxy |
| `NEO4J_DOMAIN` | — | Domain for Neo4j via Traefik reverse proxy |
| `TRAEFIK_AUTH` | — | htpasswd hash for Traefik basic auth |

## Ollama Authentication

When Ollama runs on a remote host, protect it with a reverse proxy (e.g. Traefik, Caddy, nginx) that requires Bearer token authentication.

### How it works

The `ollama` Python client reads the `OLLAMA_API_KEY` environment variable and sends it as:

```
Authorization: Bearer <OLLAMA_API_KEY>
```

This is a static shared secret — no signing, expiry, or cryptographic exchange. Security depends entirely on the transport layer:

| Transport | Security | Recommendation |
|-----------|----------|----------------|
| `https://` (TLS) | Token encrypted in transit | Use for remote/cross-network |
| `http://` (plain) | Token visible on the wire | Only on trusted private networks |

### Setup

1. **Generate a token:**

   ```bash
   openssl rand -base64 32
   ```

2. **Configure your reverse proxy** to accept `Authorization: Bearer <token>`. Example Traefik middleware (file provider):

   ```yaml
   # traefik/dynamic/ollama.yml
   http:
     middlewares:
       ollama-bearer:
         plugin:
           apikey:
             # Or use forwardAuth / custom middleware for Bearer validation
             headers:
               - "Authorization: Bearer <your-token>"
     routers:
       ollama:
         rule: "Host(`ollama.example.com`)"
         entrypoints: websecure
         tls:
           certResolver: letsencrypt
         middlewares:
           - ollama-bearer
         service: ollama
     services:
       ollama:
         loadBalancer:
           servers:
             - url: "http://localhost:11434"
   ```

   > **Note:** Traefik doesn't have built-in Bearer auth middleware. Options:
   > - [traefik-api-key-auth](https://plugins.traefik.io/plugins/669e514b2e1faa5bb4ec1128/api-key-auth) plugin — validates Bearer tokens against a list
   > - `forwardAuth` middleware pointing to a small auth service
   > - Caddy or nginx with simple `if ($http_authorization)` matching

3. **Set in MindOJO `.env`:**

   ```bash
   OLLAMA_URL=https://ollama.example.com    # no credentials in URL
   OLLAMA_API_KEY=<your-token>              # read by ollama Python client
   ```

> **Do not embed credentials in `OLLAMA_URL`** (e.g. `http://user:pass@host`). The ollama Python client silently strips userinfo from URLs. Use `OLLAMA_API_KEY` instead.

## Docker Services

```bash
docker compose up -d              # Qdrant only
docker compose --profile graph up -d  # Qdrant + Neo4j
```

Both services include Traefik labels for reverse proxy with basic auth. Set `QDRANT_DOMAIN`, `NEO4J_DOMAIN`, and `TRAEFIK_AUTH` in `.env`.

<sub>Co-authored by [Claudius the Magnificent](https://github.com/lklimek/claudius) AI Agent</sub>
