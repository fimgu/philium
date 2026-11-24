import test from "node:test";
import assert from "node:assert/strict";

// Tauri's invoke() reads window.__TAURI_INTERNALS__. Replies are held until released,
// so a concurrent sender would have several calls in flight and could finish out of order.
const calls = [];
const pending = [];
const listeners = {};
globalThis.window = {
  __TAURI_INTERNALS__: {
    transformCallback: () => 0,
    invoke: (_cmd, { method, args }) => new Promise((resolve) => {
      calls.push({ method, args });
      pending.push(() => resolve(null));
    }),
  },
  addEventListener: (type, fn) => { listeners[type] = fn; },
  removeEventListener: (type) => { delete listeners[type]; },
};
const docListeners = {};
globalThis.document = { hidden: false, addEventListener: (t, fn) => { docListeners[t] = fn; }, removeEventListener() {} };
globalThis.requestAnimationFrame = undefined; // check() runs synchronously
const { nativeView } = await import("./native-view.js");
const flush = async () => { for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r)); };
const box = (x) => ({ left: x, top: x, right: x + 100, bottom: x + 100, width: 100, height: 100 });

test("native view IPC is serialized and the latest bounds land last", async () => {
  let rect = box(0);
  const el = { isConnected: true, getBoundingClientRect: () => rect, contains: () => false };
  const stop = nativeView(() => el, () => "t1");
  // Node resolves Solid's server build, where effects don't run: drive check() by hand.
  docListeners.visibilitychange();
  await flush();
  assert.equal(calls.length, 1, "one IPC in flight at a time");

  // The box moves twice while the first set_bounds is still pending.
  rect = box(10); listeners.resize();
  rect = box(20); listeners.resize();
  while (pending.length) { pending.shift()(); await flush(); }

  const bounds = calls.filter((c) => c.method === "browser.set_bounds").map((c) => c.args.x);
  assert.deepEqual(bounds, [1, 21], "stale intermediate bounds are skipped");
  assert.equal(calls.at(-1).method, "browser.set_bounds", "final position is the last call");
  assert.ok(calls.findIndex((c) => c.method === "browser.show") > 0, "show waits for the first bounds");
  stop(); // also clears the 1 Hz poll so the test process can exit
});
