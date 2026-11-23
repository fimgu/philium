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
import { Button, Toggle, TextArea, toast } from '../ui/kit.jsx';
import {
  verdictLabel, verdictTone, tally, tallyLine, groupByKind,
  itemMark, verifyLine, originLabel, severityLabel, basisLabel, coverageLine,
} from '../lib/pr-review.js';

function ItemRow(props) {
  const it = props.item;
  const [tone, glyph, markTitle] = itemMark(it);
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
        <Show when={it.status === 'blocked'}><p class="blocked"><small>Needs you: {it.detail || 'see evidence above'}</small></p></Show>
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
            <table class="ready plan"><tbody><For each={g.items}>{(it) => <ItemRow item={it} />}</For></tbody></table>
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
          <Show when={items().length}><p><small>{tallyLine(items())}</small></p></Show>
          <Show when={coverage()}>{c => <p><small class={c().warn ? 'warn' : ''}>{c().text}</small></p>}</Show>
        </div>
        <Show when={r() && !['posted', 'superseded', 'stopped'].includes(r().state)}>
          <Button size="sm" variant="ghost" onClick={() => props.onStop?.(r().id)}>Stop</Button>
        </Show>
        <Show when={r() && ['stopped', 'failed', 'superseded'].includes(r().state)}>
          <Button size="sm" onClick={() => props.onRestart?.(r().id)}>Restart</Button>
        </Show>
      </div>
      <Show when={r()?.error}><p role="alert">{plainError(r().error).plain}</p></Show>
      <Show when={props.data?.summary}><Markdown text={props.data.summary} /></Show>
      <Checklist items={items()} />
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
      <Show when={currentPr()}>
        <NotesEditor repo={props.repo} pr={currentPr()} notes={data()?.notes} onChanged={reloadData} />
      </Show>
      <Show when={data()?.review} fallback={<p class="small faint">Paste a pull request URL above to start a review.</p>}>
        <ReviewCard data={data()} onStop={stop} onRestart={restart} />
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
