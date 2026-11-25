import { createMemo, createSignal, For, Show, onCleanup } from 'solid-js';
import { activeRepoId, useStore } from '../lib/store.js';
import { Page, Button, Segmented, Select, Input, Empty } from '../ui/kit.jsx';
import { ManagedJobs } from './managed-jobs.jsx';
import { Cleanup } from './cleanup.jsx';
import { rpc } from '../lib/rpc.js';
import { useVisibleEvent } from '../lib/visibility.js';
import '../styles/workspace.css';

// Environments: everything agents run outside the editor, in one place.
//  - Cleanup: what agents left running, what Philium could not stop, and what is ready to remove.
//  - Task sandboxes: short-lived offline containers for a claimed task (per workspace).
//  - Containers: a read-only look at local Docker / Colima / OrbStack runtimes.

const read = (m, args) => rpc(`environments.${m}`, args, { local: true });

function normalizeRuntimes(items) {
  if (!Array.isArray(items)) return [];
  return items
    .map((item) => {
      if (typeof item === 'string') return { id: item, runtime: 'docker', label: item, profile: item };
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
    return normalizeRuntimes(await read('runtimes', {}));
  } catch {
    return normalizeRuntimes(await read('profiles', {}));
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

  async function refresh(quiet = false) {
    const n = ++generation;
    if (quiet) {
      // Background re-read after cleanup: keep the list and open logs in place.
      try {
        const r = await read('containers', { profile: props.runtime.id });
        if (n === generation) setRows(r);
      } catch {}
      return;
    }
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
  for (const t of ['reaper:changed', 'managed:changed', 'agents:changed']) useVisibleEvent(t, () => !busy() && refresh(true), { debounceMs: 1500 });
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
    return m;
  });
  // Key groups by label so a background refresh keeps open groups open.
  const labels = createMemo(() => [...groups().keys()].sort((a, b) => a.localeCompare(b)), undefined, {
    equals: (a, b) => a.length === b.length && a.every((x, i) => x === b[i]),
  });

  return (
    <section class="ws-section" aria-label="Runtime inventory">
      <div class="ws-row">
        <div>
          <h2>{props.runtime.label}</h2>
          <Show when={rows()}>
            <p class="ws-muted small">
              {new Set(rows().map((c) => c.cluster).filter(Boolean)).size} kind clusters · {rows().length} containers
            </p>
          </Show>
        </div>
        <div class="ws-actions">
          <Input aria-label="Filter containers" placeholder="Filter" value={filter()} onValue={setFilter} />
          <Button size="sm" busy={busy()} onClick={() => refresh()}>Refresh</Button>
        </div>
      </div>
      <Show when={busy()}><p class="ws-muted small" role="status">Reading runtime…</p></Show>
      <Show when={issue()}><p class="ws-alert" role="alert">{issue()}</p></Show>
      <Show when={rows()}>
        <For each={labels()} fallback={<p class="ws-muted small">No matching containers.</p>}>
          {(label) => {
            const cs = () => groups().get(label) || [];
            return (
            <details class="cleanup-details">
              <summary>{label} <span class="ws-muted">({cs().length})</span></summary>
              <div class="cleanup-list">
                <For each={cs()}>
                  {(c) => (
                    <div class="cleanup-row">
                      <span class={`dot ${/^up/i.test(c.status) ? 'live' : ''}`} />
                      <div class="cleanup-main">
                        <div class="cleanup-title"><strong>{c.name}</strong><span class="ws-muted">· {c.status}</span></div>
                        <code class="ws-path" title={c.image}>{c.image}</code>
                      </div>
                      <div class="ws-actions"><Button size="sm" onClick={() => showLogs(c)}>Logs</Button></div>
                    </div>
                  )}
                </For>
              </div>
            </details>
            );
          }}
        </For>
      </Show>
      <Show when={selected()}>
        {(c) => (
          <aside aria-label="Container logs" class="cleanup-logs">
            <div class="ws-row">
              <div>
                <h3>{c().name} · logs</h3>
                <p class="ws-muted small">Last 100 lines (up to 64 KiB).{c().cluster ? ' Node logs, not workload pod logs.' : ''}</p>
              </div>
              <div class="ws-actions">
                <Button size="sm" onClick={() => showLogs(c())}>Refresh</Button>
                <Button size="sm" variant="ghost" onClick={() => { ++logRequest; select(null); setTail(null); }}>Close</Button>
              </div>
            </div>
            <Show when={tail()} fallback={<p class="ws-muted small" role="status">Reading logs…</p>}>
              {(t) => (
                <Show when={!t().error} fallback={<p class="ws-alert" role="alert">{t().error}</p>}>
                  <pre class="ws-diff">{t().text || 'No log output.'}</pre>
                  <Show when={t().truncated}><p class="ws-muted small">Output truncated at 64 KiB.</p></Show>
                </Show>
              )}
            </Show>
          </aside>
        )}
      </Show>
    </section>
  );
}

function Runtimes() {
  const [runtimes, setRuntimes] = createSignal(null);
  const [selectedId, setSelectedId] = createSignal('');
  const [err, setErr] = createSignal('');
  let live = true;
  onCleanup(() => (live = false));
  discoverRuntimes()
    .then((r) => {
      if (!live) return;
      setRuntimes(r);
      if (r.length) setSelectedId(r[0].id);
    })
    .catch((e) => live && setErr(String(e)));
  const selected = createMemo(() => (runtimes() || []).find((r) => r.id === selectedId()) || null);
  return (
    <div class="ws-settings">
      <Show when={err()}><p class="ws-alert" role="alert">{err()}</p></Show>
      <Show when={runtimes()} fallback={!err() && <p class="ws-muted small">Looking for OrbStack, Colima, Docker and Lima…</p>}>
        <Show
          when={runtimes().length}
          fallback={<Empty title="No container runtime found">Philium looked for OrbStack, Colima, Docker and Lima sockets on this Mac.</Empty>}
        >
          <Show when={runtimes().length > 1}>
            <Select value={selectedId()} options={runtimes().map((r) => ({ value: r.id, label: r.label }))} onValue={setSelectedId} />
          </Show>
          <Show when={selected()} keyed>{(rt) => <Inventory runtime={rt} />}</Show>
        </Show>
      </Show>
    </div>
  );
}

export default function Environments() {
  const store = useStore();
  const [tab, setTab] = createSignal('cleanup');
  const repoName = () => store.repos()?.find((r) => r.id === activeRepoId())?.name;
  return (
    <Page
      title="Environments"
      sub="What agents run outside your code, and cleaning it up"
      actions={
        <Segmented
          value={tab()}
          onChange={setTab}
          options={[
            { value: 'cleanup', label: 'Cleanup' },
            { value: 'sandboxes', label: 'Task sandboxes' },
            { value: 'containers', label: 'Containers' },
          ]}
        />
      }
    >
      <Show when={tab() === 'cleanup'}>
        <Cleanup pilot={store.status()?.pilot} />
      </Show>
      <Show when={tab() === 'sandboxes'}>
        <Show
          when={activeRepoId()}
          keyed
          fallback={<Empty title="Pick a workspace">Task sandboxes belong to a workspace. Choose one in the sidebar.</Empty>}
        >
          {(id) => (
            <div class="ws-settings">
              <section class="ws-section" aria-label={`${repoName() || 'Workspace'} sandboxes`}>
                <ManagedJobs id={id} />
              </section>
            </div>
          )}
        </Show>
      </Show>
      <Show when={tab() === 'containers'}>
        <Runtimes />
      </Show>
    </Page>
  );
}
