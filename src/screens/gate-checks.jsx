import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc, useEvent } from '../lib/rpc.js';

// What each computed outcome means, in plain words. Philium decides these from the
// pull request's head and its base; no agent writes them.
const OUTCOME = {
  pass: ['ok', '✓', 'Passes'],
  introduced: ['bad', '✕', 'Fails on this pull request and passes on its base: this pull request caused it'],
  pre_existing: ['unknown', '!', 'Fails on the base too: it was already broken'],
  flaky: ['unknown', '~', 'Failed, then passed when run again: flaky'],
  not_run: ['unknown', '?', 'Did not run'],
  unverified: ['unknown', '?', 'Failed, and the base could not be checked'],
  pending: ['unknown', '…', 'Not finished'],
};
const HEAD = { pass: 'Checks pass', fail: 'A check fails because of this pull request', blocked: 'Checks are not done' };

const seconds = ms => (ms == null ? '' : ms < 1000 ? `${ms} ms` : `${Math.round(ms / 1000)} s`);

export function GateChecks(props) {
  const [tier, setTier] = createSignal('fast');
  const [error, setError] = createSignal('');
  const [lines, setLines] = createSignal([]);
  const key = () => (props.repo && props.generation ? [props.repo, props.generation, tier()] : null);
  const [res, { refetch }] = createResource(key, ([repo, g, t]) => rpc('pipeline.results', { repo_id: repo, generation: g, tier: t }).catch(e => ({ error: String(e) })));
  const [status, { refetch: refetchStatus }] = createResource(() => props.repo, r => rpc('pipeline.status', { repo_id: r }).catch(() => null));
  const [lendPath, setLendPath] = createSignal({});
  async function lend(name) {
    setError('');
    try {
      await rpc('pipeline.lend', { repo_id: props.repo, name, path: (lendPath()[name] || '').trim() }, { action: true });
      refetchStatus();
    } catch (e) {
      setError(String(e));
    }
  }
  // Follow a run live, whoever started it: each finished check shows up as it lands.
  const [live, setLive] = createSignal(false);
  let lastFetch = 0;
  useEvent('pipeline:progress', e => {
    const d = e?.payload ?? e;
    if (d?.repo_id !== props.repo) return;
    setLive(true);
    setLines(l => [...l.slice(-40), d.line]);
    if (Date.now() - lastFetch > 2000) {
      lastFetch = Date.now();
      refetch();
    }
  });
  useEvent('pipeline:done', e => {
    const d = e?.payload ?? e;
    if (d?.repo_id !== props.repo) return;
    setLive(false);
    refetch();
  });
  async function run() {
    setError('');
    setLines([]);
    try {
      await rpc('pipeline.run', { repo_id: props.repo, generation: props.generation, tier: tier() }, { action: true });
      refetch();
    } catch (e) {
      setError(String(e));
    }
  }
  const outcomeOf = (v, id) => (v?.verdict?.outcomes || []).find(o => o[0] === id)?.[1]?.outcome || 'pending';
  const running = () => res()?.running || live();
  return (
    <Show when={props.generation}>
      <section aria-label="Checks on this pull request" class={`decision ${({ pass: 'ok', fail: 'bad' })[res()?.verdict?.state] || 'unknown'}`}>
        <div class="verdict">
          <span class="glyph" aria-hidden="true">{running() ? '…' : ({ pass: '✓', fail: '✕' })[res()?.verdict?.state] || '…'}</span>
          <div class="grow">
            <h3 title="Philium runs the repo's own checks on this pull request's head and on its base, so what it broke is told apart from what was already broken.">
              {running() ? 'Checks are running' : res()?.configured === false ? 'No checks are set up for this repo' : HEAD[res()?.verdict?.state] || 'Checks have not run'}
            </h3>
            <p>
              <Show when={res()?.pr}>#{res().pr} · head {String(res().head).slice(0, 12)} · base {String(res().base).slice(0, 12)}</Show>
              <Show when={res()?.configured && !res()?.verdict?.pinned}> · unpinned: the checks come from a local file, not the repo, so this can never land</Show>
            </p>
          </div>
          <div class="row">
            <select aria-label="Tier" value={tier()} onChange={e => setTier(e.currentTarget.value)}>
              <option value="fast">Fast checks</option>
              <option value="full">Full checks</option>
            </select>
            <button type="button" class="primary" disabled={running() || res()?.configured === false} onClick={run}>
              {running() ? 'Running…' : res()?.verdict?.outcomes?.length ? 'Run again' : 'Run checks'}
            </button>
          </div>
        </div>
        <Show when={res()?.configured === false}>
          <p>Add the checks this repo runs in <code>.philium/pipeline.yaml</code> in the repo, or try them first from <code>{res().local_gate}</code>.</p>
        </Show>
        <For each={(status()?.lends || []).filter(l => !l.path)}>
          {l => (
            <form class="row" aria-label={`Lend ${l.name}`} onSubmit={e => { e.preventDefault(); lend(l.name); }}>
              <span>Checks here read <b>{l.name}</b> from a folder on this Mac. Lend one, read-only:</span>
              <input aria-label={`Folder for ${l.name}`} placeholder="~/go/pkg/mod/cache/download" value={lendPath()[l.name] || ''} onInput={e => setLendPath(p => ({ ...p, [l.name]: e.currentTarget.value }))} />
              <button type="submit" disabled={!(lendPath()[l.name] || '').trim()}>Lend read-only</button>
            </form>
          )}
        </For>
        <Show when={error() || res()?.error}>
          <p role="alert">{error() || res().error}</p>
        </Show>
        <Show when={res()?.checks?.length}>
          <table class="ready plan" aria-label="Checks">
            <tbody>
              <For each={res().checks}>
                {c => {
                  const o = () => OUTCOME[outcomeOf(res(), c.id)] || OUTCOME.pending;
                  return (
                    <tr data-check={c.id} data-outcome={outcomeOf(res(), c.id)}>
                      <td class="mark"><span class={o()[0]} title={o()[2]}>{o()[1]}</span></td>
                      <th scope="row">{c.id}</th>
                      <td>
                        {o()[2]}
                        <Show when={c.candidate?.reason}> ({c.candidate.reason})</Show>
                        <Show when={c.candidate?.duration_ms != null}><small> · {seconds(c.candidate.duration_ms)}</small></Show>
                        <Show when={c.output}>
                          <details>
                            <summary>Output on this pull request</summary>
                            <pre>{c.output}</pre>
                          </details>
                        </Show>
                        <Show when={c.base_output}>
                          <details>
                            <summary>Output on the base</summary>
                            <pre>{c.base_output}</pre>
                          </details>
                        </Show>
                      </td>
                    </tr>
                  );
                }}
              </For>
            </tbody>
          </table>
        </Show>
        <Show when={(res()?.verdict?.reasons || []).length && !running()}>
          <ul aria-label="Why">
            <For each={res().verdict.reasons}>{r => <li>{r}</li>}</For>
          </ul>
        </Show>
        <Show when={running() || lines().length}>
          <details open={running()}>
            <summary>What is happening</summary>
            <pre aria-label="Check progress">{(lines().length ? lines() : res()?.lines || []).join('\n')}</pre>
          </details>
        </Show>
      </section>
    </Show>
  );
}
