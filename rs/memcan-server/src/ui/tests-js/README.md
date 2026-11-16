# Web UI tests (dev only)

Not embedded or served. Node 22+ is a test runner only; production serves the unchanged ES modules.

```bash
# Pure-module tests (router, data, auth, MCP client, task store, preferences)
node --test 'rs/memcan-server/src/ui/tests-js/*.test.mjs'

# Browser scenario against the fixture mock server (sign-in viewer / test-pass)
node rs/memcan-server/src/ui/tests-js/mock-server.mjs 8199 &
PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core [CHROMIUM=/path/to/chrome] [SCREENS=/tmp/screens] \
  node rs/memcan-server/src/ui/tests-js/browser-check.mjs http://127.0.0.1:8199
```

`mock-server.mjs` env: `MOCK_SECURE=1` (plain-HTTP sign-in not allowed: the UI shows "HTTPS required"),
`MOCK_SESSIONS=1` (session-bearing MCP), `MOCK_SSE=1` (SSE replies), `MOCK_SLOW_MS=<ms>` (delays task
exports; also settable via `GET /__slow?ms=<ms>`), `MOCK_HOST_GUARD=1` (every `/ui/api/mcp` call gets the
backend's `403 host_not_allowed`). `GET /__retitle?title=<t>` changes one fixture task to simulate a
concurrent agent write; `GET /__expire` drops every sign-in session (expiry or restart). A request
with `Authorization: Basic viewer:test-pass` (base64) to `/ui/auth/session` signs in like a proxy.
