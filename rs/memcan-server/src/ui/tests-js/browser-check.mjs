// Dev-only browser scenario against mock-server.mjs. Not part of `node --test`.
// Usage: PLAYWRIGHT_CORE=/path/to/playwright-core [CHROMIUM=/path/to/chrome] [SCREENS=dir] \
//        node browser-check.mjs http://127.0.0.1:8199

import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';

const { chromium } = await import(`${process.env.PLAYWRIGHT_CORE}/index.mjs`);
const BASE = process.argv[2] ?? 'http://127.0.0.1:8199';
const SCREENS = process.env.SCREENS ?? null;
const READ_TOOLS = new Set(['get_todo', 'list_todos', 'export_collection', 'list_collections', 'get_queue_status']);
const TASK0 = '81cbe927-0000-4000-8000-000000000000';
const TASK1 = '81cbe928-0000-4000-8000-000000000001';

if (SCREENS) await mkdir(SCREENS, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined });
const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
const page = await context.newPage();
const problems = [];
const requests = [];
page.on('console', (msg) => {
  if (msg.type() === 'error' || msg.type() === 'warning') problems.push(`console ${msg.type()}: ${msg.text()}`);
});
page.on('pageerror', (error) => problems.push(`pageerror: ${error.message}`));
page.on('dialog', (dialog) => {
  problems.push(`unexpected dialog: ${dialog.message()}`);
  dialog.dismiss();
});
page.on('request', (request) => requests.push({ url: request.url(), method: request.method(), headers: request.headers(), body: request.postData() }));

const shot = async (name, fullPage = true) => {
  if (SCREENS) await page.screenshot({ path: `${SCREENS}/${name}.png`, fullPage });
};
const calls = async () => (await (await fetch(`${BASE}/__calls`)).json()).filter((c) => c.method === 'tools/call');
// A full page load forgets the in-memory key by design; navigate inside the app instead.
const appGo = (url) =>
  page.evaluate((target) => {
    history.pushState(null, '', target);
    dispatchEvent(new PopStateEvent('popstate'));
  }, url);
const step = (name) => process.stdout.write(`• ${name}\n`);

step('direct task link without connection goes to Connect and keeps the target');
await page.goto(`${BASE}/ui/tasks/${TASK0}?project=memcan`);
await page.waitForURL(/\/ui\/connect\?returnTo=/);
await page.getByRole('heading', { name: 'Connect to MemCan' }).waitFor();
assert.ok(await page.getByText('After connecting: return to task 81cbe927…').isVisible());
assert.equal(await page.locator('#http-warning').count(), 0, 'no HTTP warning on loopback');
await shot('01-connect');

step('blank and wrong keys fail with recoverable messages');
await page.getByRole('button', { name: 'Connect', exact: true }).click();
await page.getByText('Enter the API key.').waitFor();
await page.getByLabel('API key').fill('wrong-key');
await page.getByRole('button', { name: 'Show' }).click();
assert.equal(await page.getByRole('button', { name: 'Show' }).getAttribute('aria-pressed'), 'true');
await page.getByRole('button', { name: 'Connect', exact: true }).click();
await page.getByText('API key rejected. Reconnect with a valid key.').waitFor();
assert.equal(await page.getByLabel('API key').inputValue(), '', 'key input cleared after submit');

step('connect opens the task before any export runs');
await page.getByLabel('API key').fill('test-key');
await page.getByRole('button', { name: 'Connect', exact: true }).click();
await page.waitForURL(`${BASE}/ui/tasks/${TASK0}?project=memcan`);
await page.locator('h1.task-title').waitFor();
const title = await page.locator('h1.task-title').textContent();
assert.ok(title.includes('<script>alert(1)</script>'), 'malicious title stays inert text');
const firstCalls = await calls();
assert.equal(firstCalls[0].tool, 'get_todo', 'detail fetch is the first tool call');
assert.equal(firstCalls[0].hasBasic, false);
await page.getByText('This task may have been deleted. The relationship remains visible.').waitFor();
await page.getByText('Self-reference').first().waitFor();
await page.getByText('Other project: other-project').first().waitFor();
await page.getByText('Cycle: also blocked by this task').waitFor();
assert.ok(await page.getByText('<b>not bold</b>', { exact: false }).isVisible());
await shot('02-task-detail');

step('reverse links: a scan lists blocked tasks without blocker-only wording');
await page.getByRole('button', { name: 'Scan all tasks for relationships' }).click();
const blocksPanel = page.locator('section.panel', { has: page.getByRole('heading', { name: 'Blocks (observed in loaded tasks)' }) });
await blocksPanel.getByRole('link', { name: /Task 4:/ }).waitFor();
assert.equal(await blocksPanel.getByText('Resolved blocker').count(), 0);
await page.getByText(/None found in the last scan|Based on \d+ loaded tasks\./).first().waitFor();
assert.equal(await page.locator('#blockers-heading + ul [aria-busy=true]').count(), 0, 'no blocker stuck loading');

