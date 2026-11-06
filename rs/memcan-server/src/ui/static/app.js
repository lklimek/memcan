// Application shell: routing, session lifecycle, polling, header, commands and shortcuts.

import { createAuth, sessionAuth } from './auth.js';
import { append, clear, el, errorMessage, navLink, openDialog } from './components.js';
import { vocabulary } from './data.js';
import { McpError, createMcpClient } from './mcp.js';
import { BUILT_IN_VIEWS, createPrefs } from './prefs.js';
import {
  builtInView,
  defaultTaskQuery,
  parseRoute,
  parseTaskQuery,
  safeReturnTo,
  tasksUrl,
} from './router.js';
import { createTaskStore } from './store.js';
import * as dashboardView from './views/dashboard.js';
import * as loginView from './views/login.js';
import * as taskView from './views/task.js';
import * as tasksView from './views/tasks.js';

const HEALTH_INTERVAL_MS = 30000;
const ACTIVITY_INTERVAL_MS = 60000;
const RENEW_INTERVAL_MS = 30000;
const LOOPBACK = /^(localhost|127(?:\.\d{1,3}){3}|\[::1\])$/i;

function storageOrNull() {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

const prefs = createPrefs(storageOrNull());
const insecureHttp = location.protocol === 'http:' && !LOOPBACK.test(location.hostname);
const auth = createAuth();

const state = {
  /** Signed-in principal from the backend (`username`, `role`, `method`, `csrfToken`), or null. */
  session: null,
  insecureHttpAllowed: false,
  ready: false,
  lastActivity: 0,
  lastRenew: 0,
  client: null,
  store: null,
  missingTools: [],
  connectionNotice: null,
  view: null,
  routeController: null,
  timers: [],
  health: { data: null, error: null, fetchedAt: null, loading: false },
  healthListeners: new Set(),
  recentProjects: [],
};

// ---------------------------------------------------------------- polling

/** Interval jobs pause while hidden/offline and catch up (coalesced) when visible again. */
function every(ms, fn, { scoped = true } = {}) {
  const job = { ms, fn, last: Date.now(), running: false, scoped };
  state.timers.push(job);
  return job;
}

async function runJob(job) {
  if (job.running) return;
  job.running = true;
  job.last = Date.now();
  try {
    await job.fn();
  } catch {
    // Jobs report their own errors in their panels.
  } finally {
    job.running = false;
  }
}

function tick() {
  if (document.hidden || !navigator.onLine) return;
  const now = Date.now();
  for (const job of state.timers) if (now - job.last >= job.ms) runJob(job);
}
setInterval(tick, 1000);
document.addEventListener('visibilitychange', tick);
window.addEventListener('online', tick);

// ---------------------------------------------------------------- health

async function refreshHealth() {
  const health = state.health;
  if (health.loading) return health.pending;
  health.loading = true;
  emitHealth();
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 20000);
  health.pending = (async () => {
    try {
      const response = await fetch('/health', {
        credentials: 'omit',
        cache: 'no-store',
        redirect: 'error',
        signal: controller.signal,
      });
      if (!response.ok) {
        const category = response.status === 403 ? 'forbidden' : response.status === 429 ? 'throttled' : 'unavailable';
        throw new McpError(category, 'health unavailable', { status: response.status });
      }
      const data = await response.json();
      if (!data || typeof data !== 'object' || typeof data.status !== 'string') {
        throw new McpError('protocol', 'health unreadable');
      }
      health.data = data;
      health.error = null;
      health.fetchedAt = Date.now();
    } catch (error) {
      health.error = error instanceof McpError ? error : new McpError('unavailable', 'health unavailable');
    } finally {
      clearTimeout(timer);
      health.loading = false;
      health.pending = null;
      emitHealth();
    }
  })();
  return health.pending;
}

function emitHealth() {
  renderHealthIndicator();
  for (const listener of state.healthListeners) listener(state.health);
}

// ---------------------------------------------------------------- shell

