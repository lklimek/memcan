import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createPrefs, defaultPrefs } from '../static/prefs.js';

function memoryStorage(initial = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (k) => (data.has(k) ? data.get(k) : null),
    setItem: (k, v) => data.set(k, String(v)),
    removeItem: (k) => data.delete(k),
  };
}

test('defaults when storage is empty, corrupt or another version', () => {
  assert.deepEqual(createPrefs(memoryStorage()).get(), defaultPrefs());
  assert.deepEqual(createPrefs(memoryStorage({ 'memcan.ui.prefs': '{bad' })).get(), defaultPrefs());
  assert.deepEqual(
    createPrefs(memoryStorage({ 'memcan.ui.prefs': JSON.stringify({ version: 99, theme: 'dark' }) })).get(),
    defaultPrefs(),
  );
});

test('invalid fields are dropped individually', () => {
  const stored = {
    version: 1,
    theme: 'neon',
    density: 'compact',
    shortcuts: 'yes',
    views: [{ name: 'A', query: 'status_mode=all' }, { name: 'A', query: 'dup' }, { name: 5 }, null],
    defaultView: 'missing',
  };
  const prefs = createPrefs(memoryStorage({ 'memcan.ui.prefs': JSON.stringify(stored) })).get();
  assert.equal(prefs.theme, 'system');
  assert.equal(prefs.density, 'compact');
  assert.equal(prefs.shortcuts, true);
  assert.deepEqual(prefs.views, [{ name: 'A', query: 'status_mode=all' }]);
  assert.equal(prefs.defaultView, null);
});

test('updates persist, views can be saved, renamed, defaulted and deleted', () => {
  const storage = memoryStorage();
  const prefs = createPrefs(storage);
  prefs.update({ theme: 'dark' });
  assert.ok(prefs.saveView('  Mine  ', 'owner=me'));
  prefs.update({ defaultView: 'Mine' });
  assert.ok(prefs.renameView('Mine', 'Agent A'));
  assert.equal(prefs.get().defaultView, 'Agent A');
  const reloaded = createPrefs(storage).get();
  assert.equal(reloaded.theme, 'dark');
  assert.deepEqual(reloaded.views, [{ name: 'Agent A', query: 'owner=me' }]);
  prefs.deleteView('Agent A');
  assert.equal(prefs.get().defaultView, null);
  prefs.clear();
  assert.equal(storage.data.size, 0);
});

test('stored prefs never contain anything but the documented fields', () => {
  const storage = memoryStorage();
  createPrefs(storage).update({ theme: 'light', apiKey: 'secret', session: 's' });
  assert.deepEqual(Object.keys(JSON.parse(storage.data.get('memcan.ui.prefs'))).sort(), [
    'defaultView',
    'density',
    'shortcuts',
    'theme',
    'version',
    'views',
  ]);
});

test('storage failures degrade to tab-only preferences', () => {
  const throwing = {
    getItem: () => {
      throw new Error('denied');
    },
    setItem: () => {
      throw new Error('quota');
    },
    removeItem: () => {},
  };
  const prefs = createPrefs(throwing);
  prefs.update({ theme: 'dark' });
  assert.equal(prefs.get().theme, 'dark');
  assert.equal(prefs.available, false);
  assert.equal(createPrefs(null).available, false);
});
