// Device-local preferences: theme, density, shortcuts and saved task views.
// Never stores credentials, sessions or fetched records.

const STORAGE_KEY = 'memcan.ui.prefs';
const VERSION = 1;
const THEMES = ['system', 'light', 'dark'];
const DENSITIES = ['comfortable', 'compact'];
export const BUILT_IN_VIEWS = ['open', 'high', 'blocked', 'unassigned', 'all'];
const MAX_VIEWS = 50;

export function defaultPrefs() {
  return { theme: 'system', density: 'comfortable', shortcuts: true, views: [], defaultView: null };
}

function validate(raw) {
  const prefs = defaultPrefs();
  if (!raw || typeof raw !== 'object' || raw.version !== VERSION) return prefs;
  if (THEMES.includes(raw.theme)) prefs.theme = raw.theme;
  if (DENSITIES.includes(raw.density)) prefs.density = raw.density;
  if (typeof raw.shortcuts === 'boolean') prefs.shortcuts = raw.shortcuts;
  if (Array.isArray(raw.views)) {
    const names = new Set();
    for (const view of raw.views.slice(0, MAX_VIEWS)) {
      if (!view || typeof view.name !== 'string' || typeof view.query !== 'string') continue;
      const name = view.name.trim().slice(0, 80);
      if (!name || names.has(name) || view.query.length > 4000) continue;
      names.add(name);
      prefs.views.push({ name, query: view.query });
    }
  }
  const def = raw.defaultView;
  if (typeof def === 'string' && (BUILT_IN_VIEWS.includes(def) || prefs.views.some((v) => v.name === def))) {
    prefs.defaultView = def;
  }
  return prefs;
}

/** `storage` is a Web Storage object or null; quota/private-mode errors degrade to tab-only prefs. */
export function createPrefs(storage) {
  let available = Boolean(storage);
  let current = defaultPrefs();
  if (available) {
    try {
      const text = storage.getItem(STORAGE_KEY);
      if (text) current = validate(JSON.parse(text));
    } catch {
      current = defaultPrefs();
    }
  }
  const save = () => {
    if (!available) return;
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify({ version: VERSION, ...current }));
    } catch {
      available = false;
    }
  };
  return {
    get: () => structuredClone(current),
    get available() {
      return available;
    },
    update(patch) {
      current = validate({ version: VERSION, ...current, ...patch });
      save();
      return structuredClone(current);
    },
    saveView(name, query) {
      const trimmed = name.trim().slice(0, 80);
      if (!trimmed) return false;
      const views = current.views.filter((view) => view.name !== trimmed);
      views.push({ name: trimmed, query });
      this.update({ views });
      return true;
    },
    renameView(from, to) {
      const trimmed = to.trim().slice(0, 80);
      if (!trimmed || current.views.some((view) => view.name === trimmed)) return false;
      const views = current.views.map((view) => (view.name === from ? { ...view, name: trimmed } : view));
      this.update({ views, defaultView: current.defaultView === from ? trimmed : current.defaultView });
      return true;
    },
    deleteView(name) {
      this.update({
        views: current.views.filter((view) => view.name !== name),
        defaultView: current.defaultView === name ? null : current.defaultView,
      });
    },
    clear() {
      current = defaultPrefs();
      if (!available) return;
      try {
        storage.removeItem(STORAGE_KEY);
      } catch {
        available = false;
      }
    },
  };
}
