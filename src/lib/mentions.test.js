import test from "node:test";
import assert from "node:assert/strict";
import { mentionAt, insertMention, rankSuggestions, symbolSlice, contextBlock } from "./mentions.js";

test("detects @token at caret, ignores emails", () => {
  assert.deepEqual(mentionAt("look at @src/ma", 15), { start: 8, query: "src/ma" });
  assert.deepEqual(mentionAt("@", 1), { start: 0, query: "" });
  assert.equal(mentionAt("mail a@b.com", 12), null);
  assert.equal(mentionAt("@done and more", 14), null);
});

test("inserts the picked label", () => {
  const at = mentionAt("see @ma now", 7);
  assert.deepEqual(insertMention("see @ma now", at, "src/main.jsx"), { text: "see @src/main.jsx  now", caret: 18 });
});

test("ranks basename prefix before path match", () => {
  const r = rankSuggestions([{ path: "src/lib/nav.js" }, { path: "src/navy/x.js" }, { path: "a.js" }], [{ name: "navigate", path: "n.js", line: 3 }], "nav");
  assert.deepEqual(r.map((x) => x.label), ["src/lib/nav.js", "navigate", "src/navy/x.js"]);
});

test("context block labels sources and caps size", () => {
  assert.equal(contextBlock([]), "");
  const s = contextBlock([{ kind: "file", path: "a.js", text: "x".repeat(50_000) }, { kind: "symbol", path: "b.rs", name: "f", start: 2, end: 4, text: "fn f" }]);
  assert.match(s, /source="a.js" truncated="true"/);
  assert.match(s, /source="b.rs:2-4 \(f\)"/);
  assert.deepEqual(symbolSlice("a\nb\nc", 2), { text: "b\nc", start: 2, end: 3 });
});
