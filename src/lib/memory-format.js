// Pure helpers for Memory cards and the local search models status.

export const MEMORY_CATEGORIES = [
  { value: "preference", label: "Rule" },
  { value: "correction", label: "Correction" },
  { value: "fact", label: "Fact" },
  { value: "procedure", label: "Procedure" },
  { value: "entity", label: "Entity" },
  { value: "decision", label: "Decision" },
  { value: "bugfix", label: "Bugfix" },
  { value: "discovery", label: "Discovery" },
  { value: "architecture", label: "Architecture" },
  { value: "gotcha", label: "Gotcha" },
];

export function categoryLabel(c) {
  return MEMORY_CATEGORIES.find((x) => x.value === c)?.label ?? (c || "");
}

export function memoryTitle(m) {
  const t = String(m?.title ?? "").trim();
  if (t) return t;
  const first = String(m?.content ?? "").split("\n").find((l) => l.trim()) ?? "";
  return first.trim().length > 90 ? `${first.trim().slice(0, 89)}…` : first.trim();
}

export function parseFiles(files) {
  if (Array.isArray(files)) return files.filter((f) => typeof f === "string" && f);
  if (typeof files !== "string" || !files.trim()) return [];
  try {
    const v = JSON.parse(files);
    return Array.isArray(v) ? v.filter((f) => typeof f === "string" && f) : [];
  } catch {
    return [];
  }
}

export function fileChips(files, max = 3) {
  const list = parseFiles(files);
  return { shown: list.slice(0, max), more: Math.max(0, list.length - max) };
}

export function relTime(ts, now = Date.now()) {
  if (!ts) return "";
  const t = typeof ts === "number" ? (ts < 1e12 ? ts * 1000 : ts) : Date.parse(ts);
  if (!Number.isFinite(t)) return "";
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d ago`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo}mo ago`;
  return `${Math.floor(d / 365)}y ago`;
}

function shortErr(e) {
  const s = String(e || "unknown error").split("\n")[0].trim();
  return s.length > 60 ? `${s.slice(0, 59)}…` : s;
}

/** Collapse models_status into one line of state plus an optional action. */
export function modelsSummary(st) {
  if (!st) return { state: "unknown", text: "Checking", action: null, downloading: false };
  const specs = [st.embed, st.rerank].filter(Boolean);
  if (specs.length && specs.every((s) => s.state === "ready")) {
    return { state: "ready", text: "Ready", action: null, downloading: false };
  }
  if (specs.some((s) => s.state === "downloading")) {
    const done = specs.reduce((a, s) => a + Number(s.bytes_done || 0), 0);
    const total = specs.reduce((a, s) => a + Number(s.bytes_total || 0), 0);
    const pct = total ? Math.min(99, Math.floor((done * 100) / total)) : 0;
    return { state: "downloading", text: `Downloading ${pct}%`, action: null, downloading: true };
  }
  const failed = specs.find((s) => s.state === "failed");
  if (failed) return { state: "failed", text: `Failed: ${shortErr(failed.error)}`, action: "Retry", downloading: false };
  return { state: "missing", text: "Not downloaded", action: "Download", downloading: false };
}
