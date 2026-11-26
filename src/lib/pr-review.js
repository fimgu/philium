// Pure helpers for the PR reviewer UI: tallying, grouping, sorting and labelling checklist items.
// Nothing here calls rpc or touches the DOM; the screen composes these around live data.

export const VERDICT_LABEL = { pending: 'Pending', needs_you: 'Needs you', approve: 'Approve', request_changes: 'Request changes', posted: 'Posted' };
export const VERDICT_TONE = { pending: 'unknown', needs_you: 'warn', approve: 'ok', request_changes: 'bad', posted: 'ok' };

export const KIND_LABEL = { risk: 'Changes & risks', comment: 'Comments & threads', review: 'Comments & threads', thread: 'Comments & threads', claim: 'Description claims', ci: 'CI', note: 'Your notes' };
export const KIND_ORDER = ['Changes & risks', 'Comments & threads', 'Description claims', 'CI', 'Your notes'];

export const ORIGIN_LABEL = { introduced: 'introduced', pre_existing: 'pre-existing', unverified: 'origin unverified' };
export const STATUS_LABEL = {
  open: 'To do', doing: 'In progress', pass: 'Pass', fail: 'Fail', blocked: 'Blocked',
  addressed: 'Addressed', not_addressed: 'Not addressed', outdated: 'Outdated',
  not_applicable: 'Not applicable',
};
export const VERIFY_LABEL = { none: '', needed: 'Needs verification', confirmed: 'Confirmed', refuted: 'Refuted' };

/** The verdict as a short line for the badge/header area. */
export function verdictLabel(review) {
  if (!review) return VERDICT_LABEL.pending;
  if (review.state === 'posted') return VERDICT_LABEL.posted;
  return VERDICT_LABEL[review.verdict] || VERDICT_LABEL.pending;
}

export function verdictTone(review) {
  if (!review) return VERDICT_TONE.pending;
  if (review.state === 'posted') return VERDICT_TONE.posted;
  return VERDICT_TONE[review.verdict] || VERDICT_TONE.pending;
}

/** `n of m done`: items whose status is a terminal one, not open/doing. */
const DONE = new Set(['pass', 'fail', 'blocked', 'addressed', 'not_addressed', 'outdated', 'not_applicable']);
export function tally(items) {
  const list = items || [];
  const done = list.filter((it) => DONE.has(it.status)).length;
  const counts = {};
  for (const it of list) counts[it.status] = (counts[it.status] || 0) + 1;
  return { done, total: list.length, counts };
}

/** `n items: a pass, b addressed, c P3` line, worst (fail/blocked) counts named with severity where present. */
export function tallyLine(items) {
  const list = items || [];
  if (!list.length) return 'No items yet.';
  const counts = {};
  for (const it of list) {
    const key = it.status === 'fail' && it.severity != null ? `P${it.severity}` : STATUS_LABEL[it.status] || it.status;
    counts[key] = (counts[key] || 0) + 1;
  }
  const parts = Object.entries(counts).map(([k, n]) => `${n} ${k}`);
  return `${list.length} ${list.length === 1 ? 'item' : 'items'}: ${parts.join(', ')}`;
}

/** Sort weight, worst first: blockers/fails lowest, then blocked, then needing verification, then open, then resolved. */
export function weight(it) {
  if (it.verify === 'needed') return -1;
  switch (it.status) {
    case 'fail': return (it.severity ?? 5) - 10;
    case 'not_addressed': return 0;
    case 'blocked': return 1;
    case 'open': return 2;
    case 'doing': return 3;
    case 'outdated': return 4;
    case 'not_applicable': return 5;
    case 'addressed': return 6;
    case 'pass': return 7;
    default: return 8;
  }
}

export function sortWorstFirst(items) {
  return [...(items || [])].sort((a, b) => weight(a) - weight(b));
}

/** Group items by kind label, in KIND_ORDER, each group sorted worst first. Empty groups are dropped. */
export function groupByKind(items) {
  const buckets = new Map();
  for (const it of items || []) {
    const label = KIND_LABEL[it.kind] || it.kind;
    if (!buckets.has(label)) buckets.set(label, []);
    buckets.get(label).push(it);
  }
  const order = [...KIND_ORDER, ...[...buckets.keys()].filter((k) => !KIND_ORDER.includes(k))];
  return order.filter((label) => buckets.has(label)).map((label) => ({ label, items: sortWorstFirst(buckets.get(label)) }));
}

/** Short status mark for a row: [tone, glyph, title]. */
export function itemMark(it) {
  switch (it.status) {
    case 'pass': case 'addressed': return ['ok', '✓', STATUS_LABEL[it.status]];
    case 'fail': case 'not_addressed': return ['bad', '✕', STATUS_LABEL[it.status]];
    case 'blocked': return ['warn', '!', 'Blocked: ' + (it.detail || 'needs you')];
    case 'outdated': case 'not_applicable': return ['unknown', '·', STATUS_LABEL[it.status]];
    case 'doing': return ['unknown', '…', STATUS_LABEL[it.status]];
    default: return ['unknown', '?', STATUS_LABEL[it.status] || 'To do'];
  }
}

