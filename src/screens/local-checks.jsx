import { createSignal, createEffect, For, Show, onCleanup, onMount } from 'solid-js';
import { rpc } from '../lib/rpc.js';
import { useScreenVisible, useClock } from '../lib/visibility.js';

export function LocalChecks(props) {
  const visible = useScreenVisible();
  const now = useClock(1000);
  const [rows,setRows]=createSignal([]), [tasks,setTasks]=createSignal([]), [error,setError]=createSignal('');
  const [target,setTarget]=createSignal('test'), [seconds,setSeconds]=createSignal(120), [task,setTask]=createSignal('');
  const [plan,setPlan]=createSignal(null), [busy,setBusy]=createSignal(false);
  const [memory,setMemory]=createSignal(null);let memorySeq=0;
  const call=(key,extra={})=>rpc('checks.'+key,{repo_id:props.repo,...extra},{local:true});
  let alive=true, timer=null, pollPending=false, seq=0;
  async function recall(){const n=++memorySeq;try{const m=await call('remembered');if(alive&&n===memorySeq)setMemory(m);}catch(e){if(alive&&n===memorySeq)setError(String(e));}}
  onMount(()=>{recall();});
  async function refresh() {
    const n=++seq; clearTimeout(timer); timer=null;
    try { const r=await call('list'); if (!alive || n!==seq) return; setRows(r??[]);
      if ((r??[]).some(x=>x.state==='running')) {
        if (visible()) { pollPending=false; timer=setTimeout(refresh,1000); }
        else { pollPending=true; }
      } else { pollPending=false; }
    } catch(e) { if(alive && n===seq) setError(String(e)); }
  }
  createEffect(()=>{
    if (visible() && pollPending && alive) {
      pollPending=false;
      refresh();
    } else if (!visible() && timer) {
      clearTimeout(timer);
      timer=null;
      if ((rows()??[]).some(x=>x.state==='running')) pollPending=true;
    }
  });
  onMount(()=>{refresh(); rpc('board.list',{repo_id:props.repo},{local:true}).then(r=>{if(alive) setTasks((r??[]).filter(t=>t.repo_id===props.repo));}).catch(e=>{if(alive)setError(String(e));});});
  onCleanup(()=>{alive=false; clearTimeout(timer);});
  async function act(fn) { if(busy())return; setBusy(true); setError(''); try {await fn();} catch(e){if(alive)setError(String(e));} finally{if(alive)setBusy(false);} }
  return <section aria-label="Local check runs"><h2>Local checks</h2>
    <p>Run a trusted Make target on a clean, pinned checkout. macOS only. No network or dependency downloads. Output stays on this device.</p>
    <p>Before each check, Philium looks for local Colima sockets. It does not connect to or reuse those environments.</p>
    <Show when={memory()?.observed}><p>Last observed for this checkout at {new Date(memory().observed).toLocaleString()} {now()>=memory().expires || now()<memory().observed?'(stale)':''}: {Array.isArray(memory().profiles)?(memory().profiles.join(', ')||'no direct sockets found'):'inventory unavailable'}. This is not a health or revision check.</p></Show>
    <fieldset disabled={busy() || Boolean(plan())}><label>Make target <input value={target()} maxlength="80" onInput={e=>setTarget(e.currentTarget.value)}/></label>
      <label>Timeout (seconds) <input type="number" min="1" max="600" value={seconds()} onInput={e=>setSeconds(e.currentTarget.valueAsNumber)}/></label>
      <label>Task <select value={task()} onChange={e=>setTask(e.currentTarget.value)}><option value="">No linked task</option><For each={tasks()}>{t=><option value={t.id}>{t.title}</option>}</For></select></label>
      <button disabled={!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$/.test(target()) || !Number.isInteger(seconds()) || seconds()<1 || seconds()>600} onClick={()=>act(async()=>{try{const p=await call('preview'); if(alive)setPlan({sha:p.sha,target:target(),timeout:seconds(),task_id:task()||null});}finally{if(alive)await recall();}})}>Preview check</button>
    </fieldset>
    <Show when={plan()}>{p=><div><p><code>make -f Makefile -- {p().target}</code> at <code>{p().sha}</code> · {p().timeout}s limit</p>
      <button disabled={busy()} onClick={()=>act(async()=>{try{await call('start',p());if(alive){setPlan(null);await refresh();}}finally{if(alive)await recall();}})}>Run checks</button> <button disabled={busy()} onClick={()=>setPlan(null)}>Back</button></div>}</Show>
    <Show when={error()}><p role="alert">{error()}</p></Show>
    <h3>Results recorded by Philium</h3><button disabled={busy()} onClick={()=>act(refresh)}>Refresh results</button>
    <For each={rows()} fallback={<p>No local results shown.</p>}>{r=><details><summary>{r.target} · {r.state} · {r.sha.slice(0,10)}</summary>
      <p><code>{r.sha}</code> · make -f Makefile -- {r.target} · {r.timeout}s limit · exit {r.code??'not recorded'} · {r.elapsed??'—'} ms · task {r.task_id??'none'}</p>
      <Show when={r.state==='running'}><button disabled={busy()||r.cancel} onClick={()=>act(async()=>{await call('cancel',{id:r.id});if(alive)await refresh();})}>{r.cancel?'Cancelling…':'Cancel run'}</button></Show>
      <p>stdout</p><pre>{r.stdout||'(empty)'}</pre><p>stderr</p><pre>{r.stderr||'(empty)'}</pre>
    </details>}</For></section>;
}
