import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'vite';
import puppeteer from 'puppeteer-core';

const root = new URL('../..', import.meta.url).pathname;

test('only the desktop webview starts the workspace UI and its IPC calls', async () => {
  const server = await createServer({ root, server: { host: '127.0.0.1', port: 0, strictPort: true } });
  let browser;
  try {
    await server.listen();
    browser = await puppeteer.launch({ executablePath: process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: 'new', args: ['--no-sandbox'] });
    const url = `http://127.0.0.1:${server.httpServer.address().port}/`;
    const page = (await browser.pages())[0];
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.evaluateOnNewDocument(() => {
      const label = new URLSearchParams(location.search).get('webview');
      window.calls = [];
      if (!label) return;
      window.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: 'main' }, currentWebview: { label } },
        transformCallback: () => 1,
        async invoke(cmd, args) {
          window.calls.push(args?.method ?? cmd);
          if (label !== 'main') throw new Error('Command not allowed by ACL');
          if (cmd === 'plugin:event|listen') return 1;
          return args?.method?.endsWith('list') ? [] : null;
        },
      };
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    });
    for (const label of ['', 'browser-preview', 'main']) {
      await page.goto(`${url}?webview=${label}`);
      if (label === 'main') {
        await page.waitForSelector('.page-head h1');
        assert.equal(await page.$eval('.page-head h1', (el) => el.textContent), 'Home');
        assert.ok(await page.evaluate(() => window.calls.includes('app.status')));
        assert.equal(await page.$('.desktop-notice'), null);
      } else {
        assert.equal(await page.$eval('main h1', (el) => el.textContent), 'Open Philium in the desktop app');
        assert.equal(await page.$('.sidebar'), null);
        assert.deepEqual(await page.evaluate(() => window.calls), []);
      }
      assert.deepEqual(errors, []);
    }
  } finally {
    await browser?.close();
    await server.close();
  }
});
