import test from "node:test";
import assert from "node:assert/strict";
import { modelFamily, splitLatest } from "./model-list.js";

const m = (id) => ({ ref: `p/${id}`, modelId: id });
const ids = (list) => list.map((x) => x.modelId);

test("a family is the id without its version numbers", () => {
  assert.equal(modelFamily("claude-fable-5-2"), "claude-fable");
  assert.equal(modelFamily("claude-opus-4-5-20251101"), "claude-opus");
  assert.equal(modelFamily("gpt-5.2-codex"), "gpt-codex");
  assert.equal(modelFamily("gpt-5-mini"), "gpt-mini");
  assert.notEqual(modelFamily("claude-sonnet-5-5"), modelFamily("claude-haiku-5-5"));
});

test("the newest model of each family comes first, in the provider's order", () => {
  const claude = ["claude-fable-5-2", "claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-1", "claude-opus-5",
    "claude-sonnet-5", "claude-fable-5", "claude-opus-4-8", "claude-haiku-4-5-20251001"].map(m);
  const { latest, older } = splitLatest(claude);
  assert.deepEqual(ids(latest), ["claude-fable-5-2", "claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"]);
  assert.equal(older.length, 6);
  assert.equal(older[0].modelId, "claude-fable-5-1");
});

test("the model in use is shown even when it is an older one", () => {
  const list = ["claude-opus-5-5", "claude-opus-5", "claude-opus-4-8"].map(m);
  const { latest, older } = splitLatest(list, { keep: ["p/claude-opus-4-8"] });
  assert.deepEqual(ids(latest), ["claude-opus-5-5", "claude-opus-4-8"]);
  assert.deepEqual(ids(older), ["claude-opus-5"]);
});

test("a provider with many families shows a short list, the rest stay one click away", () => {
  const many = Array.from({ length: 40 }, (_, i) => m(`vendor${String.fromCharCode(97 + (i % 26))}${i > 25 ? "x" : ""}-model`));
  const { latest, older } = splitLatest(many, { cap: 6 });
  assert.equal(latest.length, 6);
  assert.equal(latest.length + older.length, 40);
});
