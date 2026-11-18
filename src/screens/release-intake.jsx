import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc } from '../lib/rpc.js';

export function ReleaseIntake(props) {
  const [url,setUrl]=createSignal(''), [busy,setBusy]=createSignal(false), [message,setMessage]=createSignal('');
  const [graph,setGraph]=createSignal(null);
  const call=(method,extra={})=>rpc(`release_intake.${method}`,{repo_id:props.repo,...extra},{local:true});
  const [driver,setDriver]=createSignal(''),[receipt,setReceipt]=createSignal(null);
  const [profile,{refetch:reloadProfile}]=createResource(()=>call('sql_profile').catch(()=>null));
  const [drivers]=createResource(()=>call('sql_drivers').catch(()=>[]));
  async function enroll(){setBusy(true);try{await call('sql_enroll',{driver:driver()});await reloadProfile();setMessage('Pinned the driver for future PR captures in this checkout.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function compare(seq){setBusy(true);setMessage('Comparing independent SQL contracts locally…');try{setReceipt(await call('sql_compare',{generation:seq}));setMessage('Local comparison recorded. Staging and other required workflows remain unverified.');}catch(e){setMessage(`Comparison incomplete: ${String(e)}`);}finally{setBusy(false);}}
  const [rows,{refetch}]=createResource(async()=>{try{return {list:await call('history')};}catch(e){return {error:String(e)};}});
  async function resolve(event){event.preventDefault();if(busy())return;setBusy(true);setMessage('');setGraph(null);
    try{const r=await call('resolve',{url:url().trim()});await refetch();await inspect(r.generation,r.pr);if(graph()?.seq===r.generation && await call('sql_profile'))await compare(r.generation);}
    catch(e){setMessage(`Not checked: ${String(e)}`);}finally{setBusy(false);}}
  async function inspect(seq,pr){setGraph(null);setReceipt(null);setBusy(true);try{setReceipt(await call('sql_receipt',{generation:seq}));const g=await call('graph',{generation:seq});const scope=await call('followup',{generation:seq});setGraph({...g,scope,pr,seq});setMessage(`Inspected #${pr}, generation ${seq}.`);}catch(e){setMessage(`Revisions recorded for #${pr}. Ancestry not verified: ${String(e)}`);}finally{setBusy(false);}}
  async function mark(g){if(busy())return;setBusy(true);try{await call('checkpoint',{generation:g.seq});await inspect(g.seq,g.pr);}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  return <details aria-label="Release PR revisions"><summary>Release PR revisions</summary>
    <p>Capture the base and head from GitHub. If you enroll a local SQL driver, checking a PR also runs its contracts offline.</p>
    <details><summary>Configure local SQL verification</summary><p>Select a separate trusted checkout containing <code>philium/sql-contracts.json</code>. Its committed expectations are pinned until you enroll it again.</p><select aria-label="Independent SQL driver" disabled={busy()} value={driver()} onChange={e=>setDriver(e.currentTarget.value)}><option value="">Select a checkout</option><For each={drivers()||[]}>{r=><option value={r.id}>{r.name} · {r.id.slice(0,8)}</option>}</For></select> <button disabled={busy()||!driver()} onClick={enroll}>Pin driver</button><Show when={profile()}>{p=><p>Driver pinned at <code>{p().pin}</code>.</p>}</Show></details>
    <form onSubmit={resolve}><label>Pull request URL <input type="url" required value={url()} disabled={busy()} onInput={e=>setUrl(e.currentTarget.value)}/></label> <button disabled={busy()||!url().trim()}>Check revisions</button></form>
    <p role="status">{message()}</p><Show when={rows()?.error}><p role="alert">{rows().error}</p></Show>
    <Show when={graph()&&profile()}><button disabled={busy()} onClick={()=>compare(graph().seq)}>Run SQL comparison for this revision</button></Show>
    <Show when={receipt()}>{r=><section><h3>Local SQL evidence · #{r().pr}</h3><p>Head <code>{r().head}</code>. Staging not verified. No ship approval.</p><For each={r().cases}>{c=><p>{c.id}: <strong>{c.state.replaceAll('_',' ')}</strong></p>}</For><details><summary>Inspect revision pins and observation hashes</summary><pre>{JSON.stringify(r(),null,2)}</pre></details></section>}</Show>
    <Show when={graph()}>{g=><section><strong>{{full:'Full code review',followup:'Focused re-review candidate',checkpoint:'Code review recorded'}[g().scope.mode]}</strong><p>{g().scope.reason}</p><Show when={g().scope.mode==='followup'}><p>{g().scope.commits.length} commits since the reviewed head <code>{g().scope.from}</code>. Review through <code>{g().head}</code>.</p></Show><Show when={g().scope.mode!=='checkpoint'}><button disabled={busy()} onClick={()=>mark(g())}>I completed code review at {g().head.slice(0,10)}</button><small> Local checkpoint only. Does not close findings, post a review, or approve this PR.</small></Show></section>}</Show>
    <Show when={graph()}>{g=><details open><summary>#{g().pr} · generation {g().seq} · {g().commits.length} included commits</summary><p>Common ancestor <code>{g().merge_base}</code>. Target tip <code>{g().target}</code>. Merge parents show ancestry, not child PR identity or fault.</p><For each={g().commits}>{c=><div><code>{c.oid}</code> · {c.parents.length} parent(s)</div>}</For></details>}</Show>
    <For each={rows()?.list||[]}>{r=><p>#{r.pr} · recorded {new Date(r.observed).toLocaleString()} · base <code>{r.base}</code> · head <code>{r.head}</code> <button disabled={busy()} onClick={()=>inspect(r.seq,r.pr)}>Inspect local ancestry</button></p>}</For>
  </details>;
}
