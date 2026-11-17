import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { transformSync } from '@babel/core';
import { createConversationView, CONVERSATION_VIEW_KEY, conversationView, setConversationView } from '../../src/lib/conversation-view.js';

function memoryStorage(value) {
  const entries = new Map(value === undefined ? [] : [[CONVERSATION_VIEW_KEY, value]]);
  return { getItem: key => entries.get(key) ?? null, setItem: (key, value) => entries.set(key, value), entries };
}

test('default is terminal and valid stored preferences restore', () => {
  for (const value of [undefined, 'conversation', 'terminal']) {
    const storage = memoryStorage(value);
    const [view] = createConversationView(() => storage);
    assert.equal(view(), value ?? 'terminal');
  }
});

test('invalid stored values fall back without executing or overwriting them', () => {
  for (const invalid of ['', 'Terminal', 'shell', 'null', '{"mode":"terminal"}', '<script>bad</script>']) {
    const storage = memoryStorage(invalid);
    const [view] = createConversationView(() => storage);
    assert.equal(view(), 'terminal');
    assert.equal(storage.entries.get(CONVERSATION_VIEW_KEY), invalid);
  }
});

test('selection persists and a new store restores it', () => {
  const storage = memoryStorage();
  const [view, setView] = createConversationView(() => storage);
  assert.equal(setView('terminal'), 'terminal');
  assert.equal(view(), 'terminal');
  assert.equal(storage.entries.get(CONVERSATION_VIEW_KEY), 'terminal');
  assert.equal(createConversationView(() => storage)[0](), 'terminal');
  setView('conversation');
  assert.equal(view(), 'conversation');
  assert.equal(storage.entries.get(CONVERSATION_VIEW_KEY), 'conversation');
});

test('invalid setter inputs leave valid preference and storage unchanged', () => {
  const storage = memoryStorage('terminal');
  const [view, setView] = createConversationView(() => storage);
  for (const invalid of [undefined, null, {}, 1, 'shell', () => 'conversation']) setView(invalid);
  assert.equal(view(), 'terminal');
  assert.equal(storage.entries.get(CONVERSATION_VIEW_KEY), 'terminal');
});

test('missing, blocked getter and blocked reader storage do not prevent session choice', () => {
  for (const access of [() => undefined, () => { throw Error('blocked'); }, () => ({ getItem() { throw Error('read blocked'); } })]) {
    const [view, setView] = createConversationView(access);
    assert.equal(view(), 'terminal');
    assert.doesNotThrow(() => setView('conversation'));
    assert.equal(view(), 'conversation');
  }
});

test('quota failure keeps in-memory selection and does not mutate previous persisted preference', () => {
  const storage = { getItem: () => 'conversation', setItem() { throw Error('quota'); } };
  const [view, setView] = createConversationView(() => storage);
  setView('terminal');
  assert.equal(view(), 'terminal');
  assert.equal(createConversationView(() => storage)[0](), 'conversation');
});

test('shared exported signal updates for all readers', () => {
  const previous = conversationView();
  const first = conversationView, second = conversationView;
  try {
    setConversationView('terminal');
    assert.equal(first(), 'terminal');
    assert.equal(second(), 'terminal');
  } finally { setConversationView(previous); }
});

test('toggle compiles with accessible pressed buttons and transcript-not-shell explanation', () => {
  const filename = new URL('../../src/ui/conversation-view.jsx', import.meta.url);
  const source = readFileSync(filename, 'utf8');
  assert.match(source, /role="group" aria-label="Conversation view" aria-describedby=\{hintId\}/);
  assert.equal((source.match(/type="button" aria-pressed=/g) ?? []).length, 2);
  assert.match(source, /Terminal is a dense transcript, not a command shell/);
  const result = transformSync(source, { filename: filename.pathname, presets: [['babel-preset-solid', { generate: 'dom' }]], babelrc: false, configFile: false });
  assert.ok(result.code.includes('aria-pressed'));
});
