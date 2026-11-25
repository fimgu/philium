import { createMemo, createSignal, createResource, For, Show, onCleanup } from 'solid-js';
import { activeRepoId, setActiveRepoId, useStore } from '../lib/store.js';
import { go } from '../lib/nav.js';
import { Page, Button } from '../ui/kit.jsx';
import { ManagedJobs } from './managed-jobs.jsx';
import { rpc } from '../lib/rpc.js';
import { useVisibleEvent } from '../lib/visibility.js';

const read = (m, args) => rpc(`environments.${m}`, args, { local: true });

function normalizeRuntimes(items) {
  if (!Array.isArray(items)) return [];
  return items
    .map((item) => {
      if (typeof item === 'string') {
        return { id: item, runtime: 'docker', label: item, profile: item };
      }
      if (!item || typeof item !== 'object') return null;
      const id = String(item.id ?? item.profile ?? '').trim();
      if (!id) return null;
      const runtime = String(item.runtime ?? 'docker');
      const profile = String(item.profile ?? id);
      const label = String(item.label ?? (profile && profile !== 'default' ? `${runtime} (${profile})` : runtime));
      return { id, runtime, label, profile };
    })
    .filter(Boolean);
}

async function discoverRuntimes() {
  try {
    const runtimes = await read('runtimes', {});
    return normalizeRuntimes(runtimes);
  } catch {
    const profiles = await read('profiles', {});
    return normalizeRuntimes(profiles);
  }
}

function Inventory(props) {
  const [rows, setRows] = createSignal(null);
  const [issue, setIssue] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [filter, setFilter] = createSignal('');
  const [selected, select] = createSignal(null);
  const [tail, setTail] = createSignal(null);
  let generation = 0;
  let logRequest = 0;

  async function refresh() {
    const n = ++generation;
    ++logRequest;
    select(null);
    setTail(null);
    setBusy(true);
    setIssue('');
    setRows(null);
    try {
      const r = await read('containers', { profile: props.runtime.id });
      if (n === generation) setRows(r);
    } catch (e) {
      if (n === generation) setIssue(String(e));
    } finally {
      if (n === generation) setBusy(false);
    }
  }
  refresh();
  onCleanup(() => {
    generation++;
    logRequest++;
  });

  async function showLogs(c) {
    const n = ++logRequest;
    select(c);
    setTail(null);
    try {
      const t = await read('logs', { profile: props.runtime.id, id: c.id });
      if (n === logRequest) setTail(t);
    } catch (e) {
      if (n === logRequest) setTail({ error: String(e) });
    }
  }

  const groups = createMemo(() => {
    const m = new Map();
    const q = filter().trim().toLowerCase();
    for (const c of rows() || []) {
      const label = c.cluster ? `kind · ${c.cluster}` : c.project ? `Compose · ${c.project}` : 'Standalone containers';
      if (![label, c.name, c.image, c.status].join(' ').toLowerCase().includes(q)) continue;
      if (!m.has(label)) m.set(label, []);
      m.get(label).push(c);
    }
    return [...m].sort(([a], [b]) => a.localeCompare(b));
  });

  return (
    <section aria-label="Runtime inventory">
      <h3>{props.runtime.label}</h3>
      <button disabled={busy()} onClick={refresh}>Refresh inventory</button>{' '}
      <input
        aria-label="Filter containers"
        placeholder="Filter cluster, container, or status"
        value={filter()}
        onInput={(e) => setFilter(e.currentTarget.value)}
      />
      <Show when={busy()}><p role="status">Reading runtime socket…</p></Show>
      <Show when={issue()}><p role="alert">{issue()}</p></Show>
      <Show when={rows()}>
        <p title="Read-only inspection of this runtime; automatic deletion is off.">
          {new Set(rows().map((c) => c.cluster).filter(Boolean)).size} kind clusters · {rows().length} containers
        </p>
        <For each={groups()} fallback={<p>No matching containers.</p>}>
          {([label, cs]) => (
            <details>
              <summary>{label} ({cs.length})</summary>
              <ul>
                <For each={cs}>
                  {(c) => (
                    <li>
                      <b>{c.name}</b> · {c.status} · <code>{c.image}</code>{' '}
                      <button onClick={() => showLogs(c)}>View logs</button>
                    </li>
                  )}
                </For>
              </ul>
            </details>
          )}
        </For>
      </Show>
      <Show when={selected()}>
        {(c) => (
          <aside aria-label="Container logs">
            <h3>{c().name} · logs</h3>
            <p>
              {props.runtime.label} · <code>{c().id.slice(0, 12)}</code> · last 100 lines (up to 64 KiB).{' '}
              {c().cluster ? 'Node logs (not workload pod logs).' : ''}
            </p>
            <button onClick={() => showLogs(c())}>Refresh logs</button>{' '}
            <button onClick={() => { ++logRequest; select(null); setTail(null); }}>Close logs</button>
            <Show when={tail()} fallback={<p role="status">Reading logs…</p>}>
              {(t) => (
                <Show when={!t().error} fallback={<p role="alert">{t().error}</p>}>
                  <pre style={{ 'white-space': 'pre-wrap', 'overflow-wrap': 'anywhere' }}>{t().text || 'No log output.'}</pre>
                  <Show when={t().truncated}><p>Output truncated at 64 KiB.</p></Show>
                </Show>
              )}
            </Show>
          </aside>
        )}
      </Show>
    </section>
  );
}

