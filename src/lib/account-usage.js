// Live account usage (usage.accounts), shared by Stats and Providers.
// One poller for the whole app: refresh on demand and every 60s while a
// subscribed screen is visible. The backend caches for 20s and keeps the last
// good result marked stale, so a failed refresh never blanks the cards.
import { createSignal } from "solid-js";
import { rpc } from "./rpc.js";

const [data, setData] = createSignal(null);
const [error, setError] = createSignal("");
const [loading, setLoading] = createSignal(false);
let inflight = null;

export const accountUsage = { data, error, loading };

export function refreshAccountUsage(force = false) {
  if (inflight && !force) return inflight;
  setLoading(true);
  inflight = rpc("usage.accounts", { force })
    .then((r) => {
      setData(r);
      setError("");
    })
    .catch((e) => setError(String(e?.message ?? e)))
    .finally(() => {
      setLoading(false);
      inflight = null;
    });
  return inflight;
}

/** Usage for one provider id, or undefined. */
export const usageFor = (id) => data()?.accounts?.find((a) => a.provider_id === id);

/** "2d 4h", "3h 12m", "4m 09s", "now". `at` is unix seconds. */
export function countdown(at, nowMs) {
  if (!at) return "";
  let s = Math.max(0, Math.round(at - nowMs / 1000));
  if (s === 0) return "now";
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  s -= h * 3600;
  const m = Math.floor(s / 60);
  s -= m * 60;
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${String(m).padStart(2, "0")}m`;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

export const resetClock = (at) =>
  at
    ? new Date(at * 1000).toLocaleString(undefined, { weekday: "short", hour: "numeric", minute: "2-digit" })
    : "";

/** ok / warn / hot tone for a percentage used. */
export const tone = (pct) => (pct >= 90 ? "hot" : pct >= 70 ? "warn" : "ok");

/** Most constrained window, used by the pill and the verdict strip. */
export function tightest(account) {
  const ws = account?.windows ?? [];
  return ws.reduce((a, w) => (!a || w.used_percent > a.used_percent ? w : a), null);
}

/** Plain-English plan label. Unknown plans pass through unchanged. */
export function planLabel(plan) {
  if (!plan) return "";
  return plan
    .replace(/^self_serve_/, "")
    .split(/[_\s]+/)
    .map((w) => (w ? w[0].toUpperCase() + w.slice(1) : w))
    .join(" ");
}

/** One-line verdict over every account. */
export function verdict(accounts, nowMs) {
  const live = (accounts ?? []).filter((a) => a.enabled && a.windows?.length);
  if (!live.length) return null;
  const blocked = live.filter((a) => a.limit_reached || a.windows.some((w) => w.used_percent >= 100));
  if (blocked.length) {
    const a = blocked[0];
    const w = a.windows.find((x) => x.used_percent >= 100) ?? tightest(a);
    return { tone: "hot", text: `${a.provider_name} is at its limit${w?.resets_at ? `, resets in ${countdown(w.resets_at, nowMs)}` : ""}` };
  }
  const risk = live
    .flatMap((a) => a.windows.filter((w) => w.projected_full_at && (!w.resets_at || w.projected_full_at < w.resets_at)).map((w) => ({ a, w })))
    .sort((x, y) => x.w.projected_full_at - y.w.projected_full_at)[0];
  if (risk) {
    return { tone: "warn", text: `${risk.a.provider_name} ${risk.w.name} runs out in about ${countdown(risk.w.projected_full_at, nowMs)} at the current pace, before it resets` };
  }
  const t = live.map((a) => ({ a, w: tightest(a) })).sort((x, y) => y.w.used_percent - x.w.used_percent)[0];
  return { tone: tone(t.w.used_percent), text: `All accounts have headroom. Tightest: ${t.a.provider_name} ${t.w.name} at ${Math.round(t.w.used_percent)}%` };
}
