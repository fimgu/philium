import test from "node:test";
import assert from "node:assert/strict";
import { liveTps, sampleLiveTps } from "./live-tps.js";

const progress = (tokens, id = 1) => ({ id, tokens });
const first = () => sampleLiveTps(null, progress(4), 10_000);
const active = () => sampleLiveTps(first(), progress(24), 10_500);

test("estimates from consecutive output samples without including time to first output", () => {
  const waiting = sampleLiveTps(null, progress(0), 0);
  const started = sampleLiveTps(waiting, progress(4), 10_000);
  const early = sampleLiveTps(started, progress(14), 10_250);
  assert.equal(liveTps(early, 10_250), null);
  const speed = sampleLiveTps(early, progress(24), 10_500);
  assert.equal(liveTps(speed, 10_500), 40);
  assert.equal(liveTps(sampleLiveTps(speed, progress(29), 11_000), 11_000), 10);
});

test("keepalives don't prolong a rate; output after a pause starts a new sample", () => {
  const speed = active();
  const keepalive = sampleLiveTps(speed, progress(24), 12_400);
  assert.equal(liveTps(keepalive, 12_500), null);
  const resumed = sampleLiveTps(keepalive, progress(30), 12_600);
  assert.equal(liveTps(resumed, 12_600), null);
  assert.equal(liveTps(sampleLiveTps(resumed, progress(40), 13_100), 13_100), 20);
});

test("completion, stalls, and a new request clear the current rate", () => {
  const speed = active();
  const done = sampleLiveTps(speed, { id: 1, done: true }, 10_600);
  assert.equal(liveTps(done, 10_600), null);
  assert.equal(sampleLiveTps(done, progress(30), 10_700), done);
  assert.equal(liveTps(sampleLiveTps(speed, { id: 1, stalled: true }, 10_600), 10_600), null);
  const next = sampleLiveTps(speed, progress(1, 2), 10_600);
  assert.equal(liveTps(next, 10_600), null);
  assert.equal(sampleLiveTps(next, { id: 1, done: true }, 10_700), next);
});
