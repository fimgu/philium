// Pure helpers and scope rules for Philium's generalized Access Grant system.
// Kept framework-free so unit tests run directly with `node --test`.

export const ACCESS_SCOPES = [
  {
    id: "read",
    label: "Read",
    description: "Files, docs, images, PDFs",
    kinds: ["repo", "folder", "file"],
    strictAllowed: true,
    caution: false,
  },
  {
    id: "code",
    label: "Code",
    description: "Code index & AST",
    kinds: ["repo"],
    strictAllowed: true,
    caution: false,
  },
  {
    id: "context",
    label: "Context",
    description: "Repo notes, memory & tasks",
    kinds: ["repo"],
    strictAllowed: false,
    caution: false,
  },
  {
    id: "communicate",
    label: "Communicate",
    description: "Message agents in target repo",
    kinds: ["repo"],
    strictAllowed: false,
    caution: false,
  },
  {
    id: "write",
    label: "Write",
    description: "Edit files inside folder",
    kinds: ["folder", "file"],
    strictAllowed: true,
    caution: true,
  },
];

const SCOPE_BY_ID = Object.fromEntries(ACCESS_SCOPES.map((s) => [s.id, s]));

export const TARGET_KIND_LABELS = {
  repo: "Repo",
  folder: "Folder",
  file: "File",
};

export const ACCESS_LIFETIMES = [
  { value: "once", label: "Once" },
  { value: "session", label: "This agent" },
  { value: "persistent", label: "Always for repo" },
];

export function normalizeTargetKind(kind) {
  const k = String(kind ?? "").trim().toLowerCase();
  if (k === "repo" || k === "folder" || k === "file") return k;
  return "folder";
}

export function targetKindLabel(kind) {
  return TARGET_KIND_LABELS[normalizeTargetKind(kind)] ?? "Folder";
}

export function scopeMeta(scope) {
  return SCOPE_BY_ID[String(scope ?? "").trim().toLowerCase()] ?? null;
}

/**
 * Returns the ordered list of scope IDs permitted for a target kind and security mode.
 * - `read`: all target kinds (Standard & Strict)
 * - `code`: repo only (Standard & Strict)
 * - `context`: repo only, Standard only (disabled for Strict)
 * - `communicate`: repo only, Standard only (disabled for Strict)
 * - `write`: folder/file only, NEVER repo
 */
export function allowedScopesFor(targetKind, strict = false) {
  const kind = normalizeTargetKind(targetKind);
  const isStrict = Boolean(strict);
  return ACCESS_SCOPES.filter(
    (s) => s.kinds.includes(kind) && (!isStrict || s.strictAllowed),
  ).map((s) => s.id);
}

export function isScopeAllowed(scope, targetKind, strict = false) {
  return allowedScopesFor(targetKind, strict).includes(String(scope ?? "").trim().toLowerCase());
}

export function parseScopes(scopes) {
  if (Array.isArray(scopes)) {
    return scopes.map((s) => String(s ?? "").trim().toLowerCase()).filter(Boolean);
  }
  if (typeof scopes === "string") {
    const trimmed = scopes.trim();
    if (!trimmed) return [];
    if (trimmed.startsWith("[")) {
      try {
        const parsed = JSON.parse(trimmed);
        if (Array.isArray(parsed)) return parseScopes(parsed);
      } catch {
        // Fall through to comma splitting.
      }
    }
    return trimmed
      .split(",")
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean);
  }
  return [];
}

/** Filters scopes to those permitted for (targetKind, strict) in canonical order. */
export function sanitizeScopes(scopes, targetKind, strict = false) {
  const requested = new Set(parseScopes(scopes));
  return allowedScopesFor(targetKind, strict).filter((id) => requested.has(id));
}

/**
 * Computes initial checked scopes for an incoming access request.
 * `write` is ALWAYS off by default and must be explicitly enabled by the user.
 */
export function defaultScopesForRequest(requestedScopes, targetKind, strict = false) {
  const safe = sanitizeScopes(requestedScopes, targetKind, strict).filter((s) => s !== "write");
  return safe.length ? safe : ["read"];
}

/** Computes initial checked scopes when proactively creating a grant. `write` is always off. */
export function defaultScopesForGrant(targetKind, strict = false) {
  const kind = normalizeTargetKind(targetKind);
  if (kind === "repo") {
    return sanitizeScopes(["read", "code"], kind, strict);
  }
  return ["read"];
}

export function requestAskedForWrite(req) {
  return parseScopes(req?.requested_scopes).includes("write");
}

export function availableLifetimes({ hasRepo = false, includeOnce = true, includeSession = true } = {}) {
  return ACCESS_LIFETIMES.filter((l) => {
    if (l.value === "once" && !includeOnce) return false;
    if (l.value === "session" && !includeSession) return false;
    if (l.value === "persistent" && !hasRepo) return false;
    return true;
  });
}

