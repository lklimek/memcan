# MemCan — Setup Guide

Detailed setup instructions for MemCan. For a quick introduction, see the [README](README.md).

## Prerequisites

- [Ollama](https://ollama.com/) — LLM inference (embeddings are handled in-process by fastembed on the server). A GPU is strongly recommended for acceptable performance with the default model (`qwen3.5:9b`).
- Docker + Docker Compose (for containerized deployment) **or** Rust toolchain (for building from source)

## Server Setup

### Docker (recommended)

```bash
# Pull and start (lklimek/memcan:nightly from Docker Hub)
docker compose up -d

# Or build from the local Dockerfile
docker compose up -d --build

# Start with local GPU Ollama + Open WebUI
docker compose --profile gpu up -d
```

The `docker-compose.yml` provides:
- **Traefik** reverse proxy on ports 8190 (MemCan), 11434 (Ollama), 11400 (Open WebUI)
- **MemCan** server with Bearer token auth, health check, named volumes for data/models
- **Ollama** (optional, `gpu` profile) with NVIDIA runtime
- **Open WebUI** (optional, `gpu` profile) for Ollama web interface

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

> **Disk space:** The embedding model (`MultilingualE5Large`) requires ~1.3 GB of disk space, downloaded on the server's first startup. LanceDB data is stored at `~/.local/share/memcan/lancedb` (or `/data/lancedb` in Docker). Plan for ~2 GB total.

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
| `MEMCAN_URL` | `http://localhost:8190` | Server URL for thin clients (`memcan`) |
| `MEMCAN_LOG_FILE` | `~/.claude/logs/memcan-mcp.log` | Log file path (set empty for stdout) |
| `LANCEDB_PATH` | `~/.local/share/memcan/lancedb` | LanceDB storage directory |
| `DEFAULT_USER_ID` | `global` | Default memory scope |
| `DISTILL_MEMORIES` | `true` | Enable LLM fact extraction |
| `LLM_MODEL` | `qwen3.5:9b` | LLM model (genai format with provider prefix) |
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
