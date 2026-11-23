import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

test('native CSP permits local attachment previews and opt-in HTTPS images, not remote scripts', () => {
  const config = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  const directives = Object.fromEntries(config.app.security.csp.split(';').map(part => {
    const [name, ...values] = part.trim().split(/\s+/);
    return [name, values];
  }));
  // The Markdown renderer controls consent before assigning a remote img src.
  assert.deepEqual(directives['img-src'], ["'self'", 'data:', 'blob:', 'https:']);
  assert.equal(directives['script-src'], undefined, 'scripts fall back to default-src self');
  assert.deepEqual(directives['default-src'], ["'self'"]);
  assert.deepEqual(directives['connect-src'], ['ipc:', 'http://ipc.localhost']);
});
