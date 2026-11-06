import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sessionAuth } from '../static/auth.js';
import { McpError, P1_TOOLS, createMcpClient, createSseParser } from '../static/mcp.js';

const CSRF = 'csrf-token-not-real';
const AUTH = sessionAuth(CSRF);
const ALL_TOOLS = [...P1_TOOLS, 'add_todo', 'delete_todo'];

function json(body, init = {}) {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    headers: { 'content-type': 'application/json', ...(init.headers ?? {}) },
  });
}

/** Mock MCP server: `handlers.tool(name, args, req)` returns a tool payload or a Response. */
function mockServer(handlers = {}) {
  const calls = [];
  let sessionCounter = 0;
  const fetch = async (url, init) => {
    const body = init.body ? JSON.parse(init.body) : null;
    const headers = new Headers(init.headers);
    const call = { url, init, body, headers, method: init.method };
    calls.push(call);
    if (handlers.raw) {
      const raw = await handlers.raw(call, calls);
      if (raw) return raw;
    }
    if (body.method === 'initialize') {
      const extra = {};
      if (handlers.sessions) extra['mcp-session-id'] = `s${++sessionCounter}`;
      return json(
        {
          jsonrpc: '2.0',
          id: body.id,
          result: {
            protocolVersion: handlers.protocolVersion ?? '2025-11-25',
            capabilities: { tools: {} },
            serverInfo: { name: 'memcan', version: '3.0.0' },
          },
        },
        { headers: extra },
      );
    }
    if (body.method === 'notifications/initialized') return new Response(null, { status: 202 });
    if (body.method === 'tools/list') {
      const names = handlers.tools ?? ALL_TOOLS;
      return json({ jsonrpc: '2.0', id: body.id, result: { tools: names.map((name) => ({ name })) } });
    }
    if (body.method === 'tools/call') {
      const out = await (handlers.tool ?? (() => ({ ok: true })))(body.params.name, body.params.arguments, call);
      if (out instanceof Response) return out;
      return json({
        jsonrpc: '2.0',
        id: body.id,
        result: { content: [{ type: 'text', text: JSON.stringify(out) }] },
      });
    }
    throw new Error(`unexpected ${body.method}`);
  };
  return { fetch, calls };
}

function client(server, extra = {}) {
  const delays = [];
  const c = createMcpClient({
    auth: AUTH,
    fetch: server.fetch,
    sleep: async (ms) => {
      delays.push(ms);
    },
    random: () => 0.5,
    maxStartsPerSecond: 1000,
    ...extra,
  });
  return { c, delays };
}

test('stateless connect: initialize, initialized 202 without parse, tools/list; headers are explicit', async () => {
  const server = mockServer();
  const { c } = client(server);
  const info = await c.connect();
  assert.deepEqual(
    server.calls.map((call) => call.body.method),
    ['initialize', 'notifications/initialized', 'tools/list'],
  );
  assert.equal(info.protocolVersion, '2025-11-25');
  assert.deepEqual(info.missing, []);
  for (const call of server.calls) {
    assert.equal(call.url, '/ui/api/mcp');
    assert.equal(call.method, 'POST');
    assert.equal(call.headers.get('authorization'), null);
    assert.equal(call.headers.get('x-csrf-token'), CSRF);
    assert.equal(call.headers.get('content-type'), 'application/json');
    assert.equal(call.headers.get('accept'), 'application/json, text/event-stream');
    assert.equal(call.headers.get('mcp-session-id'), null);
    assert.equal(call.init.credentials, 'same-origin');
    assert.equal(call.init.redirect, 'error');
    assert.equal(call.init.cache, 'no-store');
  }
  assert.equal(server.calls[0].body.params.protocolVersion, '2025-11-25');
  assert.equal(server.calls[0].body.params.clientInfo.name, 'memcan-webui');
  assert.equal('id' in server.calls[1].body, false);
  assert.equal(server.calls[2].headers.get('mcp-protocol-version'), '2025-11-25');
  const result = await c.callTool('get_todo', { todo_id: 'x' });
  assert.deepEqual(result, { ok: true });
});

