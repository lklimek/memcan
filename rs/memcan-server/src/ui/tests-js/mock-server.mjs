// Dev-only mock MemCan server for browser checks: serves the static UI under /ui/ with the
// production headers and fakes /health, /ui/auth/* and /ui/api/mcp with fixture data. Never
// embedded or served by memcan-server. Usage: node mock-server.mjs [port]  (sign-in: viewer/test-pass)

import { randomBytes } from 'node:crypto';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const STATIC_DIR = fileURLToPath(new URL('../static/', import.meta.url));
const PORT = Number(process.argv[2] ?? 8199);
const USERNAME = 'viewer';
const PASSWORD = 'test-pass';
const BASIC = `Basic ${Buffer.from(`${USERNAME}:${PASSWORD}`).toString('base64')}`;
const COOKIE = 'memcan_ui';
// Plain HTTP sign-in is allowed unless MOCK_SECURE=1 (then the UI must show "HTTPS required").
const INSECURE_ALLOWED = process.env.MOCK_SECURE !== '1';
const CSP =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";
const MIME = { '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml' };
const READ_TOOLS = ['get_todo', 'list_todos', 'export_collection', 'list_collections', 'get_queue_status'];

const calls = [];
const scenario = { sessions: process.env.MOCK_SESSIONS === '1', sse: process.env.MOCK_SSE === '1', slow: Number(process.env.MOCK_SLOW_MS ?? 0) };

function fixtures() {
  const projects = ['memcan', 'dash-evo-tool', "o'brien & co?#/x", 'platform'];
  const statuses = ['pending', 'in_progress', 'blocked', 'postponed', 'done', 'cancelled'];
  const priorities = ['high', 'medium', 'low'];
  const owners = [null, 'agent-a', 'agent-b', 'claudius'];
  const now = Date.parse('2026-11-06T12:00:00Z');
  const todos = [];
  for (let i = 0; i < 140; i += 1) {
    const id = `${(0x81cbe927 + i).toString(16)}-0000-4000-8000-${String(i).padStart(12, '0')}`;
    todos.push({
      _collection: 'todos',
      id,
      title: `Task ${i}: ${['Fix task links', 'Review extraction', 'Resolve encoded IDs', 'Update routing', 'Deploy proxy'][i % 5]}`,
      description: i % 3 ? `Details for task ${i}.\nSecond line with context.` : null,
      project: projects[i % projects.length],
      priority: priorities[i % 3],
      status: statuses[i % statuses.length],
      owner: owners[i % owners.length],
      blocked_by: [],
      created_at: new Date(now - i * 7 * 3600 * 1000).toISOString(),
      completed_at: i % 6 >= 4 ? new Date(now - i * 3600 * 1000).toISOString() : null,
      data: 'ignored',
    });
  }
  todos[0].title = 'Fix task links <script>alert(1)</script><img src=x onerror=alert(2)>';
  todos[0].description = 'Full stored text, wrapped;\nline breaks preserved.\n<b>not bold</b> javascript:alert(3)';
  todos[0].blocked_by = [todos[1].id, todos[3].id, 'deadbeef-gone-0000-0000-000000000000', todos[0].id];
  todos[1].blocked_by = [todos[0].id];
  todos[4].blocked_by = [todos[0].id];
  todos[3].project = 'other-project';
  todos[5].status = 'archived';
  todos[6].priority = 'urgent';
  todos[7].created_at = 'not-a-date';
  todos.push({ _collection: 'todos', id: 'abcdef12-ambiguous-1', title: 'Ambiguous A', project: 'memcan', priority: 'low', status: 'pending', blocked_by: [], created_at: '2026-01-01T00:00:00Z' });
  todos.push({ _collection: 'todos', id: 'abcdef12-ambiguous-2', title: 'Ambiguous B', project: 'memcan', priority: 'low', status: 'pending', blocked_by: [], created_at: '2026-01-01T00:00:00Z' });
  return todos;
}

