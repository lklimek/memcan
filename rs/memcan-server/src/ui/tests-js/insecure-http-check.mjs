// Dev-only browser scenario: plain-HTTP warning and password form on a non-loopback host name.
// Usage: PLAYWRIGHT_CORE=/path/to/playwright-core node insecure-http-check.mjs http://127.0.0.1:8199
// The mock must allow plain-HTTP sign-in (no MOCK_SECURE). memcan.test resolves to 127.0.0.1.

import assert from 'node:assert/strict';

const { chromium } = await import(`${process.env.PLAYWRIGHT_CORE}/index.mjs`);
const port = new URL(process.argv[2] ?? 'http://127.0.0.1:8199').port;
const BASE = `http://memcan.test:${port}`;
const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM || undefined,
  args: ['--host-resolver-rules=MAP memcan.test 127.0.0.1'],
});
const step = (name) => process.stdout.write(`• ${name}\n`);
const warning = (page) => page.locator('.banner.warning', { hasText: 'Unencrypted connection' });
const password = (page) => page.getByLabel('Password', { exact: true });

try {
  step('allowed plain HTTP: one warning before and after sign-in');
  {
    const page = await browser.newPage();
    await page.goto(`${BASE}/ui/login`);
    await password(page).waitFor();
    assert.equal(await warning(page).count(), 1);
    await page.getByLabel('Username').fill('viewer');
    await password(page).fill('test-pass');
    await page.getByRole('button', { name: 'Sign in over HTTP' }).click();
    await page.waitForURL(/\/ui\/tasks/);
    await page.getByRole('button', { name: 'Sign out' }).waitFor();
    assert.equal(await warning(page).count(), 1, 'warning stays while signed in');
    await page.close();
  }

  step('unchecked sign-in status: warning shown, no password form, real cause only');
  {
    const page = await browser.newPage();
    await page.route('**/ui/auth/session', (route) => route.fulfill({ status: 503, body: 'down' }));
    await page.goto(`${BASE}/ui/login`);
    await page.getByRole('heading', { name: 'Sign-in unavailable' }).waitFor({ timeout: 15000 });
    assert.equal(await password(page).count(), 0, 'no password over HTTP before the server confirmed it allows that');
    assert.equal(await warning(page).count(), 1);
    const text = await page.locator('main').innerText();
    assert.match(text, /could not be reached or is unavailable/);
    assert.doesNotMatch(text, /HTTPS required/);
    await page.close();
  }

  step('server refuses plain HTTP: HTTPS required, no password form');
  {
    const page = await browser.newPage();
    await page.route('**/ui/auth/session', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ authenticated: false, insecureHttpAllowed: false }) }),
    );
    await page.goto(`${BASE}/ui/login`);
    await page.getByRole('heading', { name: 'HTTPS required' }).waitFor();
    assert.equal(await password(page).count(), 0);
    assert.equal(await warning(page).count(), 0, 'the HTTPS explanation replaces the warning');
    await page.close();
  }
  console.log('PASS');
} finally {
  await browser.close();
}