step('copy ID uses the clipboard; a rejected clipboard falls back to manual copy');
await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: BASE });
await page.getByRole('button', { name: 'Copy ID' }).click();
await page.getByText('ID copied').waitFor();
assert.equal(await page.evaluate(() => navigator.clipboard.readText()), TASK0);
await page.evaluate(() => {
  navigator.clipboard.writeText = () => Promise.reject(new Error('denied'));
});
await page.getByRole('button', { name: 'Copy link' }).click();
const copyDialog = page.getByRole('dialog', { name: 'Copy link' });
await copyDialog.waitFor();
assert.equal(await copyDialog.getByRole('textbox').inputValue(), `${BASE}/ui/tasks/${TASK0}`);
await page.keyboard.press('Escape');
await copyDialog.waitFor({ state: 'detached' });

step('blocker link keeps list context, then Back returns');
await page.getByRole('link', { name: /Task 1: Review extraction/ }).first().click();
await page.waitForURL(new RegExp(`/ui/tasks/${TASK1}\\?project=memcan`));
await page.getByRole('heading', { name: /Task 1: Review extraction/ }).waitFor();
await page.goBack();
await page.waitForURL(`${BASE}/ui/tasks/${TASK0}?project=memcan`);
await page.locator('h1.task-title').waitFor();

step('Back to tasks shows the project list');
await page.getByRole('link', { name: '← Back to tasks' }).click();
await page.waitForURL(/\/ui\/tasks\?project=memcan/);
await page.getByText('scan complete').waitFor();
await shot('03-tasks-project');

