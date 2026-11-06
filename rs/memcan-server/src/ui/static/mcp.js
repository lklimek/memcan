// MCP Streamable HTTP client limited to the Phase 1 read tools, spoken to the web UI backend
// (`/ui/api/mcp`). Authentication is injected (`auth`: the session's CSRF header plus the
// same-origin cookie); the server enforces its own allowlist. close() forgets the auth.

export const P1_TOOLS = ['get_todo', 'list_todos', 'export_collection', 'list_collections', 'get_queue_status'];
export const PROTOCOL_VERSION = '2025-11-25';
const SUPPORTED_VERSIONS = ['2025-11-25', '2025-06-18', '2025-03-26'];
const MAX_TOOL_PAGES = 20;
const MAX_RETRY_AFTER_S = 30;
const RETRY_DELAYS_MS = [1000, 3000];

export class McpError extends Error {
  /**
   * @param {'auth'|'forbidden'|'unavailable'|'throttled'|'missing'|'invalid-input'|'ambiguous'|'protocol'|'tool'|'compat'|'aborted'} category
   */
  constructor(category, message, details = {}) {
    super(message);
    this.name = 'McpError';
    this.category = category;
    this.status = details.status ?? null;
    this.retryAfter = details.retryAfter ?? null;
    this.code = details.code ?? null;
    /** For `auth`: 'session' (signed out or expired), 'host' (Host guard) or 'policy' (other 403). */
    this.reason = null;
    this.at = new Date().toISOString();
  }
}

/** Incremental text/event-stream parser; emits `{event, data}` per dispatched event. */
export function createSseParser(onEvent) {
  let buffer = '';
  let data = [];
  let event = '';
  const dispatch = () => {
    if (data.length) onEvent({ event: event || 'message', data: data.join('\n') });
    data = [];
    event = '';
  };
  return {
    push(chunk) {
      buffer += chunk;
      for (;;) {
        const match = /\r\n|\r|\n/.exec(buffer);
        if (!match) break;
        // A lone trailing CR may be the first half of CRLF; wait for more input.
        if (match[0] === '\r' && match.index === buffer.length - 1) break;
        const line = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        if (line === '') dispatch();
        else if (line.startsWith(':')) continue;
        else {
          const colon = line.indexOf(':');
          const field = colon === -1 ? line : line.slice(0, colon);
          let value = colon === -1 ? '' : line.slice(colon + 1);
          if (value.startsWith(' ')) value = value.slice(1);
          if (field === 'data') data.push(value);
          else if (field === 'event') event = value;
        }
      }
    },
  };
}

export function parseRetryAfter(value) {
  if (!value) return null;
  if (/^\d+$/.test(value.trim())) return Number(value.trim());
  const date = Date.parse(value);
  return Number.isNaN(date) ? null : Math.max(0, Math.ceil((date - Date.now()) / 1000));
}

const FORBIDDEN_BODY_BYTES = 4096;

/** Read at most `limit` bytes of a body as text for classification only; never displayed. */
async function readPrefix(response, limit) {
  if (!response.body) return '';
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (size < limit) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value.subarray(0, limit - size));
      size += value.byteLength;
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  const bytes = new Uint8Array(chunks.reduce((n, c) => n + c.byteLength, 0));
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

function localhostUrl() {
  const loc = globalThis.location;
  if (!loc) return 'http://localhost:8190';
  return `${loc.protocol}//localhost${loc.port ? `:${loc.port}` : ''}`;
}

/**
 * Every 403 stops API work and signs the tab out; the body only selects the explanation:
 * the backend's Host guard, or other policy (origin, security token, proxy allowlist).
 */
async function forbiddenError(response) {
  const status = 403;
  const body = await readPrefix(response, FORBIDDEN_BODY_BYTES).catch(() => '');
  if (body.includes('"host_not_allowed"')) {
    const error = new McpError(
      'auth',
      `The server only accepts this UI on localhost. Open it via ${localhostUrl()} (e.g. an SSH tunnel) or ask the operator to allow this host name.`,
      { status },
    );
    error.reason = 'host';
    return error;
  }
  const error = new McpError(
    'auth',
    'Access denied by server or network policy: the request origin, security token or an IP allowlist was not accepted. Reload and sign in again, or ask the operator.',
    { status },
  );
  error.reason = 'policy';
  return error;
}

