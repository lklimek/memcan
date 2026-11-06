import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  decodeExportPage,
  filterTasks,
  formatAge,
  highlightSegments,
  isOpenStatus,
  normalizeTodo,
  overview,
  pageTasks,
  priorityLabel,
  projectFilter,
  reverseIndex,
  searchTokens,
  sqlString,
  statusLabel,
  vocabulary,
} from '../static/data.js';
import { defaultTaskQuery } from '../static/router.js';

function task(id, fields = {}) {
  return {
    id,
    title: `Task ${id}`,
    description: null,
    project: 'p',
    priority: 'medium',
    status: 'pending',
    owner: null,
    blockedBy: [],
    createdAt: '2026-01-01T00:00:00Z',
    completedAt: null,
    ...fields,
  };
}

const query = (fields) => ({ ...defaultTaskQuery(), ...fields });

test('normalizeTodo maps export rows and preserves unknown values', () => {
  const result = normalizeTodo({
    _collection: 'todos',
    id: 'abc',
    title: 'T',
    description: 'D',
    project: 'p',
    priority: 'urgent',
    status: 'archived',
    owner: '',
    blocked_by: ['x', 5, 'y'],
    created_at: 'not a date',
    completed_at: null,
    data: 'T\nD',
  });
  assert.equal(result.ok, true);
  assert.deepEqual(result.task, {
    id: 'abc',
    title: 'T',
    description: 'D',
    project: 'p',
    priority: 'urgent',
    status: 'archived',
    owner: null,
    blockedBy: ['x', 'y'],
    createdAt: 'not a date',
    completedAt: null,
  });
  assert.equal(normalizeTodo({ title: 'no id' }).ok, false);
  assert.equal(normalizeTodo({ id: '' }).ok, false);
  assert.equal(normalizeTodo(null).ok, false);
  assert.equal(normalizeTodo(['x']).ok, false);
  const sparse = normalizeTodo({ id: 'z' }).task;
  assert.equal(sparse.status, '');
  assert.equal(sparse.title, '');
  assert.deepEqual(sparse.blockedBy, []);
});

test('normalized tasks never use attacker-keyed prototypes', () => {
  const { task: t } = normalizeTodo(JSON.parse('{"id":"__proto__","title":"x","__proto__":{"polluted":1}}'));
  assert.equal(t.id, '__proto__');
  assert.equal({}.polluted, undefined);
});

test('labels for known and unknown enums', () => {
  assert.equal(statusLabel('in_progress'), 'In progress');
  assert.equal(statusLabel('archived'), 'Unknown: archived');
  assert.equal(statusLabel(''), 'Unknown: (empty)');
  assert.equal(priorityLabel('high'), 'High');
  assert.equal(priorityLabel('urgent'), 'Unknown: urgent');
  assert.ok(isOpenStatus('archived'));
  assert.ok(isOpenStatus('postponed'));
  assert.ok(!isOpenStatus('done'));
  assert.ok(!isOpenStatus('cancelled'));
});

test('decodeExportPage parses JSONL, counts malformed rows, rejects count mismatch', () => {
  const rows = [
    JSON.stringify({ _collection: 'todos', id: 'a', title: 'A' }),
    '{not json',
    JSON.stringify({ _collection: 'todos', title: 'no id' }),
  ];
  const page = decodeExportPage({ count: 3, offset: 0, data: rows.join('\n') });
  assert.equal(page.count, 3);
  assert.equal(page.tasks.length, 1);
  assert.equal(page.malformed, 2);
  assert.ok(page.bytes > 0);
  assert.deepEqual(decodeExportPage({ count: 0, offset: 0, data: '' }).tasks, []);
  assert.throws(() => decodeExportPage({ count: 2, offset: 0, data: rows[0] }), /count/);
  assert.throws(() => decodeExportPage({ count: 'x', data: '' }));
  assert.throws(() => decodeExportPage(null));
});

test('sqlString doubles apostrophes like core sanitize_eq and keeps control characters literal', () => {
  assert.equal(sqlString("o'brien"), "'o''brien'");
  assert.equal(sqlString("''"), "''''''");
  assert.equal(sqlString('a\nb\tc\u0001'), "'a\nb\tc\u0001'");
  assert.equal(projectFilter("x' OR '1'='1"), "project = 'x'' OR ''1''=''1'");
});

