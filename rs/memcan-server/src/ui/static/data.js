// Task data normalization and local filter/sort/group/paginate rules.

export const STATUS_ORDER = ['pending', 'in_progress', 'blocked', 'postponed', 'done', 'cancelled'];
export const PRIORITY_ORDER = ['high', 'medium', 'low'];
const TERMINAL = new Set(['done', 'cancelled']);
const STATUS_LABELS = new Map([
  ['pending', 'Pending'],
  ['in_progress', 'In progress'],
  ['blocked', 'Blocked'],
  ['postponed', 'Postponed'],
  ['done', 'Done'],
  ['cancelled', 'Cancelled'],
]);
const PRIORITY_LABELS = new Map([
  ['high', 'High'],
  ['medium', 'Medium'],
  ['low', 'Low'],
]);

const unknownLabel = (value) => `Unknown: ${value === '' ? '(empty)' : value}`;
export const statusLabel = (value) => STATUS_LABELS.get(value) ?? unknownLabel(value);
export const priorityLabel = (value) => PRIORITY_LABELS.get(value) ?? unknownLabel(value);
export const isKnownStatus = (value) => STATUS_LABELS.has(value);
export const isKnownPriority = (value) => PRIORITY_LABELS.has(value);
export const isOpenStatus = (value) => !TERMINAL.has(value);

function text(value) {
  if (typeof value === 'string') return value;
  if (value === null || value === undefined) return null;
  return JSON.stringify(value);
}

/** Map a TodoItem or a flattened export row into the UI task shape. */
export function normalizeTodo(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return { ok: false, reason: 'not an object' };
  const id = raw.id;
  if (typeof id !== 'string' || id === '') return { ok: false, reason: 'missing id' };
  const owner = text(raw.owner);
  const description = text(raw.description);
  return {
    ok: true,
    task: {
      id,
      title: text(raw.title) ?? '',
      description: description === '' ? null : description,
      project: text(raw.project) ?? '',
      priority: text(raw.priority) ?? '',
      status: text(raw.status) ?? '',
      owner: owner === null || owner.trim() === '' ? null : owner,
      blockedBy: Array.isArray(raw.blocked_by) ? raw.blocked_by.filter((v) => typeof v === 'string' && v !== '') : [],
      createdAt: text(raw.created_at) ?? '',
      completedAt: text(raw.completed_at),
    },
  };
}

const encoder = new TextEncoder();

/** Decode one `export_collection` payload `{count, offset, data}` with JSONL rows. */
export function decodeExportPage(payload) {
  if (!payload || typeof payload !== 'object' || !Number.isInteger(payload.count) || payload.count < 0) {
    throw new TypeError('export response has no valid count');
  }
  if (typeof payload.data !== 'string') throw new TypeError('export response has no data');
  const lines = payload.data === '' ? [] : payload.data.split('\n');
  if (lines.length !== payload.count) {
    throw new TypeError(`export count ${payload.count} disagrees with ${lines.length} rows`);
  }
  const tasks = [];
  let malformed = 0;
  for (const line of lines) {
    let row;
    try {
      row = JSON.parse(line);
    } catch {
      malformed += 1;
      continue;
    }
    const result = normalizeTodo(row);
    if (result.ok) tasks.push(result.task);
    else malformed += 1;
  }
  return { count: payload.count, tasks, malformed, bytes: encoder.encode(payload.data).length };
}

