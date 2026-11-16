import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { createRoot, createSignal, createMemo, createEffect, onCleanup } from 'solid-js/dist/solid.js';

const conversation = readFileSync(new URL('../../src/ui/conversation.jsx', import.meta.url), 'utf8');
const agents = readFileSync(new URL('../../src/screens/agents.jsx', import.meta.url), 'utf8');
// Execute the actual pure event reducer without requiring a browser JSX loader.
const fold = vm.runInNewContext(`(${conversation.slice(conversation.indexOf('export function fold'), conversation.indexOf('// MCP tool names')).replace('export ', '')})`);
const start = { type: 'tool.started', call_id: 'a', tool: 'shell_run', seq: 1 };

test('both conversation callers pass actual busy state and effort pending copy is neutral', () => {
  const chat = readFileSync(new URL('../../src/screens/chat.jsx', import.meta.url), 'utf8');
  for (const source of [chat, agents]) assert.match(source, /<Conversation\b[^>]*\bbusy=\{busy\(\)\}/);
  assert.match(conversation, /Updating effort…/);
  assert.doesNotMatch(conversation, /Saving effort/);
});

test('live clocks survive event refolds, reset silence on output and clean up', () => {
  let now = 0, dispose, clocks, update;
  const timers = new Set();
  const setup = conversation.slice(conversation.indexOf('function Tool(props) {') + 'function Tool(props) {'.length,
    conversation.indexOf('  return (', conversation.indexOf('function Tool(props) {')));
  const context = { createSignal, createMemo, createEffect, onCleanup, Date: { now: () => now },
    setInterval: (fn) => { timers.add(fn); return fn; }, clearInterval: (fn) => timers.delete(fn) };
  vm.createContext(context);
  const makeClocks = vm.runInContext(`(props) => { ${setup}\nreturn { elapsed, silent }; }`, context);
  createRoot((cleanup) => {
    dispose = cleanup;
    const [item, setItem] = createSignal({ seq: 1, running: true, out: '' });
    update = setItem;
    clocks = makeClocks({ get item() { return item(); }, active: true });
  });
  now = 31000;
  for (const tick of [...timers]) tick();
  assert.equal(clocks.elapsed(), 31);
  assert.equal(clocks.silent(), 31);
  update({ seq: 1, running: true, out: '' });
  assert.equal(clocks.elapsed(), 31);
  assert.equal(clocks.silent(), 31);
  update({ seq: 1, running: true, out: 'new output' });
  assert.equal(clocks.elapsed(), 31);
  assert.equal(clocks.silent(), 0);
  update({ seq: 1, running: false, out: 'new output' });
  assert.equal(timers.size, 0);
  dispose();
  assert.equal(timers.size, 0);
});

test('only a tool completion event supplies success and duration', () => {
  for (const type of ['turn.cancelled', 'turn.failed', 'assistant.completed']) {
    const [tool] = fold([start, { type, error: 'failed' }]);
    assert.equal(tool.running, true);
    assert.equal(tool.inactive, true);
    assert.equal(tool.ok, undefined);
    assert.equal(tool.ms, undefined);
  }
  const [tool] = fold([start, { type: 'tool.output', call_id: 'a', data: 'working' },
    { type: 'tool.finished', call_id: 'a', ok: false, duration_ms: 1200, result: 'error' }]);
  assert.equal(tool.running, false);
  assert.equal(tool.ok, false);
  assert.equal(tool.ms, 1200);
  assert.equal(tool.out, 'working');
});

test('a later turn does not reactivate interrupted tools', () => {
  const items = fold([start, { type: 'turn.cancelled' }, { type: 'message.created', role: 'user', content: 'retry' },
    { ...start, seq: 4, call_id: 'b' }]);
  assert.equal(items[0].inactive, true);
  assert.equal(items.at(-1).inactive, undefined);
});

test('effort presets retain backend mappings without speed or cost promises', () => {
  const presets = vm.runInNewContext(conversation.slice(conversation.indexOf('export const SPEEDS'), conversation.indexOf('export function Composer'))
    .replaceAll('export ', '') + '\n({ SPEEDS, speedEffort, effortSpeed })');
  assert.equal(presets.speedEffort('fast'), 'low');
  assert.equal(presets.speedEffort('normal'), 'default');
  assert.equal(presets.speedEffort('deep'), 'high');
  assert.equal(presets.effortSpeed('unsupported'), 'normal');
  assert.match(conversation, /provider and model support varies/);
  assert.doesNotMatch(conversation, /quickest and cheapest|Low reasoning effort: quick answers/);
});

test('stale effort saves cannot update another agent or clear its pending state', async () => {
  let resolve, saved, pending = false;
  const context = { props: { id: 'first' }, effortRequest: 0, effortPending: () => pending, live: () => true,
    setEffortPending: (value) => { pending = value; }, setEffort: (value) => { saved = value; },
    speedEffort: () => 'high', rpc: () => new Promise((done) => { resolve = done; }), toast: () => assert.fail('stale error toast') };
  vm.createContext(context);
  const change = agents.slice(agents.indexOf('  const changeEffort ='), agents.indexOf('  const live =', agents.indexOf('  const changeEffort =')));
  vm.runInContext(`${change}\nthis.changeEffort = changeEffort;`, context);
  const request = context.changeEffort('deep');
  assert.equal(pending, true);
  context.props.id = 'second';
  context.effortRequest++;
  resolve('high');
  await request;
  assert.equal(saved, undefined);
  assert.equal(pending, true);
});
