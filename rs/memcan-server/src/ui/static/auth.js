// Sign-in against the web UI backend (`/ui/auth/*`). The session cookie is HttpOnly and never
// readable here; the CSRF token lives only in memory. No server secret ever reaches the browser.

import { McpError, parseRetryAfter } from './mcp.js';

function failure(response, data) {
  const { status } = response;
  const code = typeof data?.error === 'string' ? data.error : null;
  if (status === 429) {
    const retryAfter = Number.isInteger(data?.retryAfter) ? data.retryAfter : parseRetryAfter(response.headers.get('retry-after'));
    return new McpError('throttled', 'Too many sign-in attempts.', { status, retryAfter });
  }
  if (code === 'invalid_credentials') return new McpError('auth', 'Invalid username or password.', { status });
  if (status === 401) return new McpError('auth', 'Session expired. Sign in to continue.', { status });
  if (code === 'host_not_allowed') {
    return new McpError('auth', 'This host name is not allowed for sign-in. Open the UI via localhost or ask the operator to allow it.', { status });
  }
  if (status === 403) {
    return new McpError('forbidden', 'The server refused this request: its origin or security token was not accepted. Reload and try again.', { status });
  }
  if (status === 404 || status === 408 || status >= 500) return new McpError('unavailable', 'The server is unavailable.', { status });
  return new McpError('protocol', 'Server response could not be read.', { status });
}

async function send(doFetch, url, { method = 'GET', body, csrf } = {}) {
  const headers = { Accept: 'application/json' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (csrf) headers['X-CSRF-Token'] = csrf;
  let response;
  try {
    response = await doFetch(url, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: 'same-origin',
      cache: 'no-store',
      redirect: 'error',
    });
  } catch {
    throw new McpError('unavailable', 'The server could not be reached.');
  }
  const json = (response.headers.get('content-type') ?? '').includes('application/json');
  const data = json ? await response.json().catch(() => null) : null;
  if (!response.ok) throw failure(response, data);
  return data;
}

export function createAuth({ fetch: doFetch = globalThis.fetch.bind(globalThis) } = {}) {
  return {
    /** Current session, or `{authenticated: false}`; valid proxy Basic credentials start one. */
    async session() {
      const data = await send(doFetch, '/ui/auth/session');
      if (!data || typeof data !== 'object' || typeof data.authenticated !== 'boolean') {
        throw new McpError('protocol', 'Server response could not be read.');
      }
      if (data.authenticated && typeof data.csrfToken !== 'string') {
        throw new McpError('protocol', 'Server response could not be read.');
      }
      return data;
    },
    login: (username, password) => send(doFetch, '/ui/auth/login', { method: 'POST', body: { username, password } }),
    logout: (csrf) => send(doFetch, '/ui/auth/logout', { method: 'POST', csrf }),
    activity: (csrf) => send(doFetch, '/ui/auth/activity', { method: 'POST', csrf }),
  };
}

/** MCP transport auth for a signed-in tab: the CSRF header plus the same-origin session cookie. */
export function sessionAuth(csrf) {
  return { headers: () => ({ 'X-CSRF-Token': csrf }), credentials: 'same-origin' };
}
