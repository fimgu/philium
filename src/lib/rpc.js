import { plainError } from "./plain-error.js";
// Backend bridge. All Rust calls go through `rpc("module.method", args)`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { onCleanup, createSignal } from "solid-js";

// Failed calls are tracked so the shell can say "backend unreachable" instead of
// screens silently rendering as empty. A later success of the same call clears it.
const [failures, setFailures] = createSignal({});
export { failures };
export const clearFailures = () => setFailures({});
const track = (key, p) => p.then(
  (v) => { if (failures()[key]) setFailures((f) => { const n = { ...f }; delete n[key]; return n; }); return v; },
  (e) => { setFailures((f) => ({ ...f, [key]: e })); throw e; },
);

const inFlight = new Map();

export const rpc = (method, args, options = {}) => {
  const canDedup = !options.action && !options.fresh && method !== "terminals.write";
  let key = null;
  if (canDedup) {
    try {
      key = `${method}:${JSON.stringify(args ?? null)}`;
    } catch {
      key = null;
    }
    if (key && inFlight.has(key)) return inFlight.get(key);
  }
  const request = invoke("rpc", { method, args: args ?? null });
  const tracked = options.action || options.local ? request : track(method, request);
  if (key) {
    const shared = tracked.finally(() => {
      if (inFlight.get(key) === shared) inFlight.delete(key);
    });
    inFlight.set(key, shared);
    return shared;
  }
  return tracked;
};

export const hal = (method, path, body) => {
  const request = invoke("hal", { method, path, body });
  return method === "GET" ? track(`hal ${method} ${String(path).split("?")[0]}`, request) : request;
};

export const sendError = (error) => /session is busy|"type"\s*:\s*"busy"/i.test(String(error))
  ? "This agent is still working or waiting for approval. Your draft was kept. Wait, approve the request, or press Stop before sending."
  : plainError(error).plain;

const eventTopics = new Map();

/** Subscribe to a backend event for the lifetime of the current component (multiplexed per topic). */
export function useEvent(topic, fn) {
  let entry = eventTopics.get(topic);
  if (!entry) {
    entry = {
      subs: new Set(),
      unlisten: null,
      connecting: true,
    };
    eventTopics.set(topic, entry);
    listen(topic, (e) => {
      const payload = e?.payload ?? {};
      for (const cb of [...entry.subs]) {
        try {
          cb(payload);
        } catch (x) {
          console.error(`event ${topic}`, x);
        }
      }
    })
      .then((u) => {
        entry.connecting = false;
        if (entry.subs.size === 0) {
          if (eventTopics.get(topic) === entry) eventTopics.delete(topic);
          u?.();
        } else {
          entry.unlisten = u;
        }
      })
      .catch(() => {
        entry.connecting = false;
        if (entry.subs.size === 0 && eventTopics.get(topic) === entry) {
          eventTopics.delete(topic);
        }
      });
  }

  entry.subs.add(fn);

  let cleaned = false;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    entry.subs.delete(fn);
    if (entry.subs.size === 0 && !entry.connecting) {
      if (eventTopics.get(topic) === entry) eventTopics.delete(topic);
      const u = entry.unlisten;
      entry.unlisten = null;
      u?.();
    }
  };

  onCleanup(cleanup);
  return cleanup;
}

export const fmt = {
  int: (n) => (n == null ? "–" : Math.round(n).toLocaleString("en-US")),
  compact: (n) => {
    if (n == null) return "–";
    const a = Math.abs(n);
    if (a >= 1e9) return (n / 1e9).toFixed(1) + "B";
    if (a >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (a >= 1e3) return (n / 1e3) + "k" === `${(n / 1e3).toFixed(1)}k` ? (n / 1e3).toFixed(1) + "k" : (n / 1e3).toFixed(1) + "k";
    return String(Math.round(n));
  },
  money: (n) => (n == null ? "–" : "$" + n.toFixed(n < 1 ? 3 : 2)),
  ms: (n) => (n == null ? "–" : n < 1000 ? `${Math.round(n)} ms` : `${(n / 1000).toFixed(1)} s`),
  bytes: (n) => {
    if (n == null) return "–";
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
    return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
  },
  ago: (ms) => {
    if (!ms) return "–";
    const s = Math.round((Date.now() - ms) / 1000);
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.round(s / 60)}m ago`;
    if (s < 86400) return `${Math.round(s / 3600)}h ago`;
    return `${Math.round(s / 86400)}d ago`;
  },
  pct: (x) => (x == null ? "–" : `${Math.round(x * 100)}%`),
};
