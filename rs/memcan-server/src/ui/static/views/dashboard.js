// Dashboard: scoped task overview plus server-wide health, queue and collection panels.

import { append, clear, clockTime, el, navLink, panelError } from '../components.js';
import { overview } from '../data.js';
import { builtInView, defaultTaskQuery, tasksUrl } from '../router.js';

const QUEUE_INTERVAL_MS = 15000;
const COUNTS_INTERVAL_MS = 60000;
const HEALTH_INTERVAL_MS = 30000;

const DEPENDENCIES = [
  ['lancedb', 'LanceDB', 'Task reads and all storage depend on it.'],
  ['embedding', 'Embedding', 'Semantic search and new writes are affected.'],
  ['llm_primary', 'LLM primary', 'Memory extraction and indexing can be delayed or fail.'],
  ['llm_fallback', 'LLM fallback', 'Processing has no fallback if the primary fails.'],
];
const DEPENDENCY_STATES = new Map([
  ['healthy', 'Healthy'],
  ['down', 'Unavailable'],
  ['half_open', 'Checking recovery'],
]);
const COLLECTIONS = [
  ['memcan_memories', 'Memories'],
  ['memcan_todos', 'Tasks'],
  ['memcan_code', 'Code'],
  ['memcan_standards', 'Standards'],
];

