import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createTaskStore } from '../static/store.js';
import { McpError } from '../static/mcp.js';

function row(i, fields = {}) {
  return {
    _collection: 'todos',
    id: `id-${String(i).padStart(5, '0')}`,
    title: `Task ${i}`,
    project: 'p',
    priority: 'medium',
    status: 'pending',
    blocked_by: [],
    created_at: '2026-01-01T00:00:00Z',
    ...fields,
  };
}

function exportServer(rows, opts = {}) {
  const calls = [];
  const callTool = async (name, args) => {
    calls.push({ name, args });
    if (opts.before) await opts.before(name, args, calls);
    if (name === 'get_todo') {
      const found = rows.find((r) => r.id === args.todo_id || (args.todo_id.length >= 8 && r.id.startsWith(args.todo_id)));
      return found ? { ...found } : { error: 'todo not found', todo_id: args.todo_id };
    }
    assert.equal(name, 'export_collection');
    assert.equal(args.collection, 'todos');
    let source = typeof rows === 'function' ? rows() : rows;
    if (args.filter) {
      const project = /^project = '(.*)'$/s.exec(args.filter)[1].replaceAll("''", "'");
      source = source.filter((r) => typeof r === 'string' || r.project === project);
    }
    const page = source.slice(args.offset, args.offset + args.limit);
    const lines = page.map((r) => (typeof r === 'string' ? r : JSON.stringify(r)));
    return { count: lines.length, offset: args.offset, data: lines.join('\n') };
  };
  return { callTool, calls };
}

const many = (n, f) => Array.from({ length: n }, (_, i) => row(i, f?.(i)));

test('empty collection completes after one page', async () => {
  const server = exportServer([]);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  assert.equal(store.coverage().status, 'complete');
  assert.equal(store.tasks().length, 0);
  assert.equal(server.calls.length, 1);
  assert.deepEqual(server.calls[0].args, { collection: 'todos', limit: 500, offset: 0 });
});

test('500 records need a second empty page; 501 need two pages', async () => {
  for (const [n, pages] of [
    [500, 2],
    [501, 2],
    [499, 1],
  ]) {
    const server = exportServer(many(n));
    const store = createTaskStore({ callTool: server.callTool });
    await store.startScan(null);
    assert.equal(store.coverage().status, 'complete', String(n));
    assert.equal(store.tasks().length, n);
    assert.equal(server.calls.length, pages, String(n));
    assert.deepEqual(
      server.calls.map((c) => c.args.offset),
      Array.from({ length: pages }, (_, i) => i * 500),
    );
  }
});

test('10,001 records pause at the auto limit, then Continue loads the rest', async () => {
  const server = exportServer(many(10001));
  const updates = [];
  const store = createTaskStore({ callTool: server.callTool, onChange: () => updates.push(store.tasks().length) });
  await store.startScan(null);
  const cov = store.coverage();
  assert.equal(cov.status, 'paused');
  assert.equal(cov.records, 10000);
  assert.equal(cov.partial, true);
  assert.equal(store.tasks().length, 10000);
  assert.ok(updates.includes(500), 'first page usable before the scan finishes');
  await store.continueScan();
  assert.equal(store.coverage().status, 'complete');
  assert.equal(store.tasks().length, 10001);
});

test('byte budget pauses auto scan and cache budget stops Continue', async () => {
  const big = 'x'.repeat(1000);
  const server = exportServer(many(3000, () => ({ description: big })));
  const store = createTaskStore({
    callTool: server.callTool,
    limits: { autoBytes: 500_000, cacheBytes: 1_700_000 },
  });
  await store.startScan(null);
  assert.equal(store.coverage().status, 'paused');
  assert.equal(store.coverage().records, 500);
  await store.continueScan();
  assert.equal(store.coverage().status, 'paused');
  await store.continueScan();
  assert.equal(store.coverage().status, 'budget');
  const before = server.calls.length;
  await store.continueScan();
  assert.equal(server.calls.length, before);
});

test('duplicates do not inflate counts; malformed rows are reported', async () => {
  const rows = [...many(500), row(3), '{broken', JSON.stringify({ _collection: 'todos', title: 'no id' })];
  const server = exportServer(rows);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  const cov = store.coverage();
  assert.equal(cov.records, 503);
  assert.equal(cov.unique, 500);
  assert.equal(cov.duplicates, 1);
  assert.equal(cov.malformed, 2);
  assert.equal(store.tasks().length, 500);
});

