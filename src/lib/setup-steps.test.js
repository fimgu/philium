import test from "node:test";
import assert from "node:assert/strict";
import { setupSteps, setupComplete, setupVisible } from "./setup-steps.js";

const ready = { state: "ready" };
const done = {
  providers: [{ id: "p" }], repos: [{ id: "r" }], models: { embed: ready, rerank: ready },
  github: { installed: true, signed_in: true }, runtimes: [], notifications: false, skills: 0, mcp: 0,
};

test("a fresh install shows every step as not done", () => {
  const rows = setupSteps({ providers: [], repos: [], models: { embed: { state: "missing" }, rerank: { state: "missing" } }, github: { installed: false }, runtimes: [], notifications: false, skills: 0, mcp: 0 });
  assert.equal(rows.length, 7);
  assert.ok(rows.every((r) => !r.done));
  assert.ok(setupVisible(rows));
  assert.match(rows.find((r) => r.id === "github").detail, /PATH/);
});

test("hides once required steps are done, even with optional ones open", () => {
  const rows = setupSteps(done);
  assert.ok(setupComplete(rows));
  assert.ok(!setupVisible(rows));
  assert.ok(rows.filter((r) => r.optional).every((r) => !r.done));
});

test("stays hidden while required state is still loading", () => {
  assert.ok(!setupVisible(setupSteps({ ...done, providers: undefined, repos: [] })));
});

test("models need both parts ready; downloading and installed-but-signed-out read calmly", () => {
  const rows = setupSteps({ ...done, models: { embed: ready, rerank: { state: "downloading" } }, github: { installed: true, signed_in: false } });
  const m = rows.find((r) => r.id === "models");
  assert.equal(m.done, false);
  assert.equal(m.detail, "Downloading now.");
  assert.match(rows.find((r) => r.id === "github").detail, /Installed/);
  assert.ok(setupVisible(rows));
});

test("optional rows reflect live runtimes, notifications and extensions", () => {
  const rows = setupSteps({ ...done, runtimes: [{ id: "orbstack", runtime: "orbstack" }], notifications: true, skills: 2, mcp: 1 });
  for (const id of ["docker", "notifications", "extensions"]) assert.ok(rows.find((r) => r.id === id).done, id);
});