const todos = fixtures();

function json(res, status, body, headers = {}) {
  res.writeHead(status, { 'content-type': 'application/json', ...headers });
  res.end(JSON.stringify(body));
}

function toolText(value) {
  return { content: [{ type: 'text', text: JSON.stringify(value) }] };
}

function stripCollection(row) {
  const { _collection, data, ...item } = row;
  return { description: null, owner: null, completed_at: null, ...item };
}

function callTool(name, args) {
  switch (name) {
    case 'get_todo': {
      const exact = todos.find((t) => t.id === args.todo_id);
      if (exact) return { result: toolText(stripCollection(exact)) };
      const matches = args.todo_id.length >= 8 ? todos.filter((t) => t.id.startsWith(args.todo_id)) : [];
      if (matches.length > 1) {
        return { error: { code: -32603, message: `get_todo failed: ambiguous todo id prefix '${args.todo_id}' matches 2 todos — provide the full UUID` } };
      }
      if (matches.length === 1) return { result: toolText(stripCollection(matches[0])) };
      return { result: toolText({ error: 'todo not found', todo_id: args.todo_id }) };
    }
    case 'export_collection': {
      let rows = todos;
      if (args.filter) {
        const match = /^project = '((?:[^']|'')*)'$/s.exec(args.filter);
        if (!match) return { error: { code: -32603, message: 'scroll failed' } };
        const project = match[1].replaceAll("''", "'");
        rows = rows.filter((t) => t.project === project);
      }
      const page = rows.slice(args.offset ?? 0, (args.offset ?? 0) + (args.limit ?? 1000));
      return { result: toolText({ count: page.length, offset: args.offset ?? 0, data: page.map((r) => JSON.stringify(r)).join('\n') }) };
    }
    case 'list_collections':
      return { result: toolText({ collections: [{ name: 'memcan_memories', count: 920 }, { name: 'memcan_todos', count: todos.length }, { name: 'memcan_code', count: 4410 }] }) };
    case 'get_queue_status':
      return {
        result: toolText({
          pending_tasks: 2,
          operations: [
            { operation: 'add_memory', user_id: 'project:memcan', status: 'storing', warnings: [], error: null, queued_at: '2026-11-06T11:59:00Z', completed_at: null },
            { operation: 'update_memory', user_id: 'global', status: 'completed_degraded', warnings: ['<i>dedup skipped</i>'], error: null, queued_at: '2026-11-06T11:50:00Z', completed_at: '2026-11-06T11:51:00Z' },
          ],
        }),
      };
    case 'list_todos':
      return { result: toolText({ todos: todos.filter((t) => t.project === args.project).slice(0, 200).map(stripCollection), has_more: false }) };
    default:
      return { error: { code: -32602, message: `unknown tool ${name}` } };
  }
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}

let sessionCounter = 0;
const sessions = new Set();
/** UI sign-in sessions: cookie value → { csrf, method }. */
const uiSessions = new Map();

function uiSession(req) {
  const pair = (req.headers.cookie ?? '').split(';').map((part) => part.trim()).find((part) => part.startsWith(`${COOKIE}=`));
  const id = pair?.slice(COOKIE.length + 1);
  return id && uiSessions.has(id) ? { id, ...uiSessions.get(id) } : null;
}

function startUiSession(res, method) {
  const id = randomBytes(32).toString('base64url');
  const csrf = randomBytes(32).toString('base64url');
  uiSessions.set(id, { csrf, method });
  res.setHeader('set-cookie', `${COOKIE}=${id}; Path=/ui; HttpOnly; SameSite=Strict`);
  return { csrf, method };
}

function signedIn(session) {
  return {
    authenticated: true,
    username: USERNAME,
    role: 'viewer',
    method: session.method,
    csrfToken: session.csrf,
    idleExpiresIn: 1800,
    absoluteExpiresIn: 28800,
    insecureHttpAllowed: INSECURE_ALLOWED,
  };
}

