import { test } from 'node:test';
import assert from 'node:assert/strict';
import { errorMessage, fieldId } from '../static/components.js';

test('fieldId gives each option value its own id', () => {
  const ids = ['pending', 'in_progress', 'done', 'weird value', "o'brien"].map((value) => fieldId('status', value));
  assert.equal(new Set(ids).size, ids.length);
  assert.equal(fieldId('status', 'done'), 'status-done');
  for (const id of ids) assert.doesNotMatch(id, /\s|object/);
  assert.throws(() => fieldId('status', { value: 'done' }), TypeError);
});

test('unavailable errors only say "Refresh failed" when refreshing existing data', () => {
  const error = { category: 'unavailable' };
  assert.doesNotMatch(errorMessage(error), /Refresh failed/);
  assert.match(errorMessage(error, { refresh: true }), /^Refresh failed/);
});
