# MemCan — Setup Guide

Detailed setup instructions for MemCan. For a quick introduction, see the [README](README.md).

## Prerequisites

- [Ollama](https://ollama.com/) — LLM inference (embeddings are handled in-process by fastembed on the server). A GPU is strongly recommended for acceptable performance with the default model (`qwen3.5:9b`).
- Docker + Docker Compose (for containerized deployment) **or** Rust toolchain (for building from source)

## Server Setup

### Docker (recommended)

```bash
# The setup skill writes COMPOSE_PROFILES=ollama to the server .env.
# Start all configured services (Ollama included when COMPOSE_PROFILES=ollama):
docker compose up -d

# Pull the default LLM model into the bundled Ollama
docker compose exec ollama ollama pull qwen3.5:9b

# Or build from the local Dockerfile
docker compose up -d --build

# Enable Open WebUI alongside Ollama
COMPOSE_PROFILES=ollama,webui docker compose up -d
```

The `docker-compose.yml` provides:
- **Traefik** reverse proxy on ports 8190 (MemCan), 11434 (Ollama), 11400 (Open WebUI)
- **MemCan** server with Bearer token auth, health check, named volumes for data/models
- **Ollama** (`ollama` profile) — CPU mode by default; GPU requires uncommenting `runtime: nvidia` in `docker-compose.yml`; disable by setting `COMPOSE_PROFILES=` in the server `.env`
- **Open WebUI** (`webui` profile) for Ollama web interface

Set `MEMCAN_API_KEY` in `.env` before deploying — it's used for both MemCan server auth and Traefik middleware auth.

### Building from Source

```bash
cargo build --release --workspace
```

Binaries are placed in `target/release/`:
- `memcan-server` — fat server (MCP HTTP/stdio server + all admin subcommands)
- `memcan` — thin HTTP client for hooks and manual operations

Start the server (requires local Ollama):

```bash
# Pull the default model first
ollama pull qwen3.5:9b

./target/release/memcan-server serve
```

> **URL note:** Without Docker/Traefik, the server binds to `127.0.0.1:8191` directly (no proxy in front). The CLI/plugin defaults to `MEMCAN_URL=http://localhost:8190` (the Traefik port). When running from source, set `MEMCAN_URL=http://localhost:8191` in `~/.config/memcan/.env`, or start the server on port 8190 with `--listen 127.0.0.1:8190`.

> **Disk space:** The embedding model (`MultilingualE5Large`) requires ~1.3 GB of disk space, downloaded on the server's first startup. LanceDB data is stored at `~/.local/share/memcan/lancedb` (or `/data/lancedb` in Docker). Plan for ~2 GB total.

## Configuration

The `.env` file configures both the server and CLI. Search order:

| Priority | Location | Use case |
|----------|----------|----------|
| 1 | `./.env` in CWD | Development — running from source checkout |
| 2 | `~/.config/memcan/.env` (Linux) / `~/Library/Application Support/memcan/.env` (macOS) | Production — survives plugin updates |
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
| `MEMCAN_URL` | `http://localhost:8190` | Server URL for thin clients (`memcan`) |
| `MEMCAN_LOG_FILE` | `~/.claude/logs/memcan-mcp.log` | Log file path (set empty for stdout) |
| `LANCEDB_PATH` | `~/.local/share/memcan/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `ollama::qwen3.5:9b` | Ollama model name. Use the `ollama::` prefix (e.g. `ollama::qwen3.5:9b`). The prefix is stripped when calling the Ollama API. |
| `EMBED_MODEL` | `MultilingualE5Large` | Fastembed model for in-process embeddings (dimensions derived automatically) |
| `OLLAMA_HOST` | *(none)* | Ollama server URL (e.g. `http://10.29.188.1:11434`). Read by MemCan via `Settings` and passed to the Ollama client. |
| `OLLAMA_API_KEY` | *(none)* | Bearer token for Ollama endpoint auth. Read by MemCan via `Settings` and sent with every request to Ollama. |

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

## Server Subcommands

```
memcan-server serve [--stdio] [--listen ADDR]   # MCP server (default subcommand)
memcan-server index-code <dir> --project <name> [--tech-stack <s>] [--drop]
memcan-server index-standards <file> --standard-id <id> --standard-type <t> [--drop]
memcan-server migrate <file> [--dry-run]
memcan-server import-triaged <file> [--dry-run]
memcan-server test-classification --prompt <f> --model <m>
memcan-server download-model [--model <name>]
memcan-server completions <shell>
```

## CLI Subcommands

```
memcan add <memory> [--project <p>]
memcan search <query> [--project <p>] [--limit <n>]
memcan extract                        # Hook handler: reads stdin, POSTs to server
memcan status [operation_id]
memcan count [--project <p>]
```
