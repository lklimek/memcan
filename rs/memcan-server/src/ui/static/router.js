// URL contract for the task UI: routes, task query state and safe return targets.

export const OPEN_STATUSES = ['pending', 'in_progress', 'blocked', 'postponed'];
const STATUS_MODES = ['open', 'all', 'selected'];
const GROUPS = ['none', 'project', 'status'];
const SORTS = ['priority', 'created', 'project', 'status'];
const DIRS = ['asc', 'desc'];
const SIZES = [25, 50, 100];

export function defaultTaskQuery() {
  return {
    project: null,
    statusMode: 'open',
    statuses: [],
    priorities: [],
    owner: null,
    unassigned: false,
    q: '',
    group: 'none',
    sort: 'priority',
    dir: 'asc',
    page: 1,
    size: 50,
  };
}

export function defaultDir(sort) {
  return sort === 'created' ? 'desc' : 'asc';
}

export function parseRoute(pathname) {
  const path = pathname.replace(/\/+$/, '');
  if (path === '/ui') return { name: 'root' };
  if (path === '/ui/tasks') return { name: 'tasks' };
  if (path === '/ui/dashboard') return { name: 'dashboard' };
  if (path === '/ui/login') return { name: 'login' };
  const match = /^\/ui\/tasks\/([^/]+)$/.exec(path);
  if (match) {
    try {
      return { name: 'task', id: decodeURIComponent(match[1]) };
    } catch {
      return { name: 'notfound' };
    }
  }
  return { name: 'notfound' };
}

export function taskPath(id) {
  return `/ui/tasks/${encodeURIComponent(id)}`;
}

function uniq(values) {
  return [...new Set(values.filter((value) => value !== ''))];
}

export function parseTaskQuery(params) {
  const state = defaultTaskQuery();
  const notices = [];
  const scalar = (key, label) => {
    const values = params.getAll(key);
    if (values.length > 1) {
      notices.push(`${label} was specified more than once and was reset.`);
      return undefined;
    }
    return values.length ? values[0] : undefined;
  };
  const choice = (key, label, allowed) => {
    const value = scalar(key, label);
    if (value === undefined) return undefined;
    if (!allowed.includes(value)) {
      notices.push(`${label} value was not recognized and was reset.`);
      return undefined;
    }
    return value;
  };

  const project = scalar('project', 'Project');
  if (project) state.project = project;

  const statuses = uniq(params.getAll('status'));
  const marker = params.getAll('status_filter_present').includes('1');
  const mode = choice('status_mode', 'Status mode', STATUS_MODES);
  if (mode) state.statusMode = mode;
  else if (statuses.length || marker) state.statusMode = 'selected';
  state.statuses = state.statusMode === 'selected' ? statuses : [];

  state.priorities = uniq(params.getAll('priority'));

  const owner = scalar('owner', 'Owner');
  const unassigned = params.getAll('unassigned').includes('1');
  if (owner && unassigned) {
    notices.push('Owner and Unassigned cannot be combined; both were reset.');
  } else {
    if (owner) state.owner = owner;
    state.unassigned = unassigned;
  }

  state.q = scalar('q', 'Search') ?? '';
  state.group = choice('group', 'Group', GROUPS) ?? 'none';
  state.sort = choice('sort', 'Sort', SORTS) ?? 'priority';
  state.dir = choice('dir', 'Sort direction', DIRS) ?? defaultDir(state.sort);

  const page = scalar('page', 'Page');
  if (page !== undefined) {
    if (/^[1-9]\d{0,5}$/.test(page)) state.page = Number(page);
    else notices.push('Page value was not recognized and was reset.');
  }
  const size = scalar('size', 'Rows per page');
  if (size !== undefined) {
    if (SIZES.includes(Number(size)) && /^\d+$/.test(size)) state.size = Number(size);
    else notices.push('Rows per page value was not recognized and was reset.');
  }
  return { state, notices };
}

export function serializeTaskQuery(state) {
  const params = new URLSearchParams();
  if (state.project) params.set('project', state.project);
  params.set('status_mode', state.statusMode);
  if (state.statusMode === 'selected') {
    for (const status of state.statuses) params.append('status', status);
    params.set('status_filter_present', '1');
  }
  for (const priority of state.priorities) params.append('priority', priority);
  if (state.owner) params.set('owner', state.owner);
  if (state.unassigned) params.set('unassigned', '1');
  if (state.q) params.set('q', state.q);
  if (state.group !== 'none') params.set('group', state.group);
  if (state.sort !== 'priority') params.set('sort', state.sort);
  if (state.dir !== defaultDir(state.sort)) params.set('dir', state.dir);
  if (state.page !== 1) params.set('page', String(state.page));
  if (state.size !== 50) params.set('size', String(state.size));
  return params;
}

export function tasksUrl(state) {
  return `/ui/tasks?${serializeTaskQuery(state)}`;
}

/** Built-in views keep the project and reset every other filter. */
export function builtInView(name, current) {
  const next = { ...defaultTaskQuery(), project: current.project };
  switch (name) {
    case 'high':
      next.priorities = ['high'];
      break;
    case 'blocked':
      next.statusMode = 'selected';
      next.statuses = ['blocked'];
      break;
    case 'unassigned':
      next.unassigned = true;
      break;
    case 'all':
      next.statusMode = 'all';
      break;
    default:
      break;
  }
  return next;
}

/** Accept only a same-origin path to a known UI route (never the sign-in page itself). */
export function safeReturnTo(value) {
  if (typeof value !== 'string' || !value.startsWith('/ui/') || value.includes('\\')) return null;
  let url;
  try {
    url = new URL(value, 'http://memcan.invalid');
  } catch {
    return null;
  }
  if (url.origin !== 'http://memcan.invalid') return null;
  const target = url.pathname + url.search;
  if (target !== value) return null;
  const route = parseRoute(url.pathname);
  if (!['tasks', 'task', 'dashboard'].includes(route.name)) return null;
  return target;
}