const live = el('div', { class: 'visually-hidden', 'aria-live': 'polite', 'aria-atomic': 'true' });
const alertRegion = el('div', { class: 'visually-hidden', role: 'alert', 'aria-atomic': 'true' });
const toastRegion = el('div', { class: 'toasts', 'aria-live': 'polite' });
const main = el('main', { id: 'main', tabindex: '-1' });
const healthIndicator = el('a', { href: '/ui/dashboard', dataset: { nav: '' }, class: 'indicator' });
const connectionIndicator = el('span', { class: 'indicator' });
const navTasks = navLink('/ui/tasks', 'Tasks');
const navDashboard = navLink('/ui/dashboard', 'Dashboard');
const projectInput = el('input', {
  id: 'project-picker',
  type: 'search',
  list: 'project-options',
  placeholder: 'All projects',
  autocomplete: 'off',
  class: 'project-input',
});
const projectOptions = el('datalist', { id: 'project-options' });
const signOutButton = el('button', { type: 'button', class: 'button secondary', onclick: () => signOut() }, 'Sign out');
const refreshButton = el('button', { type: 'button', class: 'button secondary', onclick: () => refreshView() }, 'Refresh');
const commandsButton = el(
  'button',
  { type: 'button', class: 'button secondary', 'aria-keyshortcuts': 'Control+K Meta+K', onclick: () => openCommands() },
  'Commands',
);
const themeSelect = el(
  'select',
  { id: 'theme-select', onchange: () => setTheme(themeSelect.value) },
  el('option', { value: 'system' }, 'System'),
  el('option', { value: 'light' }, 'Light'),
  el('option', { value: 'dark' }, 'Dark'),
);
const banners = el('div', { class: 'banners' });

function buildShell() {
  const header = el(
    'header',
    { class: 'app-header' },
    el(
      'div',
      { class: 'header-row' },
      navLink('/ui/tasks', 'MemCan', { class: 'logo', 'aria-label': 'MemCan — Tasks' }),
      el('nav', { 'aria-label': 'Primary', class: 'primary-nav' }, el('ul', {}, el('li', {}, navTasks), el('li', {}, navDashboard))),
      el(
        'form',
        { class: 'project-picker', role: 'search', 'aria-label': 'Project', onsubmit: onProjectSubmit },
        el('label', { for: 'project-picker' }, 'Project'),
        projectInput,
        projectOptions,
        el('button', { type: 'submit', class: 'button secondary' }, 'Go'),
      ),
      el('div', { class: 'header-actions' }, commandsButton, refreshButton, signOutButton),
    ),
    el(
      'div',
      { class: 'header-row status-row' },
      healthIndicator,
      connectionIndicator,
      el('span', { class: 'readonly-label' }, 'Read-only'),
      el('label', { class: 'theme-label', for: 'theme-select' }, 'Theme'),
      themeSelect,
    ),
    banners,
  );
  document.body.replaceChildren(
    el('a', {
      href: '#main',
      class: 'skip-link',
      onclick: (event) => {
        // Focus without a hash navigation, which would fire popstate and re-render the view.
        event.preventDefault();
        main.focus();
      },
    }, 'Skip to content'),
    header,
    main,
    live,
    alertRegion,
    toastRegion,
  );
}

function renderBanners() {
  clear(banners);
  if (insecureHttp) {
    append(banners, 
      el(
        'p',
        { class: 'banner warning', role: 'note' },
        el('strong', {}, 'Unencrypted connection. '),
        'Your password and session can be intercepted. Use HTTPS or a localhost tunnel.',
      ),
    );
  }
  if (state.client && state.missingTools.length) {
    append(banners, 
      el(
        'p',
        { class: 'banner', role: 'note' },
        `This server does not offer: ${state.missingTools.join(', ')}. Features that need them are unavailable.`,
      ),
    );
  }
}

function renderHealthIndicator() {
  const { data, error, loading } = state.health;
  let text = 'Server: checking…';
  let tone = 'neutral';
  if (error) {
    text = 'Server: unreachable';
    tone = 'bad';
  } else if (data) {
    text = data.status === 'ok' ? 'Server: healthy' : 'Server: degraded';
    tone = data.status === 'ok' ? 'good' : 'warn';
  } else if (!loading) text = 'Server: unknown';
  healthIndicator.textContent = text;
  healthIndicator.dataset.tone = tone;
}

function renderConnection() {
  const { session } = state;
  connectionIndicator.textContent = session ? `Signed in as ${session.username} (${session.role})` : 'Not signed in';
  connectionIndicator.dataset.tone = state.client ? 'good' : 'neutral';
  signOutButton.hidden = !session;
  renderBanners();
}

