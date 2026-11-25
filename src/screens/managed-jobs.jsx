import {createSignal,createEffect,For,Show,onMount,onCleanup} from 'solid-js';
import {createStore,reconcile} from 'solid-js/store';
import {rpc} from '../lib/rpc.js';
import {Button,Field,Select,TextArea,Empty} from '../ui/kit.jsx';
import {useScreenVisible,useVisibleEvent} from '../lib/visibility.js';

// Mounted with a stable checkout key; switching checkout destroys the inspector and draft.
export function ManagedJobs(props){
  const visible = useScreenVisible();
  const [view,setView]=createStore({data:null}),data=()=>view.data,[error,setError]=createSignal(''),[busy,setBusy]=createSignal(false);
  const [profiles,setProfiles]=createSignal([]),[profile,setProfile]=createSignal(''),[task,setTask]=createSignal(props.taskId||''),[cmd,setCmd]=createSignal(''),[minutes,setMinutes]=createSignal(10),[form,setForm]=createSignal(false),[plan,setPlan]=createSignal(null);
  const [tail,setTail]=createSignal(null),[release,setRelease]=createSignal('');let live=true,seq=0,lseq=0,timer=null,pollPending=false;
  const invoke=(method,args={})=>rpc('managed.'+method,{repo_id:props.id,...args,...(props.taskId?{task_id:props.taskId}:{})},{local:true});
  async function refresh(){
    const n=++seq;clearTimeout(timer);timer=null;
    try{
      const r=await invoke('list');if(!live||seq!==n)return;
      setView('data',reconcile(r));
      if((r.jobs||[]).some(j=>j.state!=='released')){
        if(visible()){pollPending=false;timer=setTimeout(refresh,5000);}
        else{pollPending=true;}
      }else{pollPending=false;}
    }catch(e){if(live&&seq===n)setError(String(e));}
  }
  createEffect(()=>{
    if(visible()&&pollPending&&live){
      pollPending=false;
      refresh();
    }else if(!visible()&&timer){
      clearTimeout(timer);
      timer=null;
      if((data()?.jobs||[]).some(j=>j.state!=='released'))pollPending=true;
    }
  });
  onMount(refresh);onCleanup(()=>{live=false;seq++;lseq++;clearTimeout(timer);});
  useVisibleEvent('managed:changed',p=>{if(!p?.repo_id||p.repo_id===props.id)refresh();},{debounceMs:200});
  useVisibleEvent('board:changed',()=>refresh(),{debounceMs:500});
  async function act(fn){if(busy())return;setError('');setBusy(true);try{await fn();}catch(e){if(live)setError(String(e));}finally{if(live)setBusy(false);}}
  async function openForm(){await refresh();if(!live)return;const p=await rpc('environments.profiles',{},{local:true});if(!live)return;setProfiles(p);setProfile(p.length===1?p[0]:'');setForm(true);}
  async function logs(j){const n=++lseq;setError('');setTail({id:j.id,text:'Reading logs…'});try{const r=await invoke('logs',{id:j.id});if(live&&n===lseq)setTail({id:j.id,text:r.text||'No output yet.',cut:r.truncated});}catch(e){if(live&&n===lseq)setTail({id:j.id,text:String(e)});}}
  const usable=()=>data()?.tasks.find(t=>t.id===task());
  const stateLabel=(s)=>s==='active'?'Running':s==='released'?'Released':s==='retiring'?'Removing':s==='issuing'?'Starting':s;
  const ready=()=>usable()&&profile()&&cmd().trim()&&Number.isInteger(minutes())&&minutes()>=1&&minutes()<=60;
  const close=()=>{setPlan(null);setForm(false);};
  return <section class="managed" aria-label="Managed task sandboxes">
    <div class="ws-row">
      <div>
        <h2>Task sandboxes</h2>
        <p class="ws-muted small">One offline container per claimed task. Philium checks it is the same container before removing it. Shared clusters are never removed here.</p>
      </div>
      <div class="ws-actions">
        <Button size="sm" variant="primary" disabled={busy()||form()} onClick={()=>act(openForm)}>New task sandbox</Button>
        <Button size="sm" disabled={busy()} onClick={()=>act(refresh)}>Refresh</Button>
      </div>
    </div>
    <Show when={error()}><p class="ws-alert" role="alert">{error()}</p></Show>
    <Show when={form()}>
      <fieldset class="managed-form" disabled={busy()||!!plan()}>
        <legend>Run a local command</legend>
        <Field label="Claimed task" hint={!data()?.tasks.length ? 'Claim a task with an agent in this workspace first.' : ''}>
          <Select disabled={!!props.taskId} value={task()} placeholder="Choose task and agent" options={(data()?.tasks||[]).map(t=>({value:t.id,label:`${t.title} · ${t.owner}`}))} onValue={setTask}/>
        </Field>
        <Field label="Colima profile">
          <Select value={profile()} placeholder="Choose runtime" options={profiles()} onValue={setProfile}/>
        </Field>
        <Field label="Maximum minutes">
          <input class="input" type="number" min="1" max="60" value={minutes()} onInput={e=>setMinutes(e.currentTarget.valueAsNumber)}/>
        </Field>
        <Field label="Container command">
          <TextArea class="mono" maxlength="4096" placeholder="printf 'ready\\n'" value={cmd()} onValue={setCmd}/>
        </Field>
        <div class="ws-actions">
          <Button size="sm" variant="primary" disabled={!ready()} onClick={()=>setPlan({task_id:task(),agent_id:usable().agent,profile:profile(),command:cmd(),minutes:minutes(),label:`${usable().title} · ${usable().owner}`})}>Preview sandbox</Button>
          <Button size="sm" variant="ghost" disabled={busy()} onClick={close}>Cancel</Button>
        </div>
      </fieldset>
      <Show when={plan()}>{p=><div class="managed-plan">
        <p>{p().label} on <b>{p().profile}</b> · {p().minutes} min. Local Alpine 3.20, no checkout mount, network or credentials. Files in /tmp disappear on release.</p>
        <pre class="ws-diff">{p().command}</pre>
        <div class="ws-actions">
          <Button size="sm" variant="primary" disabled={busy()} onClick={()=>act(async()=>{const {label,...args}=p();await invoke('launch',args);if(live){setCmd('');close();await refresh();}})}>Start offline sandbox</Button>
          <Button size="sm" variant="ghost" disabled={busy()} onClick={()=>setPlan(null)}>Back</Button>
        </div>
      </div>}</Show>
    </Show>
    <Show when={data()} fallback={<p class="ws-muted small" role="status">Reading sandboxes…</p>}>{d=>
      <Show when={d().jobs.length} fallback={<Empty title="No task sandboxes">Sandboxes started for this workspace's tasks show up here.</Empty>}>
        <div class="cleanup-list">
          <For each={d().jobs}>{j=><details class="cleanup-details managed-job">
            <summary class="cleanup-title"><span class={`dot ${j.state==='active'?'live':''}`}/><strong>{j.task_name||j.task}</strong><span class="ws-muted">· {j.agent_name||j.agent} · {stateLabel(j.state)}</span></summary>
            <div class="cleanup-main">
              <small class="ws-muted">{j.profile} · container <code>{j.cid?.slice(0,12)||'pending'}</code> · expires {new Date(j.expires).toLocaleTimeString()} · {j.receipt||'Cleanup pending'}</small>
              <div class="ws-actions">
                <Show when={j.state==='active'}><Button size="sm" onClick={()=>logs(j)}>Sandbox logs</Button></Show>
                <Show when={j.state!=='released'&&j.cid&&release()!==j.id}><Button size="sm" variant="danger" disabled={busy()} onClick={()=>setRelease(j.id)}>Release sandbox</Button></Show>
              </div>
              <Show when={release()===j.id}>
                <div class="ws-row managed-confirm">
                  <span class="small">Remove only this task’s container and its temporary files?</span>
                  <div class="ws-actions">
                    <Button size="sm" variant="danger" disabled={busy()} onClick={()=>act(async()=>{await invoke('release',{id:j.id});if(live){setRelease('');if(tail()?.id===j.id){lseq++;setTail(null);}await refresh();}})}>Confirm release</Button>
                    <Button size="sm" variant="ghost" onClick={()=>setRelease('')}>Keep sandbox</Button>
                  </div>
                </div>
              </Show>
              <Show when={tail()?.id===j.id}><pre class="ws-diff">{tail().text}</pre><Show when={tail().cut}><small class="ws-muted">Log output truncated.</small></Show></Show>
            </div>
          </details>}</For>
        </div>
      </Show>
    }</Show>
  </section>;
}