/** Verifier line for a row, or '' when nothing to show. */
export function verifyLine(it) {
  if (!it || !it.verify || it.verify === 'none') return '';
  if (it.verify === 'needed') return VERIFY_LABEL.needed;
  if (it.verify === 'confirmed') return it.verifier ? `Confirmed by ${it.verifier}` : VERIFY_LABEL.confirmed;
  if (it.verify === 'refuted') return it.verifier ? `Refuted by ${it.verifier}` : VERIFY_LABEL.refuted;
  return '';
}

export function originLabel(it) {
  return it?.status === 'fail' ? ORIGIN_LABEL[it.origin || 'unverified'] : '';
}

export function severityLabel(it) {
  return it?.status === 'fail' && it.severity != null ? `P${it.severity}` : '';
}

export const BASIS_LABEL = { runtime: 'ran it', source_trace: 'read code', runtime_and_source: 'ran + read' };

/** Chip text for how an item's result was established, or '' when not set. */
export function basisLabel(it) {
  return BASIS_LABEL[it?.basis] || '';
}

/** "Read 21/21 comments, 1/1 reviews, 4/4 threads, 12/12 checks" plus warn when anything is missing or erred. */
export function coverageLine(coverage, errors) {
  if (!coverage) return null;
  const parts = ['comments', 'reviews', 'threads', 'checks']
    .filter((k) => coverage[k])
    .map((k) => `${coverage[k].fetched}/${coverage[k].total} ${k}`);
  if (!parts.length) return null;
  const missing = ['comments', 'reviews', 'threads', 'checks'].some((k) => coverage[k] && coverage[k].fetched < coverage[k].total);
  return { text: `Read ${parts.join(', ')}`, warn: missing || Boolean(errors && errors.length) };
}

// ---- Review policy (docs/pr-review-policy.md) ----

export const MODES = ['observe', 'comment', 'enforce'];
export const MODE_LABEL = { observe: 'Observe', comment: 'Comment', enforce: 'Enforce' };
export const LEVEL_LABEL = { quick: 'Quick', standard: 'Standard', deep: 'Deep' };
export const WOULD_POST_LABEL = { APPROVE: 'Approve', REQUEST_CHANGES: 'Request changes', COMMENT: 'Comment' };

export function modeLabel(m) { return MODE_LABEL[m] || ''; }
export function levelLabel(l) { return LEVEL_LABEL[l] || ''; }

/** Stored policy may arrive as a JSON string column or an object. Returns an object or null. */
export function parsePolicy(p) {
  if (!p) return null;
  if (typeof p === 'object') return p;
  try { return JSON.parse(p); } catch { return null; }
}

/** policy_get may return {policy, raw, override} or a bare effective policy. Normalise. */
export function normalisePolicyGet(res) {
  if (!res) return { policy: null, raw: null, override: null };
  if (res.policy !== undefined || res.raw !== undefined || res.override !== undefined) {
    return { policy: parsePolicy(res.policy), raw: res.raw ?? null, override: res.override ?? null };
  }
  return { policy: parsePolicy(res), raw: null, override: null };
}

function rank(m) { const i = MODES.indexOf(m); return i < 0 ? 0 : i; }

/** Highest mode the UI may select. No file: any. Invalid: observe. Otherwise the file's mode. */
export function modeCeiling(policy) {
  if (!policy || policy.source === 'default') return 'enforce';
  if (policy.source === 'invalid' || policy.error) return 'observe';
  return MODES.includes(policy.mode) ? policy.mode : 'observe';
}

export function modeAllowed(mode, ceiling) { return rank(mode) <= rank(ceiling); }

/** The override only ever lowers: min(ceiling, override). With no override, the ceiling applies. */
export function effectiveMode(ceiling, override) {
  if (!override || !MODES.includes(override)) return ceiling;
  return rank(override) < rank(ceiling) ? override : ceiling;
}

/** "Base a1b2c3d", "Defaults (no policy file)", "Invalid policy file". */
export function policySourceLabel(policy) {
  const s = policy?.source || 'default';
  if (s.startsWith('base:')) return `Base ${s.slice(5, 12)}`;
  if (s === 'invalid') return 'Invalid policy file';
  return 'Defaults (no policy file)';
}

export function formatTokens(n) {
  const v = Number(n) || 0;
  if (v >= 1e6) return `${+(v / 1e6).toFixed(1)}M`;
  if (v >= 1e3) return `${+(v / 1e3).toFixed(1)}k`;
  return String(v);
}

/** {text, over} for token use vs budget, or null when nothing used and no cap. */
export function budgetLine(used, budget) {
  const u = Number(used) || 0;
  if (budget == null) return u ? { text: `${formatTokens(u)} tokens`, over: false } : null;
  return { text: `${formatTokens(u)} of ${formatTokens(budget)} tokens`, over: u >= Number(budget) };
}

export function wouldPostLabel(event) {
  if (!event) return '';
  return WOULD_POST_LABEL[String(event).toUpperCase()] || String(event);
}

export function humansLine(n) {
  const h = Number(n) || 0;
  return h ? `${h} human approval${h === 1 ? '' : 's'} required` : 'No human approval required';
}
