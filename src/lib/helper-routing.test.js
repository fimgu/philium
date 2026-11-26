import test from "node:test";
import assert from "node:assert/strict";
import { defaultConfig, normaliseConfig, validateConfig, usageLabel, pickableProviders, scopeLabel, jobLabel, jobModelOptions, previewLine } from "./helper-routing.js";

const provs = [
  { id: "a", name: "A", trusted: true, enabled: true, usage_pct: 42.4 },
  { id: "b", name: "B", trusted: true, enabled: true, limit_reached: true },
  { id: "c", name: "C", trusted: false, enabled: true },
  { id: "d", name: "D", trusted: true, enabled: false },
];

test("default is off and malformed config fails closed", () => {
  assert.equal(defaultConfig().enabled, false);
  assert.deepEqual(normaliseConfig(null), defaultConfig());
  assert.deepEqual(normaliseConfig("x"), defaultConfig());
  const n = normaliseConfig({ enabled: "yes", allowed: ["a", "a", 3], jobs: { explore: "a/m", code: "bad" }, avoid_above_pct: 500, verify_elsewhere: false });
  assert.equal(n.enabled, false);
  assert.deepEqual(n.allowed, ["a"]);
  assert.deepEqual(n.jobs, { explore: "a/m", code: null, verify: null });
  assert.equal(n.avoid_above_pct, 85);
  assert.equal(n.verify_elsewhere, false);
});

test("validate", () => {
  assert.equal(validateConfig({ ...defaultConfig(), allowed: ["a"] }, provs), "");
  assert.match(validateConfig({ ...defaultConfig(), allowed: ["z"] }, provs), /Unknown provider/);
  assert.match(validateConfig({ ...defaultConfig(), jobs: { code: "nope" } }, provs), /Code model/);
  assert.match(validateConfig({ ...defaultConfig(), avoid_above_pct: 0 }, provs), /1 to 100/);
});

test("provider list keeps only trusted enabled with usage", () => {
  assert.deepEqual(pickableProviders(provs).map((p) => [p.id, p.usage]), [["a", "42% used"], ["b", "limit reached"]]);
  assert.equal(usageLabel({}), "usage unknown");
});

test("labels and options", () => {
  assert.equal(scopeLabel("default", "r1"), "Uses the global setting");
  assert.equal(scopeLabel("global", "r1"), "Uses the global setting");
  assert.equal(scopeLabel("workspace", "r1"), "Workspace setting");
  assert.equal(scopeLabel("global", null), "All workspaces");
  assert.equal(jobLabel("verify"), "Verify");
  const models = [{ provider_id: "a", provider_name: "A", model_id: "m1", ref: "a/m1", enabled: 1 }, { provider_id: "c", provider_name: "C", model_id: "m2", enabled: 1 }];
  const o = jobModelOptions(models, provs, ["a", "c"]);
  assert.deepEqual(o.map((x) => x.value), ["", "a/m1"]);
  assert.equal(o[0].label, "Same as parent");
});

test("preview line", () => {
  assert.equal(previewLine("explore", { model_ref: "a/m1", reason: "Explore job: fast model" }), "Explore: a/m1 (Explore job: fast model)");
  assert.equal(previewLine("code", { error: "x is not allowed" }), "Code: x is not allowed");
  assert.equal(previewLine("verify", null), "Verify: no preview");
});
