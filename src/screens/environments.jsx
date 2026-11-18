import { createMemo, createSignal, createResource, For, Show, onCleanup } from 'solid-js';
import { activeRepoId, useStore } from '../lib/store.js';
import { Page } from '../ui/kit.jsx';
import { rpc } from '../lib/rpc.js';
const read=(m,args)=>rpc(`environments.${m}`,args,{local:true});

function Inventory(props) {
  const [rows,setRows]=createSignal(null), [issue,setIssue]=createSignal(''), [busy,setBusy]=createSignal(false);
  const [filter,setFilter]=createSignal(''), [selected,select]=createSignal(null), [tail,setTail]=createSignal(null);
  let generation=0, logRequest=0;
  async function refresh(){ const n=++generation; ++logRequest; select(null);setTail(null);setBusy(true);setIssue('');setRows(null);
    try{const r=await read('containers',{profile:props.profile});if(n===generation)setRows(r);}catch(e){if(n===generation)setIssue(String(e));}finally{if(n===generation)setBusy(false);}}
  refresh(); onCleanup(()=>{generation++;logRequest++;});
  async function showLogs(c){const n=++logRequest;select(c);setTail(null);
    try{const t=await read('logs',{profile:props.profile,id:c.id});if(n===logRequest)setTail(t);}catch(e){if(n===logRequest)setTail({error:String(e)});}}
  const groups=createMemo(()=>{const m=new Map(),q=filter().trim().toLowerCase();for(const c of rows()||[]){const label=c.cluster?`kind · ${c.cluster}`:c.project?`Compose · ${c.project}`:'Standalone containers';if(![label,c.name,c.image,c.status].join(' ').toLowerCase().includes(q))continue;if(!m.has(label))m.set(label,[]);m.get(label).push(c);}return [...m].sort(([a],[b])=>a.localeCompare(b));});
  return <section aria-label="Runtime inventory"><h3>{props.profile} · Colima</h3>
    <p>Observed on this runtime. Ownership not verified. Automatic deletion is off.</p>
    <button disabled={busy()} onClick={refresh}>Refresh inventory</button> <input aria-label="Filter containers" placeholder="Filter cluster, container or status" value={filter()} onInput={e=>setFilter(e.currentTarget.value)}/>
    <Show when={busy()}><p role="status">Reading local socket…</p></Show><Show when={issue()}><p role="alert">{issue()}</p></Show>
    <Show when={rows()}><p>{new Set(rows().map(c=>c.cluster).filter(Boolean)).size} kind clusters · {rows().length} containers (including stopped)</p>
      <For each={groups()} fallback={<p>No matching containers.</p>}>{([label,cs])=><details><summary>{label} ({cs.length})</summary><ul><For each={cs}>{c=><li><b>{c.name}</b> · {c.status} · <code>{c.image}</code> <button onClick={()=>showLogs(c)}>View logs</button></li>}</For></ul></details>}</For>
    </Show>
    <Show when={selected()}>{c=><aside aria-label="Container logs"><h3>{c().name} · logs</h3><p>{props.profile} · <code>{c().id.slice(0,12)}</code> · last 100 lines, up to 64 KiB. {c().cluster?'These are kind node logs, not workload pod logs.':''}</p>
      <button onClick={()=>showLogs(c())}>Refresh logs</button> <button onClick={()=>{++logRequest;select(null);setTail(null);}}>Close logs</button>
      <Show when={tail()} fallback={<p role="status">Reading logs…</p>}>{t=><Show when={!t().error} fallback={<p role="alert">{t().error}</p>}><pre style={{'white-space':'pre-wrap','overflow-wrap':'anywhere'}}>{t().text||'No log output.'}</pre><Show when={t().truncated}><p>Output limited to 64 KiB.</p></Show></Show>}</Show>
    </aside>}</Show></section>;
}
function Recorded(props){const [r]=createResource(async()=>{try{return {rows:(await rpc('workspace.overview',{repo_id:props.id},{local:true})).resources};}catch(e){return {error:String(e)};}});return <section><h3>{props.name} · recorded resources</h3><p>From this checkout’s agent ledger, not a live inventory or proof of deletion rights.</p><Show when={r()} fallback={<p>Loading…</p>}>{d=><Show when={!d().error} fallback={<p role="alert">{d().error}</p>}><ul><For each={d().rows} fallback={<li>No resources recorded for current agents.</li>}>{v=><li>{v.owner} · {v.kind} · <code>{v.handle}</code> · {v.state}</li>}</For></ul></Show>}</Show></section>;}
export default function Environments(){const store=useStore(),[scope,setScope]=createSignal('checkout'),[names,setNames]=createSignal(null),[profile,setProfile]=createSignal(''),[err,setErr]=createSignal('');let epoch=0;onCleanup(()=>epoch++);
  async function scan(){const n=++epoch;setScope('machine');setProfile('');setNames(null);setErr('');try{const r=await read('profiles');if(n!==epoch)return;setNames(r);if(r.length===1)setProfile(r[0]);}catch(e){if(n===epoch)setErr(String(e));}}
  return <Page title="Environments" sub="Inspect local runtimes without changing clusters or contexts."><div class="row gap-4"><button aria-pressed={scope()==='checkout'} onClick={()=>{++epoch;setScope('checkout');setProfile('');}}>This checkout</button><button aria-pressed={scope()==='machine'} onClick={scan}>Inspect local machine</button></div>
    <Show when={scope()==='checkout'} fallback={<section><h2>Local machine</h2><p>Colima profiles with local Docker sockets. No workload or cluster is started.</p><Show when={err()}><p role="alert">{err()}</p></Show><Show when={names()} fallback={!err()&&<p>Looking for profiles…</p>}><label>Runtime <select value={profile()} onChange={e=>setProfile(e.currentTarget.value)}><option value="">Choose profile</option><For each={names()}>{p=><option value={p}>{p}</option>}</For></select></label><Show when={!names().length}><p>No local Colima Docker socket found.</p></Show></Show><Show when={profile()} keyed>{p=><Inventory profile={p}/>}</Show></section>}>
    <Show when={activeRepoId()} keyed fallback={<p>Select a checkout or inspect the local machine.</p>}>{id=><Recorded id={id} name={store.repos()?.find(r=>r.id===id)?.name||'Checkout'}/>}</Show></Show></Page>;
}