function sameOrigin(req) {
  return req.headers.origin === `http://${req.headers.host}` && INSECURE_ALLOWED;
}

async function handleAuth(req, res, path) {
  if (path === '/ui/auth/session' && req.method === 'GET') {
    const existing = uiSession(req);
    if (existing) return json(res, 200, signedIn(existing));
    if (req.headers.authorization === BASIC) return json(res, 200, signedIn(startUiSession(res, 'basic')));
    return json(res, 200, { authenticated: false, insecureHttpAllowed: INSECURE_ALLOWED });
  }
  if (req.method !== 'POST') return json(res, 405, { error: 'method_not_allowed' });
  if (!sameOrigin(req)) return json(res, 403, { error: 'cross_origin', message: 'Cross-origin requests are not allowed.' });
  if (path === '/ui/auth/login') {
    const body = JSON.parse(await readBody(req));
    calls.push({ method: 'login', hasAuthorization: Boolean(req.headers.authorization) });
    if (body.username !== USERNAME || body.password !== PASSWORD) {
      return json(res, 401, { error: 'invalid_credentials', message: 'Invalid username or password.' });
    }
    const previous = uiSession(req);
    if (previous) uiSessions.delete(previous.id);
    return json(res, 200, signedIn(startUiSession(res, 'session')));
  }
  const session = uiSession(req);
  if (path === '/ui/auth/logout') {
    if (session) {
      if (req.headers['x-csrf-token'] !== session.csrf) return json(res, 403, { error: 'csrf' });
      uiSessions.delete(session.id);
    }
    res.writeHead(204, { 'set-cookie': `${COOKIE}=; Path=/ui; HttpOnly; SameSite=Strict; Max-Age=0` });
    return res.end();
  }
  if (path === '/ui/auth/activity') {
    if (!session) return json(res, 401, { error: 'unauthenticated' });
    if (req.headers['x-csrf-token'] !== session.csrf) return json(res, 403, { error: 'csrf' });
    calls.push({ method: 'activity' });
    return json(res, 200, { idleExpiresIn: 1800, absoluteExpiresIn: 28000 });
  }
  return json(res, 404, { error: 'not_found' });
}

async function handleMcp(req, res) {
  if (process.env.MOCK_HOST_GUARD === '1') {
    return json(res, 403, { error: 'host_not_allowed', message: 'This host name is not allowed.' });
  }
  if (req.method !== 'POST') return json(res, 405, { error: 'method_not_allowed' });
  if (!sameOrigin(req)) return json(res, 403, { error: 'cross_origin' });
  const session = uiSession(req);
  if (!session) return json(res, 401, { error: 'unauthenticated', message: 'Sign in to continue.' });
  if (req.headers['x-csrf-token'] !== session.csrf) return json(res, 403, { error: 'csrf' });
  const message = JSON.parse(await readBody(req));
  calls.push({
    method: message.method,
    tool: message.params?.name ?? null,
    args: message.params?.arguments ?? null,
    hasAuthorization: Boolean(req.headers.authorization),
    sessionMethod: session.method,
    session: req.headers['mcp-session-id'] ?? null,
    protocol: req.headers['mcp-protocol-version'] ?? null,
  });
  if (message.method === 'tools/call' && !READ_TOOLS.includes(message.params?.name)) {
    return json(res, 200, { jsonrpc: '2.0', id: message.id, error: { code: -32602, message: 'Tool not available in the web UI' } });
  }
  if (scenario.slow && message.params?.name === 'export_collection') {
    await new Promise((resolve) => setTimeout(resolve, scenario.slow));
  }
  if (scenario.sessions && message.method !== 'initialize' && !sessions.has(req.headers['mcp-session-id'])) {
    res.writeHead(404);
    res.end('session not found');
    return;
  }
  if (message.method === 'initialize') {
    const headers = {};
    if (scenario.sessions) {
      const id = `session-${++sessionCounter}`;
      sessions.add(id);
      headers['mcp-session-id'] = id;
    }
    return json(res, 200, { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'memcan', version: '3.0.1' } } }, headers);
  }
  if (message.method === 'notifications/initialized') {
    res.writeHead(202);
    res.end();
    return;
  }
  let reply;
  if (message.method === 'tools/list') {
    reply = { jsonrpc: '2.0', id: message.id, result: { tools: READ_TOOLS.map((name) => ({ name, inputSchema: { type: 'object' } })) } };
  } else if (message.method === 'tools/call') {
    reply = { jsonrpc: '2.0', id: message.id, ...callTool(message.params.name, message.params.arguments ?? {}) };
  } else {
    reply = { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } };
  }
  if (scenario.sse) {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(`: keep-alive\n\nevent: message\ndata: ${JSON.stringify(reply)}\n\n`);
    return;
  }
  json(res, 200, reply);
}

