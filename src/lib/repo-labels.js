// Keep the common case quiet. For duplicate names, show the shortest unique
// checkout suffix. Labels are only presentation; option values stay repo IDs.
export function repoLabels(repos = []) {
  const labels = new Map(), groups = new Map();
  for (const r of repos) {
    if (!groups.has(r.name)) groups.set(r.name, []);
    groups.get(r.name).push(r);
  }
  for (const [name, rows] of groups) {
    if (rows.length === 1) { labels.set(rows[0].id, name); continue; }
    const paths = rows.map(r => (r.path || '').split(/[\\/]/).filter(Boolean));
    for (const [i, parts] of paths.entries()) {
      let label = `${name} (${rows[i].id})`;
      for (let n = 1; n <= parts.length; n++) {
        const suffix = parts.slice(-n).join('/');
        if (paths.every((p,j) => i === j || p.slice(-n).join('/') !== suffix)) {
          label = parts.at(-1) === name ? suffix : `${name} (${suffix})`; break;
        }
      }
      labels.set(rows[i].id, label);
    }
  }
  return labels;
}
