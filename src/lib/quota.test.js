import test from "node:test";
import assert from "node:assert/strict";
import { quotaView, untilText } from "./quota.js";

test("rows come straight from server windows and warn calmly near the limit", () => {
  const v = quotaView({ windows: [{ label: "5h", used_percent: 42.4, resets_at: 1000 + 7200 }, { label: "7d", used_percent: 91, resets_at: null }] }, 1000);
  assert.deepEqual(v.rows.map((r) => [r.label, r.pct, r.near]), [["5h", 42, false], ["7d", 91, true]]);
  assert.equal(v.rows[0].reset, "resets in 2h");
  assert.match(v.warning, /^7d window is 91% used\. Agents keep working/);
});

test("no usage means nothing to show", () => {
  assert.deepEqual(quotaView(undefined), { rows: [], warning: "" });
  assert.equal(untilText(0), "");
  assert.equal(untilText(100 + 90 * 60, 100), "resets in 1h 30m");
  assert.equal(untilText(100 + 3 * 86400, 100), "resets in 3d");
});
