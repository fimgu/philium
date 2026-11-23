import test from 'node:test';
import assert from 'node:assert/strict';
import puppeteer from 'puppeteer-core';
import { startTestVite } from './test-vite.mjs';

const root = new URL('../..', import.meta.url).pathname;

test('only the desktop webview starts the workspace UI and its IPC calls', async () => {
  const server = await startTestVite({ root });
  let browser;
  try {
    browser = await puppeteer.launch({ executablePath: process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: 'new', args: ['--no-sandbox'] });
    const { url } = server;
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

test('fixture mode renders Stats and exposes approvals even when saved history fails', async () => {
  const server = await startTestVite({ root, mode: 'fixture' });
  let browser;
  try {
    browser = await puppeteer.launch({ executablePath: process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, args: ['--no-sandbox'] });
    const { url } = server;
    const page = (await browser.pages())[0];
    await page.evaluateOnNewDocument(() => {
      const screen = new URLSearchParams(location.search).get('screen');
      localStorage.setItem('route', screen);
      localStorage.setItem('agents.viewMode', 'split');
      const agents = ['otter', 'lynx'].map((name) => ({ id: name, name, role: 'worker', state: 'running', live: true }));
      const requests = Object.fromEntries(agents.map(({ id }) => [id, [
        { request_id: id + '-permission', action: 'mcp', resource: 'philium/browser_snapshot' },
      ]]));
      window.approvals = [];
      window.__philiumFixtureBridge = {
        transformCallback: () => 1,
        unregisterCallback() {},
        async invoke(cmd, args = {}) {
          if (cmd !== 'rpc') return null;
          switch (args.method) {
            case 'agents.list': return agents;
            case 'agents.get': return agents.find(({ id }) => id === args.args.id);
            case 'agents.pending_permissions': return structuredClone(requests);
            case 'agents.events': throw new Error('Fixture saved history unavailable');
            case 'agents.permission':
              window.approvals.push(args.args);
              delete requests[args.args.id];
              return {};
            default: return args.method.endsWith('.list') || args.method === 'access.pending' ? [] : null;
          }
        },
      };
    });
    await page.goto(url + '?screen=stats');
    await page.waitForSelector('.fixture-notice');
    await page.waitForFunction(() => document.body.innerText.includes('Provider usage'));
    assert.equal(await page.$('.desktop-notice'), null);
    assert.equal(await page.evaluate(() => !!window.__TAURI_INTERNALS__), false);
    await page.goto(url + '?screen=agents');
    await page.waitForSelector('.history-error');
    await page.waitForFunction(() => document.querySelector('.fleet-sub-telemetry')?.textContent.includes('2 need you'));
    assert.equal(await page.$eval('[title="2 agents waiting for approval"]', (el) => el.dataset.count), '2');
    await page.click('[data-agent="otter"]');
    await page.waitForSelector('.agent-approvals .perm');
    await page.evaluate(() => [...document.querySelectorAll('.agent-approvals button')]
      .find((button) => button.textContent.trim() === 'Allow once').click());
    await page.waitForFunction(() => document.querySelector('.fleet-sub-telemetry')?.textContent.includes('1 need you'));
    assert.deepEqual(await page.evaluate(() => window.approvals), [
      { id: 'otter', request_id: 'otter-permission', decision: 'approve', scope: 'once' },
    ]);
  } finally {
    await browser?.close();
    await server.close();
  }
});