function updateProjectOptions() {
  const names = new Set(state.recentProjects);
  if (state.store) for (const name of vocabulary(state.store.tasks()).projects) names.add(name);
  const options = [...names].slice(0, 500).map((name) => el('option', { value: name }));
  projectOptions.replaceChildren(...options);
  projectOptions.setAttribute('aria-label', 'Projects observed in tasks');
}

function rememberProject(project) {
  if (!project) return;
  state.recentProjects = [project, ...state.recentProjects.filter((p) => p !== project)].slice(0, 8);
}

function onProjectSubmit(event) {
  event.preventDefault();
  selectProject(projectInput.value.trim() || null);
}

function selectProject(project) {
  rememberProject(project);
  const route = parseRoute(location.pathname);
  if (route.name === 'dashboard') {
    const params = new URLSearchParams();
    if (project) params.set('project', project);
    navigate(`/ui/dashboard${params.size ? `?${params}` : ''}`);
    return;
  }
  const { state: query } = parseTaskQuery(new URLSearchParams(route.name === 'tasks' ? location.search : ''));
  navigate(tasksUrl({ ...query, project, page: 1 }));
}

function setTheme(theme) {
  prefs.update({ theme });
  applyPrefs();
}

function applyPrefs() {
  const current = prefs.get();
  const root = document.documentElement;
  if (current.theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = current.theme;
  root.dataset.density = current.density;
  themeSelect.value = current.theme;
}

// ---------------------------------------------------------------- announcements

function announce(message, { assertive = false } = {}) {
  const region = assertive ? alertRegion : live;
  region.textContent = '';
  // A separate task lets screen readers notice identical consecutive messages.
  setTimeout(() => {
    region.textContent = message;
  }, 50);
}

function toast(message) {
  const item = el('div', { class: 'toast' }, message);
  let remaining = 5000;
  let started = Date.now();
  let timer = setTimeout(() => item.remove(), remaining);
  const pause = () => {
    clearTimeout(timer);
    remaining -= Date.now() - started;
  };
  const resume = () => {
    started = Date.now();
    timer = setTimeout(() => item.remove(), Math.max(remaining, 1000));
  };
  item.addEventListener('mouseenter', pause);
  item.addEventListener('mouseleave', resume);
  append(toastRegion, item);
}

// ---------------------------------------------------------------- session

/** Open the MCP client for a signed-in session returned by the backend. */
async function startSession(info) {
  endSession();
  const client = createMcpClient({ auth: sessionAuth(info.csrfToken), onAuthFailure: (error) => handleAuthFailure(client, error) });
  state.session = info;
  state.lastActivity = Date.now();
  try {
    const connected = await client.connect();
    state.missingTools = connected.missing;
  } catch (error) {
    client.close();
    if (state.session === info) state.session = null;
    throw error;
  }
  state.client = client;
  state.store = createTaskStore({
    callTool: (name, args, options) => client.callTool(name, args, options),
    onChange: () => {
      updateProjectOptions();
      state.view?.update?.();
    },
  });
  renderConnection();
}

async function login(username, password) {
  await startSession(await auth.login(username, password));
}

/** Ask the backend for the current session; valid proxy Basic credentials start one silently. */
async function restoreSession() {
  const info = await auth.session();
  state.insecureHttpAllowed = info.insecureHttpAllowed === true;
  if (info.authenticated) await startSession(info);
  else endSession();
}

/** Forget the session and all loaded data in this tab (the server session is untouched). */
function endSession() {
  state.client?.close();
  state.store?.clear();
  state.session = null;
  state.client = null;
  state.store = null;
  state.missingTools = [];
  state.recentProjects = [];
  projectOptions.replaceChildren();
}

function loginUrl() {
  const returnTo = safeReturnTo(location.pathname + location.search);
  return `/ui/login${returnTo ? `?${new URLSearchParams({ returnTo })}` : ''}`;
}

async function handleAuthFailure(client, error) {
  if (state.client !== client) return;
  endSession();
  renderConnection();
  // An expired session renews silently once in a while when the browser still sends valid Basic credentials.
  if (error.reason === 'session' && Date.now() - state.lastRenew > RENEW_INTERVAL_MS) {
    state.lastRenew = Date.now();
    try {
      await restoreSession();
      if (state.session) {
        render();
        return;
      }
    } catch {
      // Fall through to the sign-in page.
    }
  }
  state.connectionNotice = { kind: 'error', text: error.message };
  navigate(loginUrl(), { replace: true });
}

async function signOut() {
  const { session } = state;
  if (!session) return;
  await auth.logout(session.csrfToken).catch(() => {});
  endSession();
  renderConnection();
  state.connectionNotice = {
    kind: 'info',
    text:
      session.method === 'basic'
        ? 'Signed out of this session. You signed in through the proxy’s Basic authentication, which this browser keeps until it closes: close the browser to sign out completely.'
        : 'Signed out.',
  };
  navigate('/ui/login');
}

/** Foreground interaction extends the server's idle timeout; background polling never does. */
function noteActivity() {
  const { session } = state;
  if (!session || Date.now() - state.lastActivity < ACTIVITY_INTERVAL_MS) return;
  state.lastActivity = Date.now();
  auth.activity(session.csrfToken).catch(() => {});
}
document.addEventListener('pointerdown', noteActivity, { passive: true });
document.addEventListener('keydown', noteActivity, { passive: true });

window.addEventListener('pagehide', () => endSession());
window.addEventListener('pageshow', (event) => {
  if (event.persisted) start();
});

function requireSession() {
  if (state.client) return true;
  navigate(loginUrl(), { replace: true });
  return false;
}

// ---------------------------------------------------------------- routing

function navigate(url, { replace = false } = {}) {
  if (replace) history.replaceState(null, '', url);
  else {
    saveScroll();
    history.pushState(null, '', url);
  }
  render();
}

function saveScroll() {
  const focused = document.activeElement?.closest?.('[data-task-id]')?.dataset.taskId ?? null;
  history.replaceState({ ...(history.state ?? {}), scrollY: window.scrollY, focusTaskId: focused }, '');
}

function setActiveNav(name) {
  for (const [link, routes] of [
    [navTasks, ['tasks', 'task']],
    [navDashboard, ['dashboard']],
  ]) {
    if (routes.includes(name)) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  }
}

function render() {
  state.view?.dispose?.();
  state.view = null;
  state.routeController?.abort();
  state.routeController = new AbortController();
  state.timers = state.timers.filter((job) => !job.scoped);
  renderConnection();

  let route = parseRoute(location.pathname);
  const params = new URLSearchParams(location.search);
  if (route.name === 'root') {
    const query = parseTaskQuery(params).state;
    history.replaceState(null, '', params.size ? tasksUrl(query) : '/ui/tasks');
    route = { name: 'tasks' };
  }
  if (route.name === 'tasks' && location.search === '') {
    const def = prefs.get().defaultView;
    if (def) history.replaceState(null, '', defaultViewUrl(def));
  }
  setActiveNav(route.name);
  const project = new URLSearchParams(location.search).get('project');
  if (document.activeElement !== projectInput) projectInput.value = project ?? '';

  trackLocation();
  const ctx = {
    trackLocation,
    route,
    params: new URLSearchParams(location.search),
    client: state.client,
    store: state.store,
    connected: Boolean(state.client),
    signedIn: Boolean(state.client),
    httpsRequired: location.protocol === 'http:' && !state.insecureHttpAllowed,
    prefs,
    health: state.health,
    insecureHttp,
    signal: state.routeController.signal,
    navigate,
    announce,
    toast,
    login,
    refreshHealth,
    every,
    takeNotice: () => {
      const notice = state.connectionNotice;
      state.connectionNotice = null;
      return notice;
    },
    onHealth: (listener) => {
      state.healthListeners.add(listener);
      return () => state.healthListeners.delete(listener);
    },
    setTitle: (title) => {
      document.title = `${title} · MemCan`;
    },
    applyPrefs,
    focusHeading,
    restoreHistoryPosition,
  };

  clear(main);
  main.removeAttribute('aria-busy');
  const view = {
    login: loginView,
    dashboard: dashboardView,
    tasks: tasksView,
    task: taskView,
  }[route.name];
  if (!state.ready) {
    append(main, el('p', { class: 'muted', role: 'status' }, 'Checking sign-in…'));
    return;
  }
  if ((route.name === 'tasks' || route.name === 'task') && !requireSession()) return;
  if (view) state.view = view.render(main, ctx) ?? null;
  else renderNotFound();
}

function defaultViewUrl(name) {
  if (BUILT_IN_VIEWS.includes(name)) return tasksUrl(builtInView(name, defaultTaskQuery()));
  const saved = prefs.get().views.find((view) => view.name === name);
  if (!saved) return tasksUrl(defaultTaskQuery());
  return tasksUrl({ ...parseTaskQuery(new URLSearchParams(saved.query)).state, page: 1 });
}

function focusHeading() {
  const heading = main.querySelector('h1');
  heading?.focus({ preventScroll: false });
}

function restoreHistoryPosition() {
  const saved = history.state;
  if (!saved) return false;
  if (typeof saved.scrollY === 'number') window.scrollTo(0, saved.scrollY);
  if (saved.focusTaskId) {
    const link = [...main.querySelectorAll('a[data-task-id]')].find((a) => a.dataset.taskId === saved.focusTaskId);
    if (link) {
      link.focus({ preventScroll: true });
      return true;
    }
  }
  return false;
}

function renderNotFound() {
  document.title = 'Not found · MemCan';
  append(main, [
    el('h1', { tabindex: '-1' }, 'Page not found'),
    el('p', {}, 'This address does not match a MemCan page.'),
    el('p', {}, navLink('/ui/tasks', 'Go to tasks')),
  ]);
  focusHeading();
}

function refreshView() {
  refreshHealth();
  if (state.view?.refresh) state.view.refresh();
}

// ---------------------------------------------------------------- commands and shortcuts

function commandList() {
  const commands = [
    { label: 'Go to Tasks', run: () => navigate('/ui/tasks') },
    { label: 'Go to Dashboard', run: () => navigate('/ui/dashboard') },
    { label: 'Choose project…', run: () => projectInput.focus() },
    { label: 'All projects', run: () => selectProject(null) },
  ];
  const current = parseTaskQuery(new URLSearchParams(parseRoute(location.pathname).name === 'tasks' ? location.search : '')).state;
  for (const [name, label] of [
    ['open', 'Open'],
    ['high', 'High priority'],
    ['blocked', 'Blocked'],
    ['unassigned', 'Unassigned'],
    ['all', 'All'],
  ]) {
    commands.push({ label: `View: ${label}`, run: () => navigate(tasksUrl(builtInView(name, current))) });
  }
  for (const view of prefs.get().views) {
    commands.push({ label: `Saved view: ${view.name}`, run: () => navigate(defaultViewUrl(view.name)) });
  }
  commands.push(
    { label: 'Refresh', run: refreshView },
    { label: 'Theme: System', run: () => setTheme('system') },
    { label: 'Theme: Light', run: () => setTheme('light') },
    { label: 'Theme: Dark', run: () => setTheme('dark') },
    { label: 'Keyboard shortcuts', run: openShortcutHelp },
  );
  if (state.session) commands.push({ label: 'Sign out', run: () => signOut() });
  return commands;
}

let commandsDialog = null;

function openCommands() {
  if (commandsDialog) {
    commandsDialog.close();
    return;
  }
  const commands = commandList();
  const input = el('input', {
    type: 'text',
    class: 'command-input',
    'aria-label': 'Filter commands',
    role: 'combobox',
    'aria-expanded': 'true',
    'aria-controls': 'command-list',
    autocomplete: 'off',
  });
  const list = el('ul', { id: 'command-list', role: 'listbox', class: 'command-list', 'aria-label': 'Commands' });
  let active = 0;
  let visible = commands;
  const draw = () => {
    const tokens = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    visible = commands.filter((c) => tokens.every((t) => c.label.toLowerCase().includes(t)));
    active = Math.min(active, Math.max(0, visible.length - 1));
    list.replaceChildren(
      ...visible.map((command, index) =>
        el(
          'li',
          {
            id: `command-${index}`,
            role: 'option',
            'aria-selected': index === active ? 'true' : 'false',
            class: 'command-option',
            onclick: () => runCommand(command),
          },
          command.label,
        ),
      ),
    );
    if (!visible.length) append(list, el('li', { class: 'muted' }, 'No matching commands'));
    if (visible.length) input.setAttribute('aria-activedescendant', `command-${active}`);
    else input.removeAttribute('aria-activedescendant');
  };
  const runCommand = (command) => {
    commandsDialog?.close();
    command.run();
  };
  input.addEventListener('input', () => {
    active = 0;
    draw();
  });
  input.addEventListener('keydown', (event) => {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!visible.length) return;
      active = (active + (event.key === 'ArrowDown' ? 1 : visible.length - 1)) % visible.length;
      draw();
      list.children[active]?.scrollIntoView({ block: 'nearest' });
    } else if (event.key === 'Enter' && visible[active]) {
      event.preventDefault();
      runCommand(visible[active]);
    }
  });
  draw();
  commandsDialog = openDialog({
    title: 'Commands',
    body: el('div', { class: 'commands' }, input, list),
    onClose: () => {
      commandsDialog = null;
    },
  });
  input.focus();
}

