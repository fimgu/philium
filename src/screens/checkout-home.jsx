import { For, Show, createResource, createSignal } from 'solid-js';
import { SpawnModal } from '../ui/spawn-modal.jsx';
import { AgentChip } from '../ui/agent-chip.jsx';
import { Page, Button, Dot, toast } from '../ui/kit.jsx';
import { Icon } from '../ui/icons.jsx';
import { go } from '../lib/nav.js';
import { rpc } from '../lib/rpc.js';
import { useVisibleEvent } from '../lib/visibility.js';

function Records(props) {
  return <section class="home-sec"><div class="row"><h2 class="grow">{props.title}</h2>{props.action}</div>
    <For each={props.rows.slice(0,20)} fallback={<p class="small faint">{props.empty}</p>}>{props.children}</For>
    <Show when={props.rows.length > 20}><p class="small faint">Showing the first 20 records.</p></Show>
  </section>;
}

// Parent keys this entire component by registered repo ID. A late reply from
// the previous checkout cannot populate the new component, even on A → B → A.
export default function CheckoutHome(props) {
  const [result, {refetch}] = createResource(async () => {
    try {
      const data = await rpc('workspace.overview', {repo_id:props.id});
      if (data?.repo_id !== props.id) throw new Error('Checkout identity did not match');
      return {data};
    } catch (e) { return {error:String(e?.message || e)}; }
  });
  for (const topic of ['agents:changed','board:changed','workspace:changed','reaper:changed']) {
    useVisibleEvent(topic, () => refetch(), { debounceMs: 300 });
  }
  const [prReviews, {refetch: refetchPrReviews}] = createResource(() =>
    rpc('pr_review.list', {repo_id: props.id}, {local: true}).catch(() => []));
  useVisibleEvent('pr_review:changed', () => refetchPrReviews(), { debounceMs: 300 });
  const [spawnPreset, setSpawnPreset] = createSignal(null);
  const allOf = (key) => result()?.data?.[key] ?? result()?.data?.agents ?? [];
  const coordinators = () => allOf('coordinators').filter(a => a.role === 'orchestrator' || a.role === 'coordinator');
  const waitingAgents = () => allOf('attention').filter(a => a.state === 'waiting');
  const failedAgents = () => allOf('attention').filter(a => a.state === 'failed');
  const repoClashes = () => allOf('attention').flatMap(a => (a.warnings || []).filter(w => w.kind === 'clash').map(w => ({ agent: a, text: w.text })));
  const needsYouPrReviews = () => (prReviews() || []).filter(r => r.verdict === 'needs_you');
  const blockedTitles = (r) => (r.items || []).filter(it => it.status === 'blocked').map(it => it.title).join(', ');
  const needsYouCount = () => waitingAgents().length + failedAgents().length + repoClashes().length + needsYouPrReviews().length;
  const link = (label, route, params) => <Button size="sm" variant="ghost" onClick={() => go(route,params)}>{label}</Button>;
  return <Page title="Workspace" sub="Active repository" actions={<div class="row">
    <Button variant="primary" onClick={() => setSpawnPreset({ role: 'worker', repo_id: props.id })}><Icon name="plus" size={12} /> New agent</Button>
    <Button onClick={() => refetch()} disabled={result.loading}>Refresh</Button>
  </div>}>
    <Show when={result()?.error}><p role="alert">Could not load workspace: {result().error}</p></Show>
    <Show when={result()?.data} fallback={!result()?.error && <p role="status">Loading workspace…</p>}>{d => <div aria-label="Checkout overview" class="checkout-home">
      <code class="ws-path">{d().path}</code>
      <div class="row">{link('Terminals','terminals')}{link('Code','code',{repo_id:props.id})}{link('Review','review')}</div>
      <Show when={needsYouCount() > 0}>
        <section class="home-sec" aria-label="Needs you">
          <div class="row"><h2 class="grow">Needs you ({needsYouCount()})</h2></div>
          <For each={waitingAgents()}>
            {a => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('agents', { id: a.id })}>
              <span class="row"><Dot state="warn" /><AgentChip name={a.name} /> <small>is waiting for your approval</small></span>
              <Button size="sm">Review</Button>
            </div>}
          </For>
          <For each={failedAgents()}>
            {a => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('agents', { id: a.id })}>
              <span class="row"><Dot state="bad" /><AgentChip name={a.name} /> <small>failed{a.error ? `: ${a.error}` : ''}</small></span>
              <Button size="sm">Open</Button>
            </div>}
          </For>
          <For each={repoClashes()}>
            {c => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('code', { repo_id: props.id })}>
              <span class="row"><Dot state="warn" /><AgentChip name={c.agent.name} /> <small>{c.text}</small></span>
              <Button size="sm">See code</Button>
            </div>}
          </For>
          <For each={needsYouPrReviews()}>
            {r => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('review')}>
              <span class="row"><Dot state="warn" /><small>PR #{r.pr}{r.title ? ` · ${r.title}` : ''} needs you{blockedTitles(r) ? `: ${blockedTitles(r)}` : ''}</small></span>
              <Button size="sm">Review</Button>
            </div>}
          </For>
        </section>
      </Show>
      <section class="home-sec" aria-label="Coordinator"><div class="row"><h2 class="grow">Coordinator</h2><Show when={!coordinators().length}><Button variant="primary" onClick={() => setSpawnPreset({ role: 'orchestrator', repo_id: props.id })}>Start coordinator</Button></Show><Button size="sm" onClick={() => setSpawnPreset({ role: 'worker', repo_id: props.id })}>New agent</Button></div>
        <For each={coordinators()} fallback={<p class="small faint">No coordinator running.</p>}>
          {a => <div class="checkout-row"><span><strong>{a.name}</strong></span><small>{a.state}</small><Button size="sm" onClick={() => go('agents',{id:a.id})}>Open</Button></div>}
        </For>
      </section>
      <Records title="Tasks" rows={d().tasks} empty="No open tasks." action={link('Board','board')}>
        {t => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('board', { id: t.id })}><span>{t.title}</span><small>{t.status}{t.owner ? ` · ${t.owner}` : t.assigned ? ` · ${t.assigned}` : ''}</small></div>}
      </Records>
      <Records title="Agents" rows={d().agents} empty="No agents." action={link('Agents','agents')}>
        {a => <div class="checkout-row">{link(a.name,'agents',{id:a.id})}<small>{a.state}</small></div>}
      </Records>
      <Records title="Worktrees" rows={d().worktrees} empty="No worktrees." action={link('Repos','repos',{repo_id:props.id})}>
        {w => <div class="checkout-row"><span title={w.path}>{w.branch}</span><small>{w.state}</small></div>}
      </Records>
      <Records title="Resources" rows={d().resources} empty="None active.">
        {r => <div class="checkout-row"><span><strong>{r.owner}</strong> · {r.kind} <code>{r.handle}</code></span>
          <Show when={r.state === 'leaked'} fallback={<small>{r.state}</small>}>
            <span class="row"><small title="Philium could not stop this safely. Check it yourself.">could not stop</small>
              <Button size="sm" title="You checked it is gone or will handle it yourself. Nothing is stopped or deleted." onClick={() => rpc('reaper.forget', {ids:[r.id]}, {action:true}).catch(e => toast(String(e), 'danger'))}>Forget</Button></span>
          </Show></div>}
      </Records>
    </div>}</Show>
    <Show when={spawnPreset()}><SpawnModal preset={spawnPreset()} onClose={(a) => { setSpawnPreset(null); if (a) go('agents', { id: a.id }); }} /></Show>
  </Page>;
}