async function statusError(response) {
  const { status } = response;
  if (status === 401) {
    const error = new McpError('auth', 'Session expired. Sign in to continue.', { status });
    error.reason = 'session';
    return error;
  }
  if (status === 403) return forbiddenError(response);
  if (status === 429) {
    return new McpError('throttled', 'The server is limiting requests.', {
      status,
      retryAfter: parseRetryAfter(response.headers.get('retry-after')),
    });
  }
  if (status === 404) return new McpError('unavailable', 'The API endpoint was not found.', { status });
  if (status === 408 || status >= 500) return new McpError('unavailable', 'The server is unavailable.', { status });
  return new McpError('protocol', 'Server response could not be read.', { status });
}

function rpcError(error) {
  const message = typeof error?.message === 'string' ? error.message.slice(0, 300) : '';
  const code = Number.isInteger(error?.code) ? error.code : null;
  if (/ambiguous/i.test(message)) return new McpError('ambiguous', 'Use the full task ID.', { code });
  if (code === -32602) return new McpError('invalid-input', 'The server rejected the request parameters.', { code });
  return new McpError('tool', 'The server could not complete the request.', { code });
}

export function createMcpClient(options) {
  const {
    fetch: doFetch = globalThis.fetch.bind(globalThis),
    endpoint = '/ui/api/mcp',
    timeoutMs = 20000,
    maxResponseBytes = 16 * 1024 * 1024,
    sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    random = Math.random,
    retries = 2,
    maxConcurrent = 2,
    maxStartsPerSecond = 4,
    onAuthFailure = () => {},
  } = options;
  let { auth } = options;
  let closed = false;
  let sessionId = null;
  let protocolVersion = null;
  let initPromise = null;
  let available = null;
  let missing = [];
  let nextId = 1;
  const controllers = new Set();
  const queue = [];
  const starts = [];
  let running = 0;
  let pumpTimer = null;

  const aborted = () => new McpError('aborted', 'The API connection was closed.');

  function pump() {
    pumpTimer = null;
    while (running < maxConcurrent && queue.length) {
      const now = Date.now();
      while (starts.length && now - starts[0] >= 1000) starts.shift();
      if (starts.length >= maxStartsPerSecond) {
        pumpTimer = setTimeout(pump, 1000 - (now - starts[0]));
        return;
      }
      const job = queue.shift();
      starts.push(now);
      running += 1;
      job.run().finally(() => {
        running -= 1;
        if (!pumpTimer) pump();
      });
    }
  }

  function schedule(task, priority) {
    return new Promise((resolve, reject) => {
      const job = { run: () => task().then(resolve, reject), reject, priority };
      if (priority === 'high') {
        const index = queue.findIndex((queued) => queued.priority !== 'high');
        queue.splice(index === -1 ? queue.length : index, 0, job);
      } else queue.push(job);
      if (!pumpTimer) pump();
    });
  }

  async function readBody(response) {
    const type = response.headers.get('content-type') ?? '';
    if (!response.body) return { type, text: '' };
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let size = 0;
    let text = '';
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maxResponseBytes) {
        await reader.cancel().catch(() => {});
        throw new McpError('protocol', 'Server response was too large.');
      }
      text += decoder.decode(value, { stream: true });
    }
    return { type, text: text + decoder.decode() };
  }

  async function readSse(response, id) {
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let size = 0;
    let found = null;
    let invalid = false;
    const parser = createSseParser(({ data }) => {
      if (found) return;
      let message;
      try {
        message = JSON.parse(data);
      } catch {
        invalid = true;
        return;
      }
      // Server-initiated requests and notifications are not dispatched in P1.
      if (message && message.id === id && ('result' in message || 'error' in message)) found = message;
    });
    while (!found && !invalid) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maxResponseBytes) {
        await reader.cancel().catch(() => {});
        throw new McpError('protocol', 'Server response was too large.');
      }
      parser.push(decoder.decode(value, { stream: true }));
    }
    if (found || invalid) await reader.cancel().catch(() => {});
    if (!found) throw new McpError('protocol', 'Server response could not be read.');
    return found;
  }

  /** Send one HTTP POST; returns the JSON-RPC response message (or null for notifications). */
  async function send(message, signal) {
    if (closed) throw aborted();
    const controller = new AbortController();
    controllers.add(controller);
    const timer = setTimeout(() => controller.abort(new McpError('unavailable', 'The request timed out.')), timeoutMs);
    const onAbort = () => controller.abort(aborted());
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) onAbort();
    const headers = {
      ...auth.headers(),
      'Content-Type': 'application/json',
      Accept: 'application/json, text/event-stream',
    };
    if (protocolVersion && message.method !== 'initialize') headers['MCP-Protocol-Version'] = protocolVersion;
    const sentSession = message.method === 'initialize' ? null : sessionId;
    if (sentSession) headers['Mcp-Session-Id'] = sentSession;
    try {
      let response;
      try {
        response = await doFetch(endpoint, {
          method: 'POST',
          headers,
          body: JSON.stringify(message),
          credentials: auth.credentials,
          redirect: 'error',
          cache: 'no-store',
          signal: controller.signal,
        });
      } catch (error) {
        if (controller.signal.aborted) throw controller.signal.reason;
        throw new McpError('unavailable', 'The server could not be reached.');
      }
      if (closed) throw aborted();
      if (response.status === 404 && sentSession) {
        const error = new McpError('unavailable', 'The API session expired.', { status: 404 });
        error.sessionExpired = sentSession;
        throw error;
      }
      if (!response.ok) throw await statusError(response);
      if (message.method === 'initialize') sessionId = response.headers.get('mcp-session-id') || null;
      if (!('id' in message)) {
        await response.body?.cancel().catch(() => {});
        return null;
      }
      const type = response.headers.get('content-type') ?? '';
      let reply;
      if (type.includes('text/event-stream') && response.body) {
        reply = await readSse(response, message.id);
      } else {
        const body = await readBody(response);
        if (!body.type.includes('application/json')) throw new McpError('protocol', 'Server response could not be read.');
        try {
          reply = JSON.parse(body.text);
        } catch {
          throw new McpError('protocol', 'Server response could not be read.');
        }
      }
      if (closed) throw aborted();
      if (!reply || typeof reply !== 'object' || reply.jsonrpc !== '2.0' || reply.id !== message.id) {
        throw new McpError('protocol', 'Server response could not be read.');
      }
      return reply;
    } catch (error) {
      if (closed) throw aborted();
      if (error instanceof McpError) throw error;
      if (controller.signal.aborted && controller.signal.reason instanceof McpError) throw controller.signal.reason;
      throw new McpError('protocol', 'Server response could not be read.');
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      controllers.delete(controller);
    }
  }

  function fail(error) {
    if (error.category === 'auth' && !closed) {
      close();
      onAuthFailure(error);
    }
    throw error;
  }

  async function withRetry(operation, signal) {
    for (let attempt = 0; ; attempt += 1) {
      try {
        return await operation();
      } catch (error) {
        if (!(error instanceof McpError)) throw error;
        const transient = error.category === 'unavailable' || error.category === 'throttled';
        if (!transient || error.sessionExpired || attempt >= retries || closed || signal?.aborted) throw error;
        let delay;
        if (error.category === 'throttled' && error.retryAfter !== null) {
          if (error.retryAfter > MAX_RETRY_AFTER_S) throw error;
          delay = error.retryAfter * 1000;
        } else {
          const base = RETRY_DELAYS_MS[Math.min(attempt, RETRY_DELAYS_MS.length - 1)];
          delay = Math.round(base * (0.75 + random() * 0.5));
        }
        await sleep(delay);
        if (closed) throw aborted();
      }
    }
  }

  async function rpc(method, params, signal) {
    const id = nextId++;
    const reply = await send({ jsonrpc: '2.0', id, method, ...(params === undefined ? {} : { params }) }, signal);
    if (reply.error) throw rpcError(reply.error);
    if (!reply.result || typeof reply.result !== 'object') {
      throw new McpError('protocol', 'Server response could not be read.');
    }
    return reply.result;
  }

  async function initialize() {
    protocolVersion = null;
    sessionId = null;
    const result = await withRetry(() =>
      rpc('initialize', {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: 'memcan-webui', version: '1' },
      }),
    );
    if (!SUPPORTED_VERSIONS.includes(result.protocolVersion)) {
      throw new McpError('protocol', 'The server uses an unsupported MCP protocol version.');
    }
    protocolVersion = result.protocolVersion;
    await withRetry(() => send({ jsonrpc: '2.0', method: 'notifications/initialized' }));
    if (!available) {
      const names = new Set();
      let cursor;
      for (let page = 0; page < MAX_TOOL_PAGES; page += 1) {
        const listed = await withRetry(() => rpc('tools/list', cursor ? { cursor } : undefined));
        for (const tool of Array.isArray(listed.tools) ? listed.tools : []) {
          if (typeof tool?.name === 'string') names.add(tool.name);
        }
        cursor = typeof listed.nextCursor === 'string' && listed.nextCursor ? listed.nextCursor : null;
        if (!cursor) break;
      }
      available = new Set(P1_TOOLS.filter((name) => names.has(name)));
      missing = P1_TOOLS.filter((name) => !names.has(name));
    }
  }

  function ensureInit() {
    if (closed) return Promise.reject(aborted());
    if (!initPromise) {
      const attempt = initialize();
      initPromise = attempt;
      attempt.catch(() => {
        if (initPromise === attempt) initPromise = null;
      });
    }
    return initPromise;
  }

  async function connect() {
    try {
      await ensureInit();
    } catch (error) {
      if (error instanceof McpError) fail(error);
      throw error;
    }
    return { protocolVersion, missing: [...missing], tools: new Set(available) };
  }

  async function callTool(name, args = {}, { signal, priority = 'low' } = {}) {
    if (!P1_TOOLS.includes(name)) throw new McpError('compat', 'This action is not available in the read-only UI.');
    if (name === 'export_collection' && args.collection !== 'todos') {
      throw new McpError('compat', 'Only the task collection can be exported here.');
    }
    if (closed) throw aborted();
    try {
      return await schedule(async () => {
        if (signal?.aborted) throw aborted();
        await ensureInit();
        if (!available.has(name)) throw new McpError('compat', `The server does not offer ${name}.`);
        const attemptCall = async () => {
          const result = await withRetry(() => rpc('tools/call', { name, arguments: args }, signal), signal);
          if (result.isError) throw new McpError('tool', 'The server could not complete the request.');
          if (result.structuredContent && typeof result.structuredContent === 'object') return result.structuredContent;
          const block = Array.isArray(result.content) ? result.content.find((item) => item?.type === 'text') : null;
          if (!block || typeof block.text !== 'string') throw new McpError('protocol', 'Server response could not be read.');
          try {
            return JSON.parse(block.text);
          } catch {
            throw new McpError('protocol', 'Server response could not be read.');
          }
        };
        try {
          return await attemptCall();
        } catch (error) {
          if (!(error instanceof McpError) || !error.sessionExpired) throw error;
          // One shared re-initialization per expired session, then a single retry.
          if (sessionId === error.sessionExpired) {
            sessionId = null;
            initPromise = null;
          }
          await ensureInit();
          try {
            return await attemptCall();
          } catch (retryError) {
            if (retryError?.sessionExpired) throw new McpError('unavailable', 'The API session could not be restored.');
            throw retryError;
          }
        }
      }, priority);
    } catch (error) {
      if (error instanceof McpError) fail(error);
      throw error;
    }
  }

  function close() {
    if (closed) return;
    closed = true;
    const lastSession = sessionId;
    const lastAuth = auth;
    auth = null;
    sessionId = null;
    initPromise = null;
    available = null;
    for (const controller of controllers) controller.abort(aborted());
    controllers.clear();
    for (const job of queue.splice(0)) job.reject(aborted());
    if (lastSession) {
      // Best effort; never awaited so clearing local state is not delayed.
      try {
        doFetch(endpoint, {
          method: 'DELETE',
          headers: { ...lastAuth.headers(), 'Mcp-Session-Id': lastSession },
          credentials: lastAuth.credentials,
          redirect: 'error',
          cache: 'no-store',
          keepalive: true,
        }).catch(() => {});
      } catch {
        // Ignored: session cleanup is optional.
      }
    }
  }

  return {
    connect,
    callTool,
    close,
    get closed() {
      return closed;
    },
  };
}
