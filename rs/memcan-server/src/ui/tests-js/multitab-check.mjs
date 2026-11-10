// Dev-only browser scenario: a second tab after the first tab signs in again.
// Usage: PLAYWRIGHT_CORE=/path/to/playwright-core node multitab-check.mjs http://127.0.0.1:8199
// Expected (design §12: "Other tabs clear on next 401"): the stale tab either adopts the new
// session silently or shows the session-expired notice, never the network-policy denial.

import assert from 'node:assert/strict';

const { chromium } = await import(`${process.env.PLAYWRIGHT_CORE}/index.mjs`);
const BASE = process.argv[2] ?? 'http://127.0.0.1:8199';
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined });
const context = await browser.newContext();
const tabA = await context.newPage();
const tabB = await context.newPage();

const signIn = async (page) => {
  await page.getByLabel('Username').fill('viewer');
  await page.getByLabel('Password', { exact: true }).fill('test-pass');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await page.waitForURL(/\/ui\/tasks/);
};

try {
  await tabA.goto(`${BASE}/ui/login`);
  await signIn(tabA);
  await tabB.goto(`${BASE}/ui/tasks`);
  await tabB.getByRole('button', { name: 'Sign out' }).waitFor();

  // Tab A signs out and back in: the shared cookie now names a new session with a new CSRF token.
  await tabA.getByRole('button', { name: 'Sign out' }).click();
  await tabA.waitForURL(/\/ui\/login/);
  await signIn(tabA);

  // Tab B still holds the old CSRF token in memory.
  await tabB.getByRole('button', { name: 'Refresh' }).click();
  await tabB.waitForTimeout(1500);
  const text = await tabB.locator('main').innerText();
  const url = tabB.url();
  console.log(`tab B url: ${url}\ntab B main: ${text.replace(/\s+/g, ' ').slice(0, 400)}`);
  assert.ok(!/network policy|IP allowlist|ask the operator/i.test(text), 'stale tab must not report a policy denial');
  console.log('PASS');
} finally {
  await browser.close();
}