test('failed page keeps loaded data, reports incomplete coverage and resumes at the same offset', async () => {
  let fail = true;
  const server = exportServer(many(700), {
    before: (name, args) => {
      if (args.offset === 500 && fail) {
        fail = false;
        throw new McpError('unavailable', 'down');
      }
    },
  });
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  const cov = store.coverage();
  assert.equal(cov.status, 'error');
  assert.equal(cov.failedPages, 1);
  assert.equal(cov.error.category, 'unavailable');
  assert.equal(store.tasks().length, 500);
  await store.continueScan();
  assert.equal(store.coverage().status, 'complete');
  assert.equal(store.tasks().length, 700);
  assert.equal(server.calls.at(-1).args.offset, 500);
});

test('scoped scan uses an escaped equality filter and switching scope evicts', async () => {
  const project = "o'brien & co";
  const server = exportServer(many(3, () => ({ project })));
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(project);
  assert.equal(server.calls[0].args.filter, "project = 'o''brien & co'");
  assert.equal(store.coverage().scope, project);
  assert.equal(store.tasks().length, 3);
  const scanPromise = store.startScan(null);
  assert.equal(store.tasks().length, 0);
  await scanPromise;
  assert.equal(store.coverage().scope, null);
});

test('concurrent start for the same scope coalesces', async () => {
  const server = exportServer(many(10));
  const store = createTaskStore({ callTool: server.callTool });
  await Promise.all([store.startScan(null), store.startScan(null)]);
  assert.equal(server.calls.length, 1);
});

test('refresh stages new data until applied; absent rows disappear only on apply', async () => {
  let rows = many(3);
  const server = exportServer(() => rows);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  rows = [row(0, { title: 'Renamed' }), row(1), row(9)];
  await store.refresh();
  assert.equal(store.stagedReady(), true);
  assert.equal(store.get('id-00000').title, 'Task 0');
  assert.ok(store.get('id-00002'));
  store.applyStaged();
  assert.equal(store.stagedReady(), false);
  assert.equal(store.get('id-00000').title, 'Renamed');
  assert.equal(store.tasks().length, 3);
  assert.equal(store.tasks().some((t) => t.id === 'id-00002'), false);
});

test('failed refresh keeps the previous snapshot and reports the failure', async () => {
  let broken = false;
  const server = exportServer(many(3), {
    before: () => {
      if (broken) throw new McpError('unavailable', 'down');
    },
  });
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  broken = true;
  await store.refresh();
  assert.equal(store.tasks().length, 3);
  assert.equal(store.stagedReady(), false);
  assert.equal(store.coverage().refreshError.category, 'unavailable');
  assert.equal(store.coverage().status, 'complete');
});

test('direct fetch wins over an older scan response for the same task', async () => {
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const rows = many(2);
  const server = exportServer(rows, {
    before: async (name) => {
      if (name === 'export_collection') await gate;
    },
  });
  const store = createTaskStore({ callTool: server.callTool });
  const scan = store.startScan(null);
  rows[0] = row(0, { title: 'Fresh' });
  const direct = await store.fetchTask('id-00000');
  assert.equal(direct.status, 'found');
  rows[0] = row(0, { title: 'Stale' });
  release();
  await scan;
  assert.equal(store.get('id-00000').title, 'Fresh');
});

test('fetchTask resolves prefixes, reports not-found markers and keeps cross-project tasks out of the list', async () => {
  const server = exportServer([row(1, { project: 'p' }), row(2, { id: 'abcdef12-3456', project: 'other' })]);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan('p');
  const found = await store.fetchTask('id-0000');
  assert.equal(found.status, 'missing');
  const prefixed = await store.fetchTask('abcdef12');
  assert.equal(prefixed.status, 'found');
  assert.equal(prefixed.task.id, 'abcdef12-3456');
  assert.equal(store.get('abcdef12-3456').project, 'other');
  assert.deepEqual(store.tasks().map((t) => t.id), ['id-00001']);
  const missing = await store.fetchTask('nope-nope');
  assert.equal(missing.status, 'missing');
});

test('fetchTask rejects unreadable task payloads as protocol errors', async () => {
  const store = createTaskStore({ callTool: async () => ({ title: 'no id' }) });
  await assert.rejects(store.fetchTask('x'), (e) => e.category === 'protocol');
});

