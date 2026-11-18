import { For, Show, createResource, onCleanup } from 'solid-js';
import { Page, Button } from '../ui/kit.jsx';
import { go } from '../lib/nav.js';
import { rpc, useEvent } from '../lib/rpc.js';

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
  let timer;
  const refreshSoon = () => { if (!timer) timer = setTimeout(() => { timer = null; refetch(); }, 300); };
  for (const topic of ['agents:changed','board:changed','workspace:changed','reaper:changed']) useEvent(topic,refreshSoon);
  onCleanup(() => clearTimeout(timer));
  const link = (label, route, params) => <Button size="sm" variant="ghost" onClick={() => go(route,params)}>{label}</Button>;
  return <Page title="Workspace" sub="Selected checkout" actions={<Button onClick={() => refetch()} disabled={result.loading}>Refresh</Button>}>
    <Show when={result()?.error}><p role="alert">Could not load this checkout: {result().error}</p></Show>
    <Show when={result()?.data} fallback={!result()?.error && <p role="status">Loading checkout…</p>}>{d => <div aria-label="Checkout overview" class="checkout-home">
      <code class="ws-path">{d().path}</code>
      <div class="row">{link('Open terminals','terminals')}{link('Review changes','code',{repo_id:props.id})}{link('Review evidence','review')}</div>
      <Records title="Unfinished tasks" rows={d().tasks} empty="No unfinished tasks for this checkout." action={link('Open board','board')}>
        {t => <div class="checkout-row"><span>{t.title}</span><small>{t.status}</small></div>}
      </Records>
      <Records title="Agents" rows={d().agents} empty="No agents assigned." action={link('Open agents','agents')}>
        {a => <div class="checkout-row">{link(a.name,'agents',{id:a.id})}<small>{a.state}</small></div>}
      </Records>
      <Records title="Worktrees" rows={d().worktrees} empty="No managed worktrees." action={link('Inspect worktrees','repos',{repo_id:props.id})}>
        {w => <div class="checkout-row"><span title={w.path}>{w.branch}</span><small>{w.state}</small></div>}
      </Records>
      <Records title="Agent resources" rows={d().resources} empty="No unreleased resources recorded for current agents.">
        {r => <div class="checkout-row"><span><strong>{r.owner}</strong> · {r.kind}<br/><code>{r.handle}</code></span><small>{r.state}</small></div>}
      </Records>
      <p class="small faint">Recorded by Philium, not a live Docker or Kubernetes scan. Released resources and resources without a matching agent are not shown.{d().pilot && ' Host cleanup and kind cluster changes are disabled in this pilot.'}</p>
    </div>}</Show>
  </Page>;
}
