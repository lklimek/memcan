// Dev-only mock MemCan server for browser checks: serves the static UI under /ui/ with the
// production headers and fakes /health and /mcp with fixture data. Never embedded or served
// by memcan-server. Usage: node mock-server.mjs [port]  (API key: "test-key", page login: ui/ui)

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const STATIC_DIR = fileURLToPath(new URL('../static/', import.meta.url));
const PORT = Number(process.argv[2] ?? 8199);
const API_KEY = 'test-key';
const BASIC = `Basic ${Buffer.from('ui:ui').toString('base64')}`;
const CSP =
  "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'";
const MIME = { '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml' };
const READ_TOOLS = ['get_todo', 'list_todos', 'export_collection', 'list_collections', 'get_queue_status'];
const WRITE_TOOLS = ['add_todo', 'update_todo', 'complete_todo', 'delete_todo', 'add_memory'];

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

async function handleMcp(req, res) {
  if (process.env.MOCK_HOST_GUARD === '1') {
    // Mimics rmcp's DNS-rebinding guard for non-localhost Host headers.
    res.writeHead(403, { 'content-type': 'text/plain' });
    res.end('Forbidden: Host header is not allowed');
    return;
  }
  if (req.headers.authorization !== `Bearer ${API_KEY}`) {
    res.writeHead(401);
    res.end('Unauthorized');
    return;
  }
  if (req.method === 'DELETE') {
    calls.push({ method: 'DELETE', session: req.headers['mcp-session-id'] ?? null });
    sessions.delete(req.headers['mcp-session-id']);
    res.writeHead(204);
    res.end();
    return;
  }
  const message = JSON.parse(await readBody(req));
  calls.push({
    method: message.method,
    tool: message.params?.name ?? null,
    args: message.params?.arguments ?? null,
    hasBasic: /basic/i.test(req.headers.authorization ?? ''),
    session: req.headers['mcp-session-id'] ?? null,
    protocol: req.headers['mcp-protocol-version'] ?? null,
  });
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
    reply = { jsonrpc: '2.0', id: message.id, result: { tools: [...READ_TOOLS, ...WRITE_TOOLS].map((name) => ({ name, inputSchema: { type: 'object' } })) } };
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

const APP_ROUTE = /^\/ui(\/|\/tasks\/?|\/dashboard|\/connect|\/tasks\/[^/]+)?$/;

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  try {
    if (url.pathname === '/mcp') return await handleMcp(req, res);
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
    if (process.env.MOCK_BASIC === '1' && req.headers.authorization !== BASIC) {
      res.writeHead(401, { 'www-authenticate': 'Basic realm="MemCan Tasks"' });
      return res.end();
    }
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
