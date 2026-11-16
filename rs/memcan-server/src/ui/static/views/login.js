// Sign in: username/password form (or the HTTPS-required explanation), then the intended route.

import { append, el, errorMessage, shortId } from '../components.js';
import { McpError } from '../mcp.js';
import { parseRoute, safeReturnTo } from '../router.js';

function describeTarget(returnTo) {
  if (!returnTo) return null;
  const route = parseRoute(new URL(returnTo, location.origin).pathname);
  if (route.name === 'task') return `You will return to: task ${shortId(route.id)}`;
  if (route.name === 'dashboard') return 'You will return to: the dashboard';
  return 'You will return to: the task list';
}

function failureText(error) {
  if (!(error instanceof McpError)) return 'Sign-in failed.';
  if (error.category === 'throttled') {
    return error.retryAfter ? `Too many sign-in attempts. Try again in ${error.retryAfter} s.` : 'Too many sign-in attempts. Try again later.';
  }
  return errorMessage(error);
}

export function render(main, ctx) {
  ctx.setTitle('Sign in');
  const returnTo = safeReturnTo(ctx.params.get('returnTo'));
  if (ctx.signedIn) {
    ctx.navigate(returnTo ?? '/ui/tasks', { replace: true });
    return {};
  }
  const notice = ctx.takeNotice();
  append(main, el('h1', { tabindex: '-1' }, 'Sign in to MemCan'));
  if (notice) append(main, el('p', { class: notice.kind === 'error' ? 'banner warning' : 'banner', role: 'status' }, notice.text));

  if (ctx.httpsRequired) {
    append(
      main,
      el(
        'div',
        { class: 'panel' },
        el('h2', {}, 'HTTPS required'),
        el('p', {}, 'Sign-in is only available over an encrypted connection. Open this page with https://.'),
        el(
          'p',
          { class: 'muted' },
          'Operators: terminate TLS at the reverse proxy, or allow plain HTTP on a trusted network with MEMCAN_WEBUI_ALLOW_INSECURE_HTTP=true.',
        ),
      ),
    );
    ctx.focusHeading();
    return {};
  }

  if (ctx.signInUnchecked) {
    append(
      main,
      el(
        'div',
        { class: 'panel' },
        el('h2', {}, 'Sign-in unavailable'),
        el('p', {}, 'The server’s sign-in settings could not be checked, so no password is requested over this unencrypted connection.'),
        el('p', {}, el('button', { type: 'button', class: 'button primary', onclick: () => location.reload() }, 'Reload')),
      ),
    );
    ctx.focusHeading();
    return {};
  }

  const status = el('div', { id: 'login-error', class: 'form-error', role: 'alert', tabindex: '-1' });
  const username = el('input', {
    id: 'username',
    name: 'username',
    type: 'text',
    autocomplete: 'username',
    autocapitalize: 'none',
    spellcheck: false,
    required: true,
  });
  const password = el('input', {
    id: 'password',
    name: 'password',
    type: 'password',
    autocomplete: 'current-password',
    required: true,
  });
  const reveal = el('button', {
    type: 'button',
    class: 'button secondary',
    'aria-pressed': 'false',
    'aria-controls': 'password',
    onclick: () => {
      const show = password.type === 'password';
      password.type = show ? 'text' : 'password';
      reveal.setAttribute('aria-pressed', String(show));
    },
  }, 'Show');
  const label = ctx.insecureHttp ? 'Sign in over HTTP' : 'Sign in';
  const submit = el('button', { type: 'submit', class: 'button primary' }, label);

  const fail = (message, field) => {
    status.textContent = message;
    for (const input of [username, password]) {
      input.setAttribute('aria-invalid', 'true');
      input.setAttribute('aria-describedby', 'login-error');
    }
    (field ?? status).focus();
  };

  const form = el(
    'form',
    {
      class: 'panel login-form',
      novalidate: true,
      onsubmit: async (event) => {
        event.preventDefault();
        if (!username.value.trim() || !password.value) {
          fail('Enter your username and password.', username.value.trim() ? password : username);
          return;
        }
        const credentials = [username.value, password.value];
        password.value = '';
        password.type = 'password';
        reveal.setAttribute('aria-pressed', 'false');
        status.textContent = '';
        submit.disabled = true;
        submit.textContent = 'Signing in…';
        main.setAttribute('aria-busy', 'true');
        try {
          await ctx.login(...credentials);
          if (ctx.signal.aborted) return;
          ctx.announce('Signed in.');
          ctx.navigate(returnTo ?? '/ui/tasks', { replace: true });
        } catch (error) {
          if (ctx.signal.aborted) return;
          main.removeAttribute('aria-busy');
          submit.disabled = false;
          submit.textContent = label;
          fail(failureText(error));
        }
      },
    },
    el('p', {}, 'Your account determines what you can view or change.'),
    el('label', { for: 'username' }, 'Username'),
    username,
    el('label', { for: 'password' }, 'Password'),
    el('div', { class: 'password-row' }, password, reveal),
    status,
    el('div', { class: 'form-actions' }, submit),
    describeTarget(returnTo) ? el('p', { class: 'muted' }, describeTarget(returnTo)) : null,
  );

  append(main, form);
  username.focus();
  return {};
}
