// Connect: collect the API key (memory only) and return to the intended route.

import { append, el, errorMessage, navLink, shortId } from '../components.js';
import { McpError, validateKey } from '../mcp.js';
import { parseRoute, safeReturnTo } from '../router.js';

function describeTarget(returnTo) {
  if (!returnTo) return null;
  const route = parseRoute(new URL(returnTo, location.origin).pathname);
  if (route.name === 'task') return `After connecting: return to task ${shortId(route.id)}`;
  if (route.name === 'dashboard') return 'After connecting: return to the dashboard';
  return 'After connecting: return to the task list';
}

export function render(main, ctx) {
  ctx.setTitle('Connect');
  const returnTo = safeReturnTo(ctx.params.get('returnTo'));
  const notice = ctx.takeNotice();
  const errorId = 'key-error';
  const status = el('div', { id: errorId, class: 'form-error', role: 'alert', tabindex: '-1' });
  const input = el('input', {
    id: 'api-key',
    name: 'api-key',
    type: 'password',
    autocomplete: 'current-password',
    spellcheck: false,
    required: true,
    'aria-describedby': 'key-help',
  });
  const reveal = el('button', {
    type: 'button',
    class: 'button secondary',
    'aria-pressed': 'false',
    'aria-controls': 'api-key',
    onclick: () => {
      const show = input.type === 'password';
      input.type = show ? 'text' : 'password';
      reveal.setAttribute('aria-pressed', String(show));
    },
  }, 'Show');
  const submit = el('button', { type: 'submit', class: 'button primary' }, ctx.insecureHttp ? 'Connect over HTTP' : 'Connect');

  const fail = (message) => {
    status.textContent = message;
    input.setAttribute('aria-invalid', 'true');
    input.setAttribute('aria-describedby', `${errorId} key-help`);
    status.focus();
  };

  const form = el(
    'form',
    {
      class: 'panel connect-form',
      novalidate: true,
      onsubmit: async (event) => {
        event.preventDefault();
        const key = input.value;
        const invalid = validateKey(key);
        if (invalid) {
          fail(invalid);
          return;
        }
        input.value = '';
        input.type = 'password';
        reveal.setAttribute('aria-pressed', 'false');
        status.textContent = '';
        input.removeAttribute('aria-invalid');
        submit.disabled = true;
        submit.textContent = 'Connecting…';
        main.setAttribute('aria-busy', 'true');
        try {
          await ctx.connect(key);
          if (ctx.signal.aborted) return;
          ctx.announce('Connected to the MemCan API.');
          ctx.navigate(returnTo ?? '/ui/tasks', { replace: true });
        } catch (error) {
          if (ctx.signal.aborted) return;
          main.removeAttribute('aria-busy');
          submit.disabled = false;
          submit.textContent = ctx.insecureHttp ? 'Connect over HTTP' : 'Connect';
          fail(error instanceof McpError ? errorMessage(error) : 'Connection failed.');
        }
      },
    },
    el('p', {}, el('strong', {}, 'Server: '), el('code', {}, location.origin), ' (this page’s origin)'),
    el('p', {}, 'Page access granted. Connect to read tasks and server activity.'),
    ctx.insecureHttp
      ? el(
          'p',
          { class: 'banner warning', id: 'http-warning' },
          'This connection is unencrypted; page credentials and the API key can be intercepted. Use HTTPS or a localhost tunnel.',
        )
      : null,
    // Lets password managers file the key under a stable name; never sent anywhere.
    el('input', { type: 'text', autocomplete: 'username', value: 'MemCan API key', hidden: true, readOnly: true, tabIndex: -1, 'aria-hidden': 'true' }),
    el('label', { for: 'api-key' }, 'API key'),
    el('div', { class: 'key-row' }, input, reveal),
    el(
      'div',
      { id: 'key-help', class: 'help' },
      el('p', {}, 'This key grants full API access, including deletion.'),
      el('p', {}, 'It stays in this tab’s memory; reload or close clears it.'),
      el(
        'p',
        {},
        'Saved filters and URLs can contain project, owner and search text and stay in browser history; they never contain the key.',
      ),
    ),
    status,
    el('div', { class: 'form-actions' }, submit, navLink('/ui/dashboard', 'View server health', { class: 'button secondary' })),
    describeTarget(returnTo) ? el('p', { class: 'muted' }, describeTarget(returnTo)) : null,
  );

  append(main, el('h1', { tabindex: '-1' }, 'Connect to MemCan'));
  if (notice && notice.kind === 'info') append(main, el('p', { class: 'banner', role: 'status' }, notice.text));
  append(main, form);
  if (notice && notice.kind === 'error') fail(notice.text);
  else input.focus();
  return {};
}
