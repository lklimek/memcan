# MemCan — Persistent Memory MCP Server

AI agents forget everything when a session ends. Every new session starts blank — you re-explain your preferences, the agent repeats mistakes you've already corrected, and hard-won project context evaporates.

MemCan fixes this. It gives agents a persistent, searchable memory store that survives across sessions. Agents automatically save learnings, decisions, and preferences as they work, and recall them at the start of the next session. Over time your agents get smarter: they remember your coding style, know which approaches failed before, and understand the quirks of your project without being told again.

Works with any MCP-compatible agent. Tested and optimized for [Claude Code](https://claude.ai/code).

Built on embedded [LanceDB](https://lancedb.com/) + [fastembed](https://github.com/Anush008/fastembed-rs) (in-process ONNX embeddings) + [Ollama](https://ollama.com/) (local LLM for fact extraction and deduplication). No cloud, no external database — by default everything runs locally on your machine.

## Quick Start

```bash
# 1. Install the plugin (run inside a Claude Code session)
/plugin marketplace add lklimek/agents
/plugin install memcan@lklimek

# 2. Run setup — installs CLI, downloads server config, generates API keys
/setup-memcan

# 3. Start the server (command printed by setup, typically:)
cd ~/.config/memcan/server && docker compose up -d
```

`/setup-memcan` guides you through everything: CLI install, Docker Compose server config, `.env` generation, and user rule creation. Restart Claude Code after setup. For all configuration options, see the [Setup Guide](SETUP.md).

## Architecture

MemCan uses a two-component architecture:

- **Server** (`memcan-server`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service on port 8191 (internal), fronted by Traefik on port 8190.
- **CLI** (`memcan`) — thin HTTP client for hooks. Installed by `/setup-memcan`. No fastembed/LanceDB deps.

The Claude Code plugin connects to the server via HTTP MCP transport (Streamable HTTP).

### Stack

- **LanceDB** — embedded vector database (no server needed, data stored locally)
- **fastembed** — in-process ONNX embeddings (`MultilingualE5Large`, 1024 dimensions, ~1.3 GB model downloaded on first use)
- **Ollama** — LLM inference (`qwen3.5:9b` by default, via [ollama-rs](https://github.com/pepperoni21/ollama-rs)); MemCan reads `OLLAMA_HOST` and `OLLAMA_API_KEY` from settings and passes them to the Ollama client. A GPU is recommended for best performance.
- **rmcp 1.1** — Rust MCP SDK with Streamable HTTP transport
- **axum** — HTTP framework mounting MCP service + health endpoint + auth middleware
- **DISTILL_MEMORIES** — when enabled (default: `true`), the LLM extracts structured facts from raw text before storing

## Install

```bash
# In a Claude Code session:
/plugin marketplace add lklimek/agents
/plugin install memcan@lklimek
/setup-memcan
```

`/setup-memcan` installs the CLI, downloads the Docker Compose server config, generates API keys, creates `~/.config/memcan/.env`, and writes a user rule so agents always know to use memory. It prints the command to start the server (`docker compose up -d`) — run that, then restart Claude Code.

For Docker setup, building from source, environment variables, and remote Ollama, see the [Setup Guide](SETUP.md).

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

## Ollama

MemCan uses [Ollama](https://ollama.com/) for local LLM inference (fact extraction and deduplication). **A GPU is strongly recommended** — the default model (`qwen3.5:9b`) runs too slowly on CPU for interactive use.

### Using the bundled Ollama (docker compose)

The `docker-compose.yml` starts an Ollama container by default. After `docker compose up -d`, pull the model into it:

```bash
docker compose exec ollama ollama pull qwen3.5:9b
```

**GPU acceleration:** The bundled Ollama runs in CPU mode by default. To enable GPU, uncomment the `runtime: nvidia` and `deploy.resources` lines in `docker-compose.yml` (requires NVIDIA drivers and `nvidia-container-runtime`):

```yaml
  ollama:
    runtime: nvidia
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all
              capabilities: [gpu]
```

**Disable bundled Ollama:** Set `OLLAMA_HOST` in `~/.config/memcan/.env` to point at a remote or local Ollama instance, then comment out the `ollama:` service in `docker-compose.yml`.

### Using a standalone Ollama

```bash
# Install Ollama, then pull the default model
ollama pull qwen3.5:9b
```

If Ollama runs on a different machine, point MemCan at it:

```bash
OLLAMA_HOST=http://192.168.1.10:11434
# If the endpoint requires auth:
OLLAMA_API_KEY=your-token-here
```

> **Cloud LLM (OpenAI, Anthropic, etc.):** Prebuilt releases support only Ollama. The codebase has an optional `genai-llm` feature that can support other providers, but it's not enabled by default. If you need a cloud provider, [open an issue](https://github.com/lklimek/memcan/issues).

## License

MIT

<sub>Co-authored by [Claudius the Magnificent](https://github.com/lklimek/claudius) AI Agent</sub>
