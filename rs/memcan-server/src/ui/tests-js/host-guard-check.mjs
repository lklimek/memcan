// Dev-only browser scenario: the session bootstrap is refused by the backend's Host guard on a
// plain-HTTP deployment that allows insecure HTTP. Expected: the page explains the Host problem
// and does not claim HTTPS is required (MEMCAN_WEBUI_ALLOW_INSECURE_HTTP is already set).
// Usage: PLAYWRIGHT_CORE=/path/to/playwright-core node host-guard-check.mjs http://127.0.0.1:8199

import assert from 'node:assert/strict';

const { chromium } = await import(`${process.env.PLAYWRIGHT_CORE}/index.mjs`);
const BASE = process.argv[2] ?? 'http://127.0.0.1:8199';
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || undefined });
// A non-loopback host name over plain HTTP, as with http://agentic.lan:8190.
const context = await browser.newContext();
const page = await context.newPage();
await page.route('**/ui/auth/session', (route) =>
  route.fulfill({
    status: 403,
    contentType: 'application/json',
    body: JSON.stringify({ error: 'host_not_allowed', message: 'This host name is not allowed.', retryAfter: null }),
  }),
);

try {
  await page.goto(`${BASE}/ui/login`);
  await page.getByRole('heading', { name: 'Sign in to MemCan' }).waitFor();
  await page.waitForTimeout(500);
  const text = await page.locator('main').innerText();
  console.log(`main: ${text.replace(/\s+/g, ' ').slice(0, 500)}`);
  assert.ok(!/HTTPS required/.test(text), 'a Host refusal must not be reported as missing HTTPS');
  console.log('PASS');
} finally {
  await browser.close();
}
