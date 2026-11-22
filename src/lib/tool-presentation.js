// Presentation only. Never interpret terminal escape sequences as HTML.
export function toolOutput(result, streamed = "") {
  let value = result;
  if (typeof value === "string") {
    try { value = JSON.parse(value); } catch { return value || streamed; }
  }
  if (value && typeof value === "object") {
    if (Array.isArray(value.content)) return value.content.map(x => x.text ? toolOutput(x.text) : "").filter(Boolean).join("\n") || streamed;
    for (const key of ["output", "stdout", "text"]) {
      if (typeof value[key] === "string") return [value[key], value.stderr].filter(Boolean).join("\n") || streamed;
    }
    return JSON.stringify(value, null, 2);
  }
  return streamed || "";
}

export function cleanTerminalText(text) {
  return String(text ?? "")
    .replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "")
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
}

export function shellStatus(result) {
  let value = result;
  if (typeof value === "string") { try { value = JSON.parse(value); } catch { return null; } }
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value.content)) return value.content.map(x => shellStatus(x.text)).find(Boolean) ?? null;
  if (value.running === true || value.state === "running") return "Still running";
  if (value.state === "interrupted") return "Interrupted";
  if (typeof value.exit_code === "number") return `Exit ${value.exit_code}`;
  return null;
}

const shortToolName = (tool) => String(tool ?? "").replace(/^mcp_philium_/, "").replace(/^mcp_/, "");

const collapseWs = (text) => String(text ?? "").replace(/\\\r?\n\s*/g, " ").replace(/\s+/g, " ").trim();

function truncate(text, max = 84) {
  const s = String(text ?? "");
  if (!s || s.length <= max) return s;
  return s.slice(0, Math.max(1, max - 1)).trimEnd() + "…";
}

/** Collapse whitespace/newlines and strip leading `cd ... &&` / `export ... &&` boilerplate. */
export function cleanShellCommand(command, max = 82) {
  const lines = String(command ?? "")
    .replace(/\\\r?\n/g, " ")
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  while (lines.length > 1 && /^(?:cd|export)\s+/.test(lines[0]) && !/[|&;]/.test(lines[0])) {
    lines.shift();
  }
  let s = collapseWs(lines.join(" "));
  while (true) {
    const m = s.match(/^(?:cd|export)\s+(?:(?:"[^"]*"|'[^']*'|[^&;\n])+?)\s*&&\s*(.+)$/);
    if (!m || !m[1].trim()) break;
    s = m[1].trim();
  }
  return truncate(s, max);
}

function parseEnvelope(result) {
  let value = result;
  if (typeof value === "string") {
    try { value = JSON.parse(value); } catch { return null; }
  }
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value.content)) {
    for (const part of value.content) {
      if (part?.text) {
        const inner = parseEnvelope(part.text);
        if (inner) return inner;
      }
    }
  }
  return value;
}

function lastNonEmptyLine(text) {
  const lines = cleanTerminalText(text || "").split("\n");
  for (let i = lines.length - 1; i >= 0; i--) {
    const trimmed = lines[i].trim();
    if (trimmed) return trimmed;
  }
  return "";
}

function formatPathRange(a) {
  const p = a.path || a.file_path || "";
  let range = "";
  if (a.start_line != null && a.end_line != null) range = `:${a.start_line}-${a.end_line}`;
  else if (a.start_line != null) range = `:${a.start_line}`;
  else if (a.offset != null && a.limit != null) range = `:${a.offset}-${Number(a.offset) + Number(a.limit) - 1}`;
  else if (a.offset != null) range = `:${a.offset}`;
  const repoPfx = a.repo ? `[${a.repo}] ` : "";
  if (p) return `${repoPfx}${p}${range}`;
  if (a.grant_id) return `[${a.grant_id}]${range}`;
  return "";
}