test('concurrent reads share one initialization', async () => {
  const server = mockServer();
  const { c } = client(server);
  await Promise.all([c.callTool('list_collections', {}), c.callTool('get_queue_status', { limit: 10 })]);
  assert.equal(server.calls.filter((call) => call.body.method === 'initialize').length, 1);
});

test('unsupported protocol version is a protocol error', async () => {
  const { c } = client(mockServer({ protocolVersion: '1999-01-01' }));
  await assert.rejects(c.connect(), (e) => e instanceof McpError && e.category === 'protocol');
});

test('session expiry triggers one shared re-init and one retry', async () => {
  let expired = new Set(['s1']);
  const server = mockServer({
    sessions: true,
    tool: (name, args, call) => {
      if (expired.has(call.headers.get('mcp-session-id'))) return new Response('gone', { status: 404 });
      return { session: call.headers.get('mcp-session-id') };
    },
  });
  const { c } = client(server);
  await c.connect();
  assert.equal(server.calls[1].headers.get('mcp-session-id'), 's1');
  const [a, b] = await Promise.all([c.callTool('list_collections', {}), c.callTool('get_queue_status', {})]);
  assert.deepEqual(a, { session: 's2' });
  assert.deepEqual(b, { session: 's2' });
  assert.equal(server.calls.filter((call) => call.body.method === 'initialize').length, 2);
  expired = new Set(['s2', 's3', 's4']);
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'unavailable');
  assert.equal(server.calls.filter((call) => call.body.method === 'initialize').length, 3);
});

test('sessionless 404 is endpoint unavailability, not a reconnect loop', async () => {
  const server = mockServer({ tool: () => new Response('nope', { status: 404 }) });
  const { c } = client(server, { retries: 0 });
  await assert.rejects(c.callTool('get_todo', { todo_id: 'a' }), (e) => e.category === 'unavailable');
  assert.equal(server.calls.filter((call) => call.body.method === 'initialize').length, 1);
});

test('401 stops work, signs the tab out and notifies once', async () => {
  let authFailures = 0;
  const server = mockServer({ tool: () => new Response('Unauthorized', { status: 401 }) });
  const { c } = client(server, { onAuthFailure: () => (authFailures += 1) });
  await assert.rejects(
    c.callTool('get_todo', { todo_id: 'a' }),
    (e) => e.category === 'auth' && e.reason === 'session' && e.message === 'Session expired. Sign in to continue.',
  );
  assert.equal(authFailures, 1);
  const before = server.calls.length;
  await assert.rejects(c.callTool('get_todo', { todo_id: 'a' }), (e) => e.category === 'aborted');
  assert.equal(server.calls.length, before);
});

async function forbidden(body, headers = {}) {
  const failures = [];
  const server = mockServer({ tool: () => new Response(body, { status: 403, headers }) });
  const { c, delays } = client(server, { onAuthFailure: (error) => failures.push(error) });
  let error;
  try {
    await c.callTool('list_collections', {});
  } catch (caught) {
    error = caught;
  }
  assert.ok(error instanceof McpError);
  assert.deepEqual(delays, [], 'a 403 is never retried');
  assert.equal(failures.length, 1, 'polling stops and the tab is signed out');
  const before = server.calls.length;
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'aborted');
  assert.equal(server.calls.length, before);
  return error;
}

test('403 for a rejected security token or origin is a policy denial', async () => {
  const error = await forbidden(JSON.stringify({ error: 'csrf', message: 'The security token is missing or invalid.' }), {
    'content-type': 'application/json',
  });
  assert.equal(error.category, 'auth');
  assert.equal(error.reason, 'policy');
  assert.match(error.message, /origin, security token or an IP allowlist/);
});

test('403 from the backend host guard explains localhost access', async () => {
  const error = await forbidden(JSON.stringify({ error: 'host_not_allowed', message: 'x' }), { 'content-type': 'application/json' });
  assert.equal(error.reason, 'host');
  assert.match(error.message, /only accepts this UI on localhost/);
  assert.match(error.message, /http:\/\/localhost:8190/);
});

