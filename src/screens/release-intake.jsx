import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc } from '../lib/rpc.js';

export function ReleaseIntake(props) {
  const [url,setUrl]=createSignal(''), [busy,setBusy]=createSignal(false), [message,setMessage]=createSignal('');
  const [graph,setGraph]=createSignal(null);
  const call=(method,extra={})=>rpc(`release_intake.${method}`,{repo_id:props.repo,...extra},{local:true});
  const [rows,{refetch}]=createResource(async()=>{try{return {list:await call('history')};}catch(e){return {error:String(e)};}});
  async function resolve(event){event.preventDefault();if(busy())return;setBusy(true);setMessage('');setGraph(null);
    try{const r=await call('resolve',{url:url().trim()});await refetch();await inspect(r.generation,r.pr);}
    catch(e){setMessage(`Not checked: ${String(e)}`);}finally{setBusy(false);}}
  async function inspect(seq,pr){setGraph(null);setBusy(true);try{const g=await call('graph',{generation:seq});const scope=await call('followup',{generation:seq});setGraph({...g,scope,pr,seq});setMessage(`Inspected #${pr}, generation ${seq}. No release tests have run.`);}catch(e){setMessage(`Revisions recorded for #${pr}. Ancestry not verified: ${String(e)}`);}finally{setBusy(false);}}
  async function mark(g){if(busy())return;setBusy(true);try{await call('checkpoint',{generation:g.seq});await inspect(g.seq,g.pr);}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  return <details aria-label="Release PR revisions"><summary>Release PR revisions</summary>
    <p>Capture the base and head from GitHub. This reads commit IDs, not source, and does not start a release run.</p>
    <form onSubmit={resolve}><label>Pull request URL <input type="url" required value={url()} disabled={busy()} onInput={e=>setUrl(e.currentTarget.value)}/></label> <button disabled={busy()||!url().trim()}>Check revisions</button></form>
    <p role="status">{message()}</p><Show when={rows()?.error}><p role="alert">{rows().error}</p></Show>
    <Show when={graph()}>{g=><section><strong>{{full:'Full code review',followup:'Focused re-review candidate',checkpoint:'Code review recorded'}[g().scope.mode]}</strong><p>{g().scope.reason}</p><Show when={g().scope.mode==='followup'}><p>{g().scope.commits.length} commits since the reviewed head <code>{g().scope.from}</code>. Review through <code>{g().head}</code>.</p></Show><Show when={g().scope.mode!=='checkpoint'}><button disabled={busy()} onClick={()=>mark(g())}>I completed code review at {g().head.slice(0,10)}</button><small> Local checkpoint only. Does not close findings, post a review, or approve this PR.</small></Show></section>}</Show>
    <Show when={graph()}>{g=><details open><summary>#{g().pr} · generation {g().seq} · {g().commits.length} included commits</summary><p>Common ancestor <code>{g().merge_base}</code>. Target tip <code>{g().target}</code>. Merge parents show ancestry, not child PR identity or fault.</p><For each={g().commits}>{c=><div><code>{c.oid}</code> · {c.parents.length} parent(s)</div>}</For></details>}</Show>
    <For each={rows()?.list||[]}>{r=><p>#{r.pr} · recorded {new Date(r.observed).toLocaleString()} · base <code>{r.base}</code> · head <code>{r.head}</code> <button disabled={busy()} onClick={()=>inspect(r.seq,r.pr)}>Inspect local ancestry</button></p>}</For>
  </details>;
}
