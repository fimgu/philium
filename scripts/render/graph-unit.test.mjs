import test from 'node:test';
import assert from 'node:assert/strict';
import { wheelZoomFactor, zoomCamera } from '../../src/lib/graph-navigation.js';
import { formatCodeContext } from '../../src/lib/code-context-format.js';
import { draftFor, appendAgentDraft } from '../../src/lib/drafts.js';

test('trackpad, wheel, line and page scrolling always zoom', () => {
  for (const deltaMode of [0, 1, 2]) {
    for (const deltaY of [0.5, 12, 120]) {
      assert.ok(wheelZoomFactor({ deltaMode, deltaY, deltaX: 2 }) < 1);
      assert.ok(wheelZoomFactor({ deltaMode, deltaY: -deltaY, deltaX: 2 }) > 1);
    }
  }
  assert.equal(wheelZoomFactor({ deltaMode: 0, deltaY: 0, deltaX: 12 }), 1);
  assert.equal(wheelZoomFactor({ deltaY: NaN }), 1);
});

test('zoom preserves the world point under cursor and clamps scale', () => {
  const camera = { x: 30, y: -40, k: 1.2 }, cursor = { x: 111, y: -90 };
  const next = zoomCamera(camera, 1.5, cursor.x, cursor.y);
  assert.ok(Math.abs((cursor.x - camera.x) / camera.k - (cursor.x - next.x) / next.k) < 1e-9);
  assert.ok(Math.abs((cursor.y - camera.y) / camera.k - (cursor.y - next.y) / next.k) < 1e-9);
  assert.equal(zoomCamera(camera, 1e10, 0, 0).k, 8);
  assert.equal(zoomCamera(camera, 1e-10, 0, 0).k, 0.04);
});

test('code context includes exact identity and bounded quoted selection', () => {
  const text = formatCodeContext({ intent: 'explain', path: 'src/task.py', start_line: 4, end_line: 8,
    repo_id: 'r1', worktree_id: 'w1', base_commit: 'abc', revision: 'working-tree:123', kind: 'source', selection: '```\nprint(42)' });
  for (const fragment of ['src/task.py:4-8', 'Repository: r1', 'Worktree: w1', 'Comparison base: abc', 'Revision: working-tree:123', '````', 'Do not modify files.']) assert.ok(text.includes(fragment));
  const long = formatCodeContext({ path: 'x', selection: 'x'.repeat(20000) });
  assert.ok(long.includes('truncated'));
  assert.ok(long.length < 17000);
  assert.ok(formatCodeContext({ intent: 'change', path: 'x' }).includes('ask what change I want before editing'));
});

test('agent handoff appends draft, isolates peers, preserves attachments and pending state', () => {
  const one = draftFor('agent:test-one'), two = draftFor('agent:test-two');
  one.setText('existing draft'); one.setImages([{ name: 'existing.png' }]); one.setSending(true);
  appendAgentDraft('test-one', 'selected code');
  assert.equal(one.text(), 'existing draft\n\nselected code');
  assert.deepEqual(one.images(), [{ name: 'existing.png' }]);
  assert.equal(one.sending(), true);
  assert.equal(draftFor('agent:test-one'), one);
  assert.equal(two.text(), '');
  assert.equal(two.sending(), false);
});