test('status modes: open includes unknown non-terminal, all includes everything, selected is literal', () => {
  const tasks = [
    task('1', { status: 'pending' }),
    task('2', { status: 'done' }),
    task('3', { status: 'archived' }),
    task('4', { status: 'cancelled' }),
    task('5', { status: 'postponed' }),
  ];
  const ids = (q) => filterTasks(tasks, query(q)).map((t) => t.id).sort();
  assert.deepEqual(ids({}), ['1', '3', '5']);
  assert.deepEqual(ids({ statusMode: 'all' }), ['1', '2', '3', '4', '5']);
  assert.deepEqual(ids({ statusMode: 'selected', statuses: ['done', 'archived'] }), ['2', '3']);
  assert.deepEqual(ids({ statusMode: 'selected', statuses: [] }), []);
});

test('priority, owner, unassigned and project filters intersect', () => {
  const tasks = [
    task('1', { priority: 'high', owner: 'a' }),
    task('2', { priority: 'high' }),
    task('3', { priority: 'low', owner: 'a' }),
    task('4', { priority: 'high', owner: 'a', project: 'other' }),
  ];
  const ids = (q) => filterTasks(tasks, query(q)).map((t) => t.id);
  assert.deepEqual(ids({ priorities: ['high'], owner: 'a', project: 'p' }), ['1']);
  assert.deepEqual(ids({ priorities: ['high'], unassigned: true }), ['2']);
  assert.deepEqual(ids({ owner: 'A' }), []);
});

test('search is literal, case-folded, all tokens across fields, no regex', () => {
  const tasks = [
    task('abc-1', { title: 'Fix task links', owner: 'agent-a' }),
    task('abc-2', { title: 'Other', description: 'resolve LINKS (x.*)' }),
    task('abc-3', { title: 'x.* literal' }),
  ];
  const ids = (q) => filterTasks(tasks, query({ q })).map((t) => t.id);
  assert.deepEqual(ids('links'), ['abc-1', 'abc-2']);
  assert.deepEqual(ids('LINKS agent'), ['abc-1']);
  assert.deepEqual(ids('x.*'), ['abc-2', 'abc-3']);
  assert.deepEqual(ids('abc-3'), ['abc-3']);
  assert.deepEqual(ids('   '), ['abc-1', 'abc-2', 'abc-3']);
  assert.deepEqual(searchTokens('  A  b '), ['a', 'b']);
});

test('priority sort: high first, created oldest then id tie-breaks, invalid dates last', () => {
  const tasks = [
    task('d', { priority: 'low' }),
    task('c', { priority: 'high', createdAt: '2026-02-01T00:00:00Z' }),
    task('b', { priority: 'high', createdAt: '2026-01-01T00:00:00Z' }),
    task('a', { priority: 'high', createdAt: '2026-01-01T00:00:00Z' }),
    task('e', { priority: 'urgent' }),
    task('f', { priority: 'high', createdAt: 'garbage' }),
  ];
  const ids = (q) =>
    pageTasks(filterTasks(tasks, query(q)), query(q)).groups.flatMap((g) => g.rows.map((r) => r.id));
  assert.deepEqual(ids({}), ['a', 'b', 'c', 'f', 'd', 'e']);
  assert.deepEqual(ids({ dir: 'desc' }), ['e', 'd', 'a', 'b', 'c', 'f']);
});

test('created sort newest first by default and invalid dates last in both directions', () => {
  const tasks = [
    task('old', { createdAt: '2025-01-01T00:00:00Z' }),
    task('bad', { createdAt: '' }),
    task('new', { createdAt: '2026-06-01T00:00:00Z' }),
  ];
  const ids = (dir) =>
    pageTasks(tasks, query({ sort: 'created', dir })).groups.flatMap((g) => g.rows.map((r) => r.id));
  assert.deepEqual(ids('desc'), ['new', 'old', 'bad']);
  assert.deepEqual(ids('asc'), ['old', 'new', 'bad']);
});

test('status sort uses workflow order with unknown last', () => {
  const tasks = ['cancelled', 'zzz', 'done', 'pending', 'blocked', 'in_progress', 'postponed'].map(
    (status) => task(status, { status }),
  );
  const ids = pageTasks(tasks, query({ sort: 'status' })).groups.flatMap((g) => g.rows.map((r) => r.id));
  assert.deepEqual(ids, ['pending', 'in_progress', 'blocked', 'postponed', 'done', 'cancelled', 'zzz']);
});

