import test from "node:test";
import assert from "node:assert/strict";
import { createSessionPoller } from "./session-poller.js";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { createRoot, createSignal, createEffect, on, onCleanup } from "solid-js/dist/solid.js";

function harness() {
  const requests = [], published = [], timers = new Map();
  let busy = true, nextTimer = 0;
  const poller = createSessionPoller({
    fetchEvents(id, after) {
      return new Promise((resolve, reject) => requests.push({ id, after, resolve, reject }));
    },
    onEvents: events => published.push(...events),
    isBusy: () => busy,
    schedule(fn, delay) { assert.equal(delay, 400); timers.set(++nextTimer, fn); return nextTimer; },
    cancel: id => timers.delete(id),
  });
  return { poller, requests, published, timers, stop: () => { busy = false; } };
}
const settle = () => new Promise(resolve => queueMicrotask(resolve));

test("Chat selection effect resets state and unmount retires actual poll owner", async () => {
  const source = readFileSync(new URL("../screens/chat.jsx", import.meta.url), "utf8");
  const setup = source.slice(source.indexOf("  const poller ="), source.indexOf("  const newChat ="));
  assert.ok(setup.includes("onCleanup"));
  const requests = [], timers = new Map();
  let choose, dispose, events, busy;
  const context = {
    createEffect, on, onCleanup,
    createSessionPoller: options => createSessionPoller({ ...options,
      schedule: fn => { timers.set(fn, fn); return fn; }, cancel: id => timers.delete(id) }),
    hal: (method, path) => new Promise(resolve => requests.push({ method, path, resolve })),
    TERMINAL: new Set(["assistant.completed"]),
  };
  createRoot(cleanup => {
    dispose = cleanup;
    const [selected, setSelected] = createSignal("a"); choose = setSelected;
    const [getEvents, setEvents] = createSignal([]); events = getEvents;
    const [getBusy, setBusy] = createSignal(false); busy = getBusy;
    Object.assign(context, { selected, setEvents, busy, setBusy });
    vm.runInNewContext(setup, context);
  });
  requests[0].resolve([{ seq: 1, type: "turn.started" }]); await settle();
  assert.equal(busy(), true);
  assert.equal(timers.size, 1);
  choose("b");
  assert.equal(events().length, 0);
  assert.equal(busy(), false);
  assert.equal(timers.size, 0);
  assert.equal(requests[1].path, "/sessions/b/events?after=0");
  dispose();
  requests[1].resolve([{ seq: 1, type: "turn.started" }]); await settle();
  assert.equal(events().length, 0);
  assert.equal(busy(), false);
  assert.equal(timers.size, 0);
});

test("A/B/A navigation rejects both stale successes and stale failures", async () => {
  const h = harness();
  h.poller.select("a");
  h.poller.select("b");
  const latest = h.poller.select("a");
  h.requests[0].resolve([{ seq: 90, text: "stale a" }]);
  h.requests[1].reject(new Error("retired b"));
  await settle();
  assert.deepEqual(h.published, []);
  assert.equal(h.timers.size, 0);
  h.requests[2].resolve([{ seq: 2, text: "current a" }]);
  await latest;
  assert.deepEqual(h.published, [{ seq: 2, text: "current a" }]);
  h.poller.poll("a");
  assert.equal(h.requests[3].after, 2);
  h.poller.dispose();
  h.requests[3].resolve([]);
});

test("concurrent triggers are single-flight and advance the cursor once", async () => {
  const h = harness();
  const first = h.poller.select("a");
  h.poller.poll("a"); h.poller.poll("a"); h.poller.poll("b");
  assert.equal(h.requests.length, 1);
  h.requests[0].resolve([{ seq: 1 }]);
  await first;
  assert.equal(h.timers.size, 1);
  const second = h.poller.poll("a");
  assert.equal(h.timers.size, 0);
  assert.deepEqual(h.requests.map(r => r.after), [0, 1]);
  h.stop(); h.requests[1].resolve([{ seq: 2 }]); await second;
  assert.deepEqual(h.published, [{ seq: 1 }, { seq: 2 }]);
  assert.equal(h.timers.size, 0);
  h.poller.dispose();
});

for (const reject of [false, true]) {
  test(`dispose suppresses late ${reject ? "failure" : "success"} and polling resurrection`, async () => {
    const h = harness();
    const pending = h.poller.select("a");
    h.poller.dispose();
    if (reject) h.requests[0].reject(new Error("offline"));
    else h.requests[0].resolve([{ seq: 1 }]);
    await pending;
    h.poller.poll("a");
    assert.deepEqual(h.published, []);
    assert.equal(h.timers.size, 0);
    assert.equal(h.requests.length, 1);
  });
}

test("active failures retry and deselection cancels the scheduled retry", async () => {
  const h = harness();
  const pending = h.poller.select("a");
  h.requests[0].reject(new Error("offline")); await pending;
  assert.equal(h.timers.size, 1);
  const retry = [...h.timers.values()][0];
  h.poller.select(null);
  assert.equal(h.timers.size, 0);
  await retry();
  assert.equal(h.requests.length, 1);
  h.poller.dispose();
});