test('any other 403 gets generic access-denied copy and its body is never echoed', async () => {
  const error = await forbidden('<html><script>alert(1)</script>blocked by WAF</html>', { 'content-type': 'text/html' });
  assert.equal(error.reason, 'policy');
  assert.match(error.message, /Access denied by server or network policy/);
  assert.doesNotMatch(error.message, /script|WAF/);
});

test('403 body reading is bounded', async () => {
  const huge = `${'x'.repeat(1024 * 1024)}"host_not_allowed"`;
  const error = await forbidden(huge);
  assert.equal(error.reason, 'policy', 'markers beyond the read bound are ignored');
});

test('transient failures retry twice with jittered 1s/3s delays', async () => {
  let attempts = 0;
  const server = mockServer({
    tool: () => {
      attempts += 1;
      return new Response('down', { status: 503 });
    },
  });
  const { c, delays } = client(server);
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'unavailable');
  assert.equal(attempts, 3);
  assert.equal(delays.length, 2);
  assert.ok(delays[0] >= 750 && delays[0] <= 1250, String(delays[0]));
  assert.ok(delays[1] >= 2250 && delays[1] <= 3750, String(delays[1]));
});

test('network errors are unavailable and retried', async () => {
  let attempts = 0;
  const server = mockServer({
    tool: () => {
      attempts += 1;
      if (attempts < 2) throw new TypeError('network down');
      return { fine: 1 };
    },
  });
  const { c } = client(server);
  assert.deepEqual(await c.callTool('list_collections', {}), { fine: 1 });
});

test('429 honors Retry-After', async () => {
  let attempts = 0;
  const server = mockServer({
    tool: () => {
      attempts += 1;
      if (attempts === 1) return new Response('slow down', { status: 429, headers: { 'retry-after': '2' } });
      return { ok: 1 };
    },
  });
  const { c, delays } = client(server);
  assert.deepEqual(await c.callTool('list_collections', {}), { ok: 1 });
  assert.deepEqual(delays, [2000]);
});

test('long Retry-After surfaces throttled without waiting', async () => {
  const server = mockServer({
    tool: () => new Response('slow down', { status: 429, headers: { 'retry-after': '600' } }),
  });
  const { c, delays } = client(server);
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'throttled' && e.retryAfter === 600);
  assert.deepEqual(delays, []);
});

test('tool allowlist blocks writes and non-todo exports without any request', async () => {
  const server = mockServer();
  const { c } = client(server);
  await c.connect();
  const before = server.calls.length;
  for (const [name, args] of [
    ['delete_todo', { todo_id: 'a' }],
    ['add_todo', { title: 't', project: 'p' }],
    ['export_collection', { collection: 'memories' }],
    ['tools/call', {}],
  ]) {
    await assert.rejects(c.callTool(name, args), (e) => e.category === 'compat', name);
  }
  assert.equal(server.calls.length, before);
});

test('missing advertised read tools are reported and refused locally', async () => {
  const server = mockServer({ tools: ['list_collections', 'get_queue_status', 'export_collection', 'list_todos'] });
  const { c } = client(server);
  const info = await c.connect();
  assert.deepEqual(info.missing, ['get_todo']);
  await assert.rejects(c.callTool('get_todo', { todo_id: 'a' }), (e) => e.category === 'compat');
});

test('tools/list cursor is followed', async () => {
  const server = mockServer({
    raw: (call) => {
      if (call.body.method !== 'tools/list') return null;
      if (!call.body.params?.cursor) {
        return json({ jsonrpc: '2.0', id: call.body.id, result: { tools: [{ name: 'get_todo' }], nextCursor: 'c2' } });
      }
      return json({ jsonrpc: '2.0', id: call.body.id, result: { tools: P1_TOOLS.slice(1).map((name) => ({ name })) } });
    },
  });
  const { c } = client(server);
  assert.deepEqual((await c.connect()).missing, []);
});