function openShortcutHelp() {
  const shortcutsOn = prefs.get().shortcuts;
  const toggle = el('input', {
    type: 'checkbox',
    id: 'shortcut-toggle',
    checked: shortcutsOn,
    onchange: () => prefs.update({ shortcuts: toggle.checked }),
  });
  const rows = [
    ['Ctrl+K / ⌘K', 'Open or close Commands'],
    ['/', 'Focus task search'],
    ['?', 'Show this help'],
    ['Escape', 'Close the topmost dialog or popover'],
  ];
  openDialog({
    title: 'Keyboard shortcuts',
    body: el(
      'div',
      {},
      el(
        'table',
        { class: 'shortcut-table' },
        el('caption', { class: 'visually-hidden' }, 'Keyboard shortcuts'),
        el('thead', {}, el('tr', {}, el('th', { scope: 'col' }, 'Keys'), el('th', { scope: 'col' }, 'Action'))),
        el('tbody', {}, ...rows.map(([keys, action]) => el('tr', {}, el('td', {}, el('kbd', {}, keys)), el('td', {}, action)))),
      ),
      el('p', {}, el('label', { for: 'shortcut-toggle' }, toggle, ' Enable single-character shortcuts (/ and ?)')),
      el('p', { class: 'muted' }, 'Every action is also available with Tab, Enter and Space.'),
      el(
        'p',
        {},
        el(
          'button',
          {
            type: 'button',
            class: 'button secondary',
            onclick: () => {
              prefs.clear();
              applyPrefs();
              toast('Saved preferences cleared');
            },
          },
          'Clear saved preferences',
        ),
      ),
    ),
  });
}

