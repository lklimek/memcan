import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createAuth, sessionAuth } from '../static/auth.js';
import { McpError } from '../static/mcp.js';

function json(body, status = 200, headers = {}) {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', ...headers } });
}

function recorder(reply) {
  const calls = [];
  const fetch = async (url, init) => {
    calls.push({ url, init, headers: new Headers(init.headers) });
    return reply(url, init);
  };
  return { calls, auth: createAuth({ fetch }) };
}

test('session bootstrap is a same-origin GET that never sends credentials of its own', async () => {
  const { calls, auth } = recorder(() => json({ authenticated: false, insecureHttpAllowed: true }));
  assert.deepEqual(await auth.session(), { authenticated: false, insecureHttpAllowed: true });
  const [call] = calls;
  assert.equal(call.url, '/ui/auth/session');
  assert.equal(call.init.method, 'GET');
  assert.equal(call.init.credentials, 'same-origin');
  assert.equal(call.init.redirect, 'error');
  assert.equal(call.init.cache, 'no-store');
  assert.equal(call.headers.get('authorization'), null);
  assert.equal(call.headers.get('x-csrf-token'), null);
});

test('an authenticated bootstrap must carry a CSRF token', async () => {
  const ok = recorder(() => json({ authenticated: true, username: 'u', role: 'viewer', method: 'basic', csrfToken: 't' }));
  assert.equal((await ok.auth.session()).method, 'basic');
  const bad = recorder(() => json({ authenticated: true }));
  await assert.rejects(bad.auth.session(), (e) => e instanceof McpError && e.category === 'protocol');
  const garbage = recorder(() => new Response('<html>', { headers: { 'content-type': 'text/html' } }));
  await assert.rejects(garbage.auth.session(), (e) => e.category === 'protocol');
});

test('login posts JSON credentials without a CSRF token or Authorization header', async () => {
  const { calls, auth } = recorder(() => json({ authenticated: true, csrfToken: 't' }));
  await auth.login('viewer', 'pa:ss');
  const [call] = calls;
  assert.equal(call.url, '/ui/auth/login');
  assert.equal(call.init.method, 'POST');
  assert.equal(call.headers.get('content-type'), 'application/json');
  assert.equal(call.headers.get('authorization'), null);
  assert.equal(call.headers.get('x-csrf-token'), null);
  assert.deepEqual(JSON.parse(call.init.body), { username: 'viewer', password: 'pa:ss' });
});

test('login failures are generic, throttled or unavailable', async () => {
  const invalid = recorder(() => json({ error: 'invalid_credentials', message: 'Invalid username or password.' }, 401));
  await assert.rejects(invalid.auth.login('a', 'b'), (e) => e.category === 'auth' && e.message === 'Invalid username or password.');

  const throttled = recorder(() => json({ error: 'throttled', retryAfter: 42 }, 429, { 'retry-after': '42' }));
  await assert.rejects(throttled.auth.login('a', 'b'), (e) => e.category === 'throttled' && e.retryAfter === 42);

  const headerOnly = recorder(() => new Response('', { status: 429, headers: { 'retry-after': '7' } }));
  await assert.rejects(headerOnly.auth.login('a', 'b'), (e) => e.category === 'throttled' && e.retryAfter === 7);

  const origin = recorder(() => json({ error: 'cross_origin' }, 403));
  await assert.rejects(origin.auth.login('a', 'b'), (e) => e.category === 'forbidden');

  const host = recorder(() => json({ error: 'host_not_allowed' }, 403));
  await assert.rejects(host.auth.login('a', 'b'), (e) => e.category === 'auth' && /host name/.test(e.message));

  const down = createAuth({ fetch: async () => { throw new TypeError('offline'); } });
  await assert.rejects(down.login('a', 'b'), (e) => e.category === 'unavailable');
});

test('logout and activity send the CSRF token; an expired session reads as sign-in required', async () => {
  const { calls, auth } = recorder((url) => (url.endsWith('logout') ? new Response(null, { status: 204 }) : json({ error: 'unauthenticated' }, 401)));
  assert.equal(await auth.logout('tok'), null);
  await assert.rejects(auth.activity('tok'), (e) => e.category === 'auth' && e.message === 'Session expired. Sign in to continue.');
  assert.deepEqual(calls.map((c) => [c.url, c.init.method, c.headers.get('x-csrf-token')]), [
    ['/ui/auth/logout', 'POST', 'tok'],
    ['/ui/auth/activity', 'POST', 'tok'],
  ]);
});

test('session transport auth sends only the CSRF header with same-origin cookies', () => {
  const auth = sessionAuth('tok');
  assert.deepEqual(auth.headers(), { 'X-CSRF-Token': 'tok' });
  assert.equal(auth.credentials, 'same-origin');
});