test('JSON-RPC errors, isError results and invalid JSON are typed separately', async () => {
  const server = mockServer({
    tool: (name) => {
      if (name === 'get_todo') {
        return json({
          jsonrpc: '2.0',
          id: 99,
          error: { code: -32603, message: "get_todo failed: ambiguous todo id prefix 'abcd1234' — provide the full UUID" },
        });
      }
      if (name === 'list_collections') {
        return json({ jsonrpc: '2.0', id: 3, result: { isError: true, content: [{ type: 'text', text: '<b>x</b>' }] } });
      }
      if (name === 'list_todos') {
        return json({ jsonrpc: '2.0', id: 3, error: { code: -32602, message: 'bad params' } });
      }
      return json({ jsonrpc: '2.0', id: 3, result: { content: [{ type: 'text', text: '{nope' }] } });
    },
  });
  // The handlers above use fixed ids; echo the real request id so matching succeeds.
  const fetch = async (url, init) => {
    const response = await server.fetch(url, init);
    const body = JSON.parse(init.body);
    if (body.method !== 'tools/call') return response;
    const payload = await response.json();
    payload.id = body.id;
    return json(payload);
  };
  const { c } = client({ fetch });
  await assert.rejects(c.callTool('get_todo', { todo_id: 'abcd1234' }), (e) => e.category === 'ambiguous');
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'tool');
  await assert.rejects(c.callTool('list_todos', { project: 'p' }), (e) => e.category === 'invalid-input');
  await assert.rejects(c.callTool('get_queue_status', {}), (e) => e.category === 'protocol');
});

test('mismatched response id is a protocol error', async () => {
  const server = mockServer({
    tool: () => json({ jsonrpc: '2.0', id: 'other', result: { content: [{ type: 'text', text: '{}' }] } }),
  });
  const { c } = client(server);
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'protocol');
});

test('structuredContent is preferred when present', async () => {
  const server = mockServer({
    tool: (name, args, call) =>
      json({
        jsonrpc: '2.0',
        id: call.body.id,
        result: { structuredContent: { a: 1 }, content: [{ type: 'text', text: '{"a":2}' }] },
      }),
  });
  const { c } = client(server);
  assert.deepEqual(await c.callTool('list_collections', {}), { a: 1 });
});

test('SSE responses are parsed with comments and multi-line data', async () => {
  const server = mockServer({
    tool: (name, args, call) => {
      const message = JSON.stringify({
        jsonrpc: '2.0',
        id: call.body.id,
        result: { content: [{ type: 'text', text: '{"sse":true}' }] },
      });
      // Split at a token boundary: multi-line data joins with "\n", valid JSON whitespace there.
      const cut = message.indexOf('"result":') + '"result":'.length;
      const unrelated = JSON.stringify({ jsonrpc: '2.0', method: 'notifications/progress', params: {} });
      const stream = new ReadableStream({
        start(controller) {
          const enc = new TextEncoder();
          controller.enqueue(enc.encode(`: keep-alive\n\ndata: ${unrelated}\n\nevent: message\ndata: `));
          controller.enqueue(enc.encode(`${message.slice(0, cut)}\ndata: ${message.slice(cut)}\n\n`));
          controller.close();
        },
      });
      return new Response(stream, { status: 200, headers: { 'content-type': 'text/event-stream' } });
    },
  });
  const { c } = client(server);
  assert.deepEqual(await c.callTool('list_collections', {}), { sse: true });
});

test('createSseParser handles chunk boundaries, CRLF, comments and multi-line data', () => {
  const events = [];
  const parser = createSseParser((event) => events.push(event));
  parser.push(': hi\r\nevent: message\r\ndata: {"a":\r\n');
  parser.push('data: 1}\r\n\r\ndata: x');
  parser.push('\n\n');
  assert.deepEqual(events, [
    { event: 'message', data: '{"a":\n1}' },
    { event: 'message', data: 'x' },
  ]);
});

