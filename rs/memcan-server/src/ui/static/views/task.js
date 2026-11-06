// Task detail: direct get_todo lookup, progressive blocker resolution and observed reverse links.

import { append, clear, clockTime, copyText, el, exactDate, navLink, panelError, priorityBadge, statusBadge, utcDate } from '../components.js';
import { formatAge } from '../data.js';
import { builtInView, defaultTaskQuery, parseTaskQuery, serializeTaskQuery, taskPath, tasksUrl } from '../router.js';

const DETAIL_INTERVAL_MS = 60000;
const BLOCKER_CONCURRENCY = 2;

export function render(main, ctx) {
  const store = ctx.store;
  let id = ctx.route.id;
  const listParams = new URLSearchParams(location.search);
  const hasListContext = [...listParams.keys()].length > 0;
  const listQuery = parseTaskQuery(listParams).state;
  const blockerState = new Map();
  let task = store.get(id);
  let loadError = null;
  let missing = false;
  let loading = false;
  let fetchedAt = store.fetchedAt(id);
  let renderedFor = null;
  let headingFocused = false;

  const container = el('div', { class: 'task-detail' });
  append(main, container);
  ctx.setTitle('Task');

  function backHref() {
    if (hasListContext) return tasksUrl({ ...listQuery, page: listQuery.page });
    return tasksUrl(builtInView('open', { ...defaultTaskQuery(), project: task?.project || null }));
  }

  function linkTo(otherId) {
    return `${taskPath(otherId)}${hasListContext ? `?${serializeTaskQuery(listQuery)}` : ''}`;
  }

  async function load({ quiet = false } = {}) {
    if (loading) return;
    loading = true;
    if (!quiet) draw();
    try {
      const result = await store.fetchTask(id, { signal: ctx.signal });
      if (ctx.signal.aborted) return;
      loadError = null;
      if (result.status === 'missing') {
        missing = true;
        task = null;
      } else {
        missing = false;
        task = result.task;
        fetchedAt = result.fetchedAt;
        if (task.id !== id) {
          id = task.id;
          history.replaceState(history.state, '', `${taskPath(task.id)}${location.search}`);
        }
      }
    } catch (error) {
      if (ctx.signal.aborted || error?.category === 'aborted') return;
      loadError = error;
    } finally {
      loading = false;
    }
    if (!ctx.signal.aborted) {
      draw();
      if (task) resolveBlockers();
    }
  }

  // ------------------------------------------------------------ blockers
  let active = 0;
  const queue = [];

  function resolveBlockers(force = false) {
    for (const blockerId of task.blockedBy) {
      const known = blockerState.get(blockerId);
      if (!force && known && known.status !== 'error') continue;
      if (blockerId === task.id) {
        blockerState.set(blockerId, { status: 'self' });
        continue;
      }
      const cached = store.get(blockerId);
      if (cached && !force) {
        blockerState.set(blockerId, { status: 'found', task: cached });
        continue;
      }
      blockerState.set(blockerId, { status: 'loading' });
      queue.push(blockerId);
    }
    pumpBlockers();
    drawBlockers();
  }

  function pumpBlockers() {
    while (active < BLOCKER_CONCURRENCY && queue.length) {
      const blockerId = queue.shift();
      active += 1;
      store
        .fetchTask(blockerId, { signal: ctx.signal })
        .then((result) => {
          blockerState.set(blockerId, result.status === 'found' ? { status: 'found', task: result.task } : { status: 'missing' });
        })
        .catch((error) => {
          if (error?.category !== 'aborted') blockerState.set(blockerId, { status: 'error', error });
        })
        .finally(() => {
          active -= 1;
          if (ctx.signal.aborted) return;
          drawBlockers();
          pumpBlockers();
        });
    }
  }

  function retryBlocker(blockerId) {
    blockerState.delete(blockerId);
    resolveBlockers();
  }

  // ------------------------------------------------------------ drawing
  const blockersList = el('ul', { class: 'relation-list' });
  const blocksSection = el('div');
  const blockersHeading = el('h2', { id: 'blockers-heading' });

  function relationItem(other, note) {
    const crossProject = other.project !== task.project;
    const terminal = other.status === 'done' || other.status === 'cancelled';
    return el(
      'li',
      {},
      el('div', {}, navLink(linkTo(other.id), other.title || '(untitled)', { class: 'relation-title', dataset: { taskId: other.id, nav: '' } })),
      el(
        'div',
        { class: 'relation-meta' },
        statusBadge(other.status),
        crossProject ? el('span', { class: 'chip-static' }, `Other project: ${other.project || '(no project)'}`) : null,
        el('span', { class: 'muted small' }, crossProject ? (other.owner ?? 'Unassigned') : `${other.owner ?? 'Unassigned'} · ${other.project || '(no project)'}`),
        note ? el('span', { class: 'chip-static' }, note) : null,
      ),
      terminal ? el('p', { class: 'muted small' }, 'Resolved blocker — relationship retained.') : null,
    );
  }

  function drawBlockers() {
    if (!task) return;
    clear(blockersList);
    blockersHeading.textContent = `Blocked by (${task.blockedBy.length})`;
    if (!task.blockedBy.length) append(blockersList, el('li', { class: 'muted' }, 'No blockers recorded.'));
    for (const blockerId of task.blockedBy) {
      const entry = blockerState.get(blockerId) ?? { status: 'loading' };
      if (entry.status === 'found') {
        const cycle = entry.task.blockedBy.includes(task.id) ? 'Cycle: also blocked by this task' : null;
        append(blockersList, relationItem(entry.task, cycle));
      } else if (entry.status === 'self') {
        append(blockersList, el('li', {}, el('code', { class: 'id' }, blockerId), ' ', el('span', { class: 'chip-static' }, 'Self-reference')));
      } else if (entry.status === 'missing') {
        append(
          blockersList,
          el(
            'li',
            {},
            el('strong', {}, 'Task unavailable'),
            el('p', { class: 'muted small' }, 'Missing reference · ', el('code', { class: 'id' }, blockerId)),
            el('p', {}, 'This task may have been deleted. The relationship remains visible.'),
          ),
        );
      } else if (entry.status === 'error') {
        append(blockersList, 
          el(
            'li',
            {},
            el('p', {}, el('code', { class: 'id' }, blockerId), ' · Could not load this blocker.'),
            el('button', { type: 'button', class: 'button secondary', onclick: () => retryBlocker(blockerId), 'aria-label': `Retry loading blocker ${blockerId}` }, 'Retry blocker'),
          ),
        );
      } else {
        append(blockersList, el('li', { 'aria-busy': 'true' }, el('code', { class: 'id' }, blockerId), ' · Loading…'));
      }
    }
    const resolved = task.blockedBy.map((blockerId) => blockerState.get(blockerId));
    if (resolved.length && resolved.every((entry) => entry?.status === 'found' && ['done', 'cancelled'].includes(entry.task.status))) {
      append(
        blockersList,
        el('li', { class: 'muted small' }, 'All blockers are done or cancelled. The stored status is unchanged until an agent changes it.'),
      );
    }
    drawBlocks();
  }

  function drawBlocks() {
    clear(blocksSection);
    const coverage = store.coverage();
    const ids = store.blockedBy(task.id);
    append(blocksSection, el('h2', {}, 'Blocks (observed in loaded tasks)'));
    if (ids.length) {
      append(blocksSection, 
        el(
          'ul',
          { class: 'relation-list' },
          ...ids.map((otherId) => {
            const other = store.get(otherId);
            if (otherId === task.id) return el('li', {}, el('span', { class: 'chip-static' }, 'Self-reference'));
            return other ? relationItem(other, task.blockedBy.includes(otherId) ? 'Cycle' : null) : null;
          }),
        ),
      );
    } else {
      append(blocksSection, el('p', {}, coverage.status === 'complete' && coverage.scope === null ? 'None found in the last scan.' : 'None observed in loaded tasks.'));
    }
    const scopeNote =
      coverage.status === 'idle'
        ? 'No tasks scanned yet.'
        : `Based on ${coverage.unique} loaded task${coverage.unique === 1 ? '' : 's'}${coverage.scope ? ` in project ${coverage.scope}` : ''}${coverage.partial ? ' (partial)' : ''}.`;
    append(blocksSection, el('p', { class: 'muted small' }, scopeNote));
    if (!(coverage.scope === null && coverage.status === 'complete') && coverage.status !== 'loading') {
      append(blocksSection, 
        el('button', {
          type: 'button',
          class: 'button secondary',
          onclick: () => (coverage.scope === null && coverage.status !== 'idle' ? store.continueScan() : store.startScan(null)),
        }, 'Scan all tasks for relationships'),
      );
    }
    if (coverage.status === 'loading') append(blocksSection, el('p', { class: 'muted small' }, 'Scanning…'));
  }

  function draw() {
    const focusKey = container.contains(document.activeElement) ? document.activeElement.dataset.focusKey : null;
    drawContent();
    if (focusKey) container.querySelector(`[data-focus-key="${focusKey}"]`)?.focus();
  }

  function drawContent() {
    clear(container);
    main.removeAttribute('aria-busy');
    const back = navLink(backHref(), '← Back to tasks', { class: 'back-link', dataset: { nav: '', focusKey: 'back' } });
    if (!task && loading) {
      main.setAttribute('aria-busy', 'true');
      append(container, back, el('h1', { tabindex: '-1' }, 'Loading task'), el('div', { class: 'skeleton' }, el('div', { class: 'skeleton-row' }), el('div', { class: 'skeleton-row' })));
      ctx.setTitle('Loading task');
      return;
    }
    if (missing) {
      ctx.setTitle('Task not found');
      append(container, 
        back,
        el('h1', { tabindex: '-1' }, 'Task not found'),
        el('p', {}, 'No task has this ID. It may have been deleted. '),
        el('p', {}, el('code', { class: 'id' }, id)),
        el('button', { type: 'button', class: 'button secondary', onclick: () => load() }, 'Retry'),
      );
      focusOnce();
      return;
    }
    if (!task) {
      const ambiguous = loadError?.category === 'ambiguous';
      ctx.setTitle(ambiguous ? 'Use the full task ID' : 'Task unavailable');
      append(container, 
        back,
        el('h1', { tabindex: '-1' }, ambiguous ? 'Use the full task ID' : 'Task could not be loaded'),
        el('p', {}, el('code', { class: 'id' }, id)),
        loadError ? panelError(loadError, ambiguous ? null : () => load()) : null,
      );
      focusOnce();
      return;
    }

    ctx.setTitle(task.title || 'Untitled task');
    const age = formatAge(task.createdAt);
    const canonical = `${location.origin}${taskPath(task.id)}`;
    append(container, 
      el(
        'div',
        { class: 'detail-actions' },
        back,
        el('span', { class: 'inline' },
          el('button', { type: 'button', class: 'button secondary', dataset: { focusKey: 'copy-link' }, onclick: () => copyText(canonical, 'Link', ctx.toast) }, 'Copy link'),
          el('button', { type: 'button', class: 'button secondary', dataset: { focusKey: 'copy-id' }, onclick: () => copyText(task.id, 'ID', ctx.toast) }, 'Copy ID'),
        ),
      ),
      el('h1', { tabindex: '-1', class: 'task-title' }, task.title || '(untitled)'),
      el(
        'p',
        { class: 'badges' },
        priorityBadge(task.priority),
        ' ',
        statusBadge(task.status),
        ' ',
        el('span', { class: 'project-name' }, task.project || '(no project)'),
        ' ',
        el('span', { class: 'readonly-label' }, 'Read-only'),
      ),
      loadError ? panelError(loadError, () => load()) : null,
      el(
        'div',
        { class: 'detail-grid' },
        el(
          'div',
          { class: 'detail-main' },
          el('section', { 'aria-labelledby': 'description-heading', class: 'panel' }, el('h2', { id: 'description-heading' }, 'Description'), task.description ? el('div', { class: 'description' }, task.description) : el('p', { class: 'muted' }, 'No description')),
          el('section', { 'aria-labelledby': 'blockers-heading', class: 'panel' }, blockersHeading, blockersList),
          el('section', { class: 'panel' }, blocksSection),
        ),
        el(
          'section',
          { class: 'panel detail-meta', 'aria-labelledby': 'details-heading' },
          el('h2', { id: 'details-heading' }, 'Details'),
          el(
            'dl',
            {},
            el('dt', {}, 'Owner'),
            el('dd', {}, task.owner ?? 'Unassigned'),
            el('dt', {}, 'Project'),
            el('dd', {}, task.project || '(no project)'),
            el('dt', {}, 'Created'),
            el('dd', {}, age ? `${age === 'just now' ? 'just now' : `${age} ago`}` : el('span', {}, 'Unrecognized: ', el('code', {}, task.createdAt || '(empty)'))),
            el('dt', {}, 'Exact (local)'),
            el('dd', {}, exactDate(task.createdAt) ?? '—'),
            el('dt', {}, 'Exact (UTC)'),
            el('dd', {}, el('code', {}, utcDate(task.createdAt) ?? task.createdAt)),
            el('dt', {}, 'Completed/cancelled at'),
            el('dd', {}, task.completedAt ? (utcDate(task.completedAt) ?? task.completedAt) : 'Not recorded'),
            el('dt', {}, 'ID'),
            el('dd', {}, el('code', { class: 'id selectable' }, task.id)),
          ),
          el(
            'p',
            { class: 'muted small' },
            `Data fetched ${clockTime(fetchedAt)} `,
            el('button', { type: 'button', class: 'link-button', dataset: { focusKey: 'refresh' }, onclick: () => load({ quiet: true }) }, 'Refresh'),
          ),
        ),
      ),
    );
    drawBlockers();
    if (renderedFor !== task.id) {
      renderedFor = task.id;
      focusOnce();
    }
  }

  function focusOnce() {
    if (headingFocused) return;
    headingFocused = true;
    main.querySelector('h1')?.focus();
  }

  if (task) {
    draw();
    resolveBlockers();
  }
  load({ quiet: Boolean(task) });
  ctx.every(DETAIL_INTERVAL_MS, () => load({ quiet: true }));

  return {
    update: () => {
      if (!task) return;
      const latest = store.get(task.id);
      if (latest && latest !== task) {
        task = latest;
        fetchedAt = store.fetchedAt(task.id);
        draw();
        resolveBlockers();
      } else drawBlocks();
    },
    refresh: () => {
      load({ quiet: true });
      if (task) resolveBlockers(true);
    },
  };
}

