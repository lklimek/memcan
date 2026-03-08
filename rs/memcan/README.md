# memcan

Persistent memory CLI for [Claude Code](https://docs.anthropic.com/en/docs/claude-code) — store and recall learnings, decisions, and preferences across sessions.

`memcan` is the thin CLI client that communicates with the [MemCan MCP server](https://github.com/lklimek/memcan). Used by Claude Code plugin hooks for automatic memory extraction.

## Install

```bash
cargo install memcan
```

## Usage

```bash
# Add a memory
memcan add "Axum .layer() order: last added = outermost middleware" --project my-app

# Search memories
memcan search "middleware ordering" --project my-app

# Extract memories from conversation (used by hooks, reads stdin)
memcan extract

# Check server status
memcan status

# Count stored memories
memcan count --project my-app
```

## Configuration

Set in `~/.config/memcan/.env` or environment:

| Variable | Default | Description |
|---|---|---|
| `MEMCAN_URL` | `http://localhost:8190` | MemCan server URL |
| `MEMCAN_API_KEY` | *(none)* | Bearer token for server auth |

## Part of MemCan

This is the CLI component of [MemCan](https://github.com/lklimek/memcan) — a Claude Code plugin for persistent memory via LanceDB + fastembed. See the main repo for server setup, Docker deployment, and plugin installation.

## License

MIT
