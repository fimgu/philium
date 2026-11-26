import { For, Show, createResource, createSignal, createMemo } from 'solid-js';
import { SpawnModal } from '../ui/spawn-modal.jsx';
import { AgentChip } from '../ui/agent-chip.jsx';
import { Page, Button, Badge, Dot, Card, Stat, toast } from '../ui/kit.jsx';
import { Icon } from '../ui/icons.jsx';
import { go } from '../lib/nav.js';
import { rpc } from '../lib/rpc.js';
import { useStore } from '../lib/store.js';
import { repoLabels } from '../lib/repo-labels.js';
import { useVisibleEvent } from '../lib/visibility.js';

const TASK_STATUS = { open: 'Open', claimed: 'In progress', in_review: 'In review', done: 'Done', cancelled: 'Cancelled' };
const AGENT_STATE = { provisioning: ['busy', 'Starting'], running: ['busy', 'Working'], waiting: ['warn', 'Needs you'], idle: ['live', 'Idle'], stopped: ['', 'Stopped'], failed: ['bad', 'Failed'], done: ['', 'Done'] };

function Records(props) {
  return <section class="home-sec"><div class="row" style={{ 'margin-bottom': '8px' }}><h2 class="grow">{props.title}</h2>{props.action}</div>
    <Card style={{ padding: 0, overflow: 'hidden' }}>
      <For each={props.rows.slice(0,20)} fallback={<p class="small faint" style={{ padding: '14px', margin: 0 }}>{props.empty}</p>}>{props.children}</For>
      <Show when={props.rows.length > 20}><p class="small faint" style={{ padding: '8px 12px', margin: 0 }}>Showing the first 20 records.</p></Show>
    </Card>
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
  const [quickBrief, setQuickBrief] = createSignal('');
  const [quickBusy, setQuickBusy] = createSignal(false);
  const quickLaunch = async (e) => {
    e?.preventDefault();
    const text = quickBrief().trim();
    if (!text || quickBusy()) return;
    setQuickBusy(true);
    try {
      const a = await rpc('agents.spawn', { role: 'worker', repo_id: props.id, brief: text });
      setQuickBrief('');
      toast(`${a.name} is starting`, 'secure');
      store.refetchAgents?.();
      go('agents', { id: a.id });
    } catch (err) {
      toast(String(err), 'danger');
    } finally {
      setQuickBusy(false);
    }
  };
  const allOf = (key) => result()?.data?.[key] ?? result()?.data?.agents ?? [];
  const coordinators = () => allOf('coordinators').filter(a => a.role === 'orchestrator' || a.role === 'coordinator');
  const waitingAgents = () => allOf('attention').filter(a => a.state === 'waiting');
  const failedAgents = () => allOf('attention').filter(a => a.state === 'failed');
  const repoClashes = () => allOf('attention').flatMap(a => (a.warnings || []).filter(w => w.kind === 'clash').map(w => ({ agent: a, text: w.text })));
  const needsYouPrReviews = () => (prReviews() || []).filter(r => r.verdict === 'needs_you');
  const blockedTitles = (r) => (r.items || []).filter(it => it.status === 'blocked').map(it => it.title).join(', ');
  const needsYouCount = () => waitingAgents().length + failedAgents().length + repoClashes().length + needsYouPrReviews().length;
  const store = useStore();
  const labels = createMemo(() => repoLabels(store.repos() ?? []));
  const repoTitle = () => labels().get(props.id) || result()?.data?.name || result()?.data?.path?.split('/').filter(Boolean).pop() || 'Workspace';
  const repoSub = () => result()?.data?.path || 'Active repository';
  const link = (label, route, params) => <Button size="sm" variant="ghost" onClick={() => go(route,params)}>{label}</Button>;
  return <Page title={repoTitle()} sub={repoSub()} actions={<div class="row">
    <Button variant="primary" onClick={() => setSpawnPreset({ role: 'worker', repo_id: props.id })}><Icon name="plus" size={12} /> New agent</Button>
    <Button onClick={() => refetch()} disabled={result.loading}>Refresh</Button>
  </div>}>
    <Show when={result()?.error}><p role="alert">Could not load workspace: {result().error}</p></Show>
    <Show when={result()?.data} fallback={!result()?.error && <p role="status">Loading workspace…</p>}>{d => <div aria-label="Checkout overview" class="checkout-home">
      <form class="home-launch" style={{ 'margin-bottom': 0 }} onSubmit={quickLaunch}>
        <Icon name="spark" size={16} />
        <input
          type="text"
          value={quickBrief()}
          disabled={quickBusy()}
          onInput={(e) => setQuickBrief(e.currentTarget.value)}
          placeholder={`Start an agent in ${repoTitle()}…`}
          aria-label={`Quick launch agent in ${repoTitle()}`}
        />
        <div class="row" style={{ gap: '4px' }}>
          {link('Terminals','terminals')}{link('Code','code',{repo_id:props.id})}{link('Review','review')}
        </div>
        <Button variant="primary" type="submit" busy={quickBusy()} disabled={!quickBrief().trim()}>
          <Icon name="send" size={12} /> Start
        </Button>
      </form>
      <div class="home-stats" style={{ 'margin-bottom': 0 }}>
        <Stat label="Agents" value={(d().agents ?? []).length} sub={`${(d().agents ?? []).filter(a => a.state === 'running').length} working`} />
        <Stat label="Needs you" value={needsYouCount()} sub={needsYouCount() ? 'action needed' : 'all clear'} />
        <Stat label="Tasks" value={(d().tasks ?? []).length} sub={`${(d().tasks ?? []).filter(t => t.status === 'in_review').length} in review`} />
        <Stat label="Worktrees" value={(d().worktrees ?? []).length} sub="isolated branches" />
      </div>
      <Show when={needsYouCount() > 0}>
        <section class="home-sec" aria-label="Needs you" style={{ 'margin-bottom': 0 }}>
          <div class="row" style={{ 'margin-bottom': '8px' }}><h2 class="grow">Needs you ({needsYouCount()})</h2></div>
          <Card class="home-needs-card" style={{ padding: 0, overflow: 'hidden' }}>
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
          </Card>
        </section>
      </Show>
      <div class="checkout-grid">
        <Records title="Tasks" rows={d().tasks} empty="No open tasks." action={link('Board','board')}>
          {t => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('board', { id: t.id })}><span class="grow ellipsis">{t.title}</span><span class="row"><Badge tone={t.blocked ? 'danger' : t.status === 'in_review' ? 'accent' : ''}>{TASK_STATUS[t.status] ?? t.status}</Badge><Show when={t.owner || t.assigned}><AgentChip name={t.owner || t.assigned} /></Show></span></div>}
        </Records>
        <Records title="Agents" rows={d().agents} empty="No agents." action={link('Agents','agents')}>
          {a => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('agents', { id: a.id })}><span class="row"><Dot state={AGENT_STATE[a.state]?.[0] ?? ''} /><AgentChip name={a.name} /></span><small>{AGENT_STATE[a.state]?.[1] ?? a.state}</small></div>}
        </Records>
        <section class="home-sec" aria-label="Coordinator"><div class="row" style={{ 'margin-bottom': '8px' }}><h2 class="grow">Coordinator</h2><Show when={!coordinators().length}><Button size="sm" variant="primary" onClick={() => setSpawnPreset({ role: 'orchestrator', repo_id: props.id })}>Start coordinator</Button></Show></div>
          <Card style={{ padding: 0, overflow: 'hidden' }}>
            <For each={coordinators()} fallback={<p class="small faint" style={{ padding: '14px', margin: 0 }}>No coordinator running.</p>}>
              {a => <div class="checkout-row" style={{ cursor: 'pointer' }} onClick={() => go('agents',{id:a.id})}><span class="row"><Dot state={AGENT_STATE[a.state]?.[0] ?? ''} /><AgentChip name={a.name} /></span><Button size="sm">Open</Button></div>}
            </For>
          </Card>
        </section>
        <Records title="Worktrees" rows={d().worktrees} empty="No worktrees." action={link('Repos','repos',{repo_id:props.id})}>
          {w => <div class="checkout-row"><span class="mono small ellipsis" title={w.path}>{w.branch}</span><small>{w.state}</small></div>}
        </Records>
      </div>
      <Show when={(d().resources ?? []).length > 0}>
        <Records title="Resources" rows={d().resources} empty="None active.">
          {r => <div class="checkout-row"><span><strong>{r.owner}</strong> · {r.kind} <code>{r.handle}</code></span>
            <Show when={r.state === 'leaked'} fallback={<small>{r.state}</small>}>
              <span class="row"><small title="Philium could not stop this safely. Check it yourself.">could not stop</small>
                <Button size="sm" title="You checked it is gone or will handle it yourself. Nothing is stopped or deleted." onClick={() => rpc('reaper.forget', {ids:[r.id]}, {action:true}).catch(e => toast(String(e), 'danger'))}>Forget</Button></span>
            </Show></div>}
        </Records>
      </Show>
    </div>}</Show>
    <Show when={spawnPreset()}><SpawnModal preset={spawnPreset()} onClose={(a) => { setSpawnPreset(null); if (a) go('agents', { id: a.id }); }} /></Show>
  </Page>;
}
