// Sign-in against the web UI backend (`/ui/auth/*`). The session cookie is HttpOnly and never
// readable here; the CSRF token lives only in memory. No server secret ever reaches the browser.

import { McpError, parseRetryAfter } from './mcp.js';

const MAX_RETRY_AFTER_S = 30;
const RETRY_DELAYS_MS = [1000, 3000];

function failure(response, data) {
  const { status } = response;
  const code = typeof data?.error === 'string' ? data.error : null;
  if (status === 429) {
    const retryAfter = Number.isInteger(data?.retryAfter) ? data.retryAfter : parseRetryAfter(response.headers.get('retry-after'));
    return new McpError('throttled', 'The server is limiting requests.', { status, retryAfter });
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

export function createAuth({
  fetch: doFetch = globalThis.fetch.bind(globalThis),
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  retries = 2,
} = {}) {
  /** The bootstrap is read-only, so a throttled or unreachable server is retried (honouring Retry-After). */
  async function sessionWithRetry() {
    for (let attempt = 0; ; attempt += 1) {
      try {
        return await send(doFetch, '/ui/auth/session');
      } catch (error) {
        const transient = error.category === 'throttled' || error.category === 'unavailable';
        if (!transient || attempt >= retries) throw error;
        if (error.retryAfter !== null && error.retryAfter > MAX_RETRY_AFTER_S) throw error;
        await sleep(error.retryAfter !== null ? error.retryAfter * 1000 : RETRY_DELAYS_MS[Math.min(attempt, RETRY_DELAYS_MS.length - 1)]);
      }
    }
  }

  return {
    /** Current session, or `{authenticated: false}`; valid proxy Basic credentials start one. */
    async session() {
      const data = await sessionWithRetry();
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