test('oversized responses are aborted as protocol errors', async () => {
  const server = mockServer({ tool: () => ({ blob: 'x'.repeat(5000) }) });
  const { c } = client(server, { maxResponseBytes: 1000 });
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'protocol');
});

test('close aborts in-flight reads and late responses cannot resolve', async () => {
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const server = mockServer({
    tool: async () => {
      await gate;
      return { late: true };
    },
  });
  const { c } = client(server);
  await c.connect();
  const pending = c.callTool('list_collections', {});
  await new Promise((resolve) => setTimeout(resolve, 5));
  c.close();
  release();
  await assert.rejects(pending, (e) => e.category === 'aborted');
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'aborted');
});

test('close sends best-effort session DELETE only when a session exists', async () => {
  const server = mockServer({ sessions: true });
  const { c } = client(server);
  await c.connect();
  const deletes = [];
  const wrapped = createMcpClient({
    auth: AUTH,
    fetch: async (url, init) => {
      if (init.method === 'DELETE') {
        deletes.push(new Headers(init.headers).get('mcp-session-id'));
        throw new TypeError('ignored');
      }
      return server.fetch(url, init);
    },
    maxStartsPerSecond: 1000,
  });
  await wrapped.connect();
  wrapped.close();
  assert.equal(deletes.length, 1);
  const stateless = createMcpClient({ auth: AUTH, fetch: mockServer().fetch, maxStartsPerSecond: 1000 });
  await stateless.connect();
  stateless.close();
});

test('timeouts abort requests as unavailable', async () => {
  const server = mockServer({
    raw: (call) =>
      call.body.method === 'tools/call'
        ? new Promise((resolve, reject) => {
            call.init.signal.addEventListener('abort', () => reject(call.init.signal.reason));
          })
        : null,
  });
  const { c } = client(server, { timeoutMs: 20, retries: 0 });
  await assert.rejects(c.callTool('list_collections', {}), (e) => e.category === 'unavailable');
});

test('caller abort is reported as aborted, not retried', async () => {
  const server = mockServer({
    raw: (call) =>
      call.body.method === 'tools/call'
        ? new Promise((resolve, reject) => {
            call.init.signal.addEventListener('abort', () => reject(call.init.signal.reason));
          })
        : null,
  });
  const { c, delays } = client(server);
  const controller = new AbortController();
  const pending = c.callTool('list_collections', {}, { signal: controller.signal });
  setTimeout(() => controller.abort(), 5);
  await assert.rejects(pending, (e) => e.category === 'aborted');
  assert.deepEqual(delays, []);
});

test('at most two requests run concurrently and high priority jumps the queue', async () => {
  let running = 0;
  let peak = 0;
  const order = [];
  const server = mockServer({
    tool: async (name, args) => {
      running += 1;
      peak = Math.max(peak, running);
      order.push(args.tag);
      await new Promise((resolve) => setTimeout(resolve, 10));
      running -= 1;
      return {};
    },
  });
  const { c } = client(server);
  await c.connect();
  const jobs = ['a', 'b', 'c', 'd'].map((tag) => c.callTool('list_collections', { tag }));
  jobs.push(c.callTool('get_todo', { todo_id: 'x', tag: 'urgent' }, { priority: 'high' }));
  await Promise.all(jobs);
  assert.equal(peak, 2);
  assert.ok(order.indexOf('urgent') <= 2, order.join());
});

test('injected auth and endpoint are used without changing the protocol', async () => {
  const server = mockServer();
  const c = createMcpClient({
    endpoint: '/ui/api/other',
    auth: { headers: () => ({ 'X-Test-Session': 'cookie-backed' }), credentials: 'same-origin' },
    fetch: server.fetch,
    maxStartsPerSecond: 1000,
  });
  await c.connect();
  for (const call of server.calls) {
    assert.equal(call.url, '/ui/api/other');
    assert.equal(call.headers.get('authorization'), null);
    assert.equal(call.headers.get('x-test-session'), 'cookie-backed');
    assert.equal(call.init.credentials, 'same-origin');
  }
});
