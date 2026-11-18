import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc } from '../lib/rpc.js';

export function ReleaseIntake(props) {
  const [url,setUrl]=createSignal(''), [busy,setBusy]=createSignal(false), [message,setMessage]=createSignal('');
  const call=(method,extra={})=>rpc(`release_intake.${method}`,{repo_id:props.repo,...extra},{local:true});
  const [rows,{refetch}]=createResource(async()=>{try{return {list:await call('history')};}catch(e){return {error:String(e)};}});
  async function resolve(event){event.preventDefault();if(busy())return;setBusy(true);setMessage('');
    try{const r=await call('resolve',{url:url().trim()});setMessage(`Checked #${r.pr} at ${new Date(r.checked).toLocaleTimeString()}. No release tests have run.`);await refetch();}
    catch(e){setMessage(`Not checked: ${String(e)}`);}finally{setBusy(false);}}
  return <details aria-label="Release PR revisions"><summary>Release PR revisions</summary>
    <p>Capture the base and head from GitHub. This reads commit IDs, not source, and does not start a release run.</p>
    <form onSubmit={resolve}><label>Pull request URL <input type="url" required value={url()} disabled={busy()} onInput={e=>setUrl(e.currentTarget.value)}/></label> <button disabled={busy()||!url().trim()}>Check revisions</button></form>
    <p role="status">{message()}</p><Show when={rows()?.error}><p role="alert">{rows().error}</p></Show>
    <For each={rows()?.list||[]}>{r=><p>#{r.pr} · recorded {new Date(r.observed).toLocaleString()} · base <code>{r.base}</code> · head <code>{r.head}</code></p>}</For>
  </details>;
}
