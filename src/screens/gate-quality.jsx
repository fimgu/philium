import { createResource, createSignal, For, Show } from 'solid-js';
import { rpc, useEvent, fmt } from '../lib/rpc.js';

// How well the repo's checks catch real bugs: each listed past bug is rerun, and its
// check must fail on the bug and not on the fix. The history shows whether a change to
// the checks made them better or worse.
const W = 900, H = 170, PAD = { l: 40, r: 14, t: 12, b: 26 };
const pct = r => `${Math.round((r || 0) * 100)}%`;
const when = ms => new Date(ms).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });

function RateChart(props) {
  const [hover, setHover] = createSignal(null);
  const runs = () => props.runs;
  const x = i => (runs().length === 1 ? PAD.l + (W - PAD.l - PAD.r) / 2 : PAD.l + (i * (W - PAD.l - PAD.r)) / (runs().length - 1));
  const y = r => PAD.t + (1 - r) * (H - PAD.t - PAD.b);
  const path = () => runs().map((r, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(r.rate).toFixed(1)}`).join(' ');
  const step = () => (runs().length > 1 ? (W - PAD.l - PAD.r) / (runs().length - 1) : W - PAD.l - PAD.r);
  return (
    <figure class="gq-chart" aria-label="Catch rate each time the checks were measured">
      <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`Catch rate over ${runs().length} measurements, latest ${pct(runs().at(-1)?.rate)}`} onPointerLeave={() => setHover(null)}>
        <For each={[0, 0.5, 1]}>
          {g => (
            <>
              <line x1={PAD.l} x2={W - PAD.r} y1={y(g)} y2={y(g)} class="gq-grid" />
              <text x={PAD.l - 6} y={y(g) + 4} class="gq-axis" text-anchor="end">{pct(g)}</text>
            </>
          )}
        </For>
        <Show when={hover() != null}>
          <line x1={x(hover())} x2={x(hover())} y1={PAD.t} y2={H - PAD.b} class="gq-hair" />
        </Show>
        <path d={path()} class="gq-line" />
        <For each={runs()}>
          {(r, i) => (
            <>
              <circle cx={x(i())} cy={y(r.rate)} r="4" class="gq-dot" />
              <rect x={x(i()) - step() / 2} y={0} width={step()} height={H} fill="transparent" tabindex="0" aria-label={`${when(r.at)}: caught ${r.caught} of ${r.total}`} onPointerEnter={() => setHover(i())} onFocus={() => setHover(i())} onBlur={() => setHover(null)} />
            </>
          )}
        </For>
        <text x={PAD.l} y={H - 6} class="gq-axis">{when(runs()[0].at)}</text>
        <Show when={runs().length > 1}>
          <text x={W - PAD.r} y={H - 6} class="gq-axis" text-anchor="end">{when(runs().at(-1).at)}</text>
        </Show>
      </svg>
      <Show when={hover() != null}>
        <figcaption class="gq-tip" style={{ left: `${(x(hover()) / W) * 100}%` }}>
          <b>{pct(runs()[hover()].rate)}</b> · caught {runs()[hover()].caught} of {runs()[hover()].total} · {when(runs()[hover()].at)}
          <Show when={runs()[hover()].rejected_below != null}> · below the earlier {pct(runs()[hover()].rejected_below)}, so not accepted</Show>
        </figcaption>
      </Show>
    </figure>
  );
}

export function GateQuality(props) {
  const [h, { refetch }] = createResource(() => props.repo, r => rpc('pipeline.replay_history', { repo_id: r }).catch(e => ({ error: String(e) })));
  const [lines, setLines] = createSignal([]);
  const [error, setError] = createSignal('');
  const [live, setLive] = createSignal(false);
  useEvent('pipeline:replay', e => {
    const d = e?.payload ?? e;
    if (d?.repo_id !== props.repo) return;
    if (d.line) setLines(l => [...l.slice(-60), d.line]);
    setLive(!d.done);
    if (d.done) refetch();
  });
  async function measure() {
    setError('');
    setLines([]);
    try {
      await rpc('pipeline.replay', { repo_id: props.repo }, { action: true });
      setLive(true);
    } catch (e) {
      setError(String(e));
    }
  }
  const running = () => h()?.running || live();
  const last = () => h()?.runs?.at(-1);
  const text = id => (h()?.listed || []).find(b => b.id === id)?.text;
  // Every bug listed now, with its results from the measurements so far.
  const rows = () => {
    const seen = new Map((h()?.bugs || []).map(b => [b.id, b]));
    const listed = (h()?.listed || []).map(b => ({ id: b.id, check: b.check, results: seen.get(b.id)?.results || [] }));
    const gone = (h()?.bugs || []).filter(b => !listed.some(l => l.id === b.id));
    return [...listed, ...gone];
  };
  return (
    <Show when={props.repo && h() && !h().error && ((h().listed || []).length || (h().runs || []).length)}>
      <details class="gq" open={running()}>
        <summary>
          How well these checks catch real bugs:{' '}
          <Show when={last()} fallback={<span>not measured yet</span>}>
            <b>caught {last().caught} of {last().total}</b> ({pct(last().rate)}), {fmt.ago(last().at)}
          </Show>
        </summary>
        <div class="row">
          <p class="grow ws-muted">Each past bug in the list is run again: its check must fail on the bug and pass on its fix, or show the bug's own text. A change to the checks that catches fewer bugs is not accepted.</p>
          <button type="button" disabled={running()} onClick={measure}>{running() ? 'Measuring…' : last() ? 'Measure again' : 'Measure now'}</button>
        </div>
        <Show when={(h().runs || []).length}>
          <RateChart runs={h().runs} />
        </Show>
        <table class="ready plan" aria-label="Past bugs and whether the checks caught them">
          <tbody>
            <For each={rows()}>
              {b => {
                const latest = () => b.results.at(-1);
                return (
                  <tr data-bug={b.id}>
                    <td class="mark">
                      <Show when={latest()} fallback={<span class="unknown" title="Not measured yet">…</span>}>
                        <span class={latest().caught ? 'ok' : 'bad'} title={latest().caught ? 'Caught' : 'Missed'}>{latest().caught ? '✓' : '✕'}</span>
                      </Show>
                    </td>
                    <th scope="row">{b.id}<br /><small class="ws-muted">{b.check}</small></th>
                    <td>
                      {text(b.id) || ''}
                      <Show when={latest()}>
                        <br /><small>{latest().caught ? 'Caught' : 'Missed'}{latest().reason ? `: ${latest().reason.replace(/^caught: /, '')}` : ''}</small>
                      </Show>
                      <Show when={b.results.length > 1}>
                        <br /><small class="ws-muted" aria-label="Earlier measurements, oldest first">Before: {b.results.slice(0, -1).slice(-9).map(r => (r.caught ? '✓' : '✕')).join(' ')}</small>
                      </Show>
                    </td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
        <Show when={(h().runs || []).length}>
          <details>
            <summary>Every measurement as a table</summary>
            <table class="ready plan" aria-label="Every measurement">
              <tbody>
                <For each={[...h().runs].reverse()}>
                  {r => (
                    <tr>
                      <th scope="row">{when(r.at)}</th>
                      <td class="mono">{r.caught} of {r.total} ({pct(r.rate)})</td>
                      <td class="ws-muted">checks version {r.digest}{r.rejected_below != null ? `, not accepted: below ${pct(r.rejected_below)}` : ''}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </details>
        </Show>
        <p class="ws-muted">The list is {h().list_path}.</p>
        <Show when={error() || h().error}>
          <p role="alert">{error() || h().error}</p>
        </Show>
        <Show when={running() || lines().length}>
          <pre aria-label="Measuring progress">{(lines().length ? lines() : h().lines || []).join('\n')}</pre>
        </Show>
      </details>
    </Show>
  );
}
