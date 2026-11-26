// PR reviewer: paste a URL, Philium builds a checklist, agents verify it, and the review
// shows a verdict, a tally and the checklist grouped by kind, worst first. Notes are visible
// before and during a review. Earlier heads (superseded reviews) collapse under history.
import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc } from '../lib/rpc.js';
import { go } from '../lib/nav.js';
import { useGitHubConnection } from '../ui/github-account.jsx';
import { useVisibleEvent } from '../lib/visibility.js';
import { plainError } from '../lib/plain-error.js';
import { Markdown } from '../ui/markdown.jsx';
import { Button, Toggle, TextArea, toast, confirm } from '../ui/kit.jsx';
import {
  verdictLabel, verdictTone, tally, tallyLine, groupByKind,
  itemMark, verifyLine, originLabel, severityLabel, basisLabel, coverageLine,
  MODES, modeLabel, levelLabel, parsePolicy, normalisePolicyGet, modeCeiling, modeAllowed,
  effectiveMode, policySourceLabel, budgetLine, formatTokens, wouldPostLabel, humansLine, ownPr,
} from '../lib/pr-review.js';

function PolicyPanel(props) {
  const pg = () => normalisePolicyGet(props.data);
  const pol = () => pg().policy;
  const ceiling = () => modeCeiling(pol());
  const current = () => effectiveMode(ceiling(), pg().override ?? pol()?.override, pol()?.mode);
  return (
    <section aria-label="Review policy">
      <h4>Review policy</h4>
      <div class="row" style={{ 'flex-wrap': 'wrap', gap: '8px' }}>
        <div class="segmented" role="group" aria-label="Review mode">
          <For each={MODES}>
            {(m) => (
              <button
                type="button"
                class={current() === m ? 'on' : ''}
                aria-pressed={current() === m}
                disabled={!modeAllowed(m, ceiling())}
                title={modeAllowed(m, ceiling()) ? '' : `The policy file allows up to ${modeLabel(ceiling())}`}
                onClick={() => props.onMode?.(m)}
              >{modeLabel(m)}</button>
            )}
          </For>
        </div>
        <small class="faint">Ceiling: {modeLabel(ceiling())}</small>
      </div>
      <Show when={pol()} fallback={<p class="small faint">Policy unavailable.</p>}>
        {(p) => (
          <>
            <p class="small">
              <span class={`badge${p().source === 'invalid' ? ' danger' : ''}`}>{policySourceLabel(p())}</span>{' '}
              <span class="badge">Level: {levelLabel(p().level) || 'Standard'}</span>{' '}
              <span class={`badge${p().humans ? ' strict' : ''}`}>{humansLine(p().humans)}</span>
              <Show when={p().budget_tokens != null}>{' '}<span class="badge">Budget {formatTokens(p().budget_tokens)} tokens</span></Show>
            </p>
            <Show when={p().error}><p role="alert" class="small">Policy file invalid: {p().error}. Reviews run in Observe.</p></Show>
            <Show when={(p().matched || []).length}>
              <table class="ready plan"><tbody>
                <For each={p().matched}>
                  {(m) => (
                    <tr>
                      <th scope="row">{m.purpose || (m.paths || []).join(', ')}</th>
                      <td><small>{m.files} {m.files === 1 ? 'file' : 'files'}</small></td>
                    </tr>
                  )}
                </For>
              </tbody></table>
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}

function ItemRow(props) {
  const it = props.item;
  const [tone, glyph, markTitle] = itemMark(it);
  const [waiving, setWaiving] = createSignal(false);
  async function waive() {
    setWaiving(true);
    try { await props.onWaive?.(it.id); } finally { setWaiving(false); }
  }
  return (
    <tr data-item={it.id} data-status={it.status} data-kind={it.kind}>
      <td class="mark"><span class={tone} title={markTitle}>{glyph}</span></td>
      <th scope="row">
        {String(severityLabel(it) ? `${severityLabel(it)} ` : '')}{it.title}
        <Show when={it.author}><small> · {it.author}{it.human === false || it.human === 0 ? ' (bot)' : ''}</small></Show>
        <Show when={basisLabel(it)}><span class="badge" title="How this result was established">{basisLabel(it)}</span></Show>
      </th>
      <td>
        <Show when={originLabel(it)}><span class="origin">{originLabel(it)}</span>{' '}</Show>
        {it.detail}
        <Show when={it.evidence}><details><summary>Evidence</summary><p style={{ 'white-space': 'pre-wrap' }}>{it.evidence}</p></details></Show>
        <Show when={verifyLine(it)}><p><small>{verifyLine(it)}</small></p></Show>
        <Show when={it.status === 'blocked'}>
          <p class="blocked"><small>Needs you: {it.detail || 'see evidence above'}</small></p>
          <Show when={props.onWaive}>
            <Button size="sm" busy={waiving()} disabled={waiving()} onClick={waive} title="Skip this check for this review. The reason is kept with the review.">Not needed</Button>
          </Show>
        </Show>
        <Show when={it.url}><p><a href={it.url} target="_blank" rel="noreferrer">Source</a></p></Show>
      </td>
    </tr>
  );
}

function Checklist(props) {
  const groups = () => groupByKind(props.items);
  return (
    <Show when={groups().length} fallback={<p class="small faint">No checklist items yet.</p>}>
      <For each={groups()}>
        {(g) => (
          <section aria-label={g.label}>
            <h4>{g.label}</h4>
            <table class="ready plan"><tbody><For each={g.items}>{(it) => <ItemRow item={it} onWaive={props.onWaive} />}</For></tbody></table>
          </section>
        )}
      </For>
    </Show>
  );
}

function NotesEditor(props) {
  const [text, setText] = createSignal('');
  const [must, setMust] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  async function add() {
    const t = text().trim();
    if (!t) return;
    setBusy(true);
    try {
      await rpc('pr_review.note_add', { repo_id: props.repo, pr: props.pr, text: t, must: must() }, { local: true });
      setText(''); setMust(false);
      await props.onChanged?.();
    } catch (e) { toast(String(e), 'danger'); } finally { setBusy(false); }
  }
  async function remove(id) {
    try { await rpc('pr_review.note_remove', { id }, { local: true }); await props.onChanged?.(); }
    catch (e) { toast(String(e), 'danger'); }
  }
  return (
    <section aria-label="Your notes">
      <h4>Your notes</h4>
      <For each={props.notes || []}>
        {(n) => (
          <div class="checkout-row">
            <span>{n.text}{n.must ? <strong> (must)</strong> : null}</span>
            <Button size="sm" variant="ghost" onClick={() => remove(n.id)}>Remove</Button>
          </div>
        )}
      </For>
      <TextArea value={text()} onValue={setText} placeholder="Add a note for the agents reviewing this PR…" rows={2} />
      <div class="row">
        <label class="row"><input type="checkbox" checked={must()} onInput={(e) => setMust(e.currentTarget.checked)} /> Must</label>
        <Button size="sm" busy={busy()} disabled={!text().trim()} onClick={add}>Add</Button>
      </div>
    </section>
  );
}

function ReviewCard(props) {
  const r = () => props.data?.review;
  const items = () => props.data?.items || [];
  const verdict = () => props.data?.verdict;
  const t = () => tally(items());
  const coverage = () => coverageLine(r()?.coverage, r()?.errors);
  const policy = () => parsePolicy(r()?.policy);
  const budget = () => budgetLine(r()?.tokens_used, policy()?.budget_tokens);
  const [publishing, setPublishing] = createSignal(null);
  async function publish(as) {
    setPublishing(as);
    try { await props.onPublish?.(r().id, as); } finally { setPublishing(null); }
  }
  return (
    <section aria-label="PR review" class={`decision ${verdictTone(r())}`}>
      <div class="verdict">
        <span class="glyph" aria-hidden="true">
          {({ ok: '✓', bad: '✕', warn: '!' })[verdictTone(r())] || '…'}
        </span>
        <div class="grow">
          <h3>{verdictLabel(r())}</h3>
          <p>
            #{r()?.pr} · {t().done} of {t().total} done
            <Show when={r()?.github_login}>{' · '}GitHub @{r().github_login}</Show>
            <Show when={r()?.state === 'posted' && r()?.posted_url}>
              {' · '}<a href={r().posted_url} target="_blank" rel="noreferrer">Posted review</a>
            </Show>
          </p>
          <p class="row" style={{ gap: '6px', 'flex-wrap': 'wrap' }}>
            <Show when={modeLabel(r()?.mode)}><span class="badge accent" title="Review mode">{modeLabel(r().mode)}</span></Show>
            <Show when={levelLabel(r()?.level)}><span class="badge" title="Review level">{levelLabel(r().level)}</span></Show>
            <Show when={budget()}>{b => <span class={`badge${b().over ? ' danger' : ''}`} title="Token use">{b().text}</span>}</Show>
          </p>
          <Show when={policy()}>
            {p => <p><small>Policy: {policySourceLabel(p())} · {humansLine(p().humans)}
              <Show when={(p().matched || []).length}>{' · '}{p().matched.map(m => `${m.purpose || 'rule'} (${m.files})`).join(', ')}</Show>
              <Show when={p().error}>{' · '}<span class="warn">{p().error}</span></Show></small></p>}
          </Show>
          <Show when={items().length}><p><small>{tallyLine(items())}</small></p></Show>
          <Show when={r()?.state === 'observed' && ownPr(r())}>
            <div class="row" style={{ gap: '8px', 'flex-wrap': 'wrap' }} aria-label="Own pull request">
              <small>Your own pull request: Philium never approves it or requests changes. You can post the review as a comment.</small>
              <Button size="sm" busy={publishing() === 'comment'} disabled={!!publishing()} onClick={() => publish('comment')}>Post as comment</Button>
            </div>
          </Show>
          <Show when={r()?.state === 'observed' && !ownPr(r())}>
            <div class="row" style={{ gap: '8px', 'flex-wrap': 'wrap' }} aria-label="Observed review">
              <span>Would post: <strong>{wouldPostLabel(r().would_post)}</strong></span>
              <Button size="sm" busy={publishing() === 'comment'} disabled={!!publishing()} onClick={() => publish('comment')}>Publish as comment</Button>
              <Button size="sm" variant="primary" busy={publishing() === 'enforce'} disabled={!!publishing()} onClick={() => publish('enforce')}>Publish</Button>
            </div>
          </Show>
          <Show when={coverage()}>{c => <p><small class={c().warn ? 'warn' : ''}>{c().text}</small></p>}</Show>
        </div>
        <Show when={r() && !['posted', 'observed', 'superseded', 'stopped', 'done'].includes(r().state)}>
          <Button size="sm" variant="ghost" onClick={() => props.onStop?.(r().id)}>Stop</Button>
        </Show>
        <Show when={r() && ['stopped', 'failed', 'superseded', 'done'].includes(r().state)}>
          <Button size="sm" onClick={() => props.onRestart?.(r().id)}>Restart</Button>
        </Show>
        <Show when={r() && r().state !== 'done' && r().state !== 'posting'}>
          <Button size="sm" variant="ghost" onClick={() => props.onDone?.(r().id)}>Done</Button>
        </Show>
      </div>
      <Show when={r()?.error}><p role="alert">{plainError(r().error).plain}</p></Show>
      <Show when={props.data?.summary}><Markdown text={props.data.summary} /></Show>
      <Checklist items={items()} onWaive={['running', 'needs_you'].includes(r()?.state) ? props.onWaive : null} />
    </section>
  );
}

export function PrReviewer(props) {
  const [url, setUrl] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [pr, setPr] = createSignal(null);
  const [github] = useGitHubConnection(() => props.repo);

  const [settings, { refetch: reloadSettings }] = createResource(() =>
    rpc('pr_review.settings_get', { repo_id: props.repo }, { local: true }).catch(() => ({ auto: false })));

  const [data, { refetch: reloadData }] = createResource(
    () => (pr() ? `${props.repo}:${pr()}` : props.repo ? `${props.repo}:latest` : null),
    () => rpc('pr_review.get', { repo_id: props.repo, pr: pr() ?? undefined }, { local: true }).catch(() => null),
  );
  const [list, { refetch: reloadList }] = createResource(() => rpc('pr_review.list', { repo_id: props.repo }, { local: true }).catch(() => []));

  useVisibleEvent('pr_review:changed', () => { reloadData(); reloadList(); }, { debounceMs: 300 });

  async function startReview() {
    const u = url().trim();
    if (!u) return;
    setBusy(true);
    try {
      const r = await rpc('pr_review.start', { repo_id: props.repo, url: u }, { local: true });
      setPr(r.pr);
      setUrl('');
      await Promise.all([reloadData(), reloadList()]);
      toast('Review started.', 'secure');
    } catch (e) { toast(String(e), 'danger'); } finally { setBusy(false); }
  }

  async function toggleAuto(on) {
    try { await rpc('pr_review.settings_set', { auto: on }, { local: true }); await reloadSettings(); }
    catch (e) { toast(String(e), 'danger'); }
  }

  async function stop(id) {
    try { await rpc('pr_review.stop', { id }, { local: true }); await Promise.all([reloadData(), reloadList()]); }
    catch (e) { toast(String(e), 'danger'); }
  }
  async function restart(id) {
    try { await rpc('pr_review.restart', { id }, { local: true }); await Promise.all([reloadData(), reloadList()]); }
    catch (e) { toast(String(e), 'danger'); }
  }
  // Finish a review: the result stays, its agents and everything they made go.
  async function done(id) {
    if (!(await confirm('Mark this review done?', 'The checklist and verdict stay here. The review agents, their worktrees, folders and processes are removed. Nothing is posted to GitHub.', { danger: true, ok: 'Done' }))) return;
    try {
      const out = await rpc('pr_review.done', { id }, { local: true });
      await Promise.all([reloadData(), reloadList()]);
      const n = out?.cleanup?.removed?.length || 0;
      const left = out?.cleanup?.left || [];
      toast(`Review done. Removed ${n} agent${n === 1 ? '' : 's'}.`, 'secure');
      if (left.length) toast(`Philium could not stop these safely, so check them yourself: ${left.map((l) => `${l.handle} (${l.agent})`).join('; ')}`, 'danger');
      const stuck = [...(out?.cleanup?.unsaved || []).map((u) => u.name), ...(out?.cleanup?.kept || [])];
      if (stuck.length) toast(`Still there: ${stuck.join(', ')}. Remove them from Agents.`, 'danger');
    } catch (e) { toast(String(e), 'danger'); }
  }

  const [policy, { refetch: reloadPolicy }] = createResource(() => props.repo, (repo) =>
    rpc('pr_review.policy_get', { repo_id: repo }, { local: true }).catch(() => null));

  async function setMode(mode) {
    try {
      await rpc('pr_review.settings_set', { auto: Boolean(settings()?.auto), repo_id: props.repo, mode }, { local: true });
      await reloadPolicy();
    } catch (e) { toast(String(e), 'danger'); }
  }
  async function waive(id) {
    try {
      await rpc('pr_review.item_waive', { id }, { local: true });
      await Promise.all([reloadData(), reloadList()]);
      toast('Marked not needed.', 'secure');
    } catch (e) { toast(String(e), 'danger'); }
  }
  async function publish(id, as) {
    try {
      await rpc('pr_review.publish', { id, as }, { local: true });
      await Promise.all([reloadData(), reloadList()]);
      toast(as === 'comment' ? 'Published as a comment.' : 'Review published.', 'secure');
    } catch (e) { toast(String(e), 'danger'); }
  }

  const currentPr = () => pr() ?? data()?.review?.pr ?? null;
  const history = () => (list() || []).filter((r) => r.state === 'superseded' && r.pr === currentPr());

  return (
    <section aria-label="PR reviewer">
      <div class="row"><h2 class="grow">PR reviewer</h2></div>
      <p class="small faint" aria-label="Reviewer GitHub identity">
        {github.latest?.login ? <>GitHub review account: <strong>{github.latest.login}</strong>.</>
          : github.loading ? 'Checking reviewer GitHub account…'
          : github.latest?.error ? 'Reviewer GitHub account unavailable.'
          : 'No GitHub account configured for this repository.'}{' '}
        <a href="#" onClick={e => { e.preventDefault(); go('repos', { repo_id: props.repo }); }}>Repository settings</a>
      </p>
      <form class="row" style={{ 'flex-wrap': 'wrap', gap: '8px' }} onSubmit={(e) => { e.preventDefault(); startReview(); }}>
        <input
          aria-label="Pull request URL"
          class="input grow"
          placeholder="https://github.com/org/repo/pull/123"
          value={url()}
          onInput={(e) => setUrl(e.currentTarget.value)}
        />
        <Button type="submit" variant="primary" busy={busy()} disabled={!url().trim()}>Review</Button>
        <Toggle
          label="Review PRs requested from me automatically"
          on={Boolean(settings()?.auto)}
          onChange={toggleAuto}
        />
      </form>
      <PolicyPanel data={policy()} onMode={setMode} />
      <Show when={currentPr()}>
        <NotesEditor repo={props.repo} pr={currentPr()} notes={data()?.notes} onChanged={reloadData} />
      </Show>
      <Show when={data()?.review} fallback={<p class="small faint">Paste a pull request URL above to start a review.</p>}>
        <ReviewCard data={data()} onStop={stop} onRestart={restart} onDone={done} onPublish={publish} onWaive={waive} />
      </Show>
      <Show when={history().length}>
        <details aria-label="Earlier heads">
          <summary>Earlier heads ({history().length})</summary>
          <For each={history()}>
            {(h) => <div class="checkout-row"><span>{h.title}</span><small>{h.head?.slice(0, 10)} · {verdictLabel(h)}</small></div>}
          </For>
        </details>
      </Show>
    </section>
  );
}
