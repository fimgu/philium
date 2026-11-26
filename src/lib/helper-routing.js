// Pure helpers for the "Helper models" settings section (docs/helper-routing.md).

export const JOBS = [
  { id: "explore", label: "Explore", hint: "Search, read and run tests" },
  { id: "code", label: "Code", hint: "Write code" },
  { id: "verify", label: "Verify", hint: "Confirm another agent's finding" },
];

export const OFF_LINE = "Helpers use the same model as the agent that starts them.";
export const PRIVACY_LINE = "Helpers may send this workspace's code to the providers you tick.";
export const GLOBAL_LINE = "Uses the global setting";

export function defaultConfig() {
  return { enabled: false, allowed: [], jobs: { explore: null, code: null, verify: null }, avoid_above_pct: 85, verify_elsewhere: true };
}

const REF = /^[^/\s]+\/\S+$/;

// Fail closed: anything malformed falls back to the default (off).
export function normaliseConfig(raw) {
  const d = defaultConfig();
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return d;
  const jobs = {};
  for (const { id } of JOBS) {
    const v = raw.jobs?.[id];
    jobs[id] = typeof v === "string" && REF.test(v) ? v : null;
  }
  const pct = Number(raw.avoid_above_pct);
  return {
    enabled: raw.enabled === true,
    allowed: Array.isArray(raw.allowed) ? [...new Set(raw.allowed.filter((x) => typeof x === "string" && x))] : [],
    jobs,
    avoid_above_pct: Number.isInteger(pct) && pct >= 1 && pct <= 100 ? pct : d.avoid_above_pct,
    verify_elsewhere: raw.verify_elsewhere !== false,
  };
}

// Returns a plain error string, or "" when the config is fine to save.
export function validateConfig(cfg, providers = []) {
  const ids = new Set(providers.map((p) => p.id));
  for (const id of cfg.allowed ?? []) if (!ids.has(id)) return `Unknown provider: ${id}`;
  for (const { id, label } of JOBS) {
    const v = cfg.jobs?.[id];
    if (v != null && !(typeof v === "string" && REF.test(v))) return `${label} model must look like provider/model`;
  }
  const pct = Number(cfg.avoid_above_pct);
  if (!Number.isInteger(pct) || pct < 1 || pct > 100) return "Usage limit must be a whole number from 1 to 100";
  return "";
}

export function usageLabel(p) {
  if (p?.limit_reached) return "limit reached";
  if (p?.usage_pct == null || !Number.isFinite(Number(p.usage_pct))) return "usage unknown";
  return `${Math.round(Number(p.usage_pct))}% used`;
}

// Only trusted, enabled providers can be routing targets.
export function pickableProviders(providers = []) {
  return providers.filter((p) => p.trusted && p.enabled).map((p) => ({ ...p, usage: usageLabel(p) }));
}

export function scopeLabel(source, repoId) {
  if (!repoId) return "The default for every workspace without its own setting";
  return source === "workspace" ? "Workspace setting" : GLOBAL_LINE;
}

export function jobLabel(job) {
  return JOBS.find((j) => j.id === job)?.label ?? job;
}

// Model options for a job: "Same as parent" first, then models of allowed providers.
// A saved ref missing from the list is kept so the select never shows blank.
export function jobModelOptions(models = [], providers = [], allowed = [], current = null) {
  const ok = new Set(pickableProviders(providers).map((p) => p.id).filter((id) => allowed.includes(id)));
  const opts = [{ value: "", label: "Same as parent" }, ...models
    .filter((m) => m.enabled !== 0 && ok.has(m.provider_id))
    .map((m) => ({ value: m.ref || `${m.provider_id}/${m.model_id}`, label: `${m.provider_name} / ${m.display || m.model_id}` }))];
  if (current && !opts.some((o) => o.value === current)) opts.push({ value: current, label: current });
  return opts;
}

export function previewLine(job, result) {
  const name = jobLabel(job);
  if (!result) return `${name}: no preview`;
  if (result.error) return `${name}: ${result.error}`;
  const reason = result.reason ? ` (${result.reason})` : "";
  return `${name}: ${result.model_ref || "same as parent"}${reason}`;
}