/** Escape closes the popover holding focus (else the last open one); it never clears search. */
function closePopover(event) {
  const open = [...document.querySelectorAll('details.popover[open]')];
  const target = open.find((details) => details.contains(document.activeElement)) ?? open.at(-1);
  if (target) {
    event.preventDefault();
    target.open = false;
    target.querySelector('summary')?.focus();
  } else if (event.target instanceof HTMLInputElement && event.target.type === 'search') {
    event.preventDefault();
  }
}

function isEditable(target) {
  return target instanceof HTMLElement && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName));
}

document.addEventListener('keydown', (event) => {
  if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === 'k') {
    event.preventDefault();
    openCommands();
    return;
  }
  if (event.ctrlKey || event.metaKey || event.altKey || event.defaultPrevented) return;
  if (event.key === 'Escape' && !document.querySelector('dialog[open]')) {
    closePopover(event);
    return;
  }
  if (!prefs.get().shortcuts || isEditable(event.target) || document.querySelector('dialog[open]')) return;
  if (event.key === '/') {
    if (state.view?.focusSearch) {
      event.preventDefault();
      state.view.focusSearch();
    }
  } else if (event.key === '?') {
    event.preventDefault();
    openShortcutHelp();
  }
});

document.addEventListener('click', (event) => {
  if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const link = event.target.closest?.('a[data-nav]');
  if (!link || link.target) return;
  const url = new URL(link.href);
  if (url.origin !== location.origin) return;
  event.preventDefault();
  navigate(url.pathname + url.search);
});

/** Remember the path+query the current view shows; views report their own URL rewrites. */
function trackLocation() {
  state.currentLocation = location.pathname + location.search;
}

window.addEventListener('popstate', () => {
  // A fragment-only change (in-page anchor) keeps the view; any path or query change routes.
  if (location.pathname + location.search !== state.currentLocation) render();
});

// ---------------------------------------------------------------- start

buildShell();
applyPrefs();
renderHealthIndicator();
every(HEALTH_INTERVAL_MS, refreshHealth, { scoped: false });
refreshHealth();

async function start() {
  state.ready = false;
  render();
  try {
    await restoreSession();
  } catch (error) {
    endSession();
    // A throttled or unreachable server is reported as such, not as a sign-in problem.
    state.connectionNotice = { kind: 'error', text: error instanceof McpError ? errorMessage(error) : 'Sign-in status could not be checked.' };
  }
  state.ready = true;
  render();
}
start();
