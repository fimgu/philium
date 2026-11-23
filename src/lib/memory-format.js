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

// SQLite CURRENT_TIMESTAMP is UTC without a zone ("YYYY-MM-DD HH:MM:SS").
function utcIso(s) {
  const m = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?)$/.exec(String(s).trim());
  return m ? `${m[1]}T${m[2]}Z` : s;
}

export function relTime(ts, now = Date.now()) {
  if (!ts) return "";
  const t = typeof ts === "number" ? (ts < 1e12 ? ts * 1000 : ts) : Date.parse(utcIso(ts));
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

const DREAM_VERBS = { confirm: "Confirmed", correct: "Corrected", merge: "Merged", retire: "Retired", demote: "Demoted", promote: "Promoted", new: "Added", propose: "Proposed", flag: "Flagged" };

/** One header line for the Memory screen's dreaming row. */
export function dreamSummary(st, now = Date.now()) {
  if (!st) return { text: "", canRun: false };
  if (st.running) return { text: "Dreaming…", canRun: false };
  if (!st.enabled) return { text: "Dreaming off", canRun: false };
  const last = st.last_dreamed_at ? `Last dreamed ${relTime(st.last_dreamed_at, now)}` : "Not dreamed yet";
  const budget = Number(st.daily_tokens || 0);
  const over = budget > 0 && Number(st.used_today || 0) >= budget;
  return { text: over ? `${last} · daily budget used` : last, canRun: !over };
}

/** Normalize dream_actions rows for the Changes list: newest run, one line each. */
export function dreamChanges(actions) {
  return (actions || []).map((a) => ({
    id: a.id,
    verb: DREAM_VERBS[a.kind] || a.kind,
    text: a.summary || "",
    reason: a.reason || "",
    undone: !!a.undone_at,
    undoable: !a.undone_at && a.kind !== "flag" && a.kind !== "confirm",
  }));
}

// Memory retrieval card: what is actually in use, and which custom server
// fields to show. Saved custom values are only surfaced when a custom
// (non-local) model is the active one, so idle leftovers never look "in use".
export function retrievalInUse(cfg) {
  const c = cfg ?? {};
  const embed = c.active_embed_model || "Lexical (FTS5)";
  const rerank = c.active_rerank_model || (c.reranker_active ? "On" : "Off");
  return { embed, rerank, line: `Embedder: ${embed} · Reranker: ${rerank}` };
}

export function customServerFields(cfg) {
  const c = cfg ?? {};
  const isCustom = (m) => Boolean(m) && !String(m).startsWith("local:");
  const embOn = isCustom(c.active_embed_model);
  const rrOn = isCustom(c.active_rerank_model);
  return {
    open: embOn || rrOn,
    embProvider: embOn ? c.embedding_provider ?? "" : "",
    embEndpoint: embOn ? c.embedding_endpoint ?? "" : "",
    embModel: embOn ? c.embedding_model ?? "" : "",
    rrProvider: rrOn ? c.rerank_provider ?? "" : "",
    rrEndpoint: rrOn ? c.rerank_endpoint ?? "" : "",
    rrModel: rrOn ? c.rerank_model ?? "" : "",
  };
}
