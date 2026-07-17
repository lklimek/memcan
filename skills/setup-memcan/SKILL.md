---
name: setup-memcan
description: Configure MemCan environment — .env file, Claude Code settings, and user rule. Run once per machine after plugin install.
user-invocable: true
allowed-tools: Read, Write, Edit, Bash(bash *), Bash(mkdir *), Bash(curl *), Bash(which *), Bash(cp *), Bash(cat *), Bash(python3 *), Glob, Grep, AskUserQuestion, mcp__plugin_memcan_brain__search
---

# Setup MemCan

Configures the MemCan environment. The plugin and MCP server are already installed — this sets up the `.env`, injects env vars into Claude Code settings, and creates the user rule. Idempotent.

## Architecture

MemCan uses a two-component architecture:
- **Server** (`memcan-server serve`) — long-lived HTTP MCP server handling embeddings, LLM, and storage. Runs as a Docker container or system service.
- **CLI** (`memcan`) — thin HTTP client. Installed via `setup.sh` or `cargo install memcan`.

The Claude Code plugin connects to the server via HTTP MCP transport (configured in `.mcp.json`).

## Steps

### 1. Install CLI and Server

Check `command -v memcan` and `docker compose version` to assess current state. Then use `AskUserQuestion` to present the user with install options:

- **Full install (Recommended)** — CLI binary + Docker Compose server setup. Requires Docker.
- **CLI only** — just the `memcan` CLI binary (for machines where the server runs elsewhere)
- **Skip** — everything is already installed, proceed to configuration

Based on the user's choice, run the appropriate command:

Full install:
```bash
bash <(curl -fsSL https://raw.githubusercontent.com/lklimek/memcan/main/setup.sh)
```

CLI only:
```bash
bash <(curl -fsSL https://raw.githubusercontent.com/lklimek/memcan/main/setup.sh) --cli-only
```

The script:
- Downloads and installs the `memcan` CLI binary
- (Full install) Downloads `docker-compose.yml` and creates `.env` files with generated API keys
- Does NOT auto-start the server — prints instructions for `docker compose up -d`

Verify the install succeeded (`command -v memcan`). If it fails, stop and report the error.

Note: The script creates `.env` files (server + CLI) but does NOT start the server. Server connectivity is checked in Step 4.

### 2. Configure Secrets & Settings

Resolve API keys and write configuration. Secrets never appear in conversation context.

Perform these steps:

1. **Resolve MEMCAN_API_KEY**: Check `~/.config/memcan/.env` for existing value, then `$MEMCAN_API_KEY` env var. If neither exists, generate a new key: `openssl rand -hex 32` (or `head -c 32 /dev/urandom | xxd -p -c 64` if openssl unavailable).

2. **Resolve MEMCAN_URL**: Check `~/.config/memcan/.env`, then `$MEMCAN_URL` env var. Default: `http://localhost:8190`.

3. **Resolve OLLAMA_API_KEY**: Check `~/.config/memcan/.env`, then `$OLLAMA_API_KEY` env var. May be empty.

4. **Write `~/.config/memcan/.env`**: Create directory if needed. If the file exists, update `MEMCAN_API_KEY`, `MEMCAN_URL`, and `OLLAMA_API_KEY` (if non-empty) in-place. If creating new, write a template with resolved values and commented defaults for `OLLAMA_HOST`, `LLM_MODEL`, `EMBED_MODEL`, `MEMCAN_LOG_FILE`.

5. **Merge into `~/.claude/settings.json`**: Read existing file (or `{}`), set `.env.MEMCAN_API_KEY` and `.env.MEMCAN_URL`, write back. Use `jq` or `python3` for JSON manipulation.

After completing, report status (without revealing secret values):
- Whether MEMCAN_API_KEY was existing or newly generated
- The MEMCAN_URL value

Review non-secret settings in `~/.config/memcan/.env` with the user:
- `MEMCAN_URL` — Server URL (default: `http://localhost:8190`)
- `OLLAMA_HOST` — Ollama endpoint (default: `http://localhost:11434`)
- `LLM_MODEL` — LLM model with provider prefix (default: `ollama::qwen3.5:4b`)
- `EMBED_MODEL` — fastembed model name (default: `MultilingualE5Large`)
- `MEMCAN_LOG_FILE` — Log file path (default: empty = stdout)

### 3. Create User Rule

Create `~/.claude/rules/memcan.md` (create `~/.claude/rules/` dir if needed).

Write this content:

```markdown
# MemCan — Persistent Memory

Use the MemCan MCP server to store and recall knowledge across sessions.

## Session Start
- Search for project context: `search(query="project context", project="<repo-name>")`
- This searches all collections (memories, standards, code) in one call

## When to Save (add_memory)
- Lessons learned from bugs, failed approaches, workarounds
- Architecture decisions with rationale
- User preferences and coding conventions
- Discovered patterns and anti-patterns
- Project-specific configuration quirks

## When to Search
- Use `search` (unified) as the default — searches all collections at once
- Use `search_memories`, `search_standards`, `search_code` for collection-specific filtering
- Before architectural decisions — check for prior art or known pitfalls
- On errors — search for similar past issues and their fixes
- During code reviews — recall project conventions

## Scoping
- Set `project` param to git remote origin repo name — NOT the directory name (e.g., `dash-evo-tool` not `dash-evo-tool-2`)
- Omit `project` for global memories (universal learnings)

## Quality Guidelines
- Be specific: "Axum .layer() last = outermost" > "middleware ordering matters"
- Include context: project, framework, language
- Tag with metadata: type (lesson/decision/preference), source (LL-NNN, PR#)
- Search before adding to avoid duplicates
```

