// First-run checklist. Every row is derived from live state passed in; nothing is assumed.
// A value of `undefined` means "not known yet" and keeps the row out of the done count.

const known = (v) => v !== undefined && v !== null;

/**
 * s: { providers, repos, models, github, runtimes, notifications, skills, mcp }
 * Returns rows { id, label, detail, done, optional, screen, pending }.
 */
export function setupSteps(s = {}) {
  const rows = [];
  const add = (id, label, done, detail, screen, optional = false, pending = false) =>
    rows.push({ id, label, done: !!done, detail, screen, optional, pending });

  const providers = s.providers;
  add("provider", "Sign in to a provider", (providers?.length ?? 0) > 0,
    (providers?.length ?? 0) > 0 ? `${providers.length} connected.` : "Connect at least one model provider.",
    "providers", false, !known(providers));

  const repos = s.repos;
  add("repo", "Add a repository", (repos?.length ?? 0) > 0,
    (repos?.length ?? 0) > 0 ? `${repos.length} added.` : "Point Philium at a project you work on.",
    "repos", false, !known(repos));

  const m = s.models;
  const parts = m ? [m.embed, m.rerank].filter(Boolean) : [];
  const ready = parts.length > 0 && parts.every((p) => p.state === "ready");
  const busy = parts.some((p) => p.state === "downloading");
  const failed = parts.some((p) => p.state === "failed");
  add("models", "Download local memory models", ready,
    ready ? "Ready." : busy ? "Downloading now." : failed ? "The last download did not finish. Try again from Memory." : "Lets memory search work on this machine.",
    "memory", false, !known(m));

  const gh = s.github;
  add("github", "Sign in to the GitHub CLI", gh?.signed_in,
    gh?.signed_in ? "Signed in." : gh?.installed ? "Installed. Run its auth login in a terminal." : "Not found on your PATH. Install it to review and publish pull requests.",
    "terminals", false, !known(gh));

  const rt = s.runtimes;
  add("docker", "Docker", (rt?.length ?? 0) > 0,
    (rt?.length ?? 0) > 0 ? `${rt.map((r) => r.runtime || r.id).join(", ")} running.` : "No local container runtime found. Only needed for sandboxes.",
    "environments", true, !known(rt));

  add("notifications", "Notifications", s.notifications === true,
    s.notifications === true ? "On." : "Get a note when an agent finishes or needs you.",
    "settings", true, !known(s.notifications));

  const extras = (s.skills ?? 0) + (s.mcp ?? 0);
  add("extensions", "Skills and MCP servers", extras > 0,
    extras > 0 ? `${s.skills ?? 0} skills, ${s.mcp ?? 0} MCP servers.` : "Optional tools your agents can use.",
    (s.skills ?? 0) > 0 || (s.mcp ?? 0) === 0 ? "skills" : "settings", true, !known(s.skills) && !known(s.mcp));

  return rows;
}

/** Hidden once every required row is done. Optional rows never keep it open. */
export function setupComplete(rows) {
  return rows.filter((r) => !r.optional).every((r) => r.done);
}

/** Show only after required state is known, so a fresh load never flickers the card. */
export function setupVisible(rows) {
  const req = rows.filter((r) => !r.optional);
  return req.length > 0 && req.every((r) => !r.pending) && !setupComplete(rows);
}
