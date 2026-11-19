import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc } from '../lib/rpc.js';
import { readiness, summaryMarkdown, tone, HEADLINE } from '../lib/readiness.js';

const MARK = { ok: '✓', bad: '✕', unknown: '?' }, STATE = { ok: 'Pass', bad: 'Fail', unknown: 'Unknown: not a pass' };

// One plain sentence per contract state, so a child failure, a merge-only regression, a failure
// already on the target and missing evidence never read alike.
const CASE = {
  candidate_pass: 'Passes on this head',
  same_failure_on_target: 'Already failing on the target, same failure. Not introduced by this release',
  composed_head_regression: 'Merge-only regression: the target and both merged sides pass, the merged head fails',
  release_range_regression: 'Regression inside this release: the target passes, the head fails',
  failure_unattributed: 'Fails on this head. Where it started is not established',
  incomplete: 'Missing evidence: could not be observed on the pinned revisions',
};
const CHILD = {
  pass: 'passes on its own',
  same_failure_as_candidate: 'fails the same way on its own (child failure)',
  different_failure: 'fails differently on its own',
  untested: 'not tested',
};
const DEPLOYMENT = {
  match: 'matches this head and artifact',
  mismatch: 'wrong deployment: a different revision or artifact is running',
  mixed: 'mixed: replicas report different revisions or artifacts',
  not_ready: 'matching replicas are not ready',
  absent: 'not established (no observer or identity command)',
  unknown: 'unavailable, stale or incomplete',
};

