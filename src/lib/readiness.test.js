import test from "node:test";
import assert from "node:assert/strict";
import { readiness, summaryMarkdown, tone } from "./readiness.js";

const states = (rows) => Object.fromEntries(rows.map((r) => [r.id, r.state]));
const ready = () => ({
  pr: 7, generation: 2, head: "a".repeat(40), base: "b".repeat(40), artifact: "sha256:" + "c".repeat(64),
  decision: "ready_for_reviewed_scope", origin_current: true, checked_ms: 1795000000000, why: [],
  receipt: {
    cases: [{ id: "schema", state: "candidate_pass" }, { id: "view", state: "candidate_pass" }],
    live_cases: [{ id: "status", state: "pass" }],
    observer_configured: true, deployment_identity: { state: "match" }, deployment_after: { state: "match" },
    targets: [{ name: "staging", identity_before: { state: "match" }, identity_after: { state: "match" } }],
    release_records: true, review_attestation: { reviewer: "someone" }, evidence_status: { schema: "verified", status: "verified" },
    checks_confirmed_ms: 1794990000000,
  },
});
const members = { landed: [{ pr: 3 }, { pr: 4 }], edges: [{ outer: 3, inner: 4 }], missing: 0, unmapped: 0, ambiguous: 0 };

test("every row is green only when its evidence is all there", () => {
  assert.deepEqual(states(readiness(ready(), members)), { candidate: "ok", contents: "ok", checks: "ok", running: "ok", smoke: "ok", record: "ok", confirmed: "ok" });
  assert.equal(tone("ready_for_reviewed_scope"), "ok");
});

test("what is missing is unknown, never green", () => {
  const d = ready();
  d.decision = "incomplete"; d.origin_current = null; d.artifact = null;
  d.receipt = { cases: [], live_cases: [], release_records: false, checks_confirmed_ms: null };
  assert.deepEqual(states(readiness(d, null)), { candidate: "unknown", contents: "unknown", checks: "unknown", running: "unknown", smoke: "unknown", record: "unknown", confirmed: "unknown" });
  assert.equal(tone("incomplete"), "unknown");
});

test("a regression, a wrong deployment, altered evidence and a moved head are red", () => {
  const d = ready();
  d.receipt.cases[1].state = "release_range_regression";
  d.receipt.cases[1].landed_evidence = [{ pr: 31, oid: "x", result: "same_failure_as_candidate" }, { pr: 30, oid: "y", result: "pass" }];
  d.receipt.targets[0].identity_after.state = "mismatch";
  d.receipt.evidence_status.status = "altered";
  d.origin_current = false;
  const rows = readiness(d, members), s = states(rows);
  assert.deepEqual([s.candidate, s.checks, s.running, s.record], ["bad", "bad", "bad", "bad"]);
  assert.match(rows.find((r) => r.id === "checks").detail, /Failing here: view\. Fails the same way on its own: #31\./);
  assert.match(rows.find((r) => r.id === "running").detail, /staging/);
});

test("running-product checks do not count while the running build is unconfirmed", () => {
  const d = ready();
  d.receipt.targets[0].identity_before.state = "absent";
  const rows = readiness(d, members), s = states(rows);
  assert.equal(s.running, "unknown");
  assert.equal(s.smoke, "unknown");
  assert.match(rows.find((r) => r.id === "smoke").detail, /Not counted/);
});

test("a failure that was already on the target is told apart from a regression", () => {
  const d = ready();
  d.receipt.cases[0].state = "same_failure_on_target";
  const row = readiness(d, members).find((r) => r.id === "checks");
  assert.equal(row.state, "unknown");
  assert.match(row.detail, /already failing on the target/);
  const gaps = readiness(ready(), { ...members, missing: 2 }).find((r) => r.id === "contents");
  assert.equal(gaps.state, "unknown");
});

test("the summary names every row and never calls unknown a pass", () => {
  const d = ready();
  d.decision = "incomplete"; d.why = ["No independent artifact record | for this head"]; d.artifact = null;
  const text = summaryMarkdown(d, readiness(d, null));
  assert.match(text, /^## Release check for #7: Release verification incomplete/);
  assert.equal(text.split("\n").filter((l) => l.startsWith("| ")).length, 8);
  assert.match(text, /\| UNKNOWN \| What is in it \|/);
  assert.match(text, /record \\\| for this head/);
  assert.match(text, /Unknown is not a pass/);
});
