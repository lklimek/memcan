# Web UI tests (dev only)

Not embedded or served. Node 22+ is a test runner only; production serves the unchanged ES modules.

```bash
# Pure-module tests (router, data, MCP client, task store, preferences)
node --test 'rs/memcan-server/src/ui/tests-js/*.test.mjs'

# Browser scenario against the fixture mock server (API key "test-key")
node rs/memcan-server/src/ui/tests-js/mock-server.mjs 8199 &
PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core [CHROMIUM=/path/to/chrome] [SCREENS=/tmp/screens] \
  node rs/memcan-server/src/ui/tests-js/browser-check.mjs http://127.0.0.1:8199
```

`mock-server.mjs` env: `MOCK_SESSIONS=1` (session-bearing MCP), `MOCK_SSE=1` (SSE replies),
`MOCK_BASIC=1` (Basic Auth `ui:ui` on `/ui*`), `MOCK_SLOW_MS=<ms>` (delays task exports; also settable via `GET /__slow?ms=<ms>`); `GET /__retitle?title=<t>` changes
one fixture task to simulate a concurrent agent write, `MOCK_HOST_GUARD=1` (every `/mcp`
call gets rmcp's `403 Forbidden: Host header is not allowed`).
