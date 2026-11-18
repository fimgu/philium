import {createSignal,For,Show,onMount,onCleanup} from 'solid-js';
import {createStore,reconcile} from 'solid-js/store';
import {rpc} from '../lib/rpc.js';

// Mounted with a stable checkout key; switching checkout destroys the inspector and draft.
export function ManagedJobs(props){
  const [view,setView]=createStore({data:null}),data=()=>view.data,[error,setError]=createSignal(''),[busy,setBusy]=createSignal(false);
  const [profiles,setProfiles]=createSignal([]),[profile,setProfile]=createSignal(''),[task,setTask]=createSignal(props.taskId||''),[cmd,setCmd]=createSignal(''),[minutes,setMinutes]=createSignal(10),[form,setForm]=createSignal(false),[plan,setPlan]=createSignal(null);
  const [tail,setTail]=createSignal(null),[release,setRelease]=createSignal('');let live=true,seq=0,lseq=0,timer;
  const invoke=(method,args={})=>rpc('managed.'+method,{repo_id:props.id,...args,...(props.taskId?{task_id:props.taskId}:{})},{local:true});
  async function refresh(){const n=++seq;clearTimeout(timer);try{const r=await invoke('list');if(!live||seq!==n)return;setView('data',reconcile(r));if(r.jobs.some(j=>j.state!=='released'))timer=setTimeout(refresh,5000);}catch(e){if(live&&seq===n)setError(String(e));}}
  onMount(refresh);onCleanup(()=>{live=false;seq++;lseq++;clearTimeout(timer);});
  async function act(fn){if(busy())return;setError('');setBusy(true);try{await fn();}catch(e){if(live)setError(String(e));}finally{if(live)setBusy(false);}}
  async function openForm(){await refresh();if(!live)return;const p=await rpc('environments.profiles',{},{local:true});if(!live)return;setProfiles(p);setProfile(p.length===1?p[0]:'');setForm(true);}
  async function logs(j){const n=++lseq;setError('');setTail({id:j.id,text:'Reading logs…'});try{const r=await invoke('logs',{id:j.id});if(live&&n===lseq)setTail({id:j.id,text:r.text||'No output yet.',cut:r.truncated});}catch(e){if(live&&n===lseq)setTail({id:j.id,text:String(e)});}}
  const usable=()=>data()?.tasks.find(t=>t.id===task());
  return <section aria-label="Managed task sandboxes"><h2>Task sandboxes</h2><p>One offline container per claimed task. Philium verifies its identity before cleanup. Shared clusters are never removed here.</p>
    <button disabled={busy()} onClick={()=>act(openForm)}>New task sandbox</button> <button disabled={busy()} onClick={()=>act(refresh)}>Refresh sandboxes</button>
    <Show when={error()}><p role="alert">{error()}</p></Show>
    <Show when={form()}><fieldset disabled={busy()||!!plan()}><legend>Run a local command</legend>
      <label>Claimed task <select disabled={!!props.taskId} value={task()} onChange={e=>setTask(e.currentTarget.value)}><option value="">Choose task and agent</option><For each={data()?.tasks||[]}>{t=><option selected={task()===t.id} value={t.id}>{t.title} · {t.owner}</option>}</For></select></label>
      <Show when={!data()?.tasks.length}><p>Claim a task with an agent in this checkout first.</p></Show>
      <label>Colima profile <select value={profile()} onChange={e=>setProfile(e.currentTarget.value)}><option value="">Choose runtime</option><For each={profiles()}>{p=><option>{p}</option>}</For></select></label>
      <label>Maximum minutes <input type="number" min="1" max="60" value={minutes()} onInput={e=>setMinutes(e.currentTarget.valueAsNumber)}/></label>
      <label>Container command <textarea maxlength="4096" placeholder="printf 'ready\\n'" value={cmd()} onInput={e=>setCmd(e.currentTarget.value)}/></label>
      <button disabled={!usable()||!profile()||!cmd().trim()||!Number.isInteger(minutes())||minutes()<1||minutes()>60} onClick={()=>setPlan({task_id:task(),agent_id:usable().agent,profile:profile(),command:cmd(),minutes:minutes(),label:`${usable().title} · ${usable().owner}`})}>Preview sandbox</button></fieldset>
      <Show when={plan()}>{p=><div><p>{p().label} on <b>{p().profile}</b> · {p().minutes} min. Local Alpine 3.20, no checkout mount, network or credentials. Files in /tmp disappear on release.</p><pre>{p().command}</pre><button disabled={busy()} onClick={()=>act(async()=>{const {label,...args}=p();await invoke('launch',args);if(live){setCmd('');setPlan(null);setForm(false);await refresh();}})}>Start offline sandbox</button></div>}</Show>
      <button disabled={busy()} onClick={()=>{setPlan(null);setForm(false);}}>Dismiss</button></Show>
    <Show when={data()}>{d=><For each={d().jobs} fallback={<p>No managed sandboxes for this checkout.</p>}>{j=><details><summary>{j.task_name||j.task} · {j.agent_name||j.agent} · {j.state==='active'?'leased':j.state}</summary>
      <p>{j.profile} · container <code>{j.cid?.slice(0,12)||'pending'}</code> · expires {new Date(j.expires).toLocaleTimeString()} · {j.receipt||'Cleanup pending'}</p>
      <Show when={j.state==='active'}><button onClick={()=>logs(j)}>Sandbox logs</button></Show> <Show when={j.state!=='released'&&j.cid}><button disabled={busy()} onClick={()=>setRelease(j.id)}>Release sandbox</button></Show>
      <Show when={release()===j.id}><p>Remove only this task’s container and its temporary files?</p><button disabled={busy()} onClick={()=>act(async()=>{await invoke('release',{id:j.id});if(live){setRelease('');if(tail()?.id===j.id){lseq++;setTail(null);}await refresh();}})}>Confirm release</button> <button onClick={()=>setRelease('')}>Keep sandbox</button></Show>
      <Show when={tail()?.id===j.id}><pre style={{'white-space':'pre-wrap','overflow-wrap':'anywhere'}}>{tail().text}</pre>{tail().cut&&'Log output truncated.'}</Show>
    </details>}</For>}</Show></section>;
}