function fallbackSummary(a) {
  const direct = a.description || a.intent || a.title || a.command || a.path || a.file_path || a.query || a.pattern || a.symbol || (a.to ? `to ${a.to}` : "") || a.id;
  if (direct) return truncate(collapseWs(direct), 84);
  const keys = Object.keys(a);
  if (!keys.length) return "";
  return truncate(collapseWs(JSON.stringify(a)), 84);
}

/**
 * Produce a crisp 1-line summary for any Philium/HAL tool call.
 * Returns `{ label, summary, outcome, tone, errorHint }`.
 */
export function summarizeTool(tool, args, result, out = "", meta = null) {
  const name = shortToolName(tool);
  const a = args && typeof args === "object" ? args : {};
  const repoPfx = a.repo ? `[${a.repo}] ` : "";
  const cleanedOut = cleanTerminalText(toolOutput(result, out || ""));
  const status = shellStatus(result);
  const parsed = parseEnvelope(result);

  let label = name || "Tool";
  let summary = "";

  switch (name) {
    case "shell_run": {
      label = "Shell";
      const desc = a.description || a.title || a.intent;
      if (desc) summary = truncate(collapseWs(desc), 84);
      else if (a.command) summary = `$ ${cleanShellCommand(a.command, 82)}`;
      else summary = a.shell ? `${a.shell} shell` : "Command";
      break;
    }
    case "surface_exec": {
      label = "App cmd";
      const desc = a.description || a.title || a.intent;
      if (desc) summary = truncate(collapseWs(desc), 84);
      else if (a.command) summary = `$ ${cleanShellCommand(a.command, 82)}`;
      else summary = a.id ? truncate(collapseWs(a.id), 84) : "Command";
      break;
    }
    case "shell_read": {
      label = "Shell output";
      const tail = collapseWs(lastNonEmptyLine(cleanedOut));
      if (tail) summary = truncate(tail, 84);
      else if (status) summary = status.toLowerCase();
      else summary = a.id ? truncate(collapseWs(a.id), 84) : "terminal";
      break;
    }
    case "shell_open":
      label = "Open shell";
      summary = truncate(collapseWs(a.name || "shell"), 84);
      break;
    case "shell_write":
      label = "Shell input";
      summary = truncate(collapseWs(a.data || a.id || "input"), 84);
      break;
    case "shell_kill":
      label = "Stop shell";
      summary = truncate(collapseWs(a.id || "terminal"), 84);
      break;
    case "shell_list":
      label = "Shells";
      summary = "list terminals";
      break;
    case "code_find":
      label = "Code find";
      summary = truncate(`${repoPfx}${collapseWs(a.query || a.pattern || "")}`, 84);
      break;
    case "code_symbol":
      label = "Symbol";
      summary = truncate(`${repoPfx}${[a.symbol, a.path].filter(Boolean).join(" · ")}`, 84);
      break;
    case "code_outline":
      label = "Outline";
      summary = truncate(`${repoPfx}${a.path || ""}`, 84);
      break;
    case "code_refs":
      label = "Refs";
      summary = truncate(`${repoPfx}${a.symbol || ""}`, 84);
      break;
    case "code_map":
      label = "Code map";
      summary = truncate(`${repoPfx}${a.path || "repo root"}`, 84);
      break;
    case "code_impact":
      label = "Impact";
      summary = truncate(`${repoPfx}${a.path || "workspace changes"}`, 84);
      break;
    case "repo_context":
      label = "Repo";
      summary = truncate(`${repoPfx}${a.branch ? `branch ${a.branch}` : "worktrees & branches"}`, 84);
      break;
    case "read":
    case "read_file":
    case "fs_read":
      label = "Read";
      summary = truncate(formatPathRange(a), 84);
      break;
    case "edit":
    case "edit_file":
      label = "Edit";
      summary = truncate(formatPathRange(a), 84);
      break;
    case "write":
    case "write_file":
    case "fs_write":
      label = "Write";
      summary = truncate(formatPathRange(a), 84);
      break;
    case "fs_list":
      label = "List";
      summary = truncate([a.path || (a.grant_id ? `[${a.grant_id}]` : "."), a.glob].filter(Boolean).join(" "), 84);
      break;
    case "fs_search":
      label = "Search";
      summary = truncate([a.query || a.pattern, a.glob || a.path || (a.grant_id ? `[${a.grant_id}]` : "")].filter(Boolean).join(" in "), 84);
      break;
    case "memory_slate":
      label = "Memory";
      summary = truncate(`${repoPfx}${a.q ? `slate: ${collapseWs(a.q)}` : "context slate"}`, 84);
      break;
    case "memory_search":
      label = "Memory";
      summary = truncate(`${repoPfx}${collapseWs(a.q || "search")}`, 84);
      break;
    case "memory_save": {
      label = "Remember";
      const parts = [];
      if (a.category) parts.push(`[${a.category}]`);
      if (a.fact_key) parts.push(`${a.fact_key}:`);
      if (a.content) parts.push(collapseWs(a.content));
      summary = truncate(parts.join(" ") || "save memory", 84);
      break;
    }
    case "memory_as_of":
      label = "Memory";
      summary = truncate(`as of ${a.at || ""}${a.q ? ` · ${collapseWs(a.q)}` : ""}`.trim(), 84);
      break;
    case "memory_procedures":
      label = "Procedures";
      summary = truncate(collapseWs(a.q || "active procedures"), 84);
      break;
    case "working_note_get":
      label = "Handoff";
      summary = truncate(`${repoPfx}read working note`, 84);
      break;
    case "working_note_set": {
      label = "Handoff";
      const fields = ["requirements", "decisions", "failed_attempts", "next_steps"].filter((k) => a[k]);
      summary = truncate(`update ${fields.length ? fields.join(", ") : "working note"}`, 84);
      break;
    }
    case "access_request": {
      label = "Access";
      const scopes = Array.isArray(a.scopes) && a.scopes.length ? `(${a.scopes.join(", ")})` : "";
      summary = truncate([a.target, scopes, a.reason ? collapseWs(a.reason) : ""].filter(Boolean).join(" · ") || "request access", 84);
      break;
    }
    case "access_list":
      label = "Access";
      summary = "list grants";
      break;
    default:
      summary = fallbackSummary(a);
      break;
  }

  const isRunning = meta?.running === true || (meta?.running !== false && result === undefined && meta?.ok === undefined);
  let outcome = "ok";
  let tone = "ok";
  let errorHint = "";

  if (isRunning) {
    if (meta?.active === false) {
      outcome = "unconfirmed";
      tone = "neutral";
    } else {
      outcome = "Running…";
      tone = "busy";
    }
  } else if (status === "Still running") {
    outcome = "still running";
    tone = "neutral";
  } else if (status === "Interrupted") {
    outcome = "interrupted";
    tone = "bad";
  } else if (status && status.startsWith("Exit ")) {
    const code = Number(status.slice(5));
    if (!Number.isNaN(code) && code !== 0) {
      tone = "bad";
      const line = collapseWs(lastNonEmptyLine(cleanedOut));
      errorHint = line ? truncate(line, 60) : "";
      outcome = errorHint ? `exit ${code} · ${errorHint}` : `exit ${code}`;
    } else if (meta?.ok === false) {
      tone = "bad";
      const line = collapseWs(lastNonEmptyLine(cleanedOut));
      errorHint = line ? truncate(line, 60) : "";
      outcome = errorHint ? `failed · ${errorHint}` : "failed";
    } else {
      tone = "ok";
      outcome = "ok";
    }
  } else if (meta?.ok === false || parsed?.ok === false || (parsed && typeof parsed.error === "string" && parsed.error.trim())) {
    tone = "bad";
    const rawErr = (parsed && typeof parsed.error === "string" && parsed.error.trim())
      ? parsed.error
      : lastNonEmptyLine(cleanedOut);
    errorHint = rawErr ? truncate(collapseWs(rawErr), 60) : "";
    outcome = errorHint ? `failed · ${errorHint}` : "failed";
  } else {
    tone = "ok";
    outcome = "ok";
  }

  return { label, summary, outcome, tone, errorHint };
}