### 4. Clean Up Deprecated Hooks

Auto-hooks (`SubagentStop` and `PreCompact` calling `memcan extract`) are deprecated. They captured raw agent output instead of distilled facts. Use the `lessons-learned` skill from the [claudius](https://github.com/lklimek/claudius) plugin for deliberate, quality-controlled memory extraction instead.

Scan both the user-level and any project-level `settings.json` files for lingering deprecated hooks:

1. Check `~/.claude/settings.json`
2. Check `.claude/settings.json` in the current working directory (if it exists and differs from the user-level file)

For each file found, read it and inspect all hook events. For each event, remove any hook entry whose `command` contains `memcan extract` or `command -v memcan`. Do not remove non-memcan hooks.

If any hooks were removed from a file, write the updated JSON back and report: "Removed deprecated memcan hook(s) from `<path>`. Use the `lessons-learned` skill from the `claudius` plugin for manual extraction."

If no deprecated hooks were found in a file, silently skip it (no output needed).

Use `python3` or `jq` for JSON manipulation. This step is safe to run multiple times — if no deprecated hooks exist, it is a no-op.

### 5. Verify

**Version check across all three components — mandatory, always run, never skip:**

The plugin's `.claude-plugin/plugin.json` (`version` field) is the baseline. Both the CLI and the server are checked against it — neither check is optional, and both must be reported even when nothing needed fixing.

1. **Read baseline**: `version` field from `.claude-plugin/plugin.json`.
2. **CLI version**: run `memcan --version`, extract the semver. Record it as "before".
   - If missing, or older than the baseline: auto-fix. Download the installer to a file rather than piping — `curl -fsSL https://raw.githubusercontent.com/lklimek/memcan/main/setup.sh -o <tmpfile>` — because auto-mode classifiers commonly block a direct `curl | bash` pipeline; a downloaded script is also auditable before running. Briefly review the downloaded script for anything unexpected, `chmod +x`, run it with `--cli-only`, then delete the temp file. Re-run `memcan --version` and record it as "after".
   - If the classifier (or the user) blocks execution, ask the user how to proceed (run it themselves / review first / skip) — do not silently give up and do not attempt to route around a denial.
   - If newer than the baseline, note it but do not downgrade.
3. **Server version**: fetch `curl -sf "$MEMCAN_URL/health"` — if that returns 401/403, retry with `-H "Authorization: Bearer $MEMCAN_API_KEY"`. Extract `version`. Record it as "before".
   - If different from the baseline: warn ("Plugin expects vX.Y.Z but server reports vA.B.C — new MCP tools may be missing"). If Docker Compose is available, auto-fix: locate `docker-compose.yml` (check `~/.config/memcan/` then current dir), run `docker compose pull && docker compose up -d`, wait for the health check to pass, then re-fetch `/health` and record the result as "after". If Docker is not available, tell the user to rebuild the server binary.
   - If unreachable, record as "unreachable" — the MCP connectivity check below will catch it too.
4. Keep every before/after value (or "unreachable") for the summary table below — do not discard them once the fix step finishes.

Print a summary. Lead with a versions table — one row per component, showing the version before this run and after (or `unchanged` when no fix was needed):

| Component | Before | After | Status |
|---|---|---|---|
| Plugin (baseline) | — | vX.Y.Z | — |
| CLI | vA.B.C | vX.Y.Z | updated / up to date / unreachable |
| Server | vA.B.C | vX.Y.Z | updated / up to date / unreachable |

Then the rest of the checklist:
- CLI installed and on PATH (`memcan`)
- `.env` exists at `~/.config/memcan/.env` with `MEMCAN_URL` and `MEMCAN_API_KEY` configured
- Claude Code settings at `~/.claude/settings.json` has `MEMCAN_API_KEY` and `MEMCAN_URL` in `env` block
- User rule exists at `~/.claude/rules/memcan.md`
- Hooks: report whether any deprecated memcan hooks were removed, or confirm none were found
- MCP server is connected (test: call `search(query="test")` — success = connected, failure or tool unavailable = not connected)

Security warnings (show only when applicable):
- If `MEMCAN_API_KEY` is not set: warn that the MCP server has no auth — anyone with network access can read/write memories.
- If `OLLAMA_HOST` starts with `https://` and `OLLAMA_API_KEY` is not set: warn that HTTPS endpoints typically require auth.
- If `OLLAMA_API_KEY` is set and `OLLAMA_HOST` starts with `http://` (not HTTPS): warn that the Bearer token will be sent in plaintext.

If the MCP server check failed, tell the user to ensure the server is running and restart Claude Code to pick up the new env vars from `~/.claude/settings.json`.
