// Task list: local literal search, filters, grouping, sorting and client pagination over the task scan.

import { append, clear, clockTime, el, fieldId, exactDate, highlighted, navLink, panelError, priorityBadge, statusBadge } from '../components.js';
import {
  PRIORITY_ORDER,
  STATUS_ORDER,
  filterTasks,
  formatAge,
  isOpenStatus,
  pageTasks,
  priorityLabel,
  searchMatch,
  searchTokens,
  statusLabel,
  vocabulary,
} from '../data.js';
import { BUILT_IN_VIEWS } from '../prefs.js';
import {
  builtInView,
  AGE_FILTERS,
  defaultDir,
  SORTABLE_COLUMNS,
  sortByColumn,
  defaultTaskQuery,
  parseTaskQuery,
  serializeTaskQuery,
  taskPath,
  tasksUrl,
} from '../router.js';

const SEARCH_DEBOUNCE_MS = 150;
const SCAN_INTERVAL_MS = 60000;
const STALE_CHECK_MS = 15000;
const VIEW_LABELS = new Map([
  ['open', 'Open'],
  ['high', 'High priority'],
  ['blocked', 'Blocked'],
  ['unassigned', 'Unassigned'],
  ['all', 'All'],
]);
const SORT_LABELS = new Map([
  ['priority', ['High first', 'Low first']],
  ['created', ['Oldest first', 'Newest first']],
  ['project', ['A to Z', 'Z to A']],
  ['status', ['Workflow order', 'Reverse workflow order']],
]);

const AGE_LABELS = new Map([
  ['any', 'Any age'],
  ['lt1d', 'Under 1 day'],
  ['lt7d', 'Under 1 week'],
  ['lt30d', 'Under 30 days'],
  ['gt7d', 'Over 1 week'],
  ['gt30d', 'Over 30 days'],
  ['gt90d', 'Over 90 days'],
]);

const sameQuery = (a, b) => serializeTaskQuery(a).toString() === serializeTaskQuery(b).toString();

