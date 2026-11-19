import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc, useEvent } from '../lib/rpc.js';
import { plainError } from '../lib/plain-error.js';
import { Markdown } from '../ui/markdown.jsx';
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
  const [graph,setGraph]=createSignal(null),[members,setMembers]=createSignal(null),[port,setPort]=createSignal(''),[cover,setCover]=createSignal(null);
  const call=(method,extra={})=>rpc(`release_intake.${method}`,{repo_id:props.repo,...extra},{local:true});
  const [driver,setDriver]=createSignal(''),[receipt,setReceipt]=createSignal(null),[token,setToken]=createSignal('');
  const [profile,{refetch:reloadProfile}]=createResource(()=>call('sql_profile').catch(()=>null));
  const [access,{refetch:reloadAccess}]=createResource(()=>call('origin_access').catch(()=>null));
  const [drivers]=createResource(()=>call('sql_drivers').catch(()=>[]));
  // GitHub through the sign-in the GitHub CLI already has on this Mac: one press, no token.
  const [github,{refetch:reloadGithub}]=createResource(()=>call('origin_connection').catch(()=>null));
  async function connectGithub(){setBusy(true);try{const r=await call('origin_connect');await reloadGithub();setMessage(`GitHub connected as ${r.login}. Philium keeps no token; it asks the GitHub CLI when it needs to read.`);}catch(e){setMessage(`GitHub not connected: ${String(e)}`);}finally{setBusy(false);}}
  async function disconnectGithub(){setBusy(true);try{await call('origin_disconnect');await reloadGithub();setMessage('GitHub disconnected for this checkout.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  // What the selected verifier would check, read before pinning. Pinning is the confirmation.
  // A plain signal, not a resource: choosing a verifier must not blank the screen while it loads.
  const [preview,setPreview]=createSignal(null);
  async function pick(id){setDriver(id);setPreview(null);if(!id)return;const r=await call('sql_preview',{driver:id}).then(v=>({checks:v}),e=>({error:String(e)}));if(driver()===id)setPreview(r);}
  const [paths,setPaths]=createSignal({});
  // Checks reach a checkout in one of two ways that need no folder: an agent drafts them and the
  // person accepts, or (advanced) one file is read in. Either way Philium keeps them itself.
  const KEPT='kept-by-philium';
  const [file,setFile]=createSignal(''),[drafting,setDrafting]=createSignal(false);
  const [proposals,{refetch:reloadProposals}]=createResource(()=>call('sql_proposals').catch(()=>[]));
  useEvent('release:proposals',()=>reloadProposals());
  let setupEl;
  async function useKept(){await pick(KEPT);if(setupEl)setupEl.open=true;}
  async function importFile(){setBusy(true);try{await call('sql_import',{file:file().trim()});setFile('');await useKept();setMessage('Read. Check them below, then confirm.');}catch(e){setMessage(`Not read: ${String(e)}`);}finally{setBusy(false);}}
  async function accept(id){setBusy(true);try{await call('sql_accept',{id});await reloadProposals();await useKept();setMessage('Read these checks below, then confirm them.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function dismiss(id){setBusy(true);try{await call('sql_dismiss',{id});await reloadProposals();}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  const [drafter,{refetch:reloadDrafter}]=createResource(()=>call('drafter').catch(()=>null));
  // An agent Review started that failed before it ever ran holds nothing: removed on the next press, so the agent list stays clean.
  const tidy=l=>l?.state==='failed'&&!l.started?rpc('agents.remove',{id:l.id}).catch(()=>null):null;
  async function draft(){setDrafting(true);try{const brief=await call('sql_brief',{generation:graph()?.seq??decision()?.generation??null});await tidy(drafter());const a=await rpc('agents.spawn',{role:'reviewer',repo_id:props.repo,brief},{action:true});await call('lead',{kind:'draft',agent:a.id}).catch(()=>null);await reloadDrafter();setMessage(`${a?.name||'An agent'} is drafting checks. They appear here when it is done. You can keep working.`);}catch(e){setMessage(`Not started: ${String(e)}`);}finally{setDrafting(false);}}
  const pathFor=name=>paths()[name]??(profile.latest?.programs||[]).find(p=>p.name===name)?.path??'';
  async function enroll(){setBusy(true);try{const programs=Object.fromEntries((preview()?.checks?.programs||[]).map(n=>[n,pathFor(n)]));await call('sql_enroll',{driver:driver(),stage_port:port()?Number(port()):null,programs});await reloadProfile();setMessage('Checks confirmed and pinned for this checkout.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  const show=v=>typeof v==='string'?v:JSON.stringify(v);
  // The table as text, to paste where the team reads it. Copying needs the window to be in front.
  async function copySummary(d){try{await navigator.clipboard.writeText(summaryMarkdown(d,readiness(d,members(),cover()),review.latest));setMessage('Summary copied. Paste it where your team reads it.');}catch{setMessage('Copying is not available right now. Open "Summary to paste" and select the text.');}}
  const Checks=props=><div aria-label={props.label}>
    <For each={props.checks.cases}>{c=><p data-check={c.id}><strong>{c.why || c.id}</strong><br/><small>{c.why ? `${c.id} · ` : ''}{c.kind}: </small><code>{show(c.what)}</code><small> must be </small><code>{show(c.expect)}</code></p>}</For>
    <For each={props.checks.live_cases}>{c=><p data-check={c.id}><strong>{c.id}</strong> · {c.kind}: <code>{c.what}</code> must be <code>{show(c.expect)}</code></p>}</For>
    <For each={props.checks.targets}>{t=><div data-target={t.name}><p><strong>Target {t.name}</strong>: {t.identity?<>revision and artifact read with <code>{t.identity.what}</code></>:'no identity command. Its results are shown but can never count toward a decision.'}</p>
      <For each={t.cases}>{c=><p data-check={c.id}><strong>{c.id}</strong> · runs <code>{c.what}</code> and expects <code>{show(c.expect)}</code></p>}</For></div>}</For>
    <p><small>{props.checks.release_records?'Also reads the build record and the review kept for each release.':'On their own these checks cannot make a release Ready: that also needs a build record and a review for the exact head.'}</small></p>
  </div>;
  async function saveToken(){setBusy(true);try{await call('origin_token_set',{token:token()});setToken('');await reloadAccess();setMessage('Read-only origin token saved in the private vault.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function clearToken(){setBusy(true);try{await call('origin_token_clear');await reloadAccess();setMessage('Origin token removed.');}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  async function compare(seq){setBusy(true);setMessage('Comparing pinned contracts locally…');try{setDecision(null);setReceipt(await call('sql_compare',{generation:seq}));setMessage('Local comparison recorded. This is an observation, not a release decision. Choose Verify release for a decision.');}catch(e){setMessage(`Comparison incomplete: ${String(e)}`);}finally{setBusy(false);}}
  const [rows,{refetch}]=createResource(async()=>{try{return {list:await call('history')};}catch(e){return {error:String(e)};}});
  async function resolve(event){event.preventDefault();if(busy())return;setBusy(true);setMessage('Reading the pull request…');setGraph(null);setReceipt(null);
    setDecision(null);try{const r=await call('verify_url',{url:url().trim()});await refetch();await inspect(r.generation,r.pr);setDecision(r);setReceipt(r.receipt);setMessage('');}
    catch(e){setMessage(`Not checked: ${String(e)}`);}finally{setBusy(false);}}
  async function inspect(seq,pr,discover=false){setDecision(null);setMembers(null);setCover(null);setGraph(null);setReceipt(null);setBusy(true);try{setDecision(await call('decision',{generation:seq}));setReceipt(await call('sql_receipt',{generation:seq}));const g=await call('graph',{generation:seq});const scope=await call('followup',{generation:seq});setGraph({...g,scope,pr,seq});setMembers(await call(discover?'discover_members':'members',{generation:seq}).catch(()=>null));setCover(await call('coverage',{generation:seq}).catch(()=>null));setMessage(`Inspected #${pr}, generation ${seq}.`);}catch(e){setMessage(`Revisions recorded for #${pr}. Ancestry not verified: ${String(e)}`);}finally{setBusy(false);}}
  // One action for the whole review: check the facts, then start an orchestrator that plans the
  // checks, hands them to helpers, and writes the report with every finding's severity and origin.
  const [review,{refetch:reloadReview}]=createResource(()=>decision()?.generation??graph()?.seq??null,g=>g?call('review_items',{generation:g}).catch(()=>null):null);
  useEvent('release:review',()=>{reloadReview();reloadDrafter();});useEvent('board:changed',()=>reloadReview());useEvent('agents:changed',()=>{reloadReview();reloadDrafter();});
  async function reviewWithAgents(){if(busy())return;await resolve({preventDefault(){}});const d=decision();if(!d)return;setBusy(true);
    try{const r=await call('review_start',{generation:d.generation});if(!r?.brief)throw new Error('Philium did not prepare the review. Try again.');await tidy(r.previous_lead);
      // The orchestrator and its helpers start on the pull request's head, the revision under review.
      const a=await rpc('agents.spawn',{role:'orchestrator',repo_id:props.repo,brief:r.brief,base:r.head},{action:true});await call('lead',{kind:'review',agent:a.id}).catch(()=>null);await reloadReview();setMessage(`${a?.name||'An orchestrator'} is organising the review of #${r.pr}. The plan appears below as it is written. You can leave this screen.`);}
    catch(e){setMessage(`Review not started: ${String(e)}`);}finally{setBusy(false);}}
  const reviewHead=v=>{const l=v.lead;if(l?.state==='failed')return 'The review could not start';if(v.items?.length)return AGENT_HEAD[v.headline]||v.headline;if(!l)return 'No orchestrator is working on this review';if(l.state==='provisioning')return `Starting ${l.name}`;if(l.state==='stopped')return `${l.name} is stopped`;return `${l.name} is writing the plan`;};
  const ORIGIN={introduced:'introduced',pre_existing:'pre-existing',unverified:'origin unverified'};
  const ORIGIN_MEANS={introduced:'Fails on this release and passes on the target: this release caused it.',pre_existing:'Fails the same way on the target: it was there before this release.',unverified:'The target could not be checked, or fails differently: it may be this release.'};
  // Findings first, worst on top; then what could not be checked, what is under way, what is to do, what passed.
  const byWeight=items=>[...items].sort((a,b)=>weight(a)-weight(b));
  const weight=it=>it.result==='fail'?(it.severity??1):it.result==='blocked'?10:it.status==='claimed'?11:!it.result?12:20;
  const itemMark=it=>it.result==='pass'?['ok','✓','Passed']:it.result==='fail'?['bad','✕','Failed']:it.result==='blocked'?['unknown','!','Blocked: could not be checked']:it.status==='claimed'?['doing','…','In progress']:it.status==='open'?['todo','○','To do']:['unknown','?','Handed in without a result'];
  const AGENT_HEAD={none:'The orchestrator is writing the plan',do_not_ship:'Agents\' review: do not ship',not_ready:'Agents\' review: not ready yet',passed:'Agents\' review: passed. Your sign-off is next'};
  async function mark(g){if(busy())return;setBusy(true);try{await call('checkpoint',{generation:g.seq});await inspect(g.seq,g.pr);}catch(e){setMessage(String(e));}finally{setBusy(false);}}
  const deployment=(d,r)=>{const before=r?.deployment_identity?.state,after=r?.deployment_after?.state;if(!before)return 'not observed';const text=s=>DEPLOYMENT[s]||s;return after&&after!==before?`before: ${text(before)}; after: ${text(after)}`:`${text(before)}${after?' before and after the checks':''}`;};
  return <details aria-label="Release PR revisions" open><summary>Release check</summary>
    <form onSubmit={resolve}><input type="url" required aria-label="Pull request URL" placeholder="Paste a pull request link" value={url()} disabled={busy()} onInput={e=>setUrl(e.currentTarget.value)}/> <button type="button" disabled={busy()||!url().trim()} onClick={reviewWithAgents} title="Check the facts, then an orchestrator plans the review in tiers P0 to P5, helpers check each item, and it writes a report">Review with agents</button> <button disabled={busy()||!url().trim()} title="Only the facts Philium checks itself, without agents">Verify release</button></form>
    <div class="status-line">
    <span aria-label="GitHub connection" title={github.latest?.login?'Read through the GitHub CLI sign-in on this Mac. Philium keeps no token.':'Uses the sign-in the GitHub CLI already has on this Mac. No token to create.'}>{github.latest?.login?<>GitHub · <strong>{github.latest.login}</strong> <button type="button" class="link" disabled={busy()} onClick={disconnectGithub}>Disconnect</button></>:github.latest?.token_saved||access.latest?.token_saved?<>GitHub · read-only token</>:<>GitHub · not connected <button type="button" class="primary" disabled={busy()} onClick={connectGithub}>Connect GitHub</button></>}</span>
    <details aria-label="Release verification setup" ref={setupEl}><summary title="Facts that must hold in every release of this checkout. Philium keeps them outside the checkout, so a pull request cannot change what judges it.">Checks · <span aria-label="Verification setup summary">{profile.latest ? <>{profile.latest.driver===KEPT?<>kept by Philium</>:<>verifier {profile.latest.driver_name || profile.latest.driver}</>}, pinned at <code>{String(profile.latest.pin).slice(0, 12)}</code></> : 'none yet'}</span></summary>
      <p><button type="button" disabled={busy()||drafting()} onClick={draft} title={graph()||decision()?'An agent reads this release and proposes checks. You decide which count.':'Verify a pull request first, so the agent reads exactly that release.'}>Draft checks with an agent</button><Show when={drafter()&&['failed','provisioning','running','waiting'].includes(drafter().state)}>{l=><span role={l().state==='failed'?'alert':'status'} aria-label="Drafting agent"> {l().state==='failed'?`${l().name} could not start: ${plainError(l().error||'no reason was recorded').plain.replace(/\.\s*$/,'')}. Press the button to try again.`:l().state==='provisioning'?`Starting ${l().name}…`:`${l().name} is drafting checks.`}</span>}</Show></p>
      <For each={proposals.latest||[]}>{p=><section aria-label="Proposed checks"><h3>Proposed by {p.agent}</h3><Checks label="Proposed by an agent" checks={p.checks}/><button type="button" disabled={busy()} onClick={()=>accept(p.id)}>Use these checks</button> <button type="button" disabled={busy()} onClick={()=>dismiss(p.id)}>Dismiss</button></section>}</For>
      <details aria-label="Advanced setup"><summary>Advanced</summary><p><label>Checks file (absolute path) <input aria-label="Checks file" disabled={busy()} value={file()} onInput={e=>setFile(e.currentTarget.value)}/></label> <button type="button" disabled={busy()||!file().trim()} onClick={importFile}>Read this file</button></p><p><select aria-label="Independent SQL driver" disabled={busy()} value={driver()===KEPT?'':driver()} onChange={e=>pick(e.currentTarget.value)}><option value="">Use a registered checkout</option><For each={drivers.latest||[]}>{r=><option value={r.id}>{r.name} · {r.id.slice(0,8)}</option>}</For></select> <label>Trusted loopback deployment observer port (optional) <input type="number" min="1024" max="65535" disabled={busy()} value={port()} onInput={e=>setPort(e.currentTarget.value)}/></label></p><p aria-label="Origin read access">{access.latest?.token_saved ? 'A read-only token is saved for private repositories.' : 'Private repositories need the GitHub CLI sign-in or a read-only token.'}</p><p><input type="password" autocomplete="off" aria-label="GitHub read-only token" placeholder="GitHub read-only token" title="Kept in the private vault, which agents cannot read. Sent only to api.github.com." disabled={busy()} value={token()} onInput={e=>setToken(e.currentTarget.value)}/> <button disabled={busy()||!token().trim()} onClick={saveToken}>Save token</button> <Show when={access.latest?.token_saved}><button disabled={busy()} onClick={clearToken}>Remove token</button></Show></p></details>
      <Show when={preview()?.error}><p role="alert">{preview().error}</p></Show>
      <Show when={preview()?.checks}>{c=><section aria-label="Checks to confirm"><h3 title={`Confirming means exactly these count for this checkout, as recorded at ${String(c().pin).slice(0,12)}. Nothing counts as ready before you do.`}>Read these checks, then confirm</h3><Checks label="Proposed checks" checks={c()}/>
        <For each={c().programs}>{name=><label>Where is the <strong>{name}</strong> program? (absolute path) <input aria-label={`Path of ${name}`} disabled={busy()} value={pathFor(name)} onInput={e=>setPaths({...paths(),[name]:e.currentTarget.value})}/></label>}</For>
        </section>}</Show>
      <button disabled={busy()||!driver()||!preview()?.checks} onClick={enroll}>Confirm and pin these checks</button>
      <Show when={profile.latest} fallback={<p aria-label="Pinned verifier">No checks confirmed for this checkout yet.</p>}>{p=><><p aria-label="Pinned verifier">{p().driver===KEPT?<>Checks <strong>kept by Philium</strong></>:<>Verifier <strong>{p().driver_name || p().driver}</strong></>} pinned at <code>{p().pin}</code>. {p().confirmed ? `You confirmed its checks on ${new Date(p().confirmed).toLocaleString()}.` : 'Pinned before confirmations were recorded: read the checks and pin again.'} Deployment observer: {p().observer_port ? `loopback port ${p().observer_port}` : 'none'}.<For each={p().programs||[]}>{g=><> Program {g.name}: <code>{g.path}</code>.</>}</For></p><Show when={p().checks}><details aria-label="Pinned checks"><summary>Checks pinned for this checkout</summary><Checks label="Confirmed checks" checks={p().checks}/></details></Show></>}</Show>
    </details>
    </div>
    <Show when={review.latest&&(review.latest.items?.length||review.latest.run)?review.latest:null}>{v=><section aria-label="Agent review" class={`decision ${({do_not_ship:'bad',passed:'ok'})[v().headline]||'unknown'}`}>
      <div class="verdict"><span class="glyph" aria-hidden="true">{v().lead?.state==='failed'?'!':({do_not_ship:'✕',passed:'✓'})[v().headline]||'…'}</span><div class="grow"><h3 title="P0 and P1 decide: a failure this release introduced, or whose origin is unverified, means do not ship; a pre-existing one, or anything not passed, means not ready. P2 to P5 inform.">{reviewHead(v())}</h3><p>#{v().pr}{v().items.length?` · ${v().items.filter(i=>i.result).length} of ${v().items.length} items have a result`:''}{v().lead&&v().items.length?` · led by ${v().lead.name}`:''}</p></div></div>
      <Show when={v().lead?.state==='failed'}><p role="alert" aria-label="Review start failed">{v().lead.name} could not start: {plainError(v().lead.error||'no reason was recorded').plain.replace(/\.\s*$/,'')}. No model was asked anything.</p></Show>
      <Show when={v().lead?.state==='stopped'&&!v().report}><p aria-label="Review stopped">{v().lead.name} stopped before finishing. Resume it under Agents, or start again.</p></Show>
      <Show when={!v().items.length&&(!v().lead||v().lead.state==='failed'||v().lead.state==='stopped')}><p><button type="button" class="primary" disabled={busy()} onClick={reviewWithAgents}>{v().lead?'Start the review again':'Start the review'}</button></p></Show>
      <table class="ready plan" aria-label="Review plan"><tbody><For each={byWeight(v().items)}>{it=><tr data-item={it.id} data-severity={it.result==='fail'?String(it.severity??''):''} data-result={it.result||''} data-origin={it.result==='fail'?(it.origin||'unverified'):''}><td class="mark"><span class={itemMark(it)[0]} title={itemMark(it)[2]}>{itemMark(it)[1]}</span></td><th scope="row"><Show when={it.result==='fail'}><span class={`tier sev${it.severity??1}`} title={v().severity?.[it.severity??1]?.means}>P{it.severity??'?'}</span></Show>{String(it.title).replace(/^P\d\s+/,'')}</th><td><Show when={it.result==='fail'}><span class={`origin ${it.origin||'unverified'}`} title={ORIGIN_MEANS[it.origin||'unverified']}>{ORIGIN[it.origin||'unverified']}</span> </Show>{it.result?it.evidence:itemMark(it)[2]}<Show when={it.owner&&!it.result}><small> · {it.owner}</small></Show></td></tr>}</For></tbody></table>
      <Show when={v().report}><details open aria-label="Review report"><summary>Report by {v().report.by}</summary><div class="report" data-report={v().report.text.slice(0, 400)}><Markdown text={v().report.text}/></div></details></Show>
    </section>}</Show>
    <Show when={decision()&&review.latest?.items?.length}><div class="why">What Philium checked itself</div></Show>
    <Show when={decision()}>{d=><section aria-label="Release decision" class={`decision ${tone(d().decision)}`}><div class="verdict"><span class="glyph" aria-hidden="true">{({ok:'✓',bad:'✕',old:'↻'})[tone(d().decision)]||'?'}</span><div class="grow"><h3>{HEADLINE[d().decision] || d().decision}</h3><p title="Checked locally. Nothing is posted to GitHub.">#{d().pr} · <code>{String(d().head).slice(0,10)}</code> · {new Date(d().checked_ms).toLocaleString()} · Contracts: {d().contract_decision.replaceAll('_',' ')}</p></div><button type="button" onClick={()=>copySummary(d())}>Copy summary</button></div>
      <table class="ready" aria-label="Release readiness"><tbody><For each={readiness(d(),members(),cover())}>{row=><tr data-row={row.id} data-state={row.state} data-informs={row.informs?'yes':'no'}><td class="mark"><span class={row.state} title={STATE[row.state]}>{MARK[row.state]}</span></td><th scope="row">{row.label}{row.informs?<span class="informs" title="Shown for your judgement. It does not change the verdict.">informs</span>:null}</th><td>{row.detail}</td></tr>}</For></tbody></table>
      <details aria-label="Summary to paste"><summary>Summary to paste</summary><pre>{summaryMarkdown(d(),readiness(d(),members(),cover()),review.latest)}</pre></details>
      <Show when={(rows.latest?.list||[]).some(r=>r.pr===d().pr&&r.seq>d().generation)}><p aria-label="Earlier revision"><strong>Saved result for an earlier revision.</strong> A newer head of #{d().pr} starts its own table.</p></Show>
      <details aria-label="Decision details"><summary>Details</summary><dl aria-label="Verified identities">
        <dt>Generation</dt><dd>{d().generation}</dd>
        <dt>Target (base)</dt><dd><code>{d().base}</code></dd>
        <dt>Head</dt><dd><code>{d().head}</code></dd>
        <dt>Artifact</dt><dd>{d().artifact ? <code>{d().artifact}</code> : 'No independent artifact record for this exact head'}</dd>
        <dt>Origin</dt><dd>{d().origin_current === true ? 'Rechecked after the checks: unchanged' : d().origin_current === false ? 'Moved during verification' : 'Could not be reconfirmed'}</dd>
        <dt>Deployment</dt><dd>{deployment(d(), d().receipt)}</dd>
        <dt>Verifier</dt><dd>{d().receipt?.driver_pin ? <>pinned at <code>{d().receipt.driver_pin}</code>{d().receipt.release_record_pin ? <>, record read at <code>{d().receipt.release_record_pin}</code></> : ', no release record for this head'}</> : 'none pinned'}</dd>
      </dl><Show when={d().receipt?.checks_confirmed_ms}><p aria-label="Checks confirmation">Checks confirmed by you on {new Date(d().receipt.checks_confirmed_ms).toLocaleString()}.</p></Show></details>
      <Show when={(d().why || []).length} fallback={<Show when={d().why}><p aria-label="Decision reasons">Nothing stands in the way within the reviewed scope. This is not a GitHub approval.</p></Show>}><><div class="why">Why it is not ready</div><ul aria-label="Decision reasons"><For each={d().why}>{w=><li>{w}</li>}</For></ul></></Show>
      <Show when={!d().why && d().reason}><p aria-label="Decision reasons">{d().reason}</p></Show>
    </section>}</Show>
    <p role="status">{message()}</p><Show when={rows.latest?.error}><p role="alert">{rows.latest.error}</p></Show>
    <Show when={graph()||(rows.latest?.list||[]).length}><details aria-label="History and evidence"><summary>History and evidence</summary>
    <Show when={graph()&&profile.latest}><button disabled={busy()} onClick={()=>compare(graph().seq)}>Run pinned contracts for this revision</button></Show>
    <Show when={receipt()}>{r=><section aria-label="Contract observations"><h3>{r().release_decision==='not_ready_at_tested_head'?'Regression at tested revision':'Contract observation detail'} · #{r().pr}</h3><p>Local contract evidence for <code>{r().head}</code> against target <code>{r().target}</code>. Deployment identity at start: {r().deployment_identity?.state || 'unobserved'}. Local observations are not a ship approval.</p><For each={r().cases}>{c=><div data-case-state={c.state}><p>{c.id}: <strong>{CASE[c.state] || c.state.replaceAll('_',' ')}{c.new_at_head ? '. New in this release, so there is nothing to compare on the target' : ''}</strong></p><For each={c.landed_evidence || []}>{e=><small>#{e.pr} at {e.oid.slice(0,10)}: {CHILD[e.result] || e.result.replaceAll('_',' ')}. </small>}</For></div>}</For><For each={r().targets || []}>{t=><p data-target-identity={t.identity_before?.state}>Target {t.name}: identity before the checks {DEPLOYMENT[t.identity_before?.state] || t.identity_before?.state}; after {DEPLOYMENT[t.identity_after?.state] || t.identity_after?.state}.</p>}</For><For each={r().live_cases || []}>{c=><div data-live-state={c.state}><p>{c.id} ({c.target ? `target ${c.target}` : 'running product'}): <strong>{c.state === 'pass' ? 'observed twice, matches the contract' : c.state === 'mismatch' ? 'observed twice, differs from the contract' : 'not observed'}</strong></p></div>}</For><details><summary>Inspect revision pins and observation hashes</summary><pre>{JSON.stringify(r(),null,2)}</pre></details></section>}</Show>
    <Show when={graph()}>{g=><section><strong>{{full:'Full code review',followup:'Focused re-review candidate',checkpoint:'Code review recorded'}[g().scope.mode]}</strong><p>{g().scope.reason}</p><Show when={g().scope.mode==='followup'}><p>{g().scope.commits.length} commits since the reviewed head <code>{g().scope.from}</code>. Review through <code>{g().head}</code>.</p></Show><Show when={g().scope.mode!=='checkpoint'}><button disabled={busy()} onClick={()=>mark(g())}>I completed code review at {g().head.slice(0,10)}</button><small> Local checkpoint only. Does not close findings, post a review, or approve this PR.</small></Show></section>}</Show>
    <Show when={graph()}>{g=><details aria-label="Included commits"><summary>#{g().pr} · generation {g().seq} · {g().commits.length} included commits</summary><p>Common ancestor <code>{g().merge_base}</code>. Target tip <code>{g().target}</code>. Merge parents show ancestry, not child PR identity or fault.</p><For each={g().commits}>{c=><div><code>{c.oid}</code> · {c.parents.length} parent(s)</div>}</For></details>}</Show>
    <Show when={graph()}><section aria-label="Landed pull requests"><h3>Landed pull requests</h3><Show when={members()} fallback={<p>Origin membership not established.</p>}>{m=><><For each={m().landed}>{p=><p>#{p.pr} landed at <code>{p.oid}</code></p>}</For><For each={m().edges}>{e=><p>#{e.outer} contains #{e.inner}</p>}</For><p>{m().missing} missing lookups, {m().unmapped} unmapped commits, {m().ambiguous} ambiguous. Membership does not identify fault.</p></>}</Show></section></Show>
    <For each={rows.latest?.list||[]}>{r=><p data-generation={r.seq}>#{r.pr} · revision {r.seq} · {new Date(r.observed).toLocaleString()} · head <code title={`base ${r.base}, head ${r.head}`}>{String(r.head).slice(0,10)}</code> <button disabled={busy()} onClick={()=>inspect(r.seq,r.pr)}>Inspect local ancestry</button></p>}</For>
    </details></Show>
  </details>;
}
