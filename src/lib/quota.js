// Live provider usage, as each provider reported it in its own response headers.
// No limits live here: labels, percent and resets all come from the server.

/** Percent used at which the meter turns to a calm "near limit" note. */
export const NEAR = 80;

export function untilText(resetsAt, nowSec = Date.now() / 1000) {
  if (!resetsAt) return "";
  const s = Math.max(0, Math.round(resetsAt - nowSec));
  if (s < 60) return "resets now";
  const m = Math.round(s / 60);
  if (m < 60) return `resets in ${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `resets in ${h}h${m % 60 ? ` ${m % 60}m` : ""}`;
  return `resets in ${Math.round(h / 24)}d`;
}

/** Turn one provider's usage into display rows plus an optional calm warning. */
export function quotaView(usage, nowSec = Date.now() / 1000) {
  const windows = usage?.windows ?? [];
  const rows = windows.map((w) => {
    const pct = Math.max(0, Math.min(100, Math.round(w.used_percent)));
    return { label: w.label, pct, near: pct >= NEAR, reset: untilText(w.resets_at, nowSec), title: `${w.label}: ${pct}% used${w.resets_at ? `, ${untilText(w.resets_at, nowSec)}` : ""}` };
  });
  const worst = rows.reduce((a, r) => (!a || r.pct > a.pct ? r : a), null);
  const warning = worst?.near
    ? `${worst.label} window is ${worst.pct}% used${worst.reset ? ` and ${worst.reset}` : ""}. Agents keep working; consider a lighter model for now.`
    : "";
  return { rows, warning };
}