export function render(main, ctx) {
  const store = ctx.store;
  const parsed = parseTaskQuery(ctx.params);
  let query = parsed.state;
  const collapsed = new Set();
  let searchTimer = null;
  let composing = false;
  let lastCoverageStatus = null;
  ctx.setTitle(query.project ? `Tasks · ${query.project}` : 'Tasks');

  // ------------------------------------------------------------ controls
  const heading = el('h1', { tabindex: '-1' }, 'Tasks');
  const scopeLine = el('p', { class: 'muted' });
  const notices = parsed.notices.length
    ? el('div', { class: 'banner', role: 'status' }, el('p', {}, 'Some link settings were not recognized and were reset:'), el('ul', {}, ...parsed.notices.map((n) => el('li', {}, n))))
    : null;

  const viewButtons = BUILT_IN_VIEWS.map((name) =>
    el('button', { type: 'button', class: 'chip', 'aria-pressed': 'false', dataset: { view: name }, onclick: () => setQuery(builtInView(name, query)) }, VIEW_LABELS.get(name)),
  );
  const savedViews = el('details', { class: 'popover saved-views' });

  const searchInput = el('input', {
    id: 'task-search',
    type: 'search',
    autocomplete: 'off',
    spellcheck: false,
    value: query.q,
    'aria-describedby': 'search-help',
    'aria-keyshortcuts': '/',
  });
  searchInput.addEventListener('compositionstart', () => {
    composing = true;
  });
  searchInput.addEventListener('compositionend', () => {
    composing = false;
    scheduleSearch();
  });
  searchInput.addEventListener('input', () => {
    if (!composing) scheduleSearch();
  });

  const statusPopover = el('details', { class: 'popover' });
  const priorityPopover = el('details', { class: 'popover' });
  const ownerInput = el('input', { id: 'owner-filter', type: 'search', list: 'owner-options', autocomplete: 'off', placeholder: 'Any owner' });
  const ownerOptions = el('datalist', { id: 'owner-options' });
  const unassignedBox = el('input', { id: 'unassigned-filter', type: 'checkbox' });
  ownerInput.addEventListener('change', () => {
    const owner = ownerInput.value.trim() || null;
    setQuery({ ...query, owner, unassigned: owner ? false : query.unassigned, page: 1 });
  });
  const ageSelect = el(
    'select',
    { id: 'age-filter', onchange: () => setQuery({ ...query, age: ageSelect.value, page: 1 }) },
    ...AGE_FILTERS.map((value) => el('option', { value }, AGE_LABELS.get(value))),
  );
  unassignedBox.addEventListener('change', () => setQuery({ ...query, unassigned: unassignedBox.checked, owner: unassignedBox.checked ? null : query.owner, page: 1 }));

  const groupSelect = el(
    'select',
    { id: 'group-select', onchange: () => setQuery({ ...query, group: groupSelect.value, page: 1 }) },
    el('option', { value: 'none' }, 'None'),
    el('option', { value: 'project' }, 'Project'),
    el('option', { value: 'status' }, 'Status'),
  );
  const sortSelect = el(
    'select',
    { id: 'sort-select', onchange: () => setQuery({ ...query, sort: sortSelect.value, dir: defaultDir(sortSelect.value), page: 1 }) },
    el('option', { value: 'priority' }, 'Priority'),
    el('option', { value: 'created' }, 'Created'),
    el('option', { value: 'project' }, 'Project'),
    el('option', { value: 'status' }, 'Status'),
  );
  const dirButton = el('button', {
    type: 'button',
    class: 'button secondary',
    'aria-label': 'Sort direction',
    onclick: () => setQuery({ ...query, dir: query.dir === 'asc' ? 'desc' : 'asc', page: 1 }),
  });
  const densityButton = el('button', {
    type: 'button',
    class: 'button secondary',
    'aria-pressed': 'false',
    onclick: () => {
      const compact = ctx.prefs.get().density !== 'compact';
      ctx.prefs.update({ density: compact ? 'compact' : 'comfortable' });
      ctx.applyPrefs();
      syncControls();
    },
  }, 'Compact rows');
  const clearButton = el('button', { type: 'button', class: 'button secondary', onclick: () => setQuery({ ...defaultTaskQuery(), project: query.project, group: query.group, sort: query.sort, dir: query.dir, size: query.size }) }, 'Clear filters');

  const filtersRegion = el(
    'div',
    { id: 'task-filters', class: 'filters' },
    statusPopover,
    priorityPopover,
    el('div', { class: 'field' }, el('label', { for: 'owner-filter' }, 'Owner (exact)'), ownerInput, ownerOptions),
    el('div', { class: 'field checkbox-field' }, unassignedBox, el('label', { for: 'unassigned-filter' }, 'Unassigned only')),
    el('div', { class: 'field' }, el('label', { for: 'age-filter' }, 'Age'), ageSelect),
    el('div', { class: 'field' }, el('label', { for: 'group-select' }, 'Group'), groupSelect),
    el('div', { class: 'field' }, el('label', { for: 'sort-select' }, 'Sort'), el('div', { class: 'inline' }, sortSelect, dirButton)),
    el('div', { class: 'field field-actions' }, densityButton, clearButton),
  );
  const filtersToggle = el('button', {
    type: 'button',
    class: 'button secondary filters-toggle',
    'aria-expanded': 'false',
    'aria-controls': 'task-filters',
    onclick: () => {
      const open = filtersToggle.getAttribute('aria-expanded') !== 'true';
      filtersToggle.setAttribute('aria-expanded', String(open));
      filtersRegion.classList.toggle('open', open);
    },
  });

  // Persistent live region: content changes only when staged data appears or is applied.
  const updateBanner = el('div', { class: 'update-banner', role: 'status' });
  let bannerShown = false;
  const coverageRegion = el('div', { class: 'coverage-region' });
  const resultsHeading = el('h2', { id: 'results-heading', tabindex: '-1' }, 'Results');
  const summary = el('p', { class: 'results-summary' });
  const results = el('div', { class: 'results' });
  const pager = el('nav', { class: 'pager', 'aria-label': 'Pagination' });

  append(main, 
    heading,
    scopeLine,
    notices,
    el('div', { class: 'views-bar', role: 'group', 'aria-label': 'Views' }, ...viewButtons, savedViews),
    el(
      'div',
      { class: 'search-row' },
      el('label', { for: 'task-search' }, 'Find title, description, project, owner or ID'),
      searchInput,
      el('p', { id: 'search-help', class: 'muted small' }, 'Searches loaded tasks only; all words must match.'),
    ),
    filtersToggle,
    filtersRegion,
    updateBanner,
    coverageRegion,
    el('section', { 'aria-labelledby': 'results-heading', class: 'results-section' }, resultsHeading, summary, results, pager),
  );

  // ------------------------------------------------------------ state changes
  function writeUrl(push) {
    const url = tasksUrl(query);
    if (push) history.pushState(null, '', url);
    else history.replaceState(history.state, '', url);
    ctx.trackLocation();
  }

  function setQuery(next, { push = true, announce = true, focusResults = false } = {}) {
    if (sameQuery(next, query)) return;
    query = next;
    writeUrl(push);
    ctx.setTitle(query.project ? `Tasks · ${query.project}` : 'Tasks');
    if (store.coverage().scope !== query.project) store.startScan(query.project);
    syncControls();
    renderResults({ announce });
    if (focusResults) resultsHeading.focus();
  }

  function scheduleSearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      setQuery({ ...query, q: searchInput.value, page: 1 }, { push: false });
    }, SEARCH_DEBOUNCE_MS);
  }

  // ------------------------------------------------------------ controls sync
  function checkboxPopover(details, label, values, selected, labelFor, onToggle, extra) {
    const summaryText = `${label}: ${selected}`;
    const open = details.open;
    const focusedValue = details.contains(document.activeElement) ? document.activeElement.value : null;
    clear(details);
    append(details, 
      el('summary', { class: 'button secondary' }, summaryText),
      el(
        'fieldset',
        { class: 'popover-body' },
        el('legend', {}, label),
        ...values.map((value) => {
          const id = fieldId(label.toLowerCase(), value.value);
          const box = el('input', { type: 'checkbox', id, value: value.value, checked: value.checked, onchange: () => onToggle(value.value, box.checked) });
          return el('div', { class: 'checkbox-field' }, box, el('label', { for: id }, labelFor(value.value)));
        }),
        extra,
      ),
    );
    details.open = open;
    if (focusedValue !== null) [...details.querySelectorAll('input')].find((input) => input.value === focusedValue)?.focus();
  }

  function syncControls() {
    const vocab = vocabulary(store.tasks());
    const observedStatuses = new Set(store.tasks().map((t) => t.status));
    const statuses = [...STATUS_ORDER, ...[...observedStatuses, ...query.statuses].filter((s) => !STATUS_ORDER.includes(s)).sort()];
    const statusChecked = (s) => {
      if (query.statusMode === 'all') return true;
      if (query.statusMode === 'open') return isOpenStatus(s);
      return query.statuses.includes(s);
    };
    const statusSummary = query.statusMode === 'all' ? 'All' : query.statusMode === 'open' ? 'Open' : `${query.statuses.length} selected`;
    checkboxPopover(
      statusPopover,
      'Status',
      [...new Set(statuses)].map((value) => ({ value, checked: statusChecked(value) })),
      statusSummary,
      statusLabel,
      (value, checked) => {
        const current = new Set(statuses.filter(statusChecked));
        if (checked) current.add(value);
        else current.delete(value);
        setQuery({ ...query, statusMode: 'selected', statuses: statuses.filter((s) => current.has(s)), page: 1 });
      },
      el(
        'div',
        { class: 'inline' },
        el('button', { type: 'button', class: 'link-button', onclick: () => setQuery({ ...query, statusMode: 'open', statuses: [], page: 1 }) }, 'Open'),
        el('button', { type: 'button', class: 'link-button', onclick: () => setQuery({ ...query, statusMode: 'all', statuses: [], page: 1 }) }, 'All'),
      ),
    );
    const observedPriorities = new Set(store.tasks().map((t) => t.priority));
    const priorities = [...PRIORITY_ORDER, ...[...observedPriorities, ...query.priorities].filter((p) => !PRIORITY_ORDER.includes(p)).sort()];
    checkboxPopover(
      priorityPopover,
      'Priority',
      [...new Set(priorities)].map((value) => ({ value, checked: query.priorities.includes(value) })),
      query.priorities.length ? `${query.priorities.length} selected` : 'Any',
      priorityLabel,
      (value, checked) => {
        const next = new Set(query.priorities);
        if (checked) next.add(value);
        else next.delete(value);
        setQuery({ ...query, priorities: priorities.filter((p) => next.has(p)), page: 1 });
      },
    );
    if (document.activeElement !== ownerInput) ownerInput.value = query.owner ?? '';
    unassignedBox.checked = query.unassigned;
    ageSelect.value = query.age;
    const owners = new Set(vocab.owners);
    if (query.owner) owners.add(query.owner);
    ownerOptions.replaceChildren(...[...owners].map((owner) => el('option', { value: owner })));
    groupSelect.value = query.group;
    sortSelect.value = query.sort;
    const [ascLabel, descLabel] = SORT_LABELS.get(query.sort);
    dirButton.textContent = query.dir === 'asc' ? ascLabel : descLabel;
    dirButton.setAttribute('aria-label', `Sort direction: ${dirButton.textContent}`);
    const compact = ctx.prefs.get().density === 'compact';
    densityButton.setAttribute('aria-pressed', String(compact));
    if (document.activeElement !== searchInput && searchInput.value !== query.q) searchInput.value = query.q;
    for (const button of viewButtons) {
      button.setAttribute('aria-pressed', String(sameQuery(builtInView(button.dataset.view, query), { ...query, page: 1 })));
    }
    const active = activeFilters();
    filtersToggle.textContent = active.length ? `Filters (${active.length} active)` : 'Filters';
    clear(scopeLine);
    append(scopeLine, 
      query.project ? `Project: ${query.project} · ` : 'All projects · ',
      'Projects observed in tasks are listed in the header picker.',
    );
    renderSavedViews();
  }

  function activeFilters() {
    const list = [];
    if (query.statusMode === 'all') list.push('Status: all');
    if (query.statusMode === 'selected') list.push(`Status: ${query.statuses.map(statusLabel).join(', ') || 'none'}`);
    if (query.priorities.length) list.push(`Priority: ${query.priorities.map(priorityLabel).join(', ')}`);
    if (query.owner) list.push(`Owner: ${query.owner}`);
    if (query.unassigned) list.push('Unassigned only');
    if (query.age !== 'any') list.push(`Age: ${AGE_LABELS.get(query.age).toLowerCase()}`);
    if (query.q) list.push(`Search: ${query.q}`);
    return list;
  }

  function renderSavedViews() {
    const prefs = ctx.prefs.get();
    const open = savedViews.open;
    const nameInput = el('input', { id: 'save-view-name', type: 'text', maxLength: 80, autocomplete: 'off' });
    clear(savedViews);
    append(savedViews, 
      el('summary', { class: 'button secondary' }, 'Saved views'),
      el(
        'div',
        { class: 'popover-body' },
        ctx.prefs.available ? null : el('p', { class: 'muted small' }, 'Preferences apply for this tab.'),
        prefs.views.length
          ? el(
              'ul',
              { class: 'saved-list' },
              ...prefs.views.map((view) =>
                el(
                  'li',
                  {},
                  el('button', { type: 'button', class: 'link-button', onclick: () => setQuery({ ...parseTaskQuery(new URLSearchParams(view.query)).state, page: 1 }) }, view.name),
                  prefs.defaultView === view.name ? el('span', { class: 'muted small' }, ' (default)') : null,
                  el('span', { class: 'saved-actions' },
                    el('button', { type: 'button', class: 'link-button', 'aria-label': `Set ${view.name} as default`, onclick: () => { ctx.prefs.update({ defaultView: view.name }); renderSavedViews(); ctx.toast('Default view set'); } }, 'Default'),
                    el('button', {
                      type: 'button',
                      class: 'link-button',
                      'aria-label': `Rename ${view.name}`,
                      onclick: () => {
                        const name = window.prompt('New name for this view', view.name);
                        if (name !== null && !ctx.prefs.renameView(view.name, name)) ctx.toast('That name is empty or already used');
                        renderSavedViews();
                      },
                    }, 'Rename'),
                    el('button', { type: 'button', class: 'link-button', 'aria-label': `Delete ${view.name}`, onclick: () => { ctx.prefs.deleteView(view.name); renderSavedViews(); } }, 'Delete'),
                  ),
                ),
              ),
            )
          : el('p', { class: 'muted small' }, 'No saved views yet.'),
        el(
          'form',
          {
            class: 'save-view',
            onsubmit: (event) => {
              event.preventDefault();
              const snapshot = serializeTaskQuery({ ...query, page: 1 }).toString();
              if (ctx.prefs.saveView(nameInput.value, snapshot)) {
                ctx.toast('View saved');
                renderSavedViews();
              }
            },
          },
          el('label', { for: 'save-view-name' }, 'Save current filters as'),
          el('div', { class: 'inline' }, nameInput, el('button', { type: 'submit', class: 'button secondary' }, 'Save')),
        ),
        el(
          'p',
          { class: 'inline' },
          el('button', {
            type: 'button',
            class: 'link-button',
            onclick: () => {
              ctx.prefs.update({ defaultView: null });
              renderSavedViews();
              ctx.toast('Default view reset to Open');
            },
          }, 'Use Open as default'),
        ),
        el('p', { class: 'muted small' }, 'Saved views keep filters on this device and may appear in browser history; never your password.'),
      ),
    );
    savedViews.open = open;
  }

  // ------------------------------------------------------------ results
  // Updated in place by the staleness timer so focus and alert regions are never rebuilt.
  const freshness = el('span', { class: 'freshness' });

  function updateFreshness() {
    const { fetchedAt } = store.coverage();
    clear(freshness);
    if (!fetchedAt) return;
    const stale = Date.now() - fetchedAt > SCAN_INTERVAL_MS * 2;
    append(freshness, ` · Data fetched ${clockTime(fetchedAt)}`, stale ? el('strong', { class: 'stale' }, ' · Stale') : null);
  }

  // Error panels are alerts: re-insert them only when the error itself changes.
  const scanErrorSlot = el('div');
  const refreshErrorSlot = el('div');
  const coverageLine = el('p', { class: 'coverage' });
  append(coverageRegion, scanErrorSlot, refreshErrorSlot, coverageLine);
  let shownScanError = null;
  let shownRefreshError = null;

  function syncErrorSlot(slot, shown, error, build) {
    if (shown === error) return shown;
    clear(slot);
    if (error) append(slot, build(error));
    return error;
  }

  function renderCoverage(matches) {
    const coverage = store.coverage();
    shownScanError = syncErrorSlot(scanErrorSlot, shownScanError, coverage.status === 'error' ? coverage.error : null, (error) =>
      panelError(error, () => store.continueScan()),
    );
    shownRefreshError = syncErrorSlot(refreshErrorSlot, shownRefreshError, coverage.refreshError, (error) =>
      panelError(error, () => manualRefresh(), { refresh: true }),
    );
    const focusKey = coverageLine.contains(document.activeElement) ? document.activeElement.dataset.focusKey : null;
    const line = clear(coverageLine);
    const scanned = `${coverage.unique} loaded`;
    const action = (key, label, onclick) => el('button', { type: 'button', class: 'link-button', dataset: { focusKey: key }, onclick }, label);
    switch (coverage.status) {
      case 'loading':
        append(line, `${coverage.unique} records loaded; loading more `, action('stop', 'Stop loading', () => store.stopScan()));
        break;
      case 'paused':
      case 'stopped':
        append(line, `Partial data: ${scanned}. `, action('continue', 'Continue loading', () => store.continueScan()));
        break;
      case 'budget':
        append(line, `Partial data: ${scanned}. The browser memory budget is reached — narrow to a project to see more.`);
        break;
      case 'error':
        append(line, `Incomplete: ${scanned}; loading stopped after a failure.`);
        break;
      case 'complete':
        append(line, `${scanned} · scan complete`);
        break;
      default:
        append(line, 'Not loaded yet');
    }
    if (coverage.malformed) append(line, ` · ${coverage.malformed} unreadable record${coverage.malformed === 1 ? '' : 's'} skipped`);
    updateFreshness();
    append(line, freshness);
    if (focusKey) {
      // The action may have changed (Stop → Continue); keep focus inside the line either way.
      (line.querySelector(`[data-focus-key="${focusKey}"]`) ?? line.querySelector('button') ?? resultsHeading).focus();
    }
    clear(summary);
    if (matches.total) append(summary, `Showing ${matches.start}–${matches.end} of ${matches.total} matches · ${scanned}${coverage.partial ? ' (partial)' : ''}`);
    else append(summary, `0 matches · ${scanned}${coverage.partial ? ' (partial)' : ''}`);
    const ready = store.stagedReady();
    if (ready !== bannerShown) {
      bannerShown = ready;
      clear(updateBanner);
      if (ready) {
        append(
          updateBanner,
          el('p', {}, 'Updated data available. ', el('button', { type: 'button', class: 'button secondary', onclick: () => applyUpdate() }, 'Show updates')),
        );
      }
    }
  }

  function applyUpdate() {
    const focusedId = document.activeElement?.closest?.('[data-task-id]')?.dataset.taskId ?? null;
    store.applyStaged();
    if (focusedId) results.querySelector(`a[data-task-id="${CSS.escape(focusedId)}"]`)?.focus();
    else resultsHeading.focus();
    ctx.announce('Task list updated.');
  }

  function renderResults({ announce = false } = {}) {
    const focusedId = results.contains(document.activeElement) ? document.activeElement.closest?.('[data-task-id]')?.dataset.taskId : null;
    const coverage = store.coverage();
    const all = store.tasks();
    const filtered = filterTasks(all, query);
    const matches = pageTasks(filtered, query);
    if (matches.clamped) {
      query = { ...query, page: matches.page };
      writeUrl(false);
      ctx.announce(`Page changed to ${matches.page}, the last page.`);
    }
    renderCoverage(matches);
    clear(results);
    clear(pager);
    results.removeAttribute('aria-busy');

    if (!all.length && (coverage.status === 'loading' || coverage.status === 'idle')) {
      results.setAttribute('aria-busy', 'true');
      append(results, el('div', { class: 'skeleton' }, el('p', {}, 'Loading tasks'), el('div', { class: 'skeleton-row' }), el('div', { class: 'skeleton-row' }), el('div', { class: 'skeleton-row' })));
      return;
    }
    if (query.statusMode === 'selected' && !query.statuses.length) {
      append(results, el('p', { class: 'empty' }, 'Select a status or choose Open. ', el('button', { type: 'button', class: 'button secondary', onclick: () => setQuery({ ...query, statusMode: 'open', statuses: [], page: 1 }) }, 'Choose Open')));
      if (announce) ctx.announce('No statuses selected.');
      return;
    }
    if (!all.length && coverage.status === 'complete') {
      append(results, el('p', { class: 'empty' }, query.project ? `No tasks observed for project ${query.project}.` : 'No tasks yet. Agents can add tasks through MemCan.'));
      if (announce) ctx.announce('No tasks.');
      return;
    }
    if (!filtered.length) {
      if (coverage.partial) {
        append(results, 
          el(
            'div',
            { class: 'empty' },
            el('p', {}, 'No matches in loaded tasks.'),
            ['paused', 'stopped', 'error'].includes(coverage.status)
              ? el('button', { type: 'button', class: 'button secondary', onclick: () => store.continueScan() }, 'Continue loading')
              : null,
          ),
        );
      } else {
        const active = activeFilters();
        append(results, 
          el(
            'div',
            { class: 'empty' },
            el('p', {}, 'No tasks match the active filters.'),
            active.length ? el('ul', {}, ...active.map((item) => el('li', {}, item))) : null,
            el('button', { type: 'button', class: 'button secondary', onclick: () => clearButton.click() }, 'Clear filters'),
          ),
        );
      }
      if (announce) ctx.announce('No matching tasks.');
      return;
    }

    append(results, taskTable(matches));
    renderPager(matches);
    if (announce) ctx.announce(`${matches.total} matching task${matches.total === 1 ? '' : 's'}${coverage.partial ? ' in loaded data' : ''}.`);
    if (focusedId) results.querySelector(`a[data-task-id="${CSS.escape(focusedId)}"]`)?.focus();
  }

  function sortAttr(column) {
    if (query.sort !== column) return null;
    // The Age column shows elapsed time, so oldest-created first is descending age.
    const ascending = column === 'created' ? query.dir === 'desc' : query.dir === 'asc';
    return ascending ? 'ascending' : 'descending';
  }

  function columnHeader(key, label) {
    const attrs = { scope: 'col', class: `col-${key}`, 'aria-sort': sortAttr(key) };
    if (!SORTABLE_COLUMNS.includes(key)) return el('th', attrs, label);
    const [ascLabel, descLabel] = SORT_LABELS.get(key);
    const next = sortByColumn(query, key);
    return el(
      'th',
      attrs,
      el(
        'button',
        {
          type: 'button',
          class: 'sort-header',
          title: `Sort: ${next.dir === 'asc' ? ascLabel : descLabel}`,
          dataset: { sort: key },
          onclick: () => {
            setQuery(next);
            results.querySelector(`.sort-header[data-sort="${key}"]`)?.focus();
          },
        },
        label,
      ),
    );
  }

  function taskTable(matches) {
    const tokens = searchTokens(query.q);
    const showProject = query.group !== 'project';
    const columns = [
      ['priority', 'Priority'],
      ['task', 'Task'],
      showProject ? ['project', 'Project'] : null,
      ['status', 'Status'],
      ['owner', 'Owner'],
      ['created', 'Age'],
    ].filter(Boolean);
    const detailQuery = serializeTaskQuery(query).toString();
    const table = el(
      'table',
      { class: 'task-table' },
      el('caption', { class: 'visually-hidden' }, `Tasks, page ${matches.page} of ${matches.pageCount}`),
      el('thead', {}, el('tr', {}, ...columns.map(([key, label]) => columnHeader(key, label)))),
    );
    for (const group of matches.groups) {
      const body = el('tbody', {});
      if (group.key !== null) {
        const label = query.group === 'project' ? group.key || '(no project)' : statusLabel(group.key);
        const expanded = !collapsed.has(group.key);
        append(body, 
          el(
            'tr',
            { class: 'group-row' },
            el(
              'th',
              { colSpan: columns.length, scope: 'rowgroup' },
              el(
                'button',
                {
                  type: 'button',
                  class: 'group-toggle',
                  'aria-expanded': String(expanded),
                  onclick: () => {
                    if (collapsed.has(group.key)) collapsed.delete(group.key);
                    else collapsed.add(group.key);
                    renderResults();
                    results.querySelector(`.group-toggle[data-key="${CSS.escape(group.key)}"]`)?.focus();
                  },
                  dataset: { key: group.key },
                },
                el('span', { 'aria-hidden': 'true' }, expanded ? '▾ ' : '▸ '),
                `${label} (${group.count} match${group.count === 1 ? '' : 'es'})${group.continued ? ' — continued' : ''}`,
              ),
            ),
          ),
        );
        if (!expanded) {
          append(table, body);
          continue;
        }
      }
      for (const task of group.rows) {
        const match = searchMatch(task, tokens);
        const href = `${taskPath(task.id)}?${detailQuery}`;
        const age = formatAge(task.createdAt);
        const cells = [
          el('td', { class: 'col-priority' }, el('span', { class: 'cell-label' }, 'Priority'), priorityBadge(task.priority)),
          el(
            'td',
            { class: 'col-task' },
            navLink(href, highlighted(task.title || '(untitled)', tokens), { class: 'task-link', dataset: { taskId: task.id, nav: '' } }),
            task.description ? el('div', { class: 'preview muted' }, highlighted(task.description.split('\n', 1)[0], tokens)) : null,
            el(
              'div',
              { class: 'row-meta small' },
              task.blockedBy.length ? el('span', {}, `Blockers: ${task.blockedBy.length}`) : null,
              match?.descriptionOnly ? el('span', { class: 'muted' }, 'Match in description') : null,
            ),
          ),
          showProject ? el('td', { class: 'col-project wrap' }, el('span', { class: 'cell-label' }, 'Project'), highlighted(task.project || '(no project)', tokens)) : null,
          el('td', { class: 'col-status' }, el('span', { class: 'cell-label' }, 'Status'), statusBadge(task.status)),
          el('td', { class: 'col-owner wrap' }, el('span', { class: 'cell-label' }, 'Owner'), task.owner ? highlighted(task.owner, tokens) : el('span', { class: 'muted' }, 'Unassigned')),
          el(
            'td',
            { class: 'col-created' },
            el('span', { class: 'cell-label' }, 'Age'),
            age ? el('time', { dateTime: task.createdAt, title: exactDate(task.createdAt) ?? task.createdAt }, age) : el('span', { class: 'muted', title: task.createdAt }, 'Unrecognized date'),
          ),
        ];
        append(body, el('tr', {}, ...cells));
      }
      append(table, body);
    }
    return el('div', { class: 'table-wrap' }, table);
  }

  function renderPager(matches) {
    const prev = el('button', { type: 'button', class: 'button secondary', disabled: matches.page <= 1, onclick: () => setQuery({ ...query, page: matches.page - 1 }, { focusResults: true }) }, 'Previous');
    const next = el('button', { type: 'button', class: 'button secondary', disabled: matches.page >= matches.pageCount, onclick: () => setQuery({ ...query, page: matches.page + 1 }, { focusResults: true }) }, 'Next');
    const size = el(
      'select',
      { id: 'page-size', onchange: () => setQuery({ ...query, size: Number(size.value), page: 1 }) },
      ...[25, 50, 100].map((n) => el('option', { value: String(n), selected: n === query.size }, String(n))),
    );
    append(pager, 
      prev,
      el('span', { class: 'page-info' }, `Page ${matches.page} of ${matches.pageCount}`),
      next,
      el('span', { class: 'field inline' }, el('label', { for: 'page-size' }, 'Rows'), size),
    );
  }

  // ------------------------------------------------------------ refresh
  async function manualRefresh() {
    await store.refresh();
    if (store.stagedReady()) applyUpdate();
  }

  function update() {
    const status = store.coverage().status;
    syncControlsLight();
    const announce = status !== lastCoverageStatus && status === 'complete' && lastCoverageStatus === 'loading';
    lastCoverageStatus = status;
    renderResults({ announce });
  }

  function syncControlsLight() {
    // Only vocabularies change with data; avoid rebuilding focused controls mid-interaction.
    if (!filtersRegion.contains(document.activeElement) && !savedViews.contains(document.activeElement)) syncControls();
  }

  store.startScan(query.project);
  syncControls();
  renderResults();
  lastCoverageStatus = store.coverage().status;
  ctx.every(SCAN_INTERVAL_MS, () => store.refresh());
  ctx.every(STALE_CHECK_MS, updateFreshness);
  if (!ctx.restoreHistoryPosition()) heading.focus();

  return {
    update,
    refresh: manualRefresh,
    focusSearch: () => searchInput.focus(),
    dispose: () => clearTimeout(searchTimer),
  };
}
