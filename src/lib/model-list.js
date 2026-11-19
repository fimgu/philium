// Which models to show first in the picker. Providers list their models newest first, so the
// first entry of each family (the id with its version numbers removed) is that family's latest.

export const modelFamily = (id) =>
  String(id || "").toLowerCase().replace(/\d+/g, "").replace(/[-_.:]+/g, "-").replace(/^-+|-+$/g, "");

/**
 * Split one provider's models, given in the provider's own order, into the latest of each
 * family and everything older. A model named in `keep` (the one in use) is always shown.
 */
export function splitLatest(items, { keep = [], cap = 6 } = {}) {
  const latest = [], older = [], seen = new Set();
  for (const m of items) {
    const f = modelFamily(m.modelId ?? m.model_id ?? m.ref);
    if (!seen.has(f) && latest.length < cap) { seen.add(f); latest.push(m); }
    else older.push(m);
  }
  for (const ref of keep) {
    const i = older.findIndex((m) => m.ref === ref);
    if (i >= 0) latest.push(...older.splice(i, 1));
  }
  return { latest, older };
}
