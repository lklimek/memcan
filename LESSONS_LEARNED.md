# Lessons Learned — MemCan HTTP Refactoring (2026-03-08)

Import these into MemCan memory once the server is running.

## rmcp 0.16 → 1.1 Migration

### Breaking Changes

1. **`ServerInfo` and `Implementation` are non-exhaustive** — cannot use struct literal syntax. Use builder pattern or `ServerInfo { ..Default::default() }` with field overrides.

2. **`CallToolRequestParam` → `CallToolRequestParams`** (plural) — renamed in 1.0.

3. **`Peer<RoleClient>` → `RunningService<RoleClient, ()>`** — the return type of `.serve(transport)` changed. `RunningService` derefs to the peer, so `.call_tool()` works directly.

4. **`StreamableHttpService`** import path: `rmcp::transport::streamable_http_server::tower::{StreamableHttpServerConfig, StreamableHttpService}`. Session manager: `rmcp::transport::streamable_http_server::session::local::LocalSessionManager`.

5. **Client transport**: `StreamableHttpClientTransport::with_client(reqwest::Client, config)` — config built via `StreamableHttpClientTransportConfig::with_uri(url)`. Auth: `.auth_header("Bearer {key}")`.

6. **Client initialization**: `().serve(transport).await?` returns `RunningService<RoleClient, ()>`. The unit `()` implements `ClientHandler` as a no-op handler.

### tower-http Auth

`ValidateRequestHeaderLayer::bearer()` is deprecated in tower-http 0.6 as "too basic for real applications". Use custom axum middleware instead:

```rust
async fn bearer_auth(req: Request, next: Next) -> Response {
    let expected = std::env::var("MEMCAN_API_KEY").unwrap_or_default();
    if expected.is_empty() { return next.run(req).await; }
    match req.headers().get("authorization").and_then(|v| v.to_str().ok()) {
        Some(h) if h.strip_prefix("Bearer ").map(|t| t == expected).unwrap_or(false) => next.run(req).await,
        _ => (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    }
}
```

### StreamableHttpService Mounting

```rust
let config = StreamableHttpServerConfig::default();
let session_manager = LocalSessionManager::default();
let mcp = StreamableHttpService::new(move || service.clone(), config, session_manager);
let app = Router::new()
    .nest_service("/mcp", mcp)
    .route("/health", get(health_handler))
    .layer(middleware::from_fn(bearer_auth));
```

## Architecture Decisions

### Two-Binary Split
- Fat server `memcan` (~180MB): fastembed ONNX + LanceDB + genai + all subcommands
- Thin CLI `memcan-cli` (~5-10MB): reqwest + rmcp client only, NO memcan-core dep
- Key insight: fastembed's ONNX runtime is the size driver. Any binary linking memcan-core inherits it.

### Docker Networking
- Traefik uses `network_mode: host` but discovers containers via Docker API filtered by `--providers.docker.network=ollama_traefik`
- Containers MUST join the `traefik` (ollama_traefik) network for discovery, even if they don't need external access
- No `ports:` mapping on memcan = not directly accessible from host
- Memcan also joins `backend` (internal) for direct Ollama access

### Nothink Model Auto-Creation
- Ollama API: `POST /api/show {"name": "model"}` returns 200 if exists, 404 if not
- Ollama API: `POST /api/create {"model": "name", "from": "base", "system": "prompt"}` creates derived model
- Must send `Authorization: Bearer {key}` header when behind auth proxy
- Derived name pattern: `{base}-memcan-nothink` (e.g. `qwen3.5:9b-memcan-nothink`)
- System prompt: `/no_think\nAlways respond with valid JSON only. No markdown, no commentary.`

### genai Reasoning vs /no_think
- genai 0.5's `ReasoningEffort::None` is NOT implemented for Ollama adapter — only OpenAI/Anthropic/Gemini/DeepSeek
- The `/no_think` prefix workaround in system prompts remains necessary for Ollama models
- `chat_opts.with_normalize_reasoning_content(true)` strips thinking tags from response but doesn't prevent thinking

## Gotchas

1. **Docker Compose profiles**: `profiles: ["gpu"]` on a service means it's excluded from `docker compose up` unless `--profile gpu` or `COMPOSE_PROFILES=gpu` is set.

2. **Settings backward compat**: When renaming env vars (LOG_FILE → MEMCAN_LOG_FILE), read both and prefer the new name.

3. **Workspace Cargo.toml**: When adding new workspace members, also add their deps to `[workspace.dependencies]` if they use `workspace = true`.

4. **Subcommand migration pattern**: Convert `fn main()` to `pub async fn run(args: &CliArgs) -> Result<()>`, keeping the clap `#[derive(Parser)]` structs as `pub` for the parent to compose.

5. **Stdio MCP servers die between Claude Code tool calls** — the root cause of this entire refactoring. Background `tokio::spawn` tasks are killed when the process exits. Long-lived HTTP server solves this.