step('filters, search and grouping are local and URL-backed');
await appGo(`/ui/tasks`);
await page.getByText('scan complete').waitFor();
const before = (await calls()).length;
await page.getByRole('button', { name: 'High priority' }).click();
await page.waitForURL(/priority=high/);
await page.getByLabel('Find title, description, project, owner or ID').fill('routing');
await page.waitForURL(/q=routing/);
await page.getByLabel('Group').selectOption('project');
await page.waitForURL(/group=project/);
const summary = await page.locator('.results-summary').textContent();
assert.match(summary, /matches/);
assert.equal((await calls()).length, before, 'search/filter emit no MCP calls');
await page.getByRole('button', { name: /o'brien & co\?#\/x/ }).first().waitFor();
await shot('04-tasks-filtered-grouped');

step('status checkboxes have their own labels; clicking a label toggles that box');
await appGo('/ui/tasks');
await page.getByText('scan complete').waitFor();
await page.locator('summary', { hasText: 'Status:' }).click();
const doneBox = page.getByRole('checkbox', { name: 'Done', exact: true });
assert.equal(await doneBox.isChecked(), false);
await page.locator('label', { hasText: /^Done$/ }).click();
await page.waitForURL(/status=done/);
const statusParams = new URL(page.url()).searchParams.getAll('status');
assert.ok(statusParams.includes('pending') && statusParams.includes('done'), statusParams.join());
assert.equal(await page.getByRole('checkbox', { name: 'Done', exact: true }).isChecked(), true);
const ids = await page.locator('details.popover input[type=checkbox]').evaluateAll((boxes) => boxes.map((b) => b.id));
assert.equal(new Set(ids).size, ids.length, 'unique checkbox ids');

step('Escape closes an open popover and returns focus to its summary');
await page.keyboard.press('Escape');
assert.equal(await page.locator('details.popover[open]').count(), 0);
assert.ok(await page.evaluate(() => document.activeElement?.tagName === 'SUMMARY'));
await appGo('/ui/tasks?q=routing&priority=high&group=project&status_mode=open');
await page.getByText('scan complete').waitFor();

step('skip link focuses main without changing the URL or re-rendering');
const searchBefore = await page.getByLabel('Find title, description, project, owner or ID').inputValue();
await page.locator('.skip-link').focus();
await page.keyboard.press('Enter');
assert.equal(await page.evaluate(() => document.activeElement?.id), 'main');
assert.equal(new URL(page.url()).hash, '');
assert.equal(await page.getByLabel('Find title, description, project, owner or ID').inputValue(), searchBefore);

step('detail from list and Back restores filters and focus');
const link = page.locator('a.task-link').first();
const linkId = await link.getAttribute('data-task-id');
await link.focus();
await page.keyboard.press('Enter');
await page.waitForURL(/\/ui\/tasks\/[^?]+\?/);
await page.goBack();
await page.waitForURL(/q=routing/);
await page.waitForFunction((id) => document.activeElement?.dataset?.taskId === id, linkId);

step('zero statuses and quoted project deep link round-trip');
await appGo(`/ui/tasks?status_filter_present=1`);
await page.getByText('Select a status or choose Open.').waitFor();
const quoted = "o'brien & co?#/x";
await appGo(`/ui/tasks?${new URLSearchParams({ project: quoted })}`);
await page.getByText(`Project: ${quoted}`).waitFor();
await page.getByText('scan complete').waitFor();
assert.equal(new URL(page.url()).searchParams.get('project'), quoted);
const projects = await page.locator('td.col-project').allTextContents();
assert.ok(projects.length > 0 && projects.every((text) => text.endsWith(quoted)), 'only the quoted project is listed');

step('short prefix canonicalizes; ambiguous, missing and malformed IDs have distinct states');
await appGo(`/ui/tasks/81cbe928`);
await page.waitForURL(`${BASE}/ui/tasks/${TASK1}`);
await appGo(`/ui/tasks/abcdef12`);
await page.getByRole('heading', { name: 'Use the full task ID' }).waitFor();
await appGo(`/ui/tasks/does-not-exist`);
await page.getByRole('heading', { name: 'Task not found' }).waitFor();
await appGo(`/ui/tasks/%E0%A4%A`);
await page.getByRole('heading', { name: 'Page not found' }).waitFor();

step('dashboard shows degraded dependency, absent fallback and server-wide panels');
await appGo(`/ui/dashboard`);
await page.getByText('Server: Degraded', { exact: true }).waitFor();
await page.getByText('Not configured').waitFor();
await page.getByText('Unavailable', { exact: true }).waitFor();
await page.getByText('ollama / qwen3.5:9b').waitFor();
await page.getByText('2 pending operations').waitFor();
await page.getByText('Unavailable or not created').waitFor();
await page.getByText(/Task coverage: \d+ records loaded · scan complete/).waitFor();
await shot('05-dashboard');

step('command palette opens with Ctrl+K and closes with Escape');
await page.locator('body').click({ position: { x: 5, y: 600 } });
await page.keyboard.press('Control+k');
await page.getByRole('dialog', { name: 'Commands' }).waitFor();
await page.keyboard.type('dash');
await shot('06-commands');
await page.keyboard.press('Escape');
await page.getByRole('dialog', { name: 'Commands' }).waitFor({ state: 'detached' });

step('dark theme and mobile cards');
await page.emulateMedia({ colorScheme: 'dark' });
await appGo(`/ui/tasks`);
await page.getByText('scan complete').waitFor();
await shot('07-tasks-dark');
await page.setViewportSize({ width: 375, height: 800 });
await page.getByRole('button', { name: /^Filters/ }).waitFor();
const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
assert.ok(scrollWidth <= 375, `no horizontal page scroll at 375px (got ${scrollWidth})`);
await shot('08-tasks-mobile-dark', false);
await appGo(`/ui/tasks/${TASK0}`);
await page.locator('h1.task-title').waitFor();
await shot('09-detail-mobile-dark', false);
await page.setViewportSize({ width: 320, height: 800 });
assert.ok((await page.evaluate(() => document.documentElement.scrollWidth)) <= 320, 'no horizontal scroll at 320px');
await page.emulateMedia({ colorScheme: 'light' });
await page.setViewportSize({ width: 1280, height: 900 });

step('reload forgets the key; reconnecting returns to the quoted project with an escaped scoped scan');
await appGo(`/ui/tasks?${new URLSearchParams({ project: quoted })}`);
await page.reload();
await page.waitForURL(/\/ui\/connect\?returnTo=/);
await page.getByLabel('API key').fill('test-key');
await page.getByRole('button', { name: 'Connect', exact: true }).click();
await page.waitForURL(/\/ui\/tasks\?project=/);
await page.getByText('scan complete').waitFor();
assert.equal(new URL(page.url()).searchParams.get('project'), quoted);
const filterCall = (await calls()).reverse().find((c) => c.tool === 'export_collection');
assert.equal(filterCall.args.filter, "project = 'o''brien & co?#/x'");

step('the staleness timer keeps focus on coverage actions during a slow scan');
await fetch(`${BASE}/__slow?ms=25000`);
await page.reload();
await page.waitForURL(/\/ui\/connect\?returnTo=/);
await page.getByLabel('API key').fill('test-key');
await page.getByRole('button', { name: 'Connect', exact: true }).click();
const stopButton = page.getByRole('button', { name: 'Stop loading' });
await stopButton.waitFor();
await stopButton.focus();
await page.waitForTimeout(16500);
assert.equal(await page.evaluate(() => document.activeElement?.textContent), 'Stop loading', 'focus survives the staleness tick');
await page.keyboard.press('Enter');
await page.locator('.coverage').getByRole('button', { name: 'Continue loading' }).waitFor();
assert.equal(await page.evaluate(() => document.activeElement?.textContent), 'Continue loading', 'focus follows Stop → Continue');
await fetch(`${BASE}/__slow?ms=0`);
await page.keyboard.press('Enter');
await page.getByText('scan complete').waitFor();

step('fragment-only history changes keep the view; path changes with a fragment route');
await appGo('/ui/connect#x');
await page.getByRole('heading', { name: 'Connect to MemCan' }).waitFor();
await appGo('/ui/dashboard');
await page.getByRole('heading', { name: 'Dashboard' }).waitFor();
await page.evaluate(() => {
  document.querySelector('main h1').dataset.marker = 'kept';
  location.hash = 'frag';
});
await page.waitForTimeout(300);
assert.equal(await page.evaluate(() => document.querySelector('main h1')?.dataset.marker), 'kept', 'no re-render on hash change');
await page.goBack();
await page.waitForTimeout(300);
assert.equal(await page.evaluate(() => document.querySelector('main h1')?.dataset.marker), 'kept');
await page.goBack();
await page.getByRole('heading', { name: 'Connect to MemCan' }).waitFor();

step('background refresh keeps focus on "Show updates" while newer data is staged again');
{
  const timed = await context.newPage();
  await timed.clock.install();
  await timed.goto(`${BASE}/ui/tasks`);
  await timed.getByLabel('API key').fill('test-key');
  await timed.getByRole('button', { name: 'Connect', exact: true }).click();
  await timed.getByText('scan complete').waitFor();
  await fetch(`${BASE}/__retitle?title=${encodeURIComponent('Retitled once')}`);
  await timed.clock.runFor(62000);
  const showUpdates = timed.getByRole('button', { name: 'Show updates' });
  await showUpdates.waitFor();
  await showUpdates.focus();
  await fetch(`${BASE}/__retitle?title=${encodeURIComponent('Retitled twice')}`);
  await timed.clock.runFor(62000);
  await timed.waitForTimeout(300);
  assert.equal(await timed.evaluate(() => document.activeElement?.textContent), 'Show updates', 'focus survives the next refresh');
  assert.equal(await timed.locator('.update-banner [role=status], .update-banner p').count(), 1);
  await timed.keyboard.press('Enter');
  await timed.getByRole('button', { name: 'Show updates' }).waitFor({ state: 'detached' });
  await timed.close();
  await fetch(`${BASE}/__retitle?title=${encodeURIComponent('Task 2: Resolve encoded IDs')}`);
}

step('disconnect clears key, data and storage; health-only dashboard still works');
await page.getByRole('button', { name: 'Disconnect / forget key' }).click();
await page.getByText('API connection cleared. Browser page login may remain active.').waitFor();
const storage = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage }, cookie: document.cookie }));
assert.ok(!JSON.stringify(storage).includes('test-key'), 'key never stored');
assert.deepEqual(storage.session, {});
assert.equal(storage.cookie, '');
await page.goto(`${BASE}/ui/dashboard`);
await page.getByText('Server: Degraded', { exact: true }).waitFor();
assert.ok((await page.getByRole('link', { name: 'Connect' }).count()) >= 1);
await shot('10-dashboard-health-only');

step('network assertions');
const all = await calls();
for (const call of all) assert.ok(READ_TOOLS.has(call.tool), `unexpected tool ${call.tool}`);
for (const request of requests) {
  assert.ok(request.url.startsWith(BASE), `external request ${request.url}`);
  assert.ok(!request.url.includes('test-key'), 'key in URL');
  if (request.url.endsWith('/health')) assert.equal(request.headers.authorization, undefined);
}
const mcpRequests = requests.filter((r) => r.url.endsWith('/mcp'));
assert.ok(mcpRequests.every((r) => r.method === 'POST' || r.method === 'DELETE'));
assert.ok(mcpRequests.every((r) => /^Bearer /.test(r.headers.authorization ?? '')), 'explicit Bearer on every MCP request');
const initialRequests = new Set(requests.filter((r) => r.url.includes('/ui/static/')).map((r) => r.url));
assert.ok(initialRequests.size < 20, `asset fan-out ${initialRequests.size}`);

const unexpected = problems.filter((p) => !/401|Failed to load resource/.test(p));
assert.deepEqual(unexpected, []);
await browser.close();
process.stdout.write('browser check passed\n');
