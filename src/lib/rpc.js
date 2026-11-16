// Backend bridge. All Rust calls go through `rpc("module.method", args)`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { onCleanup } from "solid-js";

export const rpc = (method, args) => invoke("rpc", { method, args: args ?? null });
export const hal = (method, path, body) => invoke("hal", { method, path, body });

/** Subscribe to a backend event for the lifetime of the current component. */
export function useEvent(topic, fn) {
  let un;
  let dead = false;
  listen(topic, (e) => fn(e.payload)).then((u) => (dead ? u() : (un = u)));
  onCleanup(() => { dead = true; un?.(); });
}

export const fmt = {
  int: (n) => (n == null ? "–" : Math.round(n).toLocaleString("en-US")),
  compact: (n) => {
    if (n == null) return "–";
    const a = Math.abs(n);
    if (a >= 1e9) return (n / 1e9).toFixed(1) + "B";
    if (a >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (a >= 1e3) return (n / 1e3).toFixed(1) + "k";
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
