// In-memory task cache: progressive export scans, direct lookups and staged refreshes.

import { decodeExportPage, normalizeTodo, projectFilter, reverseIndex } from './data.js';
import { McpError } from './mcp.js';

export const DEFAULT_LIMITS = {
  pageSize: 500,
  autoRecords: 10000,
  autoBytes: 16 * 1024 * 1024,
  cacheBytes: 32 * 1024 * 1024,
};

function scanState(scope) {
  return {
    scope,
    status: 'idle',
    records: 0,
    malformed: 0,
    duplicates: 0,
    bytes: 0,
    offset: 0,
    failedPages: 0,
    error: null,
    seen: new Set(),
    startedAt: null,
    finishedAt: null,
  };
}

export function createTaskStore({ callTool, limits = {}, onChange = () => {}, now = () => Date.now() }) {
  const L = { ...DEFAULT_LIMITS, ...limits };
  let seq = 0;
  // `generation` invalidates scans on scope switches; `session` invalidates everything on clear().
  let generation = 0;
  let session = 0;
  let fetchedAt = null;
  let snapshot = new Map();
  let direct = new Map();
  let missingAt = new Map();
  let scan = scanState(null);
  let staged = null;
  let running = null;
  let refreshError = null;
  let tasksCache = null;
  let reverseCache = null;

  const notify = () => {
    tasksCache = null;
    reverseCache = null;
    onChange();
  };

  /** Prefer whichever source fetched the task most recently. */
  function merge(map, task, stamp) {
    const current = map.get(task.id) ?? direct.get(task.id);
    if ((missingAt.get(task.id) ?? -1) > stamp) return;
    if (current && current.stamp > stamp) {
      map.set(task.id, current);
      direct.delete(task.id);
      return;
    }
    map.set(task.id, { task, stamp, fetchedAt: now() });
  }

  async function runPages(target, mode) {
    const { state, map, gen, controller } = target;
    state.status = 'loading';
    state.error = null;
    if (target.live) notify();
    for (;;) {
      const stamp = ++seq;
      const args = { collection: 'todos', limit: L.pageSize, offset: state.offset };
      if (state.scope !== null) args.filter = projectFilter(state.scope);
      let page;
      try {
        const payload = await callTool('export_collection', args, { signal: controller.signal });
        if (gen !== generation || target.stopped) return;
        try {
          page = decodeExportPage(payload);
        } catch {
          throw new McpError('protocol', 'Server response could not be read.');
        }
      } catch (error) {
        if (gen !== generation) return;
        if (target.stopped) {
          state.status = 'stopped';
        } else {
          state.status = 'error';
          state.error = error instanceof McpError ? error : new McpError('protocol', 'Server response could not be read.');
          state.failedPages += 1;
        }
        if (target.live) notify();
        return;
      }
      for (const task of page.tasks) {
        if (state.seen.has(task.id)) state.duplicates += 1;
        state.seen.add(task.id);
        merge(map, task, stamp);
      }
      state.records += page.count;
      state.offset += page.count;
      state.malformed += page.malformed;
      state.bytes += page.bytes;
      state.finishedAt = now();
      if (page.count < L.pageSize) state.status = 'complete';
      else if (state.bytes >= L.cacheBytes) state.status = 'budget';
      else if (mode === 'one' || state.records >= L.autoRecords || state.bytes >= L.autoBytes) state.status = 'paused';
      if (target.live && state.status !== 'loading') fetchedAt = now();
      if (target.live) notify();
      if (state.status !== 'loading') return;
    }
  }

  function launch(target, mode) {
    const promise = runPages(target, mode).finally(() => {
      if (running === target) running = null;
    });
    target.promise = promise;
    running = target;
    return promise;
  }

  function newTarget(kind, state, map, live) {
    return { kind, state, map, live, gen: generation, controller: new AbortController(), stopped: false };
  }

  function evict() {
    generation += 1;
    running?.controller.abort();
    running = null;
    snapshot = new Map();
    staged = null;
    refreshError = null;
  }

  function covers(scope) {
    return scan.status !== 'idle' && (scan.scope === scope || (scan.scope === null && scan.status === 'complete'));
  }

  function startScan(scope) {
    if (covers(scope)) return running?.promise ?? Promise.resolve();
    evict();
    scan = scanState(scope);
    scan.startedAt = now();
    return launch(newTarget('initial', scan, snapshot, true), 'auto');
  }

  function continueScan() {
    if (running) return running.promise;
    if (!['paused', 'error', 'stopped'].includes(scan.status)) return Promise.resolve();
    const beyondAuto = scan.records >= L.autoRecords || scan.bytes >= L.autoBytes;
    return launch(newTarget('initial', scan, snapshot, true), beyondAuto ? 'one' : 'auto');
  }

  function stopScan() {
    if (!running) return;
    running.stopped = true;
    running.controller.abort();
    if (running.live) {
      scan.status = 'stopped';
      notify();
    }
  }

  function sameTask(a, b) {
    return (
      a.title === b.title &&
      a.description === b.description &&
      a.project === b.project &&
      a.priority === b.priority &&
      a.status === b.status &&
      a.owner === b.owner &&
      a.createdAt === b.createdAt &&
      a.completedAt === b.completedAt &&
      a.blockedBy.length === b.blockedBy.length &&
      a.blockedBy.every((id, index) => id === b.blockedBy[index])
    );
  }

  /** True when applying `map` (a complete or partial replacement) would change what is shown. */
  function differs(map, complete) {
    if (complete && map.size !== snapshot.size) return true;
    for (const [id, entry] of map) {
      const current = snapshot.get(id);
      if (!current || !sameTask(current.task, entry.task)) return true;
    }
    return false;
  }

  /** Adopt a capped replacement's coverage so Continue resumes from where it stopped. */
  function adoptPartialCoverage(state) {
    if (scan.status === 'complete' || state.records >= scan.records) scan = state;
  }

  async function refresh() {
    if (running) return running.promise;
    if (scan.status === 'idle') return startScan(scan.scope);
    const state = scanState(scan.scope);
    state.startedAt = now();
    const target = newTarget('refresh', state, new Map(), false);
    await launch(target, 'auto');
    if (target.gen !== generation) return;
    if (state.status === 'complete' || state.status === 'paused' || state.status === 'budget') {
      refreshError = null;
      fetchedAt = now();
      if (differs(target.map, state.status === 'complete')) staged = target;
      else if (state.status === 'complete') scan = state;
      else adoptPartialCoverage(state);
    } else if (state.status === 'error') {
      refreshError = state.error;
    }
    notify();
  }

  function applyStaged() {
    if (!staged) return;
    const { state, map } = staged;
    staged = null;
    if (state.status === 'complete') {
      for (const [id, entry] of snapshot) {
        const replacement = map.get(id);
        if (replacement && entry.stamp > replacement.stamp) map.set(id, entry);
      }
      snapshot = map;
      scan = state;
    } else {
      // Partial replacement: update loaded rows but never drop rows it did not reach.
      for (const [id, entry] of map) {
        const current = snapshot.get(id);
        if (!current || current.stamp < entry.stamp) snapshot.set(id, entry);
      }
      adoptPartialCoverage(state);
    }
    notify();
  }

  async function fetchTask(id, { signal } = {}) {
    const stamp = ++seq;
    const started = session;
    const result = await callTool('get_todo', { todo_id: id }, { signal, priority: 'high' });
    if (started !== session) throw new McpError('aborted', 'The request was superseded.');
    if (result && typeof result === 'object' && result.error === 'todo not found') {
      missingAt.set(id, stamp);
      const removed = snapshot.delete(id) || direct.delete(id);
      if (removed) notify();
      return { status: 'missing' };
    }
    const normalized = normalizeTodo(result);
    if (!normalized.ok) throw new McpError('protocol', 'Server response could not be read.');
    const { task } = normalized;
    const entry = { task, stamp, fetchedAt: now() };
    missingAt.delete(task.id);
    if (snapshot.has(task.id)) snapshot.set(task.id, entry);
    else direct.set(task.id, entry);
    notify();
    return { status: 'found', task, fetchedAt: entry.fetchedAt };
  }

  function entry(id) {
    const a = snapshot.get(id);
    const b = direct.get(id);
    if (a && b) return a.stamp >= b.stamp ? a : b;
    return a ?? b ?? null;
  }

  function tasks() {
    if (!tasksCache) tasksCache = [...snapshot.values()].map((value) => value.task);
    return tasksCache;
  }

  function blockedBy(id) {
    if (!reverseCache) reverseCache = reverseIndex(tasks());
    return reverseCache.get(id) ?? [];
  }

  function coverage() {
    return {
      scope: scan.scope,
      status: scan.status,
      partial: scan.status !== 'complete',
      records: scan.records,
      unique: snapshot.size,
      malformed: scan.malformed,
      duplicates: scan.duplicates,
      failedPages: scan.failedPages,
      bytes: scan.bytes,
      error: scan.error,
      refreshError,
      startedAt: scan.startedAt,
      finishedAt: scan.finishedAt,
      fetchedAt,
      refreshing: running?.kind === 'refresh',
      loading: running?.kind === 'initial',
    };
  }

  function clear() {
    session += 1;
    fetchedAt = null;
    evict();
    direct = new Map();
    missingAt = new Map();
    scan = scanState(null);
    notify();
  }

  return {
    startScan,
    continueScan,
    stopScan,
    refresh,
    applyStaged,
    stagedReady: () => staged !== null,
    fetchTask,
    get: (id) => entry(id)?.task ?? null,
    fetchedAt: (id) => entry(id)?.fetchedAt ?? null,
    tasks,
    blockedBy,
    coverage,
    clear,
    limits: L,
  };
}