function securityHeaders(res) {
  res.setHeader('content-security-policy', CSP);
  res.setHeader('x-content-type-options', 'nosniff');
  res.setHeader('cache-control', 'no-store');
  res.setHeader('referrer-policy', 'no-referrer');
}

const APP_ROUTE = /^\/ui(\/|\/tasks\/?|\/dashboard|\/login|\/tasks\/[^/]+)?$/;

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  try {
    if (url.pathname === '/ui/api/mcp') {
      securityHeaders(res);
      return await handleMcp(req, res);
    }
    if (url.pathname.startsWith('/ui/auth/')) {
      securityHeaders(res);
      return await handleAuth(req, res, url.pathname);
    }
    if (url.pathname === '/health') {
      return json(res, 200, {
        status: 'degraded',
        version: '3.0.1',
        dependencies: {
          lancedb: { status: 'healthy', last_checked_secs_ago: 10.2 },
          embedding: { status: 'healthy', last_checked_secs_ago: 61 },
          llm_primary: { status: 'down', last_checked_secs_ago: 125, error: 'LLM unavailable', backend: 'ollama', model: 'qwen3.5:9b' },
        },
      });
    }
    if (url.pathname === '/__calls') return json(res, 200, calls);
    if (url.pathname === '/__expire') {
      // Simulates session expiry or a server restart.
      uiSessions.clear();
      return json(res, 200, { expired: true });
    }
    if (url.pathname === '/__retitle') {
      todos[2].title = url.searchParams.get('title') ?? todos[2].title;
      return json(res, 200, { title: todos[2].title });
    }
    if (url.pathname === '/__slow') {
      scenario.slow = Number(url.searchParams.get('ms') ?? 0);
      return json(res, 200, { slow: scenario.slow });
    }
    if (!url.pathname.startsWith('/ui')) {
      res.writeHead(404);
      return res.end();
    }
    securityHeaders(res);
    if (url.pathname.startsWith('/ui/static/')) {
      const rel = normalize(url.pathname.slice('/ui/static/'.length));
      const mime = MIME[extname(rel)];
      if (rel.startsWith('..') || !mime || rel.endsWith('.html')) {
        res.writeHead(404);
        return res.end();
      }
      const body = await readFile(join(STATIC_DIR, rel)).catch(() => null);
      if (!body) {
        res.writeHead(404);
        return res.end();
      }
      res.writeHead(200, { 'content-type': mime });
      return res.end(body);
    }
    const shell = await readFile(join(STATIC_DIR, 'index.html'));
    res.writeHead(APP_ROUTE.test(url.pathname) ? 200 : 404, { 'content-type': 'text/html; charset=utf-8' });
    res.end(shell);
  } catch (error) {
    res.writeHead(500);
    res.end(String(error));
  }
});

server.listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`mock memcan on http://127.0.0.1:${PORT}/ui/tasks\n`);
});