export function ReleaseIntake(props) {
  const [url,setUrl]=createSignal(''), [busy,setBusy]=createSignal(false), [message,setMessage]=createSignal('');
  const [decision,setDecision]=createSignal(null);
  const [graph,setGraph]=createSignal(null),[members,setMembers]=createSignal(null),[port,setPort]=createSignal('');
  const call=(method,extra={})=>rpc(`release_intake.${method}`,{repo_id:props.repo,...extra},{local:true});
  const [driver,setDriver]=createSignal(''),[receipt,setReceipt]=createSignal(null),[token,setToken]=createSignal('');
  const [profile,{refetch:reloadProfile}]=createResource(()=>call('sql_profile').catch(()=>null));
  const [access,{refetch:reloadAccess}]=createResource(()=>call('origin_access').catch(()=>null));
  const [drivers]=createResource(()=>call('sql_drivers').catch(()=>[]));
  // What the selected verifier would check, read before pinning. Pinning is the confirmation.
  // A plain signal, not a resource: choosing a verifier must not blank the screen while it loads.
  const [preview,setPreview]=createSignal(null);
  async function pick(id){setDriver(id);setPreview(null);if(!id)return;const r=await call('sql_preview',{driver:id}).then(v=>({checks:v}),e=>({error:String(e)}));if(driver()===id)setPreview(r);}
  const [paths,setPaths]=createSignal({});
  const pathFor=name=>paths()[name]??(profile.latest?.programs||[]).find(p=>p.name===name)?.path??'';
  async function enroll(){setBusy(true);try{const programs=Object.fromEntries((preview()?.checks?.programs||[]).map(n=>[n,pathFor(n)]));await call('sql_enroll',{driver:driver(),stage_port:port()?Number(port()):null,programs});await reloadProfile();setMessage('Checks confirmed and pinned for this checkout.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  const show=v=>typeof v==='string'?v:JSON.stringify(v);
  // The table as text, to paste where the team reads it. Copying needs the window to be in front.
  async function copySummary(d){try{await navigator.clipboard.writeText(summaryMarkdown(d,readiness(d,members())));setMessage('Summary copied. Paste it where your team reads it.');}catch{setMessage('Copying is not available right now. Open "Summary to paste" and select the text.');}}
  const Checks=props=><div aria-label={props.label}>
    <For each={props.checks.cases}>{c=><p data-check={c.id}><strong>{c.id}</strong> · {c.kind}: <code>{show(c.what)}</code> must be <code>{show(c.expect)}</code></p>}</For>
    <For each={props.checks.live_cases}>{c=><p data-check={c.id}><strong>{c.id}</strong> · {c.kind}: <code>{c.what}</code> must be <code>{show(c.expect)}</code></p>}</For>
    <For each={props.checks.targets}>{t=><div data-target={t.name}><p><strong>Target {t.name}</strong>: {t.identity?<>revision and artifact read with <code>{t.identity.what}</code></>:'no identity command. Its results are shown but can never count toward a decision.'}</p>
      <For each={t.cases}>{c=><p data-check={c.id}><strong>{c.id}</strong> · runs <code>{c.what}</code> and expects <code>{show(c.expect)}</code></p>}</For></div>}</For>
    <p>{props.checks.release_records?'Reads independent release records and evidence from this verifier.':'Does not read release records, so it can never produce a ready verdict.'}</p>
  </div>;
  async function saveToken(){setBusy(true);try{await call('origin_token_set',{token:token()});setToken('');await reloadAccess();setMessage('Read-only origin token saved in the private vault.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function clearToken(){setBusy(true);try{await call('origin_token_clear');await reloadAccess();setMessage('Origin token removed.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function compare(seq){setBusy(true);setMessage('Comparing pinned contracts locally…');try{setDecision(null);setReceipt(await call('sql_compare',{generation:seq}));setMessage('Local comparison recorded. This is an observation, not a release decision. Choose Verify release for a decision.');}catch(e){setMessage(`Comparison incomplete: ${String(e)}`);}finally{setBusy(false);}}
  const [rows,{refetch}]=createResource(async()=>{try{return {list:await call('history')};}catch(e){return {error:String(e)};}});
  async function resolve(event){event.preventDefault();if(busy())return;setBusy(true);setMessage('');setGraph(null);setReceipt(null);
    setDecision(null);try{const r=await call('verify_url',{url:url().trim()});await refetch();await inspect(r.generation,r.pr);setDecision(r);setReceipt(r.receipt);setMessage(r.decision==='ready_for_reviewed_scope'?'Origin rechecked. The decision applies to the displayed revision and time.':'See the reasons listed under the decision.');}
    catch(e){setMessage(`Not checked: ${String(e)}`);}finally{setBusy(false);}}
  async function inspect(seq,pr,discover=false){setDecision(null);setMembers(null);setGraph(null);setReceipt(null);setBusy(true);try{setDecision(await call('decision',{generation:seq}));setReceipt(await call('sql_receipt',{generation:seq}));const g=await call('graph',{generation:seq});const scope=await call('followup',{generation:seq});setGraph({...g,scope,pr,seq});setMembers(await call(discover?'discover_members':'members',{generation:seq}).catch(()=>null));setMessage(`Inspected #${pr}, generation ${seq}.`);}catch(e){setMessage(`Revisions recorded for #${pr}. Ancestry not verified: ${String(e)}`);}finally{setBusy(false);}}
  async function mark(g){if(busy())return;setBusy(true);try{await call('checkpoint',{generation:g.seq});await inspect(g.seq,g.pr);}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  const deployment=(d,r)=>{const before=r?.deployment_identity?.state,after=r?.deployment_after?.state;if(!before)return 'not observed';const text=s=>DEPLOYMENT[s]||s;return after&&after!==before?`before: ${text(before)}; after: ${text(after)}`:`${text(before)}${after?' before and after the checks':''}`;};
  return <details aria-label="Release PR revisions" open><summary>Release check</summary>
    <p>Paste a release pull request. Philium checks its exact base, head, landed pull requests and your pinned checks, and answers one question: is this revision good to go? Checking never posts, approves, pushes or deploys.</p>
    <details aria-label="Release verification setup"><summary>Configure local contract verification</summary><p>Select a separate trusted checkout containing <code>philium/sql-contracts.json</code>. Its committed expectations are pinned until you enroll it again.</p><select aria-label="Independent SQL driver" disabled={busy()} value={driver()} onChange={e=>pick(e.currentTarget.value)}><option value="">Select a checkout</option><For each={drivers.latest||[]}>{r=><option value={r.id}>{r.name} · {r.id.slice(0,8)}</option>}</For></select> <label>Trusted loopback deployment observer port (optional) <input type="number" min="1024" max="65535" disabled={busy()} value={port()} onInput={e=>setPort(e.currentTarget.value)}/></label>
      <Show when={preview()?.error}><p role="alert">{preview().error}</p></Show>
      <Show when={preview()?.checks}>{c=><section aria-label="Checks to confirm"><h3>Read these checks before you pin</h3><p>Verifier commit <code>{c().pin}</code>. Pinning confirms exactly these checks. Nothing counts as ready before you do.</p><Checks label="Proposed checks" checks={c()}/>
        <For each={c().programs}>{name=><label>Where is the <strong>{name}</strong> program? (absolute path) <input aria-label={`Path of ${name}`} disabled={busy()} value={pathFor(name)} onInput={e=>setPaths({...paths(),[name]:e.currentTarget.value})}/></label>}</For>
        <Show when={c().programs.length}><small> These programs run with your own login. Philium only runs the reads listed above, and refuses commands that look like a change.</small></Show></section>}</Show>
      <button disabled={busy()||!driver()||!preview()?.checks} onClick={enroll}>Confirm and pin these checks</button>
      <Show when={profile.latest} fallback={<p aria-label="Pinned verifier">No verifier pinned for this checkout yet.</p>}>{p=><><p aria-label="Pinned verifier">Verifier <strong>{p().driver_name || p().driver}</strong> pinned at <code>{p().pin}</code>. {p().confirmed ? `You confirmed its checks on ${new Date(p().confirmed).toLocaleString()}.` : 'Pinned before confirmations were recorded: read the checks and pin again.'} Deployment observer: {p().observer_port ? `loopback port ${p().observer_port}` : 'none'}.<For each={p().programs||[]}>{g=><> Program {g.name}: <code>{g.path}</code>.</>}</For></p><Show when={p().checks}><details aria-label="Pinned checks"><summary>Checks pinned for this checkout</summary><Checks label="Confirmed checks" checks={p().checks}/></details></Show></>}</Show>
      <p aria-label="Origin read access">Origin read access: public pull requests are read without credentials. {access.latest?.token_saved ? 'A read-only token is saved for private repositories.' : 'Private repositories need a read-only GitHub token.'}</p>
      <label>GitHub read-only token (optional, for private repositories) <input type="password" autocomplete="off" aria-label="GitHub read-only token" disabled={busy()} value={token()} onInput={e=>setToken(e.currentTarget.value)}/></label> <button disabled={busy()||!token().trim()} onClick={saveToken}>Save token</button> <Show when={access.latest?.token_saved}><button disabled={busy()} onClick={clearToken}>Remove token</button></Show>
      <small> Stored in the private vault, which agents cannot read. Sent only to api.github.com. Grant it read access to pull requests and contents only.</small>
    </details>
    <p aria-label="Verification setup summary">{profile.latest ? <>Set up for this checkout: verifier <strong>{profile.latest.driver_name || profile.latest.driver}</strong> pinned at <code>{String(profile.latest.pin).slice(0, 12)}</code>, deployment observer {profile.latest.observer_port ? `on loopback port ${profile.latest.observer_port}` : 'not configured'}. Paste a pull request to verify.</> : 'Not set up yet: without a pinned verifier a pull request can be resolved but not verified. Open the configuration above once.'}</p>
    <form onSubmit={resolve}><label>Pull request URL <input type="url" required value={url()} disabled={busy()} onInput={e=>setUrl(e.currentTarget.value)}/></label> <button disabled={busy()||!url().trim()}>Verify release</button></form>
    <Show when={decision()}>{d=><section aria-label="Release decision" class={`decision ${tone(d().decision)}`}><div class="verdict"><span class="glyph" aria-hidden="true">{({ok:'✓',bad:'✕',old:'↻'})[tone(d().decision)]||'?'}</span><div class="grow"><h3>{HEADLINE[d().decision] || d().decision}</h3><p>#{d().pr} · <code>{d().head}</code> · {new Date(d().checked_ms).toLocaleString()}. Contracts: {d().contract_decision.replaceAll('_',' ')}. No review posted.</p></div><button type="button" onClick={()=>copySummary(d())}>Copy summary</button></div>
      <table class="ready" aria-label="Release readiness"><tbody><For each={readiness(d(),members())}>{row=><tr data-row={row.id} data-state={row.state}><td class="mark"><span class={row.state} title={STATE[row.state]}>{MARK[row.state]}</span></td><th scope="row">{row.label}</th><td>{row.detail}</td></tr>}</For></tbody></table>
      <small>Green needs all of its evidence. Unknown is not a pass, and a new head starts a new table.</small>
      <details aria-label="Summary to paste"><summary>Summary to paste</summary><pre>{summaryMarkdown(d(),readiness(d(),members()))}</pre></details><Show when={d().receipt?.checks_confirmed_ms}><p aria-label="Checks confirmation">Checks confirmed by you on {new Date(d().receipt.checks_confirmed_ms).toLocaleString()}.</p></Show>
      <Show when={(rows.latest?.list||[]).some(r=>r.pr===d().pr&&r.seq>d().generation)}><p aria-label="Earlier revision"><strong>Saved result for an earlier revision.</strong> A newer head of #{d().pr} is recorded. It starts without a verdict and does not inherit this one.</p></Show>
      <dl aria-label="Verified identities">
        <dt>Generation</dt><dd>{d().generation}</dd>
        <dt>Target (base)</dt><dd><code>{d().base}</code></dd>
        <dt>Head</dt><dd><code>{d().head}</code></dd>
        <dt>Artifact</dt><dd>{d().artifact ? <code>{d().artifact}</code> : 'No independent artifact record for this exact head'}</dd>
        <dt>Origin</dt><dd>{d().origin_current === true ? 'Rechecked after the checks: unchanged' : d().origin_current === false ? 'Moved during verification' : 'Could not be reconfirmed'}</dd>
        <dt>Deployment</dt><dd>{deployment(d(), d().receipt)}</dd>
        <dt>Verifier</dt><dd>{d().receipt?.driver_pin ? <>pinned at <code>{d().receipt.driver_pin}</code>{d().receipt.release_record_pin ? <>, record read at <code>{d().receipt.release_record_pin}</code></> : ', no release record for this head'}</> : 'none pinned'}</dd>
      </dl>
      <Show when={(d().why || []).length} fallback={<Show when={d().why}><p aria-label="Decision reasons">Nothing stands in the way within the reviewed scope. This is not a GitHub approval.</p></Show>}><><div class="why">Why it is not ready</div><ul aria-label="Decision reasons"><For each={d().why}>{w=><li>{w}</li>}</For></ul></></Show>
      <Show when={!d().why && d().reason}><p aria-label="Decision reasons">{d().reason}</p></Show>
    </section>}</Show>
    <p role="status">{message()}</p><Show when={rows.latest?.error}><p role="alert">{rows.latest.error}</p></Show>
    <Show when={graph()&&profile.latest}><button disabled={busy()} onClick={()=>compare(graph().seq)}>Run pinned contracts for this revision</button></Show>
    <Show when={receipt()}>{r=><section aria-label="Contract observations"><h3>{r().release_decision==='not_ready_at_tested_head'?'Regression at tested revision':'Contract observation detail'} · #{r().pr}</h3><p>Local contract evidence for <code>{r().head}</code> against target <code>{r().target}</code>. Deployment identity at start: {r().deployment_identity?.state || 'unobserved'}. Local observations are not a ship approval.</p><For each={r().cases}>{c=><div data-case-state={c.state}><p>{c.id}: <strong>{CASE[c.state] || c.state.replaceAll('_',' ')}{c.new_at_head ? '. New in this release, so there is nothing to compare on the target' : ''}</strong></p><For each={c.landed_evidence || []}>{e=><small>#{e.pr} at {e.oid.slice(0,10)}: {CHILD[e.result] || e.result.replaceAll('_',' ')}. </small>}</For></div>}</For><For each={r().targets || []}>{t=><p data-target-identity={t.identity_before?.state}>Target {t.name}: identity before the checks {DEPLOYMENT[t.identity_before?.state] || t.identity_before?.state}; after {DEPLOYMENT[t.identity_after?.state] || t.identity_after?.state}.</p>}</For><For each={r().live_cases || []}>{c=><div data-live-state={c.state}><p>{c.id} ({c.target ? `target ${c.target}` : 'running product'}): <strong>{c.state === 'pass' ? 'observed twice, matches the contract' : c.state === 'mismatch' ? 'observed twice, differs from the contract' : 'not observed'}</strong></p></div>}</For><details><summary>Inspect revision pins and observation hashes</summary><pre>{JSON.stringify(r(),null,2)}</pre></details></section>}</Show>
    <Show when={graph()}>{g=><section><strong>{{full:'Full code review',followup:'Focused re-review candidate',checkpoint:'Code review recorded'}[g().scope.mode]}</strong><p>{g().scope.reason}</p><Show when={g().scope.mode==='followup'}><p>{g().scope.commits.length} commits since the reviewed head <code>{g().scope.from}</code>. Review through <code>{g().head}</code>.</p></Show><Show when={g().scope.mode!=='checkpoint'}><button disabled={busy()} onClick={()=>mark(g())}>I completed code review at {g().head.slice(0,10)}</button><small> Local checkpoint only. Does not close findings, post a review, or approve this PR.</small></Show></section>}</Show>
    <Show when={graph()}>{g=><details open aria-label="Included commits"><summary>#{g().pr} · generation {g().seq} · {g().commits.length} included commits</summary><p>Common ancestor <code>{g().merge_base}</code>. Target tip <code>{g().target}</code>. Merge parents show ancestry, not child PR identity or fault.</p><For each={g().commits}>{c=><div><code>{c.oid}</code> · {c.parents.length} parent(s)</div>}</For></details>}</Show>
    <Show when={graph()}><section aria-label="Landed pull requests"><h3>Landed pull requests</h3><Show when={members()} fallback={<p>Origin membership not established.</p>}>{m=><><For each={m().landed}>{p=><p>#{p.pr} landed at <code>{p.oid}</code></p>}</For><For each={m().edges}>{e=><p>#{e.outer} contains #{e.inner}</p>}</For><p>{m().missing} missing lookups, {m().unmapped} unmapped commits, {m().ambiguous} ambiguous. Membership does not identify fault.</p></>}</Show></section></Show>
    <For each={rows.latest?.list||[]}>{r=><p data-generation={r.seq}>#{r.pr} · generation {r.seq} · recorded {new Date(r.observed).toLocaleString()} · base <code>{r.base}</code> · head <code>{r.head}</code> <button disabled={busy()} onClick={()=>inspect(r.seq,r.pr)}>Inspect local ancestry</button></p>}</For>
  </details>;
}