test('a missing task is removed from the snapshot', async () => {
  const rows = many(2);
  const server = exportServer(rows);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  rows.splice(0, 1);
  await store.fetchTask('id-00000');
  assert.equal(store.tasks().length, 1);
});

test('reverse index follows the snapshot version', async () => {
  const server = exportServer([row(1, { blocked_by: ['id-00002'] }), row(2)]);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  assert.deepEqual(store.blockedBy('id-00002'), ['id-00001']);
  assert.deepEqual(store.blockedBy('id-00001'), []);
});

test('clear stops scans and forgets everything; late pages are ignored', async () => {
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const server = exportServer(many(5), { before: () => gate });
  const store = createTaskStore({ callTool: server.callTool });
  const scan = store.startScan(null);
  store.clear();
  release();
  await scan;
  assert.equal(store.tasks().length, 0);
  assert.equal(store.coverage().status, 'idle');
});

test('stopScan pauses a running scan', async () => {
  const server = exportServer(many(2000), {
    before: async () => new Promise((resolve) => setTimeout(resolve, 1)),
  });
  const store = createTaskStore({ callTool: server.callTool });
  const scan = store.startScan(null);
  await new Promise((resolve) => setTimeout(resolve, 3));
  store.stopScan();
  await scan;
  assert.equal(store.coverage().status, 'stopped');
  assert.ok(store.coverage().records < 2000);
});

test('refresh with unchanged data stages nothing and only bumps the fetched time', async () => {
  let clock = 1000;
  const server = exportServer(many(3));
  const store = createTaskStore({ callTool: server.callTool, now: () => clock });
  await store.startScan(null);
  assert.equal(store.coverage().fetchedAt, 1000);
  clock = 5000;
  await store.refresh();
  assert.equal(store.stagedReady(), false);
  assert.equal(store.coverage().fetchedAt, 5000);
  assert.equal(store.coverage().status, 'complete');
});

test('refresh stages when only a field or a blocker changes', async () => {
  const rows = many(2);
  const server = exportServer(() => rows);
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan(null);
  rows[1] = row(1, { blocked_by: ['id-00000'] });
  await store.refresh();
  assert.equal(store.stagedReady(), true);
});

test('a capped refresh after the collection outgrew the auto limit is not reported complete', async () => {
  let rows = many(3);
  const server = exportServer(() => rows);
  const store = createTaskStore({ callTool: server.callTool, limits: { pageSize: 2, autoRecords: 4 } });
  await store.startScan(null);
  assert.equal(store.coverage().status, 'complete');
  rows = many(9);
  await store.refresh();
  if (store.stagedReady()) store.applyStaged();
  assert.equal(store.coverage().status, 'paused');
  assert.equal(store.coverage().partial, true);
  await store.continueScan();
  await store.continueScan();
  await store.continueScan();
  assert.equal(store.coverage().status, 'complete');
  assert.equal(store.tasks().length, 9);
});

test('a capped unchanged refresh does not announce updates every cycle', async () => {
  const server = exportServer(many(9));
  const store = createTaskStore({ callTool: server.callTool, limits: { pageSize: 2, autoRecords: 4 } });
  await store.startScan(null);
  assert.equal(store.coverage().status, 'paused');
  await store.refresh();
  assert.equal(store.stagedReady(), false);
  assert.equal(store.coverage().status, 'paused');
});

test('direct lookups survive a scope switch triggered by a new scan', async () => {
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const server = exportServer([row(1, { project: 'p' }), row(2, { project: 'other' })], {
    before: async (name) => {
      if (name === 'get_todo') await gate;
    },
  });
  const store = createTaskStore({ callTool: server.callTool });
  await store.startScan('p');
  const lookup = store.fetchTask('id-00002');
  const scan = store.startScan(null);
  release();
  const result = await lookup;
  assert.equal(result.status, 'found');
  await scan;
  assert.equal(store.get('id-00002').project, 'other');
});

test('clear still invalidates in-flight direct lookups', async () => {
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  const server = exportServer([row(1)], { before: async () => gate });
  const store = createTaskStore({ callTool: server.callTool });
  const lookup = store.fetchTask('id-00001');
  store.clear();
  release();
  await assert.rejects(lookup, (e) => e.category === 'aborted');
  assert.equal(store.get('id-00001'), null);
});
