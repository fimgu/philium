// A preview, not an approval gate. Only project a bounded set of display fields.
export const maxPacketBytes = 1024 * 1024;
export function githubIdentity(remote) {
  if (typeof remote !== 'string') return null;
  const match = /^(?:https:\/\/github\.com\/|git@github\.com:|ssh:\/\/git@github\.com\/)([\w.-]+\/[\w.-]+?)\/?$/i.exec(remote);
  const slug = match?.[1].replace(/\.git$/i, '').toLowerCase();
  return slug && slug.split('/').every(x => x && x !== '.' && x !== '..') ? slug : null;
}
const invalid = () => { throw new Error('Not a supported review packet.'); };
const label = s => typeof s === 'string' ? s.slice(0, 1200) : '';
const statuses = new Set(['pass', 'fail', 'blocked', 'untested', 'skipped']);
export function reviewPreview(packet, workspace) {
  const plan = packet?.plan;
  const identity = githubIdentity(workspace?.remote_url);
  if (!workspace?.id || !identity || typeof plan?.repo !== 'string' || plan.repo.toLowerCase() !== identity) {
    throw new Error('This packet does not match the selected GitHub repository.');
  }
  if (!Number.isSafeInteger(plan.pr) || plan.pr < 1 || typeof plan.head_sha !== 'string' || !/^[a-f\d]{40}$/i.test(plan.head_sha)) invalid();
  if (!Array.isArray(plan.cases) || plan.cases.length > 500 || !Array.isArray(packet.results) || packet.results.length > 500) invalid();
  const ids = new Set();
  const cases = plan.cases.map(c => {
    if (!c || typeof c.id !== 'string' || !c.id || ids.has(c.id) || !label(c.workflow)) invalid();
    ids.add(c.id);
    return { id: c.id, title: label(c.workflow), status: 'untested', note: '' };
  });
  const rows = new Map(cases.map(c => [c.id, c]));
  for (const r of packet.results) {
    if (!r || !ids.delete(r.id) || !statuses.has(r.status)) invalid();
    const row = rows.get(r.id);
    row.status = r.head_sha === plan.head_sha ? r.status : 'wrong revision';
    row.note = label(r.observation);
  }
  // Membership is explicit, never inferred from mentions in prose. Absence means unknown.
  let bundle = null;
  if (packet.bundle !== undefined) {
    const seen = new Set();
    const list = values => {
      if (!Array.isArray(values) || values.length > 500) invalid();
      for (const n of values) {
        if (!Number.isSafeInteger(n) || n < 1 || n === plan.pr || seen.has(n)) invalid();
        seen.add(n);
      }
      return [...values];
    };
    if (packet.bundle?.head_sha !== plan.head_sha) invalid();
    bundle = { included: list(packet.bundle.included), moved: list(packet.bundle.moved) };
  }
  const first = cases.find(c => ['fail','blocked','wrong revision'].includes(c.status)) ?? cases.find(c => c.status !== 'pass');
  return { repo: identity, pr: plan.pr, sha: plan.head_sha, cases, first, bundle };
}