test('grouping orders groups then paginates flattened rows with continued headers', () => {
  const tasks = [];
  for (let i = 0; i < 30; i += 1) tasks.push(task(`b${String(i).padStart(2, '0')}`, { project: 'beta' }));
  for (let i = 0; i < 30; i += 1) tasks.push(task(`a${String(i).padStart(2, '0')}`, { project: 'Alpha' }));
  const first = pageTasks(tasks, query({ group: 'project', size: 25 }));
  assert.equal(first.total, 60);
  assert.equal(first.pageCount, 3);
  assert.equal(first.start, 1);
  assert.equal(first.end, 25);
  assert.deepEqual(first.groups.map((g) => [g.key, g.count, g.continued, g.rows.length]), [['Alpha', 30, false, 25]]);
  const second = pageTasks(tasks, query({ group: 'project', size: 25, page: 2 }));
  assert.deepEqual(second.groups.map((g) => [g.key, g.continued, g.rows.length]), [
    ['Alpha', true, 5],
    ['beta', false, 20],
  ]);
});

test('out-of-range page is clamped and flagged', () => {
  const result = pageTasks([task('a')], query({ page: 9 }));
  assert.equal(result.page, 1);
  assert.equal(result.clamped, true);
  const empty = pageTasks([], query({}));
  assert.equal(empty.pageCount, 1);
  assert.equal(empty.total, 0);
  assert.equal(empty.clamped, false);
});

test('reverse index handles self links and cycles without recursion', () => {
  const tasks = [
    task('a', { blockedBy: ['b', 'a'] }),
    task('b', { blockedBy: ['a'] }),
    task('c', { blockedBy: ['b', 'missing'] }),
  ];
  const index = reverseIndex(tasks);
  assert.deepEqual(index.get('b'), ['a', 'c']);
  assert.deepEqual(index.get('a'), ['a', 'b']);
  assert.deepEqual(index.get('missing'), ['c']);
  assert.ok(index instanceof Map);
});

test('overview counts open, high, blocked, unassigned and ranks projects', () => {
  const tasks = [
    task('1', { project: 'x', priority: 'high' }),
    task('2', { project: 'x', status: 'blocked', owner: 'a' }),
    task('3', { project: 'y', priority: 'high', status: 'done' }),
    task('4', { project: 'y', priority: 'high' }),
    task('5', { project: 'y', priority: 'high', status: 'blocked' }),
    task('6', { project: 'z', status: 'cancelled' }),
  ];
  const result = overview(tasks);
  assert.deepEqual(
    { open: result.open, high: result.highOpen, blocked: result.blocked, unassigned: result.unassignedOpen },
    { open: 4, high: 3, blocked: 2, unassigned: 3 },
  );
  assert.deepEqual(result.projects.map((p) => [p.project, p.open, p.high, p.blocked]), [
    ['y', 2, 2, 1],
    ['x', 2, 1, 1],
  ]);
});

test('vocabulary lists observed projects and owners', () => {
  const v = vocabulary([task('1', { project: 'b', owner: 'o' }), task('2', { project: 'a' })]);
  assert.deepEqual(v.projects, ['a', 'b']);
  assert.deepEqual(v.owners, ['o']);
});

test('formatAge renders compact relative ages', () => {
  const now = Date.parse('2026-11-06T12:00:00Z');
  assert.equal(formatAge('2026-11-06T11:59:30Z', now), 'just now');
  assert.equal(formatAge('2026-11-06T11:00:00Z', now), '1h');
  assert.equal(formatAge('2026-11-03T12:00:00Z', now), '3d');
  assert.equal(formatAge('2026-10-20T12:00:00Z', now), '2w');
  assert.equal(formatAge('2025-10-01T12:00:00Z', now), '1y');
  assert.equal(formatAge('nope', now), null);
});

test('highlightSegments merges overlapping case-folded matches', () => {
  assert.deepEqual(highlightSegments('Fix Task links', ['task', 'ask l']), [
    { text: 'Fix ', mark: false },
    { text: 'Task l', mark: true },
    { text: 'inks', mark: false },
  ]);
  assert.deepEqual(highlightSegments('abc', []), [{ text: 'abc', mark: false }]);
});