function observedAge(seconds) {
  if (typeof seconds !== 'number' || !Number.isFinite(seconds)) return 'Not observed yet';
  if (seconds < 60) return `${Math.round(seconds)} s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  return `${Math.round(seconds / 3600)} h ago`;
}

function panel(id, title, scopeLabel) {
  const body = el('div', { class: 'panel-body' });
  const meta = el('p', { class: 'panel-meta muted' });
  const section = el(
    'section',
    { class: 'panel', 'aria-labelledby': `${id}-title` },
    el('div', { class: 'panel-head' }, el('h2', { id: `${id}-title` }, title), scopeLabel ? el('span', { class: 'scope-tag' }, scopeLabel) : null),
    meta,
    body,
  );
  return { section, body, meta };
}

function metaLine(meta, resource, interval, onRetry) {
  clear(meta);
  const stale = resource.fetchedAt && Date.now() - resource.fetchedAt > interval * 2;
  append(meta, 
    resource.loading ? 'Loading… ' : '',
    resource.fetchedAt ? `Updated ${clockTime(resource.fetchedAt)}` : 'Not loaded yet',
    stale ? ' · Stale' : '',
    resource.error && resource.fetchedAt ? ' · Refresh failed' : '',
    ' ',
    onRetry ? el('button', { type: 'button', class: 'link-button', onclick: onRetry }, 'Refresh') : null,
  );
}

export function render(main, ctx) {
  const project = ctx.params.get('project') || null;
  ctx.setTitle(project ? `Dashboard · ${project}` : 'Dashboard');
  const connectHref = `/ui/connect?${new URLSearchParams({ returnTo: location.pathname + location.search })}`;
  const queue = { data: null, error: null, fetchedAt: null, loading: false };
  const counts = { data: null, error: null, fetchedAt: null, loading: false };

  const overviewPanel = panel('overview', 'Overview', project ? `Project: ${project}` : 'All projects');
  const healthPanel = panel('health', 'Server health', 'Server-wide');
  const queuePanel = panel('queue', 'Queue', 'Server-wide');
  const collectionsPanel = panel('collections', 'Collections', 'Server-wide');
  const projectsPanel = panel('projects', 'Projects needing attention', project ? `Project: ${project}` : 'All projects');

  append(main, 
    el('h1', { tabindex: '-1' }, 'Dashboard'),
    el(
      'p',
      { class: 'muted' },
      project ? `Task counts for ${project}. ` : 'Task counts for all projects. ',
      'Server health, queue and collections are server-wide.',
    ),
    el('div', { class: 'dashboard-grid' }, overviewPanel.section, healthPanel.section, queuePanel.section, collectionsPanel.section),
    projectsPanel.section,
  );

  function needConnection(target) {
    clear(target.body);
    append(target.body, el('p', {}, 'Connect to the API to load this panel. ', navLink(connectHref, 'Connect')));
  }

  // ---- overview + projects from the task scan
  function renderOverview() {
    if (!ctx.connected) {
      needConnection(overviewPanel);
      needConnection(projectsPanel);
      clear(overviewPanel.meta);
      clear(projectsPanel.meta);
      return;
    }
    const store = ctx.store;
    const coverage = store.coverage();
    const tasks = project ? store.tasks().filter((t) => t.project === project) : store.tasks();
    const stats = overview(tasks);
    const partial = coverage.partial;
    const fmt = (n) => (partial ? `≥${n}` : String(n));
    const base = { ...defaultTaskQuery(), project };
    const cards = [
      ['Open', stats.open, tasksUrl(builtInView('open', base))],
      ['High + open', stats.highOpen, tasksUrl(builtInView('high', base))],
      ['Blocked', stats.blocked, tasksUrl(builtInView('blocked', base))],
      ['Unassigned + open', stats.unassignedOpen, tasksUrl(builtInView('unassigned', base))],
    ];
    clear(overviewPanel.body);
    append(overviewPanel.body, 
      el(
        'ul',
        { class: 'stat-cards' },
        ...cards.map(([label, value, href]) =>
          el('li', {}, navLink(href, [el('span', { class: 'stat-value' }, fmt(value)), el('span', { class: 'stat-label' }, label)], { class: 'stat-card' })),
        ),
      ),
      coverageLine(coverage),
    );
    clear(overviewPanel.meta);
    append(overviewPanel.meta, coverage.finishedAt ? `Updated ${clockTime(coverage.finishedAt)}` : 'Loading tasks…');

    clear(projectsPanel.body);
    const rows = stats.projects;
    if (!rows.length) {
      append(projectsPanel.body, el('p', {}, coverage.status === 'loading' ? 'Loading tasks…' : 'No open tasks observed.'));
    } else {
      append(projectsPanel.body, 
        el(
          'div',
          { class: 'table-wrap' },
          el(
            'table',
            { class: 'data-table' },
            el('caption', { class: 'visually-hidden' }, 'Projects with open tasks, most high-priority work first'),
            el(
              'thead',
              {},
              el(
                'tr',
                {},
                el('th', { scope: 'col' }, 'Project'),
                el('th', { scope: 'col', class: 'num' }, 'Open'),
                el('th', { scope: 'col', class: 'num' }, 'High'),
                el('th', { scope: 'col', class: 'num' }, 'Blocked'),
                el('th', { scope: 'col' }, el('span', { class: 'visually-hidden' }, 'Actions')),
              ),
            ),
            el(
              'tbody',
              {},
              ...rows.map((row) =>
                el(
                  'tr',
                  {},
                  el('th', { scope: 'row', class: 'wrap' }, row.project || '(no project)'),
                  el('td', { class: 'num' }, fmt(row.open)),
                  el('td', { class: 'num' }, fmt(row.high)),
                  el('td', { class: 'num' }, fmt(row.blocked)),
                  el('td', {}, navLink(tasksUrl({ ...defaultTaskQuery(), project: row.project }), 'View tasks', { 'aria-label': `View tasks for ${row.project}` })),
                ),
              ),
            ),
          ),
        ),
      );
    }
  }

  function coverageLine(coverage) {
    const parts = [`Task coverage: ${coverage.unique} records loaded`];
    let action = null;
    switch (coverage.status) {
      case 'loading':
        parts.push('loading more');
        break;
      case 'complete':
        parts.push(`scan complete ${clockTime(coverage.finishedAt)}`);
        break;
      case 'paused':
      case 'stopped':
        parts.push('partial data');
        action = el('button', { type: 'button', class: 'link-button', onclick: () => ctx.store.continueScan() }, 'Continue loading');
        break;
      case 'budget':
        parts.push('memory budget reached — narrow to a project');
        break;
      case 'error':
        parts.push('loading failed');
        action = el('button', { type: 'button', class: 'link-button', onclick: () => ctx.store.continueScan() }, 'Retry');
        break;
      default:
        break;
    }
    if (coverage.malformed) parts.push(`${coverage.malformed} unreadable`);
    return el('p', { class: 'coverage' }, parts.join(' · '), ' ', action);
  }

  // ---- health
  function renderHealth() {
    const health = ctx.health;
    metaLine(healthPanel.meta, health, HEALTH_INTERVAL_MS, () => ctx.refreshHealth());
    clear(healthPanel.body);
    if (health.error && !health.data) {
      append(healthPanel.body, panelError(health.error, () => ctx.refreshHealth()));
      return;
    }
    if (!health.data) {
      append(healthPanel.body, el('p', { 'aria-busy': 'true' }, 'Loading server health…'));
      return;
    }
    const data = health.data;
    const deps = data.dependencies && typeof data.dependencies === 'object' ? data.dependencies : {};
    const overall = data.status === 'ok' ? 'Healthy' : data.status === 'degraded' ? 'Degraded' : `Unknown: ${data.status}`;
    append(healthPanel.body, 
      el(
        'p',
        { class: 'health-summary' },
        el('strong', { dataset: { tone: data.status === 'ok' ? 'good' : 'warn' }, class: 'indicator' }, `Server: ${overall}`),
        typeof data.version === 'string' ? ` · version ${data.version}` : '',
      ),
    );
    const impacts = [];
    const rows = DEPENDENCIES.map(([key, label, impact]) => {
      const dep = Object.hasOwn(deps, key) ? deps[key] : null;
      let stateText;
      if (!dep) stateText = key === 'llm_fallback' ? 'Not configured' : 'Not reported';
      else stateText = DEPENDENCY_STATES.get(dep.status) ?? `Unknown: ${String(dep.status)}`;
      if (dep && dep.status !== 'healthy') impacts.push(`${label}: ${impact}`);
      const detail = dep && (dep.backend || dep.model) ? [dep.backend, dep.model].filter((v) => typeof v === 'string').join(' / ') : '';
      return el(
        'tr',
        {},
        el('th', { scope: 'row' }, label, detail ? el('div', { class: 'muted small wrap' }, detail) : null),
        el('td', {}, el('span', { class: `dep-state dep-${dep ? String(dep.status).replace(/[^a-z_]/g, '') : 'none'}` }, stateText)),
        el('td', {}, dep ? observedAge(dep.last_checked_secs_ago) : '—'),
      );
    });
    append(healthPanel.body, 
      el(
        'div',
        { class: 'table-wrap' },
        el(
          'table',
          { class: 'data-table' },
          el('caption', { class: 'visually-hidden' }, 'Dependency circuit-breaker state'),
          el('thead', {}, el('tr', {}, el('th', { scope: 'col' }, 'Dependency'), el('th', { scope: 'col' }, 'State'), el('th', { scope: 'col' }, 'Last observed'))),
          el('tbody', {}, ...rows),
        ),
      ),
      impacts.length ? el('ul', { class: 'impacts' }, ...impacts.map((text) => el('li', {}, text))) : null,
      el('p', { class: 'muted small' }, 'Circuit-breaker state as last observed by the server; Refresh reads it and does not probe models. Task reads mainly need LanceDB.'),
    );
    if (health.error) append(healthPanel.body, panelError(health.error, () => ctx.refreshHealth(), { refresh: true }));
  }

  // ---- queue
  async function loadQueue() {
    if (!ctx.connected || queue.loading) return;
    queue.loading = true;
    renderQueue();
    try {
      const data = await ctx.client.callTool('get_queue_status', { limit: 10 }, { signal: ctx.signal });
      if (!data || typeof data !== 'object' || !Array.isArray(data.operations)) throw Object.assign(new Error(), { category: 'protocol' });
      queue.data = data;
      queue.error = null;
      queue.fetchedAt = Date.now();
    } catch (error) {
      if (ctx.signal.aborted) return;
      queue.error = error;
    } finally {
      queue.loading = false;
    }
    if (!ctx.signal.aborted) renderQueue();
  }

  function renderQueue() {
    if (!ctx.connected) {
      needConnection(queuePanel);
      clear(queuePanel.meta);
      return;
    }
    metaLine(queuePanel.meta, queue, QUEUE_INTERVAL_MS, loadQueue);
    clear(queuePanel.body);
    if (queue.error) append(queuePanel.body, panelError(queue.error, loadQueue, { refresh: Boolean(queue.data) }));
    const data = queue.data;
    if (!data) {
      if (!queue.error) append(queuePanel.body, el('p', { 'aria-busy': 'true' }, 'Loading queue…'));
      return;
    }
    const pending = Number.isInteger(data.pending_tasks) ? data.pending_tasks : null;
    append(queuePanel.body, 
      el('p', {}, el('strong', {}, pending === null ? 'Pending operations unknown' : `${pending} pending operation${pending === 1 ? '' : 's'}`), ' (queued or running)'),
      el('h3', {}, `Recent operations (${data.operations.length})`),
      el('p', { class: 'muted small' }, 'A retained sample of recent operations, not a complete or time-ordered history.'),
    );
    if (!data.operations.length) {
      append(queuePanel.body, el('p', {}, 'No retained operations.'));
      return;
    }
    append(queuePanel.body, 
      el(
        'ul',
        { class: 'queue-list' },
        ...data.operations.map((op) => {
          const str = (v) => (typeof v === 'string' ? v : v === null || v === undefined ? '' : JSON.stringify(v));
          const warnings = Array.isArray(op.warnings) ? op.warnings.map(str) : [];
          const problems = [...warnings, str(op.error)].filter(Boolean);
          return el(
            'li',
            {},
            el('span', { class: 'wrap' }, `${str(op.operation) || 'operation'} · ${str(op.status) || 'unknown status'}`),
            el('span', { class: 'muted small wrap' }, ` ${str(op.user_id)} · queued ${str(op.queued_at) || '—'}${op.completed_at ? ` · completed ${str(op.completed_at)}` : ''}`),
            problems.length
              ? el('details', {}, el('summary', {}, `${problems.length} warning${problems.length === 1 ? '' : 's'}/errors`), el('pre', { class: 'plain' }, problems.join('\n')))
              : null,
          );
        }),
      ),
    );
  }

  // ---- collections
  async function loadCounts() {
    if (!ctx.connected || counts.loading) return;
    counts.loading = true;
    renderCounts();
    try {
      const data = await ctx.client.callTool('list_collections', {}, { signal: ctx.signal });
      if (!data || !Array.isArray(data.collections)) throw Object.assign(new Error(), { category: 'protocol' });
      counts.data = data;
      counts.error = null;
      counts.fetchedAt = Date.now();
    } catch (error) {
      if (ctx.signal.aborted) return;
      counts.error = error;
    } finally {
      counts.loading = false;
    }
    if (!ctx.signal.aborted) renderCounts();
  }

  function renderCounts() {
    if (!ctx.connected) {
      needConnection(collectionsPanel);
      clear(collectionsPanel.meta);
      return;
    }
    metaLine(collectionsPanel.meta, counts, COUNTS_INTERVAL_MS, loadCounts);
    clear(collectionsPanel.body);
    if (counts.error) append(collectionsPanel.body, panelError(counts.error, loadCounts, { refresh: Boolean(counts.data) }));
    if (!counts.data) {
      if (!counts.error) append(collectionsPanel.body, el('p', { 'aria-busy': 'true' }, 'Loading collections…'));
      return;
    }
    const byName = new Map();
    for (const item of counts.data.collections) {
      if (item && typeof item.name === 'string' && Number.isInteger(item.count)) byName.set(item.name, item.count);
    }
    append(collectionsPanel.body, 
      el(
        'dl',
        { class: 'collection-list' },
        ...COLLECTIONS.flatMap(([name, label]) => [
          el('dt', {}, label),
          el('dd', {}, byName.has(name) ? String(byName.get(name)) : 'Unavailable or not created'),
        ]),
      ),
    );
  }

  function update() {
    renderOverview();
  }

  const unsubscribe = ctx.onHealth(renderHealth);
  renderHealth();
  renderOverview();
  renderQueue();
  renderCounts();
  if (ctx.connected) {
    ctx.store.startScan(project);
    loadQueue();
    loadCounts();
    ctx.every(QUEUE_INTERVAL_MS, loadQueue);
    ctx.every(COUNTS_INTERVAL_MS, loadCounts);
    ctx.every(60000, async () => {
      await ctx.store.refresh();
      ctx.store.applyStaged();
    });
  }
  ctx.focusHeading();

  return {
    update,
    refresh: () => {
      loadQueue();
      loadCounts();
      if (ctx.connected) ctx.store.refresh().then(() => ctx.store.applyStaged());
    },
    dispose: unsubscribe,
  };
}
