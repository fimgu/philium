import { createMemo, createResource, createSignal, For, Show, onCleanup } from 'solid-js';
import { activeRepoId, useStore } from '../lib/store.js';
import { go } from '../lib/nav.js';
import { rpc } from '../lib/rpc.js';
import { useVisibleEvent } from '../lib/visibility.js';
import { Button, Empty, Page, toast } from '../ui/kit.jsx';
import { githubIdentity, maxPacketBytes, reviewPreview } from '../lib/review-packet.js';
import { LocalChecks } from './local-checks.jsx';
import { ReleaseIntake } from './release-intake.jsx';

function Reader(props) {
  const [data, setData] = createSignal(null), [error, setError] = createSignal('');
  let generation = 0;
  onCleanup(() => { generation++; });
  async function open(e) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = '';
    const seq = ++generation;
    setData(null); setError('');
    if (!file) return;
    if (file.size > maxPacketBytes) return setError('Choose a packet smaller than 1 MiB.');
    try {
      const text = await file.text();
      if (seq !== generation) return;
      let packet;
      try { packet = JSON.parse(text); } catch { throw new Error('This file is not valid JSON.'); }
      setData(reviewPreview(packet, props.workspace()));
    } catch (err) { if (seq === generation) setError(err.message); }
  }
  const numbers = values => values.map(n => `#${n}`).join(', ') || 'None listed';
  return <section aria-label="Local review preview">
    <p title="Read locally on this device; nothing is run or published.">Open a saved JSON review packet for <strong>{props.slug}</strong>.</p>
    <label>Local review packet <input aria-label="Local review packet" type="file" accept=".json,application/json" onChange={open} /></label>
    <Show when={error()}><p role="alert">{error()}</p></Show>
    <Show when={data()}>{d => <article>
      <h2>PR #{d().pr}</h2>
      <p><strong>Imported evidence · Not run by Philium</strong></p>
      <p>Revision: <code>{d().sha}</code></p>
      <p>{d().first ? `Needs attention: ${d().first.title} (${d().first.status})` : d().cases.length ? 'No unresolved case reported.' : 'No test cases recorded.'}</p>
      <details><summary>Bundle membership</summary><Show when={d().bundle} fallback={<p>Not recorded in this packet.</p>}>{b => <><p>Included: {numbers(b().included)}</p><p>Moved elsewhere: {numbers(b().moved)}</p></>}</Show></details>
      <h3>Recorded checks</h3><For each={d().cases}>{c => <details><summary>{c.title} · {c.status}</summary><p>{c.note || 'No observation recorded.'}</p></details>}</For>
    </article>}</Show>
  </section>;
}

function BranchReviewBar(props) {
  const [selectedId, setSelectedId] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const chosen = () => props.agents.find(a => a.id === selectedId()) || props.agents[0];

  async function startBranchReview() {
    const target = chosen();
    if (!target || busy()) return;
    setBusy(true);
    try {
      const brief = `Review branch "${target.branch}" from agent ${target.name} against the repository base. Inspect the diff, run repo tests/checks, and report findings by severity.`;
      const reviewer = await rpc('agents.spawn', {
        role: 'reviewer',
        repo_id: props.repoId,
        base: target.branch,
        brief,
      }, { action: true });
      toast(`Reviewer ${reviewer.name} started on ${target.branch}`, 'secure');
      go('agents', { id: reviewer.id });
    } catch (e) {
      toast(String(e), 'danger');
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-label="Branch review">
      <div class="row" style={{ 'flex-wrap': 'wrap', gap: '8px' }}>
        <strong class="small">Agent branch</strong>
        <select
          aria-label="Pick an active agent branch"
          value={chosen()?.id || ''}
          onChange={e => setSelectedId(e.currentTarget.value)}
        >
          <For each={props.agents}>
            {a => <option value={a.id}>{a.name} · {a.branch}</option>}
          </For>
        </select>
        <Button size="sm" variant="primary" busy={busy()} disabled={!chosen()} onClick={startBranchReview}>
          Review with agent
        </Button>
        <Show when={chosen()}>
          {a => (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => go('code', { repo_id: props.repoId, ...(a().worktree_id ? { worktree_id: a().worktree_id } : {}) })}
            >
              Inspect diff
            </Button>
          )}
        </Show>
      </div>
    </section>
  );
}

export default function Review() {
  const store = useStore();
  const workspace = () => store.repos()?.find(r => r.id === activeRepoId());
  const slug = () => githubIdentity(workspace()?.remote_url);
  // Key by stable values, not the repo row object: background refresh is not a switch.
  const key = () => slug() && JSON.stringify([activeRepoId(), slug()]);

  const [releaseTools, { refetch: reloadSettings }] = createResource(() =>
    rpc('app.settings_get', {}, { local: true })
      .then(s => s?.release_engineering_tools === 'true')
      .catch(() => false),
  );
  useVisibleEvent('settings', () => reloadSettings());

  const branchAgents = createMemo(() => {
    const id = workspace()?.id;
    if (!id) return [];
    return (store.agents() ?? []).filter(a => a.repo_id === id && a.branch);
  });

  return (
    <Page title="Review" sub="Pull request and branch review">
      <div class="sheet">
        <Show
          when={workspace()?.id}
          keyed
          fallback={<Empty title="Select a workspace" body="Choose a repository in the sidebar to review a PR or branch." />}
        >
          {id => (
            <>
              <Show when={key()} keyed fallback={
                <Show when={!branchAgents().length}>
                  <Empty title="No PR remote or agent branch" body="Add a GitHub remote for PR links, or start an agent to review its branch." />
                </Show>
              }>
                {k => <ReleaseIntake repo={JSON.parse(k)[0]} releaseTools={Boolean(releaseTools())} />}
              </Show>
              <Show when={branchAgents().length}>
                <BranchReviewBar repoId={id} agents={branchAgents()} />
              </Show>
              <Show when={releaseTools()}>
                <details aria-label="More review tools">
                  <summary>Release engineering tools</summary>
                  <LocalChecks repo={id} />
                  <Show when={key()} keyed>
                    {_identity => <Reader workspace={workspace} slug={slug()} />}
                  </Show>
                </details>
              </Show>
            </>
          )}
        </Show>
      </div>
    </Page>
  );
}
