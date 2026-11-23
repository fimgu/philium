import test from "node:test";
import assert from "node:assert/strict";
import { countdown, planLabel, tightest, tone, verdict } from "./account-usage.js";

const NOW = 1_800_000_000_000;
const s = NOW / 1000;

test("countdown formats days, hours, minutes and seconds", () => {
  assert.equal(countdown(s + 2 * 86400 + 4 * 3600 + 5, NOW), "2d 4h");
  assert.equal(countdown(s + 3 * 3600 + 7 * 60, NOW), "3h 07m");
  assert.equal(countdown(s + 4 * 60 + 9, NOW), "4m 09s");
  assert.equal(countdown(s - 10, NOW), "now");
  assert.equal(countdown(null, NOW), "");
});

test("plan labels and tones", () => {
  assert.equal(planLabel("self_serve_business_prolite"), "Business Prolite");
  assert.equal(planLabel("Max"), "Max");
  assert.equal(tone(50), "ok");
  assert.equal(tone(70), "warn");
  assert.equal(tone(95), "hot");
});

const acct = (name, windows, extra = {}) => ({ provider_name: name, enabled: true, windows, ...extra });
const w = (name, used_percent, extra = {}) => ({ name, used_percent, resets_at: s + 3600, ...extra });

test("tightest picks the most used window", () => {
  assert.equal(tightest(acct("A", [w("5h", 10), w("7d", 80)])).name, "7d");
  assert.equal(tightest(acct("A", [])), null);
});

test("verdict ranks limit, projected run-out, then headroom", () => {
  assert.equal(verdict([], NOW), null);
  assert.match(verdict([acct("Claude", [w("7d", 100)])], NOW).text, /Claude is at its limit, resets in 1h 00m/);
  const risk = verdict([acct("Claude", [w("7d", 80, { projected_full_at: s + 600 })])], NOW);
  assert.equal(risk.tone, "warn");
  assert.match(risk.text, /runs out in about 10m 00s/);
  // Projection after the reset is not a risk.
  const ok = verdict([acct("Claude", [w("7d", 40, { projected_full_at: s + 7200 })]), acct("ChatGPT", [w("7d", 35)])], NOW);
  assert.equal(ok.tone, "ok");
  assert.match(ok.text, /Tightest: Claude 7d at 40%/);
  // Disabled accounts are ignored.
  assert.equal(verdict([acct("X", [w("7d", 100)], { enabled: false })], NOW), null);
});
