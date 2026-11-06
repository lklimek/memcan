import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  OPEN_STATUSES,
  defaultTaskQuery,
  parseRoute,
  parseTaskQuery,
  serializeTaskQuery,
  taskPath,
  tasksUrl,
  safeReturnTo,
  builtInView,
} from '../static/router.js';

const q = (s) => new URLSearchParams(s);

test('parseRoute recognizes P1 routes and decodes task ids', () => {
  assert.deepEqual(parseRoute('/ui'), { name: 'root' });
  assert.deepEqual(parseRoute('/ui/'), { name: 'root' });
  assert.deepEqual(parseRoute('/ui/tasks'), { name: 'tasks' });
  assert.deepEqual(parseRoute('/ui/tasks/'), { name: 'tasks' });
  assert.deepEqual(parseRoute('/ui/dashboard'), { name: 'dashboard' });
  assert.deepEqual(parseRoute('/ui/login'), { name: 'login' });
  assert.deepEqual(parseRoute('/ui/connect'), { name: 'notfound' });
  assert.deepEqual(parseRoute('/ui/tasks/abc%2Fdef%20x'), { name: 'task', id: 'abc/def x' });
  assert.deepEqual(parseRoute('/ui/tasks/a/b'), { name: 'notfound' });
  assert.deepEqual(parseRoute('/ui/tasks/%E0%A4%A'), { name: 'notfound' });
  assert.deepEqual(parseRoute('/ui/memories'), { name: 'notfound' });
});

test('taskPath encodes reserved characters and round-trips', () => {
  const id = "a/b?c#d e'&%";
  const path = taskPath(id);
  assert.equal(path, '/ui/tasks/a%2Fb%3Fc%23d%20e\'%26%25');
  assert.deepEqual(parseRoute(path), { name: 'task', id });
});

test('default query is Open with priority sort', () => {
  const { state, notices } = parseTaskQuery(q(''));
  assert.deepEqual(state, defaultTaskQuery());
  assert.equal(state.statusMode, 'open');
  assert.equal(state.sort, 'priority');
  assert.equal(state.dir, 'asc');
  assert.equal(state.size, 50);
  assert.equal(state.page, 1);
  assert.deepEqual(notices, []);
  assert.deepEqual(OPEN_STATUSES, ['pending', 'in_progress', 'blocked', 'postponed']);
});

test('legacy status values and marker imply selected mode', () => {
  let { state } = parseTaskQuery(q('status=done&status=pending&status=done'));
  assert.equal(state.statusMode, 'selected');
  assert.deepEqual(state.statuses, ['done', 'pending']);
  ({ state } = parseTaskQuery(q('status_filter_present=1')));
  assert.equal(state.statusMode, 'selected');
  assert.deepEqual(state.statuses, []);
});

test('legacy created sort without dir is newest first', () => {
  assert.equal(parseTaskQuery(q('sort=created')).state.dir, 'desc');
  assert.equal(parseTaskQuery(q('sort=created&dir=asc')).state.dir, 'asc');
  assert.equal(parseTaskQuery(q('sort=project')).state.dir, 'asc');
});

test('invalid or duplicated scalars reset with a notice; unknown keys ignored', () => {
  const { state, notices } = parseTaskQuery(
    q('project=a&project=b&sort=bogus&size=7&page=-2&group=x&dir=up&foo=bar&status_mode=weird'),
  );
  assert.equal(state.project, null);
  assert.equal(state.sort, 'priority');
  assert.equal(state.size, 50);
  assert.equal(state.page, 1);
  assert.equal(state.group, 'none');
  assert.equal(state.dir, 'asc');
  assert.equal(state.statusMode, 'open');
  assert.ok(notices.length >= 6, notices.join());
});

test('owner and unassigned are exclusive', () => {
  const { state, notices } = parseTaskQuery(q('owner=agent&unassigned=1'));
  assert.equal(state.owner, null);
  assert.equal(state.unassigned, false);
  assert.equal(notices.length, 1);
});

test('serialize round-trips every field including quotes and URL punctuation', () => {
  const state = {
    ...defaultTaskQuery(),
    project: "o'brien & co?#/x",
    statusMode: 'selected',
    statuses: ['pending', 'weird-imported'],
    priorities: ['high', 'low'],
    owner: 'agent "a"',
    q: 'fix  links',
    group: 'project',
    sort: 'created',
    dir: 'asc',
    page: 3,
    size: 25,
  };
  const params = serializeTaskQuery(state);
  assert.deepEqual(parseTaskQuery(params).state, state);
  const url = tasksUrl(state);
  assert.ok(url.startsWith('/ui/tasks?'));
  assert.deepEqual(parseTaskQuery(new URL(url, 'http://x').searchParams).state, state);
});

test('selected mode with zero statuses survives round trip', () => {
  const state = { ...defaultTaskQuery(), statusMode: 'selected', statuses: [] };
  const params = serializeTaskQuery(state);
  assert.equal(params.get('status_filter_present'), '1');
  assert.deepEqual(parseTaskQuery(params).state, state);
});

test('serialize always writes explicit status mode', () => {
  assert.equal(serializeTaskQuery(defaultTaskQuery()).get('status_mode'), 'open');
});

test('safeReturnTo accepts only known same-origin /ui routes', () => {
  assert.equal(safeReturnTo('/ui/tasks/abc?project=x'), '/ui/tasks/abc?project=x');
  assert.equal(safeReturnTo('/ui/dashboard'), '/ui/dashboard');
  assert.equal(safeReturnTo('//evil.example/ui/tasks'), null);
  assert.equal(safeReturnTo('https://evil.example/ui/tasks'), null);
  assert.equal(safeReturnTo('/ui/login'), null);
  assert.equal(safeReturnTo('/ui/login?returnTo=%2Fui%2Ftasks'), null);
  assert.equal(safeReturnTo('/mcp'), null);
  assert.equal(safeReturnTo('/ui/../mcp'), null);
  assert.equal(safeReturnTo('/ui/tasks\\evil'), null);
  assert.equal(safeReturnTo(null), null);
});

test('built-in views preserve project and reset other filters', () => {
  const base = { ...defaultTaskQuery(), project: 'p', q: 'x', owner: 'o', page: 4 };
  const high = builtInView('high', base);
  assert.equal(high.project, 'p');
  assert.equal(high.q, '');
  assert.equal(high.owner, null);
  assert.equal(high.page, 1);
  assert.equal(high.statusMode, 'open');
  assert.deepEqual(high.priorities, ['high']);
  const blocked = builtInView('blocked', base);
  assert.equal(blocked.statusMode, 'selected');
  assert.deepEqual(blocked.statuses, ['blocked']);
  assert.equal(builtInView('unassigned', base).unassigned, true);
  assert.equal(builtInView('all', base).statusMode, 'all');
});
