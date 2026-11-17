import test from 'node:test';
import assert from 'node:assert/strict';
import { conversationActivity as activity } from '../../src/lib/conversation-activity.js';
const start = { type: 'turn.started', turn_id: 'one' };
const tool = { type: 'tool.started', call_id: 'a', tool: 'mcp_philium_shell_run' };
test('idle and historical views never show live activity', () => {
  assert.equal(activity([start], false), null);
  assert.equal(activity([start], true, true), null);
});
test('model lifecycle labels observed activity without claiming thinking', () => {
  assert.equal(activity([start], true).label, 'Waiting for model response');
  assert.equal(activity([start, { type: 'model.delta', delta: 'hello' }], true).kind, 'responding');
  assert.equal(activity([start, { type: 'model.finished' }], true).kind, 'working');
});
test('concurrent tool counts and permission precedence track matching ids', () => {
  const events = [start, tool, { ...tool, call_id: 'b' }];
  assert.equal(activity(events, true).label, 'Running 2 tools');
  events.push({ type: 'permission.requested', request_id: 'p' });
  assert.equal(activity(events, true).kind, 'permission');
  events.push({ type: 'permission.resolved', request_id: 'other' });
  assert.equal(activity(events, true).kind, 'permission');
  events.push({ type: 'permission.resolved', request_id: 'p' }, { type: 'tool.finished', call_id: 'a' });
  assert.equal(activity(events, true).label, 'Running shell_run');
  events.push({ type: 'tool.finished', call_id: 'b' });
  assert.equal(activity(events, true).kind, 'working');
});
test('terminal outcomes clear stale tools even when busy state lags', () => {
  for (const type of ['assistant.completed', 'turn.failed', 'turn.cancelled']) {
    const events = [start, tool, { type }];
    assert.equal(activity(events, true), null);
    assert.equal(activity([...events, { type: 'turn.started', turn_id: 'two' }], true).key, 'two:waiting');
  }
});
test('tool output does not restart activity phase observation timer', () => {
  assert.equal(activity([start, tool], true).key, activity([start, tool, { type: 'tool.output', call_id: 'a', data: '.' }], true).key);
});
test('actual retry events expose attempt but no fabricated countdown or error text', () => {
  const state = activity([start, { type: 'model.retrying', attempt: 2, max_attempts: 3, error: 'private detail' }], true);
  assert.equal(state.label, 'Retrying model request (2/3)');
  assert.equal(JSON.stringify(state).includes('private detail'), false);
  assert.equal(activity([start, { type: 'model.retrying' }], true).label, 'Retrying model request');
});
