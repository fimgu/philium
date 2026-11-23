// @-mentions in the composer: find the token at the caret, rank file and symbol
// suggestions, and turn picked items into labeled context sections at send time.

const MAX_ITEM = 40_000;
const MAX_TOTAL = 120_000;

/** `{start, query}` for an `@token` ending at the caret, or null. Email-like text is ignored. */
export function mentionAt(text, caret) {
  const before = String(text ?? "").slice(0, caret);
  const m = /(^|[\s(\[{"'`])@([^\s@]*)$/.exec(before);
  if (!m) return null;
  return { start: before.length - m[2].length - 1, query: m[2] };
}

/** Replace the active `@token` with `@label ` and return the new text and caret. */
export function insertMention(text, at, label) {
  const end = at.start + 1 + at.query.length;
  const ins = `@${label} `;
  return { text: text.slice(0, at.start) + ins + text.slice(end), caret: at.start + ins.length };
}

/** Rank suggestions: files from `tree` and symbols from `find`. Prefix and basename matches first. */
export function rankSuggestions(files, symbols, query, limit = 8) {
  const q = String(query || "").toLowerCase();
  const score = (name, path) => {
    const n = name.toLowerCase(), p = path.toLowerCase();
    if (!q) return 3;
    if (n === q) return 0;
    if (n.startsWith(q)) return 1;
    if (n.includes(q)) return 2;
    if (p.includes(q)) return 3;
    return -1;
  };
  const out = [];
  for (const f of files || []) {
    const base = f.path.split("/").pop();
    const s = score(base, f.path);
    if (s >= 0) out.push({ kind: "file", path: f.path, label: f.path, s });
  }
  for (const y of symbols || []) {
    const s = score(y.name, y.path);
    if (s >= 0) out.push({ kind: "symbol", path: y.path, line: y.line, name: y.name, label: `${y.name}`, detail: `${y.path}:${y.line}`, s: s + 0.5 });
  }
  return out.sort((a, b) => a.s - b.s || a.label.length - b.label.length).slice(0, limit);
}

/** Lines of a symbol starting at `line`, up to the next blank-line gap after 60 lines or 200 lines. */
export function symbolSlice(text, line) {
  const lines = String(text).split("\n");
  const from = Math.max(0, (line || 1) - 1);
  let to = Math.min(lines.length, from + 200);
  for (let i = from + 60; i < to; i++) if (!lines[i].trim()) { to = i; break; }
  return { text: lines.slice(from, to).join("\n"), start: from + 1, end: to };
}

/** Labeled context sections appended to the outgoing message. Reports truncation instead of dropping silently. */
export function contextBlock(resolved) {
  let total = 0;
  const parts = [];
  for (const r of resolved) {
    const where = r.kind === "symbol" ? `${r.path}:${r.start}-${r.end} (${r.name})` : r.path;
    if (r.error) { parts.push(`<context source="${where}" error="${r.error}" />`); continue; }
    let body = String(r.text ?? "");
    let cut = r.truncated;
    if (body.length > MAX_ITEM) { body = body.slice(0, MAX_ITEM); cut = true; }
    if (total + body.length > MAX_TOTAL) { body = body.slice(0, Math.max(0, MAX_TOTAL - total)); cut = true; }
    total += body.length;
    parts.push(`<context source="${where}"${cut ? ' truncated="true"' : ""}>\n${body}\n</context>`);
  }
  return parts.length ? `\n\nReferenced context:\n${parts.join("\n")}` : "";
}
