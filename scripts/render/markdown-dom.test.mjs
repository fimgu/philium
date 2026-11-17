// Source DOM integration checks, not native clipboard/provider acceptance.
// Runs an isolated ephemeral Vite server. Does not read/write dist or restart apps.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'vite';
import solid from 'vite-plugin-solid';
import puppeteer from 'puppeteer-core';

const fixtureId = new URL('./__markdown-fixture.jsx', import.meta.url).pathname;
const fixture = `import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { Conversation, Composer } from '/src/ui/conversation.jsx';
import '/src/styles/tokens.css';
import '/src/styles/components.css';
import '/src/styles/agents.css';
const [events, setEvents] = createSignal([]);
window.setEvents = setEvents;
window.copies = [];
Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
  writeText: async value => { if (window.failCopy) throw Error('denied'); window.copies.push(value); }
} });
render(() => <><Conversation events={events()} busy={true} /><Composer busy={true} /></>, document.getElementById('app'));
`;

test('Conversation Markdown DOM, streaming, unsafe content and clipboard matrix', async () => {
  const server = await createServer({
    configFile: false, root: new URL('../..', import.meta.url).pathname,
    plugins: [solid(), { name: 'markdown-test-fixture',
      configureServer(server) { server.middlewares.use('/markdown-test', (_req, res) => {
        res.setHeader('Content-Type', 'text/html');
        res.end('<div id="app"></div><script type="module" src="/@markdown-fixture.jsx"></script>');
      }); },
      resolveId(id) { if (id === '/@markdown-fixture.jsx') return fixtureId; },
      load(id) { if (id === fixtureId) return fixture; }
    }], server: { port: 0, host: '127.0.0.1' }
  });
  let browser;
  try {
    await server.listen();
    browser = await puppeteer.launch({ executablePath: process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => { errors.push(error.message); console.error('DOM fixture error:', error.message); });
    const requests = [];
    page.on('request', request => { if (!request.url().startsWith('http://127.0.0.1:')) requests.push(request.url()); });
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/markdown-test`);
    await page.waitForFunction(() => typeof window.setEvents === 'function');
    const set = async (text, done = true) => {
      await page.evaluate(({ text, done }) => window.setEvents([
        { type: 'message.created', role: 'user', content: '**literal user** <img src=x>', seq: 1 },
        { type: 'tool.started', call_id: 'test', tool: 'shell_run', arguments: {}, seq: 2 },
        { type: 'tool.finished', call_id: 'test', ok: true, result: '**literal tool** <script>bad</script>', seq: 3 },
        { type: 'model.delta', delta: text, seq: 4 },
        ...(done ? [{ type: 'assistant.completed', seq: 5 }] : [])
      ]), { text, done });
      await page.waitForSelector('.assistant-markdown');
    };
    const markdown = '# Title\n\n**bold** and *em* and `inline`\n\n- one\n- two\n- [x] done\n\n~~removed~~\n\n> quote\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n```js\nconst x = "<b>";\n\n```\n\n[good](https://example.com)';
    await set(markdown);
    assert.deepEqual(await page.evaluate(() => ['h1','strong','em','code','li','blockquote','table'].map(s => !!document.querySelector('.assistant-markdown ' + s))), Array(7).fill(true));
    assert.ok(await page.$('.assistant-markdown del'));
    assert.equal(await page.$eval('.assistant-markdown input', el => el.checked && el.disabled), true);
    assert.equal(await page.$$eval('.assistant-markdown li', els => els.at(-1).textContent), 'done');
    if (process.env.MARKDOWN_SCREENSHOT) await page.screenshot({ path: process.env.MARKDOWN_SCREENSHOT, fullPage: true });
    assert.equal(await page.$eval('.msg.user', el => el.textContent), '**literal user** <img src=x>');
    assert.equal(await page.$('.msg.user strong'), null);
    await page.click('.conv-tool .row');
    assert.equal(await page.$('.conv-tool strong'), null);
    assert.ok((await page.$eval('.conv-tool', el => el.textContent)).includes('**literal tool**'));
    await page.click('[aria-label="Copy reply"]');
    assert.equal(await page.evaluate(() => window.copies.at(-1)), markdown);
    assert.equal(await page.$eval('[aria-label="Copy reply"]', el => el.textContent.trim()), 'Copied!');
    await page.click('[aria-label="Copy code"]');
    assert.equal(await page.evaluate(() => window.copies.at(-1)), 'const x = "<b>";\n\n');
    await page.evaluate(() => { window.failCopy = true; });
    await page.click('[aria-label="Copy code"]');
    assert.equal(await page.$eval('[aria-label="Copy code"]', el => el.textContent.trim()), 'Copy failed, try again');
    await page.click('[aria-label="Copy reply"]');
    assert.equal(await page.$eval('[aria-label="Copy reply"]', el => el.textContent.trim()), 'Copy failed, try again');
    await page.evaluate(() => { window.failCopy = false; });
    await page.click('[aria-label="Copy reply"]');
    assert.equal(await page.$eval('[aria-label="Copy reply"]', el => el.textContent.trim()), 'Copied!');
    await set('**stream', false);
    assert.ok(await page.$('.msg.live .caret'));
    await set('**streamed**\n\n```js\npartial', false);
    assert.equal(await page.$eval('.assistant-markdown strong', el => el.textContent), 'streamed');
    await page.click('[aria-label="Copy code"]');
    assert.equal(await page.evaluate(() => window.copies.at(-1)), 'partial');
    await set('**streamed**\n\n```js\npartial\ncomplete\n```');
    assert.equal(await page.$('.msg.live'), null);
    assert.equal(await page.$eval('pre code', el => el.textContent), 'partial\ncomplete');
    await set('```txt\r\n  exact\r\n\r\n```\r\n');
    await page.click('[aria-label="Copy code"]');
    assert.equal(await page.evaluate(() => window.copies.at(-1)), '  exact\r\n\r\n');
    await set('```txt\nsame\n```\n\n```txt\r\nsame\r\n```\r\n');
    await page.$$eval('[aria-label="Copy code"]', buttons => buttons[1].click());
    assert.equal(await page.evaluate(() => window.copies.at(-1)), 'same\r\n');
    await set('Entities: &amp; &lt;img src=x&gt; and `&amp;`');
    assert.equal(await page.$eval('.assistant-markdown', el => el.textContent), 'Entities: & <img src=x> and &amp;');
    await page.type('textarea', 'Keep this draft through view changes');
    await page.evaluate(() => { window.originalComposer = document.querySelector('textarea'); window.originalConversation = document.querySelector('.conv'); });
    await page.click('.conversation-view-toggle button:nth-child(2)');
    assert.ok(await page.$('.conv-terminal'));
    assert.equal(await page.$eval('.conversation-view-toggle button:nth-child(2)', el => el.getAttribute('aria-pressed')), 'true');
    assert.equal(await page.$eval('textarea', el => el.value), 'Keep this draft through view changes');
    assert.equal(await page.evaluate(() => document.querySelector('textarea') === window.originalComposer && document.querySelector('.conv') === window.originalConversation), true);
    assert.equal(await page.evaluate(() => localStorage.getItem('philium.conversation.view')), 'terminal');
    await page.click('.conversation-view-toggle button:first-child');
    assert.ok(await page.$('.conv-conversation'));
    assert.equal(await page.$eval('textarea', el => el.value), 'Keep this draft through view changes');
    await page.evaluate(() => window.setEvents([{type:'turn.started',turn_id:'active',seq:1},{type:'model.started',seq:2}]));
    assert.equal(await page.$eval('.conversation-activity [role="status"]', el => el.textContent), 'Waiting for model response');
    await page.evaluate(() => window.setEvents([{type:'turn.started',turn_id:'active',seq:1},{type:'model.retrying',attempt:2,max_attempts:3,seq:2}]));
    assert.equal(await page.$eval('.conversation-activity [role="status"]', el => el.textContent), 'Retrying model request (2/3)');
    await page.evaluate(() => window.setEvents([{type:'turn.started',turn_id:'active',seq:1},{type:'tool.started',call_id:'x',tool:'shell_run',arguments:{},seq:2},{type:'permission.requested',request_id:'p',action:'shell',resource:'test',seq:3}]));
    assert.equal(await page.$eval('.conversation-activity [role="status"]', el => el.textContent), 'Waiting for your approval');
    await page.click('.conversation-view-toggle button:nth-child(2)');
    assert.equal(await page.$eval('.conv-tool-toggle', el => el.getAttribute('aria-expanded')), 'true');
    await page.focus('.conv-tool-toggle');
    await page.keyboard.press('Enter');
    assert.equal(await page.$eval('.conv-tool-toggle', el => el.getAttribute('aria-expanded')), 'false');
    await page.evaluate(() => window.setEvents([{type:'turn.started',turn_id:'active',seq:1},{type:'model.delta',delta:'partial',seq:2},{type:'turn.cancelled',seq:3}]));
    assert.equal(await page.$('.conversation-activity'), null);
    assert.equal(await page.$('.caret'), null);
    await page.reload();
    await page.waitForFunction(() => typeof window.setEvents === 'function');
    assert.ok(await page.$('.conv-terminal'));
    const unsafe = '<script>window.pwned=1</script>\n\n<img src="https://tracking.invalid/pixel" onerror="window.pwned=2">\n\n[bad](javascript:alert(1)) [data](data:text/html,bad) [encoded](jav&#x61;script:alert(1))\n\n![pixel](https://tracking.invalid/pixel)\n\n[good](https://example.com)';
    await set(unsafe);
    assert.equal(await page.$('.assistant-markdown script, .assistant-markdown img, .assistant-markdown iframe'), null);
    assert.equal(await page.evaluate(() => window.pwned), undefined);
    assert.deepEqual(await page.$$eval('.assistant-markdown a', links => links.map(a => a.protocol)), ['https:']);
    assert.equal(await page.$eval('.assistant-markdown a', el => el.rel), 'noopener noreferrer');
    assert.deepEqual(requests, []);
    assert.deepEqual(errors, []);
    await set('# Long history\n\n' + 'Paragraph in restored history.\n\n'.repeat(80) + '~~~js\nconst value = 1;\n~~~');
    await page.evaluate(() => { const conv = document.querySelector('.conv'); conv.style.height = '300px'; conv.style.flex = 'none'; conv.scrollTop = conv.scrollHeight; });
    assert.equal(await page.evaluate(() => {
      const conv = document.querySelector('.conv').getBoundingClientRect();
      const controls = document.querySelector('.conversation-view-control').getBoundingClientRect();
      return controls.top >= conv.top && controls.bottom <= conv.bottom;
    }), true, 'view switch stays visible at bottom of long history');
    assert.match(await page.$eval('.assistant-markdown code', el => getComputedStyle(el).fontFamily), /monospace|Menlo/);
    await page.click('.conversation-view-toggle button:first-child');
    await page.evaluate(() => window.setEvents([
      {type:'tool.started', call_id:'shell-ui', tool:'mcp_philium_shell_run', arguments:{command:'printf hello'}, seq:1},
      {type:'tool.finished', call_id:'shell-ui', ok:true, result:{content:[{type:'text',text:JSON.stringify({output:'hello\n',running:true,exit_code:null})}]}, seq:2}
    ]));
    await page.waitForSelector('.tool-command');
    assert.equal(await page.$eval('.tool-command', el => el.textContent), '$ printf hello');
    assert.match(await page.$eval('.conv-tool-toggle', el => el.textContent), /Still running/);
    assert.match(await page.$eval('.tool-output-preview', el => el.textContent), /hello/);
    await page.click('.conv-tool-toggle');
    assert.equal(await page.$eval('.tool-raw', el => el.open), false);
    assert.equal(await page.$eval('.conv-tool > .conv-pre', el => el.textContent), 'hello\n');
  } catch (error) { console.error(error); throw error; }
  finally { await browser?.close(); server.httpServer?.closeAllConnections(); await server.close(); }
});