export function defaultLifetime({ forRepoScreen = false } = {}) {
  return forRepoScreen ? "persistent" : "session";
}

export function lifetimeLabel(lifetime, usesRemaining = null) {
  if (lifetime === "once") {
    if (typeof usesRemaining === "number" && usesRemaining > 1) return `${usesRemaining} uses left`;
    return "Once";
  }
  if (lifetime === "persistent") return "Always for repo";
  return "This agent";
}

export function pathBasename(path) {
  const clean = String(path ?? "").trim().replace(/[\\/]+$/, "");
  if (!clean) return "";
  const parts = clean.split(/[\\/]/).filter(Boolean);
  return parts.at(-1) ?? clean;
}

export function inferTargetLabel(targetKind, repoName, targetPath) {
  const kind = normalizeTargetKind(targetKind);
  if (kind === "repo" && repoName) return String(repoName).trim();
  return pathBasename(targetPath) || (repoName ? String(repoName).trim() : "");
}

export function targetDisplay(item) {
  if (!item) return "";
  return (
    item.label ||
    item.resolved_repo_name ||
    item.resolved_path ||
    item.target_path ||
    item.target_hint ||
    item.target_repo_id ||
    ""
  );
}

export function hasResolvedTarget({ targetKind, targetRepoId, targetPath }) {
  const kind = normalizeTargetKind(targetKind);
  if (kind === "repo") {
    return Boolean(String(targetRepoId ?? "").trim() || String(targetPath ?? "").trim());
  }
  return Boolean(String(targetPath ?? "").trim());
}

export function canApproveAccess({ targetKind, targetRepoId, targetPath, scopes, strict = false }) {
  if (!hasResolvedTarget({ targetKind, targetRepoId, targetPath })) return false;
  return sanitizeScopes(scopes, targetKind, strict).length > 0;
}

export function buildDecisionPayload({
  requestId,
  decision,
  targetKind,
  targetRepoId,
  targetPath,
  label,
  scopes,
  lifetime = "session",
  strict = false,
  note,
}) {
  if (decision === "deny") {
    return {
      request_id: requestId,
      decision: "deny",
      ...(note ? { note } : {}),
    };
  }
  const kind = normalizeTargetKind(targetKind);
  const validScopes = sanitizeScopes(scopes, kind, strict);
  const payload = {
    request_id: requestId,
    decision: "approve",
    target_kind: kind,
    scopes: validScopes,
    lifetime: lifetime || "session",
  };
  if (kind === "repo" && targetRepoId) payload.target_repo_id = targetRepoId;
  if (targetPath && String(targetPath).trim()) payload.target_path = String(targetPath).trim();
  const resolvedLabel = String(label ?? "").trim();
  if (resolvedLabel) payload.label = resolvedLabel;
  if (strict && payload.lifetime === "persistent") payload.strict_allowed = true;
  if (note) payload.note = note;
  return payload;
}

export function buildGrantPayload({
  sourceRepoId,
  agentId,
  targetKind,
  targetRepoId,
  targetPath,
  label,
  scopes,
  lifetime = "session",
  strict = false,
  strictAllowed = false,
}) {
  const kind = normalizeTargetKind(targetKind);
  const validScopes = sanitizeScopes(scopes, kind, strict);
  const payload = {
    target_kind: kind,
    scopes: validScopes,
    lifetime: lifetime || "session",
  };
  if (sourceRepoId) payload.source_repo_id = sourceRepoId;
  if (agentId) payload.agent_id = agentId;
  if (kind === "repo" && targetRepoId) payload.target_repo_id = targetRepoId;
  if (targetPath && String(targetPath).trim()) payload.target_path = String(targetPath).trim();
  const resolvedLabel = String(label ?? "").trim();
  if (resolvedLabel) payload.label = resolvedLabel;
  if (strictAllowed || (strict && payload.lifetime === "persistent")) {
    payload.strict_allowed = true;
  }
  return payload;
}

export function isGrantActive(grant) {
  if (!grant || grant.revoked_at) return false;
  if (typeof grant.uses_remaining === "number" && grant.uses_remaining <= 0) return false;
  return true;
}

export function pendingAccessByAgent(pendingList) {
  const counts = new Map();
  for (const req of pendingList ?? []) {
    if (!req?.agent_id) continue;
    if (req.status && req.status !== "pending") continue;
    counts.set(req.agent_id, (counts.get(req.agent_id) ?? 0) + 1);
  }
  return counts;
}

export function agentNeedsToolApproval(agent, approvals) {
  if (!agent || agent.live === false) return false;
  const requests = approvals?.[agent.id];
  return Array.isArray(requests) ? requests.length > 0 : agent.state === "waiting";
}

export function agentNeedsUser(agent, pendingByAgent, approvals) {
  if (!agent) return false;
  return agentNeedsToolApproval(agent, approvals) || (pendingByAgent?.get?.(agent.id) ?? 0) > 0;
}