/** SQL string literal matching core `sanitize_eq` (apostrophes doubled). */
export function sqlString(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

export function projectFilter(project) {
  return `project = ${sqlString(project)}`;
}

export function searchTokens(q) {
  return q.toLowerCase().split(/\s+/).filter(Boolean);
}

/** Return null when the task does not match, else whether a token matched only in the description. */
export function searchMatch(task, tokens) {
  if (!tokens.length) return { descriptionOnly: false };
  const visible = [task.title, task.project, task.owner ?? '', task.id].join('\n').toLowerCase();
  const description = (task.description ?? '').toLowerCase();
  let descriptionOnly = false;
  for (const token of tokens) {
    if (visible.includes(token)) continue;
    if (description.includes(token)) descriptionOnly = true;
    else return null;
  }
  return { descriptionOnly };
}

export function matchesFilters(task, state) {
  if (state.project && task.project !== state.project) return false;
  if (state.statusMode === 'open' && !isOpenStatus(task.status)) return false;
  if (state.statusMode === 'selected' && !state.statuses.includes(task.status)) return false;
  if (state.priorities.length && !state.priorities.includes(task.priority)) return false;
  if (state.owner && task.owner !== state.owner) return false;
  if (state.unassigned && task.owner !== null) return false;
  return true;
}

export function filterTasks(tasks, state) {
  const tokens = searchTokens(state.q);
  const out = [];
  for (const task of tasks) {
    if (matchesFilters(task, state) && searchMatch(task, tokens)) out.push(task);
  }
  return out;
}

const rank = (order, value) => {
  const index = order.indexOf(value);
  return index === -1 ? order.length : index;
};

export function parseDate(value) {
  if (typeof value !== 'string' || value === '') return null;
  const ms = Date.parse(value);
  return Number.isNaN(ms) ? null : ms;
}

function compareDates(a, b, dir) {
  const left = parseDate(a);
  const right = parseDate(b);
  if (left === null || right === null) {
    if (left === right) return 0;
    return left === null ? 1 : -1;
  }
  return dir === 'desc' ? right - left : left - right;
}

const compareText = (a, b) => {
  const left = a.toLowerCase();
  const right = b.toLowerCase();
  if (left !== right) return left < right ? -1 : 1;
  return a < b ? -1 : a > b ? 1 : 0;
};

const tieBreak = (a, b) => compareDates(a.createdAt, b.createdAt, 'asc') || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

export function compareTasks(sort, dir) {
  const sign = dir === 'desc' ? -1 : 1;
  switch (sort) {
    case 'created':
      return (a, b) => compareDates(a.createdAt, b.createdAt, dir) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    case 'project':
      return (a, b) => sign * compareText(a.project, b.project) || tieBreak(a, b);
    case 'status':
      return (a, b) => sign * (rank(STATUS_ORDER, a.status) - rank(STATUS_ORDER, b.status)) || tieBreak(a, b);
    default:
      return (a, b) => sign * (rank(PRIORITY_ORDER, a.priority) - rank(PRIORITY_ORDER, b.priority)) || tieBreak(a, b);
  }
}

function groupKey(task, group) {
  return group === 'project' ? task.project : task.status;
}

/** Order groups, sort rows within groups, then paginate the flattened set. */
export function pageTasks(tasks, state) {
  const compare = compareTasks(state.sort, state.dir);
  let ordered;
  const counts = new Map();
  if (state.group === 'none') {
    ordered = [...tasks].sort(compare);
  } else {
    const groups = new Map();
    for (const task of tasks) {
      const key = groupKey(task, state.group);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(task);
    }
    const keys = [...groups.keys()].sort(
      state.group === 'project'
        ? compareText
        : (a, b) => rank(STATUS_ORDER, a) - rank(STATUS_ORDER, b) || compareText(a, b),
    );
    ordered = [];
    for (const key of keys) {
      const rows = groups.get(key).sort(compare);
      counts.set(key, rows.length);
      ordered.push(...rows);
    }
  }
  const total = ordered.length;
  const pageCount = Math.max(1, Math.ceil(total / state.size));
  const page = Math.min(state.page, pageCount);
  const startIndex = (page - 1) * state.size;
  const slice = ordered.slice(startIndex, startIndex + state.size);
  const groups = [];
  if (state.group === 'none') {
    if (slice.length) groups.push({ key: null, count: total, continued: false, rows: slice });
  } else {
    let previousKey = startIndex > 0 ? groupKey(ordered[startIndex - 1], state.group) : undefined;
    for (const task of slice) {
      const key = groupKey(task, state.group);
      const last = groups[groups.length - 1];
      if (last && last.key === key) last.rows.push(task);
      else groups.push({ key, count: counts.get(key), continued: previousKey === key, rows: [task] });
      previousKey = undefined;
    }
  }
  return {
    groups,
    total,
    page,
    pageCount,
    clamped: page !== state.page,
    start: total ? startIndex + 1 : 0,
    end: startIndex + slice.length,
  };
}

/** Map each blocker ID to the IDs of loaded tasks that list it in `blocked_by`. */
export function reverseIndex(tasks) {
  const index = new Map();
  for (const task of tasks) {
    for (const blocker of task.blockedBy) {
      if (!index.has(blocker)) index.set(blocker, []);
      const list = index.get(blocker);
      if (!list.includes(task.id)) list.push(task.id);
    }
  }
  for (const list of index.values()) list.sort();
  return index;
}

export function overview(tasks) {
  const result = { open: 0, highOpen: 0, blocked: 0, unassignedOpen: 0, projects: [] };
  const projects = new Map();
  for (const task of tasks) {
    if (!isOpenStatus(task.status)) continue;
    const high = task.priority === 'high';
    const blocked = task.status === 'blocked';
    result.open += 1;
    if (high) result.highOpen += 1;
    if (blocked) result.blocked += 1;
    if (task.owner === null) result.unassignedOpen += 1;
    if (!projects.has(task.project)) projects.set(task.project, { project: task.project, open: 0, high: 0, blocked: 0 });
    const entry = projects.get(task.project);
    entry.open += 1;
    if (high) entry.high += 1;
    if (blocked) entry.blocked += 1;
  }
  result.projects = [...projects.values()].sort(
    (a, b) => b.high - a.high || b.blocked - a.blocked || compareText(a.project, b.project),
  );
  return result;
}

export function vocabulary(tasks) {
  const projects = new Set();
  const owners = new Set();
  for (const task of tasks) {
    if (task.project) projects.add(task.project);
    if (task.owner) owners.add(task.owner);
  }
  return { projects: [...projects].sort(compareText), owners: [...owners].sort(compareText) };
}

export function formatAge(value, now = Date.now()) {
  const ms = parseDate(value);
  if (ms === null) return null;
  const seconds = Math.max(0, (now - ms) / 1000);
  if (seconds < 60) return 'just now';
  const minutes = seconds / 60;
  if (minutes < 60) return `${Math.floor(minutes)}m`;
  const hours = minutes / 60;
  if (hours < 24) return `${Math.floor(hours)}h`;
  const days = hours / 24;
  if (days < 14) return `${Math.floor(days)}d`;
  if (days < 60) return `${Math.floor(days / 7)}w`;
  if (days < 365) return `${Math.floor(days / 30)}mo`;
  return `${Math.floor(days / 365)}y`;
}

/** Split text into marked/unmarked segments for literal case-folded tokens. */
export function highlightSegments(value, tokens) {
  const lower = value.toLowerCase();
  const ranges = [];
  for (const token of tokens) {
    if (!token) continue;
    let from = 0;
    for (;;) {
      const at = lower.indexOf(token, from);
      if (at === -1) break;
      ranges.push([at, at + token.length]);
      from = at + 1;
    }
  }
  if (!ranges.length || lower.length !== value.length) return [{ text: value, mark: false }];
  ranges.sort((a, b) => a[0] - b[0]);
  const merged = [];
  for (const range of ranges) {
    const last = merged[merged.length - 1];
    if (last && range[0] <= last[1]) last[1] = Math.max(last[1], range[1]);
    else merged.push([...range]);
  }
  const segments = [];
  let cursor = 0;
  for (const [start, end] of merged) {
    if (start > cursor) segments.push({ text: value.slice(cursor, start), mark: false });
    segments.push({ text: value.slice(start, end), mark: true });
    cursor = end;
  }
  if (cursor < value.length) segments.push({ text: value.slice(cursor), mark: false });
  return segments;
}
