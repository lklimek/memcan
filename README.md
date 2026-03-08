# MemCan — Persistent Memory for Claude Code

Rust MCP server providing persistent memory via embedded LanceDB + fastembed + genai. Store and recall learnings, decisions, and preferences across Claude Code sessions.

## Quick Start

```bash
# 1. Install Ollama (https://ollama.com/download) — needed for LLM only
ollama pull qwen3.5:4b

# 2. Start the MemCan server (choose one):
#    a) Docker (recommended):
docker compose up -d
#    b) From source:
cargo build --release -p memcan-server
./target/release/memcan serve

# 3. Install plugin in Claude Code
#    Settings → Plugins → enable memcan@lklimek
#    Or add to ~/.claude/settings.json:
#      "enabledPlugins": { "memcan@lklimek": true }

# 4. Configure environment (in a Claude Code session)
/setup-memcan
```

No external database required — LanceDB runs embedded on the server, storing data at `~/.local/share/memcan/lancedb`.

## Architecture

MemCan uses a two-component architecture:

- **Server** (`memcan serve`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service on port 8191 (internal), fronted by Traefik on port 8190.
- **CLI** (`memcan-cli`) — thin HTTP client for hooks. No fastembed/LanceDB deps (~5 MB vs ~180 MB server).

The Claude Code plugin connects to the server via HTTP MCP transport (Streamable HTTP).

### Stack

- **LanceDB** — embedded vector database (no server needed, data stored locally)
- **fastembed** — in-process ONNX embeddings (`MultilingualE5Large`, 1024 dimensions, ~1.3 GB model downloaded on first use)
- **genai + Ollama** — LLM inference (`ollama::qwen3.5:4b`); MemCan reads `OLLAMA_HOST` and passes it to the genai client
- **rmcp 1.1** — Rust MCP SDK with Streamable HTTP transport
- **axum** — HTTP framework mounting MCP service + health endpoint + auth middleware
- **DISTILL_MEMORIES** — when enabled (default: `true`), the LLM extracts structured facts from raw text before storing

## Install

### Prerequisites

- [Ollama](https://ollama.com/) — LLM inference (embeddings are handled in-process by fastembed on the server)
- Docker + Docker Compose (for containerized deployment) or Rust toolchain (for building from source)

### Plugin Install

Enable `memcan@lklimek` in `~/.claude/settings.json`:

```json
{
  "enabledPlugins": {
    "memcan@lklimek": true
  }
}
```

The plugin's `setup.sh` downloads the `memcan-cli` binary for your platform. The MCP server connection is registered automatically via `.mcp.json` — no manual `claude mcp add` needed.

> **Disk space:** The embedding model (`MultilingualE5Large`) requires ~1.3 GB of disk space, downloaded on the server's first startup. LanceDB data is stored at `~/.local/share/memcan/lancedb` (or `/data/lancedb` in Docker). Plan for ~2 GB total.

### Building from Source

```bash
cargo build --release --workspace
```

Binaries are placed in `target/release/`:
- `memcan` — fat server (MCP HTTP/stdio server + all admin subcommands)
- `memcan-cli` — thin HTTP client for hooks and manual operations

### Environment Setup

After enabling the plugin, run `/setup-memcan` in a Claude Code session. It will:

1. **Check prerequisites** — MemCan CLI binary, server reachability, Ollama reachability
2. **Configure `.env`** — copy `.env.example`, set server URL, API key, Ollama host
3. **Create user rule** — writes `~/.claude/rules/memcan.md` so agents know to use memory

Restart Claude Code after setup to connect the MCP server.

## MCP Tools

| Tool | Description |
|------|-------------|
| `add_memory` | Store a memory with optional project scope and metadata (async, returns queued) |
| `search_memories` | Semantic search across memories |
| `get_memories` | List all memories for a scope |
| `delete_memory` | Remove a memory by ID |
| `update_memory` | Modify existing memory content (async, returns queued) |
| `count_memories` | Count memories for a scope (without fetching content) |
| `list_collections` | Discover available collections, point counts, and valid filter values |
| `search_standards` | Search indexed standards (CWE, OWASP, etc.) by semantic similarity |
| `search_code` | Search indexed code snippets by semantic similarity |
| `get_queue_status` | Check status of async add/update operations |

## Server Subcommands

```
memcan serve [--stdio] [--listen ADDR]   # MCP server (default subcommand)
memcan index-code <dir> --project <name> [--tech-stack <s>] [--drop]
memcan index-standards <file> --standard-id <id> --standard-type <t> [--drop]
memcan migrate <file> [--dry-run]
memcan import-triaged <file> [--dry-run]
memcan test-classification --prompt <f> --model <m>
memcan download-model [--model <name>]
memcan completions <shell>
```

## CLI Subcommands

```
memcan-cli add <memory> [--project <p>]
memcan-cli search <query> [--project <p>] [--limit <n>]
memcan-cli extract                        # Hook handler: reads stdin, POSTs to server
memcan-cli status [operation_id]
memcan-cli count [--project <p>]
```

## Memory Scoping

- `project="penny"` → scoped to project (stored as `user_id=project:penny`)
- No project → global scope (stored as `user_id=global`)

## Claude Code Context Persistence

Claude Code loads context into the attention window via several mechanisms. MemCan leverages them to ensure agents always know to use memory:

| Mechanism | Location | When Loaded | Shared? |
|-----------|----------|-------------|---------|
| **User CLAUDE.md** | `~/.claude/CLAUDE.md` | Every session, all projects | Just you |
| **User rules** | `~/.claude/rules/*.md` | Every session, all projects | Just you |
| **Project CLAUDE.md** | `./CLAUDE.md` or `./.claude/CLAUDE.md` | When in that project | Team (via git) |
| **Project rules** | `./.claude/rules/*.md` | When in that project | Team (via git) |
| **Local CLAUDE.md** | `./CLAUDE.local.md` | When in that project | Just you (gitignored) |
| **Path-scoped rules** | `.claude/rules/*.md` with `paths:` frontmatter | On-demand, when matching files are touched | Team (via git) |
| **Auto memory** | `~/.claude/projects/<project>/memory/` | First 200 lines at session start | Just you |

The user rule created by `/setup-memcan` lives in `~/.claude/rules/memcan.md` — loaded into every session so agents always know to search and save memories.

## Configuration

The `.env` file configures both the server and CLI. Search order:

| Priority | Location | Use case |
|----------|----------|----------|
| 1 | `~/.config/memcan/.env` (Linux) / `~/Library/Application Support/memcan/.env` (macOS) | Production — survives plugin updates |
| 2 | `./.env` in CWD | Development — running from source checkout |
| 3 | Defaults | Fallback (localhost, default LanceDB path) |

Environment variables always override `.env` values. Run `/setup-memcan` to create the config file, or copy `.env.example` manually:

```bash
mkdir -p ~/.config/memcan
cp .env.example ~/.config/memcan/.env
```

**Settings reference** (see `.env.example`):

| Variable | Default | Description |
|----------|---------|-------------|
| `MEMCAN_LISTEN` | `127.0.0.1:8191` | Server bind address (Docker overrides to `0.0.0.0:8191`) |
| `MEMCAN_API_KEY` | *(none)* | Bearer token auth for MCP API |
| `MEMCAN_URL` | `http://localhost:8190` | Server URL for thin clients (`memcan-cli`) |
| `MEMCAN_LOG_FILE` | `~/.claude/logs/memcan-mcp.log` | Log file path (set empty for stdout) |
| `LANCEDB_PATH` | `~/.local/share/memcan/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `ollama::qwen3.5:4b` | LLM model (genai format with provider prefix) |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`) |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth |

> **Note:** The genai crate does **not** read `OLLAMA_HOST` or `OLLAMA_API_KEY` from environment — MemCan reads them via `Settings` and passes them to the genai client via `ServiceTargetResolver`.

## Remote Ollama

When Ollama runs on a remote host, set `OLLAMA_HOST` to point to it:

```bash
OLLAMA_HOST=https://ollama.example.com
```

If the endpoint is behind an auth proxy (e.g. Traefik, Caddy, nginx), set `OLLAMA_API_KEY` to send a Bearer token with every request:

```bash
OLLAMA_API_KEY=your-token-here
```

For production deployments, protect the Ollama endpoint with a reverse proxy providing TLS and access control.

## Docker Deployment

```bash
# Start Traefik + MemCan (uses remote Ollama via OLLAMA_HOST)
docker compose up -d

# Start with local GPU Ollama + Open WebUI
docker compose --profile gpu up -d
```

The `docker-compose.yml` provides:
- **Traefik** reverse proxy on ports 8190 (MemCan), 11434 (Ollama), 11400 (Open WebUI)
- **MemCan** server with Bearer token auth, health check, named volumes for data/models
- **Ollama** (optional, `gpu` profile) with NVIDIA runtime
- **Open WebUI** (optional, `gpu` profile) for Ollama web interface

Set `MEMCAN_API_KEY` in `.env` before deploying — it's used for both MemCan server auth and Traefik middleware auth.

## License

MIT

<sub>Co-authored by [Claudius the Magnificent](https://github.com/lklimek/claudius) AI Agent</sub>