function Recorded(props) {
  const [r, { refetch }] = createResource(async () => {
    try {
      return { rows: (await rpc('workspace.overview', { repo_id: props.id }, { local: true })).resources };
    } catch (e) {
      return { error: String(e) };
    }
  });
  for (const topic of ['reaper:changed', 'agents:changed']) useVisibleEvent(topic, () => refetch(), { debounceMs: 300 });
  return (
    <section>
      <h3 title="From this workspace's agent resource ledger.">{props.name} · recorded resources</h3>
      <Show when={r()} fallback={<p>Loading…</p>}>
        {(d) => (
          <Show when={!d().error} fallback={<p role="alert">{d().error}</p>}>
            <ul>
              <For each={d().rows} fallback={<li>No resources recorded for current agents.</li>}>
                {(v) => (
                  <li>
                    {[v.owner || v.agent_name, v.kind].filter(Boolean).join(' · ')} · <code>{v.handle}</code> · {v.state}
                  </li>
                )}
              </For>
            </ul>
          </Show>
        )}
      </Show>
    </section>
  );
}

export default function Environments() {
  const store = useStore();
  const [scope, setScope] = createSignal('checkout');
  const [runtimes, setRuntimes] = createSignal(null);
  const [selectedId, setSelectedId] = createSignal('');
  const [err, setErr] = createSignal('');
  let epoch = 0;
  onCleanup(() => epoch++);

  const selectedRuntime = createMemo(() => (runtimes() || []).find((r) => r.id === selectedId()) || null);

  async function scan() {
    const n = ++epoch;
    setScope('machine');
    setSelectedId('');
    setRuntimes(null);
    setErr('');
    try {
      const r = await discoverRuntimes();
      if (n !== epoch) return;
      setRuntimes(r);
      if (r.length === 1) setSelectedId(r[0].id);
    } catch (e) {
      if (n === epoch) setErr(String(e));
    }
  }

  return (
    <Page title="Environments" sub="Read-only inspection of local container runtimes">
      <div class="sheet">
        <div class="row gap-4">
          <button
            aria-pressed={scope() === 'checkout'}
            onClick={() => {
              ++epoch;
              setScope('checkout');
              setSelectedId('');
            }}
          >
            This workspace
          </button>
          <button aria-pressed={scope() === 'machine'} onClick={scan}>
            Local runtimes
          </button>
        </div>
        <Show
          when={scope() === 'checkout'}
          fallback={
            <section>
              <h2 title="Detected OrbStack, Colima, Docker, and Lima sockets. Read-only; never starts clusters or switches contexts.">
                Local container runtimes
              </h2>
              <Show when={err()}><p role="alert">{err()}</p></Show>
              <Show when={runtimes()} fallback={!err() && <p>Detecting runtimes…</p>}>
                <label>
                  Runtime{' '}
                  <select value={selectedId()} onChange={(e) => setSelectedId(e.currentTarget.value)}>
                    <option value="">Choose runtime</option>
                    <For each={runtimes()}>{(r) => <option value={r.id}>{r.label}</option>}</For>
                  </select>
                </label>
                <Show when={!runtimes().length}>
                  <p>No local OrbStack, Colima, Docker, or Lima socket detected.</p>
                </Show>
              </Show>
              <Show when={selectedRuntime()} keyed>{(rt) => <Inventory runtime={rt} />}</Show>
            </section>
          }
        >
          <Show
            when={activeRepoId()}
            keyed
            fallback={
              <div class="col" style={{ gap: '10px', 'margin-top': '8px' }}>
                <p>Select a workspace or inspect local container runtimes.</p>
                <div class="row gap-8" style={{ 'align-items': 'center', 'flex-wrap': 'wrap' }}>
                  <Show
                    when={(store.repos() ?? []).length}
                    fallback={<Button variant="primary" onClick={() => go('repos')}>Open Repositories</Button>}
                  >
                    <label>
                      Workspace{' '}
                      <select
                        aria-label="Select workspace"
                        value={activeRepoId()}
                        onChange={(e) => setActiveRepoId(e.currentTarget.value)}
                      >
                        <option value="">Choose a workspace…</option>
                        <For each={store.repos() ?? []}>{(r) => <option value={r.id}>{r.name}</option>}</For>
                      </select>
                    </label>
                    <Button size="sm" variant="ghost" onClick={() => go('repos')}>Manage repositories</Button>
                  </Show>
                </div>
              </div>
            }
          >
            {(id) => (
              <>
                <ManagedJobs id={id} />
                <Recorded id={id} name={store.repos()?.find((r) => r.id === id)?.name || 'Workspace'} />
              </>
            )}
          </Show>
        </Show>
      </div>
    </Page>
  );
}
