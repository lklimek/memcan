# MindOJO — Persistent Memory for Claude Code

Rust MCP server providing persistent memory via embedded LanceDB + Ollama. Store and recall learnings, decisions, and preferences across Claude Code sessions.

## Quick Start

```bash
# 1. Install Ollama (https://ollama.com/download)
ollama pull qwen3-embedding:4b && ollama pull qwen3.5:4b

# 2. Install plugin in Claude Code
#    Settings → Plugins → enable mindojo@lklimek
#    Or add to ~/.claude/settings.json:
#      "enabledPlugins": { "mindojo@lklimek": true }

# 3. Configure environment (in a Claude Code session)
/setup-mindojo
```

No external database required — LanceDB runs embedded, storing data at `~/.local/share/mindojo/lancedb`.

## Install

### Prerequisites

- [Ollama](https://ollama.com/) — LLM + embeddings

### Plugin Install

Enable `mindojo@lklimek` in `~/.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "mindojo@lklimek": true
  }
}
```

The plugin's `setup.sh` downloads pre-built binaries for your platform. The MCP server is registered automatically via `.mcp.json` — no manual `claude mcp add` needed.

### Building from Source

```bash
cargo build --release --workspace
```

Binaries are placed in `target/release/`: `mindojo-mcp`, `mindojo-extract`, `mindojo-import-triaged`, `mindojo-index-code`, `mindojo-index-standards`, `mindojo-migrate`.

### Environment Setup

After enabling the plugin, run `/setup-mindojo` in a Claude Code session. It will:

1. **Check prerequisites** — MindOJO binary, Ollama reachability
2. **Configure `.env`** — copy `.env.example`, set your `OLLAMA_URL`
3. **Create user rule** — writes `~/.claude/rules/mindojo.md` so agents know to use memory

Restart Claude Code after setup to connect the MCP server.

## Architecture

- **LanceDB** — embedded vector database (no server needed, data stored locally)
- **Ollama** — LLM (`qwen3.5:4b`) + embeddings (`qwen3-embedding:4b`)
- **DISTILL_MEMORIES** — when enabled (default: `true`), the LLM extracts structured facts from raw text before storing

## MCP Tools

| Tool | Description |
|------|-------------|
| `add_memory` | Store a memory with optional project scope and metadata |
| `search_memories` | Semantic search across memories |
| `get_memories` | List all memories for a scope |
| `delete_memory` | Remove a memory by ID |
| `update_memory` | Modify existing memory content |
| `count_memories` | Count memories for a scope (without fetching content) |
| `list_collections` | Discover available collections, point counts, and valid filter values |
| `search_standards` | Search indexed standards (CWE, OWASP, etc.) by semantic similarity |
| `search_code` | Search indexed code snippets by semantic similarity |

## Memory Scoping

- `project="penny"` → scoped to project (stored as `user_id=project:penny`)
- No project → global scope (stored as `user_id=global`)

## CLI Tools

| Binary | Description |
|--------|-------------|
| `mindojo-mcp` | MCP server (stdio transport) — registered by the plugin |
| `mindojo-extract` | Hook binary — extracts learnings from conversations |
| `mindojo-import-triaged` | Imports triaged findings from JSON into memories |
| `mindojo-index-code` | Indexes source code files for semantic search |
| `mindojo-index-standards` | Indexes technical standards documents (CWE, OWASP, etc.) |
| `mindojo-migrate` | Migrates/imports legacy JSON data |

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

## Configuration

The MCP server searches for `.env` in order:

| Priority | Location | Use case |
|----------|----------|----------|
| 1 | `~/.config/mindojo/.env` (Linux) / `~/Library/Application Support/mindojo/.env` (macOS) | Production — survives plugin updates |
| 2 | `./.env` in CWD | Development — running from source checkout |
| 3 | Defaults | Fallback (localhost Ollama, default LanceDB path) |

Environment variables always override `.env` values. Run `/setup-mindojo` to create the config file, or copy `.env.example` manually:

```bash
mkdir -p ~/.config/mindojo
cp .env.example ~/.config/mindojo/.env
```

**Settings reference** (see `.env.example`):

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_URL` | `http://localhost:11434` | Ollama API endpoint |
| `OLLAMA_API_KEY` | — | Bearer token for Ollama auth (see [Ollama Authentication](#ollama-authentication)) |
| `LANCEDB_PATH` | `~/.local/share/mindojo/lancedb` | LanceDB storage directory |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction before storing |
| `DEFAULT_USER_ID` | `global` | Default user ID for memory scoping |
| `TECH_STACK` | — | Default tech stack filter (e.g. "rust", "python") |
| `LLM_MODEL` | `qwen3.5:4b` | Ollama chat model |
| `EMBED_MODEL` | `qwen3-embedding:4b` | Ollama embedding model |
| `EMBED_DIMS` | `2560` | Embedding vector dimensions |
| `LOG_FILE` | `~/.claude/logs/mindojo-mcp.log` | Log file path |

## Ollama Authentication

When Ollama runs on a remote host, protect it with a reverse proxy (e.g. Traefik, Caddy, nginx) that requires Bearer token authentication.

### How it works

The MCP server reads the `OLLAMA_API_KEY` environment variable and sends it as:

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
   OLLAMA_URL=https://ollama.example.com
   OLLAMA_API_KEY=<your-token>
   ```

> **Do not embed credentials in `OLLAMA_URL`** (e.g. `http://user:pass@host`). Use `OLLAMA_API_KEY` instead.

## Docker Services

```bash
docker compose up -d              # Ollama (optional)
```

The `docker-compose.yml` provides an optional Ollama container for development. LanceDB is embedded and requires no container. For most setups, install Ollama directly from [ollama.com](https://ollama.com/download).

<sub>Co-authored by [Claudius the Magnificent](https://github.com/lklimek/claudius) AI Agent</sub>
