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
  assert.equal(relTime("2026-01-01 09:00:00", now), "3h ago");
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

test("dreamSummary reads SQLite UTC timestamps and budget", async () => {
  const { dreamSummary } = await import("./memory-format.js");
  const now = Date.parse("2026-11-23T12:00:00Z");
  assert.equal(dreamSummary(null).text, "");
  assert.equal(dreamSummary({ enabled: true, last_dreamed_at: "2026-11-23 10:00:00", daily_tokens: 50000, used_today: 10 }, now).text, "Last dreamed 2h ago");
  assert.equal(dreamSummary({ enabled: true, daily_tokens: 100, used_today: 100 }, now).text, "Not dreamed yet · daily budget used");
  assert.equal(dreamSummary({ enabled: true, daily_tokens: 100, used_today: 100 }, now).canRun, false);
  assert.equal(dreamSummary({ enabled: false }).text, "Dreaming off");
  assert.equal(dreamSummary({ enabled: true, running: true }).canRun, false);
});

test("dreamChanges labels actions and undo eligibility", async () => {
  const { dreamChanges } = await import("./memory-format.js");
  const c = dreamChanges([
    { id: 1, kind: "correct", summary: "Corrected X", reason: "r" },
    { id: 2, kind: "flag", summary: "Flagged Y" },
    { id: 3, kind: "merge", summary: "Merged", undone_at: "2026-11-23 10:00:00" },
  ]);
  assert.deepEqual(c.map((x) => [x.verb, x.undoable, x.undone]), [["Corrected", true, false], ["Flagged", false, false], ["Merged", false, true]]);
  assert.deepEqual(dreamChanges(null), []);
});
