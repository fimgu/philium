import test from "node:test";
import assert from "node:assert/strict";
import { matchShortcut, shortcutLabel, cycle, SHORTCUTS } from "./shortcuts.js";

test("maps CLI-style keys per platform", () => {
  assert.equal(matchShortcut({ key: "n", metaKey: true }, true), "agent.new");
  assert.equal(matchShortcut({ key: "n", ctrlKey: true }, false), "agent.new");
  assert.equal(matchShortcut({ key: "n", ctrlKey: true }, true), null);
  assert.equal(matchShortcut({ key: "]", metaKey: true }, true), "agent.next");
  assert.equal(matchShortcut({ key: "D", metaKey: true, shiftKey: true }, true), "agent.grid");
  assert.equal(matchShortcut({ key: "d", metaKey: true }, true), "agent.split");
  assert.equal(matchShortcut({ key: "ArrowRight", metaKey: true, altKey: true }, true), "pane.next");
  assert.equal(matchShortcut({ key: "f", metaKey: true }, true), "agent.search");
  assert.equal(matchShortcut({ key: "f" }, true), null);
});

test("labels and ids are unique", () => {
  assert.equal(shortcutLabel("agent.grid", true), "⌘⇧D");
  assert.equal(shortcutLabel("agent.grid", false), "Ctrl+Shift+D");
  assert.equal(new Set(SHORTCUTS.map((s) => s.keys)).size, SHORTCUTS.length);
});

test("cycle wraps", () => {
  assert.equal(cycle(-1, 3, 1), 0);
  assert.equal(cycle(-1, 3, -1), 2);
  assert.equal(cycle(2, 3, 1), 0);
  assert.equal(cycle(0, 0, 1), -1);
});
