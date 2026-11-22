import test from "node:test";
import assert from "node:assert/strict";
import { memoryTitle, fileChips, relTime, modelsSummary, categoryLabel } from "./memory-format.js";

test("memoryTitle falls back to first content line", () => {
  assert.equal(memoryTitle({ title: "T", content: "x" }), "T");
  assert.equal(memoryTitle({ title: null, content: "\n first \nsecond" }), "first");
});

test("fileChips parses JSON and caps", () => {
  assert.deepEqual(fileChips('["a","b","c","d","e"]'), { shown: ["a", "b", "c"], more: 2 });
  assert.deepEqual(fileChips("not json"), { shown: [], more: 0 });
  assert.deepEqual(fileChips(null), { shown: [], more: 0 });
});

test("relTime", () => {
  const now = Date.parse("2026-01-01T12:00:00Z");
  assert.equal(relTime("2026-01-01T09:00:00Z", now), "3h ago");
  assert.equal(relTime("2026-01-01T11:59:30Z", now), "just now");
  assert.equal(relTime(null, now), "");
});

test("modelsSummary states", () => {
  const r = (state, extra = {}) => ({ state, bytes_done: 0, bytes_total: 50, ...extra });
  assert.equal(modelsSummary({ embed: r("ready"), rerank: r("ready") }).text, "Ready");
  assert.equal(modelsSummary({ embed: r("ready", { bytes_done: 50 }), rerank: r("downloading", { bytes_done: 34 }) }).text, "Downloading 84%");
  const f = modelsSummary({ embed: r("failed", { error: "HTTP 503" }), rerank: r("missing") });
  assert.equal(f.text, "Failed: HTTP 503");
  assert.equal(f.action, "Retry");
  assert.equal(modelsSummary({ embed: r("missing"), rerank: r("missing") }).action, "Download");
  assert.equal(categoryLabel("gotcha"), "Gotcha");
});
