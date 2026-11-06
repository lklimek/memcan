// DOM helpers. Data is only ever inserted as text nodes or safe properties — never as HTML.

import { highlightSegments, isKnownPriority, isKnownStatus, parseDate, priorityLabel, statusLabel } from './data.js';

/** Create an element: `attrs` keys map to properties/attributes; children are nodes or text. */
export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'dataset') Object.assign(node.dataset, value);
    else if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2), value);
    else if (key.startsWith('aria-') || key === 'role' || key === 'for' || key === 'list') {
      node.setAttribute(key, value === true ? 'true' : String(value));
    } else if (key in node) node[key] = value;
    else node.setAttribute(key, value === true ? '' : String(value));
  }
  append(node, ...children);
  return node;
}

/** Null-safe append: skips null/undefined/false, unlike the native `append` which renders "null". */
export function append(node, ...children) {
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

export function clear(node) {
  node.replaceChildren();
  return node;
}

/** Same-origin in-app link handled by the router (`data-nav`). */
export function navLink(href, label, attrs = {}) {
  return el('a', { href, dataset: { nav: '' }, ...attrs }, label);
}

const STATUS_GLYPHS = new Map([
  ['pending', '○'],
  ['in_progress', '◐'],
  ['blocked', '●'],
  ['done', '✓'],
  ['postponed', '⏸'],
  ['cancelled', '✕'],
]);
const PRIORITY_GLYPHS = new Map([
  ['high', '▲'],
  ['medium', '■'],
  ['low', '▽'],
]);

function badge(kind, value, label, glyph) {
  return el(
    'span',
    { class: `badge ${kind}-${value.replace(/[^a-z_]/g, '') || 'unknown'}`, title: label },
    el('span', { 'aria-hidden': 'true', class: 'glyph' }, glyph),
    label,
  );
}

export function statusBadge(status) {
  const known = isKnownStatus(status);
  return badge('status', known ? status : 'unknown', statusLabel(status), STATUS_GLYPHS.get(status) ?? '◆');
}

export function priorityBadge(priority) {
  const known = isKnownPriority(priority);
  return badge('priority', known ? priority : 'unknown', priorityLabel(priority), PRIORITY_GLYPHS.get(priority) ?? '◆');
}

export function highlighted(text, tokens) {
  return highlightSegments(text, tokens).map((segment) =>
    segment.mark ? el('mark', {}, segment.text) : document.createTextNode(segment.text),
  );
}

const exactFormat = new Intl.DateTimeFormat(undefined, {
  year: 'numeric',
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
  timeZoneName: 'short',
});
const timeFormat = new Intl.DateTimeFormat(undefined, { timeStyle: 'medium' });

export function exactDate(value) {
  const ms = parseDate(value);
  return ms === null ? null : exactFormat.format(ms);
}

export function utcDate(value) {
  const ms = parseDate(value);
  return ms === null ? null : `${new Date(ms).toISOString().replace('T', ' ').slice(0, 19)} UTC`;
}

export function clockTime(ms) {
  return ms ? timeFormat.format(ms) : '—';
}

export function shortId(id) {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/** Modal dialog with focus return; Escape closes via the native dialog. */
export function openDialog({ title, body, actions = [], onClose, labelledBy }) {
  const opener = document.activeElement;
  const heading = el('h2', { id: labelledBy ?? `dialog-${Math.random().toString(36).slice(2)}` }, title);
  const close = el('button', { type: 'button', class: 'button secondary' }, 'Close');
  const dialog = el(
    'dialog',
    { class: 'dialog', 'aria-labelledby': heading.id },
    heading,
    body,
    el('div', { class: 'dialog-actions' }, ...actions, close),
  );
  const finish = () => {
    dialog.remove();
    onClose?.();
    if (opener && opener.isConnected && typeof opener.focus === 'function') opener.focus();
  };
  close.addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', finish, { once: true });
  document.body.append(dialog);
  dialog.showModal();
  return dialog;
}

/** Copy on a user gesture; on rejection show selectable text instead of a false success. */
export async function copyText(value, label, toast) {
  try {
    if (!navigator.clipboard || !window.isSecureContext) throw new Error('clipboard unavailable');
    await navigator.clipboard.writeText(value);
    toast(`${label} copied`);
  } catch {
    const field = el('input', { type: 'text', readOnly: true, value, class: 'copy-field', 'aria-label': label });
    const dialog = openDialog({
      title: `Copy ${label.toLowerCase()}`,
      body: el(
        'div',
        {},
        el('p', {}, 'Automatic copying is not available here. Select the text and press Ctrl+C (⌘C on Mac).'),
        field,
      ),
    });
    if (dialog.open) {
      field.focus();
      field.select();
    }
  }
}

export function panelError(error, onRetry) {
  return el(
    'div',
    { class: 'panel-error', role: 'alert' },
    el('p', {}, errorMessage(error)),
    onRetry ? el('button', { type: 'button', class: 'button secondary', onclick: onRetry }, 'Retry') : null,
    errorDetails(error),
  );
}

export function errorMessage(error) {
  switch (error?.category) {
    case 'auth':
      return 'API key rejected. Reconnect with a valid key.';
    case 'forbidden':
      return 'Access denied by server/network policy. An IP allowlist or origin policy may be responsible, not necessarily the key.';
    case 'throttled':
      return error.retryAfter ? `The server is limiting requests. Retry in ${error.retryAfter} s.` : 'The server is limiting requests. Retry shortly.';
    case 'unavailable':
      return 'Refresh failed: the server could not be reached or is unavailable.';
    case 'ambiguous':
      return 'Use the full task ID: this short ID matches more than one task.';
    case 'invalid-input':
      return 'The server rejected the request parameters.';
    case 'compat':
      return error.message;
    case 'tool':
      return 'The server could not complete the request.';
    default:
      return 'Server response could not be read.';
  }
}

function errorDetails(error) {
  if (!error) return null;
  const parts = [`category: ${error.category ?? 'unknown'}`];
  if (error.status) parts.push(`HTTP ${error.status}`);
  if (error.code !== null && error.code !== undefined) parts.push(`code ${error.code}`);
  if (error.at) parts.push(error.at);
  return el('details', { class: 'diagnostics' }, el('summary', {}, 'Diagnostics'), el('code', {}, parts.join(' · ')));
}

export function visuallyHidden(text) {
  return el('span', { class: 'visually-hidden' }, text);
}
