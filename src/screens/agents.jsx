// Agents: live conversations, approvals, terminals, diffs, and fleet overview.
// Supports Coordinator -> Agent / Reviewer hierarchies, KV cache telemetry, and clash warnings.
import { workspaceAgents, selectedAgentId, spawnRepoDefault } from "../lib/agent-workspace.js";
import { repoLabels } from "../lib/repo-labels.js";
import { kindResources } from "../lib/kind-resources.js";
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup, Suspense, lazy } from "solid-js";
import { NotFound, Page, Button, Badge, Dot, Empty, Segmented, ModelPicker, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { Conversation, Composer, KvTelemetryBar, speedEffort, effortSpeed, agentAttachmentLoader, PlainError } from "../ui/conversation.jsx";
import { agentHue, AgentChip, RoleBadge, KvBadge, WarningPills } from "../ui/agent-chip.jsx";
import { SpawnModal } from "../ui/spawn-modal.jsx";
import { AgentProgressLine, AgentProgressPanel } from "../ui/agent-progress.jsx";
import { AccessApprovalBar } from "../ui/access-requests.jsx";
import { pendingAccessByAgent, agentNeedsUser } from "../lib/access-helpers.js";
import { rpc, fmt, sendError } from "../lib/rpc.js";
import { route, go, params, registerCommands } from "../lib/nav.js";
import { matchShortcut, shortcutLabel, cycle } from "../lib/shortcuts.js";
import { useStore, activeRepoId, agentEventsCache } from "../lib/store.js";
import { useScreenVisible, useVisibleEvent } from "../lib/visibility.js";
import "../styles/agents.css";
import "../styles/code.css";

export { agentHue, AgentChip, RoleBadge, KvBadge, WarningPills, SpawnModal };

const AgentTerminals = lazy(() => import("../ui/agent-terminals.jsx").then((m) => ({ default: m.AgentTerminals })));
const AgentBrowser = lazy(() => import("../ui/agent-browser.jsx").then((m) => ({ default: m.AgentBrowser })));
const AgentContext = lazy(() => import("../ui/agent-context.jsx").then((m) => ({ default: m.AgentContext })));
const AgentSurfaces = lazy(() => import("../ui/agent-surfaces.jsx").then((m) => ({ default: m.AgentSurfaces })));

const STATE = {
  provisioning: ["busy", "Starting"],
  running: ["busy", "Working"],
  waiting: ["warn", "Needs you"],
  idle: ["live", "Idle"],
  stopped: ["", "Stopped"],
  failed: ["bad", "Failed"],
  tearing_down: ["busy", "Cleaning up"],
  done: ["", "Done"],
};
const stateDot = (s) => STATE[s]?.[0] ?? "";
const stateLabel = (s) => STATE[s]?.[1] ?? s;
const roleLabel = (r) => (r === "orchestrator" ? "Coordinator" : r === "reviewer" ? "Reviewer" : "Agent");
// "Needs you" badge + inline approval card already say "waiting"; don't repeat it as a warning pill.
const calmWarnings = (ws) => (ws ?? []).filter((w) => w?.kind !== "waiting");
// Repo memory loaded for this agent: count of active memories, or a dot for a working note only.
const memoryHint = (a) => {
  if (!a || a.strict) return "";
  const n = Number(a.memory_count) || 0;
  if (n > 0) return String(n);
  return a.has_working_note ? "•" : "";
};

/**
 * Fast-path O(K) event merge when incoming events are strictly newer than previous.at(-1).seq.
 * Falls back to Map deduplication + seq sort when events overlap or arrive out of order.
 */
function mergeEvents(previous, incoming) {
  if (!incoming?.length) return previous;
  if (!previous?.length) {
    let sorted = true;
    for (let i = 1; i < incoming.length; i++) {
      if (incoming[i].seq == null || incoming[i - 1].seq == null || incoming[i].seq <= incoming[i - 1].seq) {
        sorted = false;
        break;
      }
    }
    if (sorted) return incoming.slice();
  } else {
    const lastSeq = previous.at(-1)?.seq;
    if (lastSeq != null) {
      let canAppend = true;
      let prevSeq = lastSeq;
      for (let i = 0; i < incoming.length; i++) {
        const s = incoming[i].seq;
        if (s == null || s <= prevSeq) {
          canAppend = false;
          break;
        }
        prevSeq = s;
      }
      if (canAppend) return previous.concat(incoming);
    }
  }
  const bySeq = new Map(previous.map((event) => [event.seq, event]));
  for (const event of incoming) if (!bySeq.has(event.seq)) bySeq.set(event.seq, event);
  return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

function AgentRow(props) {
  const a = () => props.agent;
  return (
    <div class={`list-item agent-row ${props.selected ? "on" : ""} ${props.picked ? "picked" : ""}`} style={{ "padding-left": `${12 + props.depth * 14}px` }} onClick={props.onClick}
      role="option" aria-selected={props.picked ? "true" : "false"} data-agent={a().name} title="Tick, or ⌘-click, to pick several; then Delete removes them">
      <input type="checkbox" class="pick" checked={!!props.picked} aria-label={`Pick ${a().name}`} onClick={(e) => { e.stopPropagation(); props.onPick?.(); }} />
      <Dot state={stateDot(a().state)} title={stateLabel(a().state)} />
      <div class="grow" style={{ "min-width": 0 }}>
        <div class="row" style={{ gap: "6px" }}>
          <AgentChip name={a().name} />
          <Show when={(props.pendingAccess ?? 0) > 0}><Badge tone="strict">Access request</Badge></Show>
          <Show when={a().state === "waiting"}><Badge tone="strict">Needs you</Badge></Show>
          <Show when={(a().warnings ?? []).some((w) => w.kind === "clash")}><Badge tone="danger">Clash</Badge></Show>
        </div>
        <div class="xs faint ellipsis">{a().task_title || a().repo_name || a().branch || (a().model_ref ?? "").split("/").pop()}</div>
        <AgentProgressLine agent={a()} />
      </div>
      <span class="xs faint">{fmt.ago(a().last_active_at)}</span>
    </div>
  );
}

function Terminals(props) {
  const [list, { refetch }] = createResource(() => props.id, (id) => rpc("terminals.list").then((t) => t.filter((x) => x.agent_id === id)).catch(() => []));
  useVisibleEvent("terminals:exit", () => refetch(), { debounceMs: 120 });
  useVisibleEvent("terminals:changed", () => refetch(), { debounceMs: 120 });
  return (
    <div class="page-body">
      <Show when={(list() ?? []).length} fallback={<Empty title="No commands yet">Commands run by this agent appear here.</Empty>}>
        <div class="list">
          <For each={list()}>
            {(t) => (
              <div class="list-item" onClick={() => go("terminals", { id: t.id })}>
                <Dot state={t.state === "running" ? "busy" : t.exit_code === 0 ? "live" : "bad"} />
                <span class="mono small grow ellipsis">{t.title}</span>
                <span class="xs faint">{t.state === "running" ? "running" : `exit ${t.exit_code ?? "?"}`}</span>
                <span class="xs faint">{fmt.ago(t.created_at)}</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function Impact(props) {
  const [imp] = createResource(() => props.id, (id) => rpc("codeindex.impact", { agent: id }).catch(() => null));
  const byFile = (list) => { const m = new Map(); for (const x of list ?? []) { if (!m.has(x.path)) m.set(x.path, []); m.get(x.path).push(x); } return [...m.entries()]; };
  return (
    <Show when={imp() && ((imp().changed ?? []).length || (imp().dependents ?? []).length)}>
      <div class="impact">
        <h3>Impact</h3>
        <div class="xs faint" title='"name match" means same symbol name only, not confirmed via resolution.'>Changed symbols and callers.</div>
        <div class="small dim" style={{ "margin-top": "8px" }}>Changed symbols ({imp().changed.length})</div>
        <For each={byFile(imp().changed)}>{([path, xs]) => (<><div class="ifile">{path}</div><For each={xs}>{(x) => <div class="irow">{x.kind} {x.name} :{x.line}</div>}</For></>)}</For>
        <Show when={(imp().dependents ?? []).length}>
          <div class="small dim" style={{ "margin-top": "8px" }}>Used by ({imp().dependents.length})</div>
          <For each={byFile(imp().dependents)}>{([path, xs]) => (<><div class="ifile">{path}</div><For each={xs}>{(x) => <div class={`irow ${x.exact ? "" : "possible"}`}>{x.name} :{x.line} uses {x.via}{x.exact ? "" : " (name match)"}</div>}</For></>)}</For>
        </Show>
      </div>
    </Show>
  );
}

function Changes(props) {
  const [diff, { refetch }] = createResource(() => props.id, (id) => rpc("agents.diff", { id }).catch((e) => ({ error: String(e) })));
  const a = () => props.agent;
  return (
    <div class="page-body">
      <div class="row" style={{ "margin-bottom": "8px", "flex-wrap": "wrap" }}>
        <span class="small dim grow">Worktree diff against base.</span>
        <Show when={a()?.repo_id}>
          <Button size="sm" variant="primary" onClick={() => go("code", { repo_id: a().repo_id, ...(a().worktree_id ? { worktree_id: a().worktree_id } : {}) })}>
            <Icon name="graph" size={12} /> Inspect & comment in Code
          </Button>
        </Show>
        <Button size="sm" onClick={() => props.onOpenShell ? props.onOpenShell() : go("terminals")}>
          <Icon name="terminal" size={12} /> Open terminal
        </Button>
        <Button size="sm" variant="ghost" onClick={() => go("review")}>
          <Icon name="pr" size={12} /> Review
        </Button>
        <Button size="sm" onClick={refetch}><Icon name="refresh" size={12} /> Refresh</Button>
      </div>
      <Impact id={props.id} />
      <Show when={diff()?.error}><div class="err small">{diff().error}</div></Show>
      <Show when={diff() && !diff().error && !(diff().patch || diff().stat)}><Empty title="No changes yet" /></Show>
      <Show when={(diff()?.untracked || []).length}><p class="small" aria-label="New files not added yet">{diff().untracked.length} new file(s) not added yet: {diff().untracked.slice(0, 12).join(", ")}{diff().untracked.length > 12 ? " …" : ""}</p></Show>
      <Show when={diff()?.truncated}><p class="small dim">Some changes are too large to show here. Open the worktree to inspect everything.</p></Show>
      <Show when={diff()?.stat}><pre class="conv-pre">{diff().stat}</pre></Show>
      <Show when={diff()?.patch}><pre class="conv-pre diff">{diff().patch}</pre></Show>
    </div>
  );
}

function Details(props) {
  const a = () => props.agent;
  const [host] = createResource(() => rpc("app.status").catch(() => null));
  const canChangeClusters = () => host()?.pilot === false;
  const kv = () => a()?.kv || {};
  const [kindInfo, { refetch: refetchKind }] = createResource(
    () => a()?.id,
    async (id) => {
      try { return { id, clusters: kindResources(await rpc("agents.kind_list", { id })) }; }
      catch (error) { return { id, clusters: [], error: String(error) }; }
    }
  );
  // Resources may retain the previous value while a new agent loads.
  const kindState = () => kindInfo()?.id === a()?.id ? kindInfo() : undefined;
  const [kindBusy, setKindBusy] = createSignal(false);

  const createKind = async () => {
    setKindBusy(true);
    try {
      const res = await rpc("agents.kind_create", { id: a().id }, { action: true });
      toast(`Created kind cluster ${res.cluster} (HTTP :${res.http_port})`, "secure");
      refetchKind();
      props.onRefetch?.();
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setKindBusy(false);
    }
  };

  const deleteKind = async (cluster) => {
    const owner = a().id;
    if (!canChangeClusters() || !await confirm("Delete test cluster?", `Delete ${cluster} and the workloads inside it? This cannot be undone.`)) return;
    if (a()?.id !== owner) return;
    setKindBusy(true);
    try {
      await rpc("agents.kind_delete", { id: owner, cluster }, { action: true });
      toast(`Deleted kind cluster ${cluster}`);
      refetchKind();
      props.onRefetch?.();
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setKindBusy(false);
    }
  };

  return (
    <div class="page-body agent-details">
      <KvTelemetryBar kv={kv()} />
      <div class="grid-2" style={{ "margin-top": "12px" }}>
        <div><div class="xs faint">Role</div><div class="row gap-4"><RoleBadge role={a().role} /><span class="small">{a().parent_name ? `Helper of ${a().parent_name}` : roleLabel(a().role)}</span></div></div>
        <div><div class="xs faint">Model</div><div class="mono small">{a().model_ref}</div></div>
        <div><div class="xs faint">Reasoning Effort</div><div class="mono small">{a().effort ?? "default"}</div></div>
        <div><div class="xs faint">Security</div>
          <Show when={a().strict} fallback={<Badge tone="secure">Standard: trusted provider</Badge>}>
            <Badge tone="strict">Strict: sandboxed, network allowlist</Badge>
          </Show>
        </div>
        <div><div class="xs faint">Workspace</div><div class="mono xs ellipsis" title={a().cwd ?? a().worktree}>{a().cwd ?? a().worktree ?? "not running"}</div></div>
        <div><div class="xs faint">Branch</div><div class="mono small">{a().branch ?? "none"}</div></div>
        <div><div class="xs faint">Ports</div><div class="mono small">{a().port_base}–{a().port_base + 19} (kind HTTP :{a().port_base + 10}, HTTPS :{a().port_base + 11})</div></div>
        <div><div class="xs faint">Started</div><div class="small">{fmt.ago(a().created_at)}</div></div>
      </div>

      <h3 class="section-title">kind clusters</h3>
      <div class="card" style={{ padding: "12px", "margin-bottom": "12px" }}>
        <div class="row" style={{ "margin-bottom": "8px", "flex-wrap": "wrap" }}>
          <span class="small dim grow">
            Isolated cluster on ports <code class="mono">:{a().port_base + 10}</code> / <code class="mono">:{a().port_base + 11}</code>.
          </span>
          <Button size="sm" variant="primary" disabled={!canChangeClusters()} busy={kindBusy()} onClick={createKind}>
            <Icon name="plus" size={11} /> Create cluster
          </Button>
        </div>
        <Show when={host()?.pilot}><p class="small faint">Cluster inventory is read-only in this pilot.</p></Show>
        <Show when={(kindState()?.clusters ?? []).length} fallback={
          <div class="xs faint" role={kindState()?.error ? "alert" : undefined}>
            {kindState()?.error
              ? `Could not load kind clusters: ${kindState().error}`
              : kindInfo.loading || !kindState() ? "Loading kind clusters…" : "No active kind clusters for this agent."}
          </div>
        }>
          <div class="list">
            <For each={kindState()?.clusters ?? []}>
              {(cl) => (
                <div class="list-item">
                  <Dot state="live" />
                  <span class="mono small grow">{cl.handle}</span>
                  <span class="mono xs faint">HTTP :{cl.meta.http_port ?? "?"} · HTTPS :{cl.meta.https_port ?? "?"}</span>
                  <span class="mono xs faint ellipsis" style={{ "max-width": "240px" }} title={cl.meta.kubeconfig}>{cl.meta.kubeconfig}</span>
                  <Button size="sm" variant="ghost" disabled={!canChangeClusters() || kindBusy()} onClick={() => deleteKind(cl.handle)}>
                    <Icon name="trash" size={12} /> Delete
                  </Button>
                </div>
              )}
            </For>
          </div>
        </Show>
      </div>

      <Show when={(a().warnings ?? []).length}>
        <h3 class="section-title">Active Warnings</h3>
        <WarningPills warnings={a().warnings} max={10} />
      </Show>
      <h3 class="section-title">Resources owned</h3>
      <Show when={(a().resources ?? []).length} fallback={<div class="small faint">Nothing yet.</div>}>
        <table class="table">
          <thead><tr><th>Kind</th><th>Handle</th><th>State</th><th>Created</th></tr></thead>
          <tbody>
            <For each={a().resources}>
              {(r) => <tr><td>{r.kind}</td><td class="mono xs ellipsis" style={{ "max-width": "380px" }}>{r.handle}</td><td><Badge tone={r.state === "leaked" ? "danger" : ""}>{r.state}</Badge></td><td class="xs faint">{fmt.ago(r.created_at)}</td></tr>}
            </For>
          </tbody>
        </table>
      </Show>
      <Show when={(a().children ?? []).length}>
        <h3 class="section-title">Helpers ({a().children.length})</h3>
        <div class="row" style={{ "flex-wrap": "wrap", gap: "6px" }}>
          <For each={a().children}>
            {(c) => (
              <span onClick={() => go("agents", { id: c.id })} style={{ cursor: "pointer" }} class="row gap-4">
                <Dot state={stateDot(c.state)} />
                <AgentChip name={c.name} />
                <RoleBadge role={c.role} />
              </span>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function AgentView(props) {
  const screenVisible = useScreenVisible();
  const [tab, setTab] = createSignal("chat");
  const [aside, setAsideRaw] = createSignal(localStorage.getItem("agent.aside") ?? "none");
  const setAside = (v) => { setAsideRaw(v); localStorage.setItem("agent.aside", v); };
  const [asideW, setAsideW] = createSignal(Number(localStorage.getItem("agent.aside.w")) || 520);
  const [browserPing, setBrowserPing] = createSignal(false);
  createEffect(on(aside, (v, prev) => {
    if (v === "browser") setBrowserPing(false);
    else if (prev === "browser" && props.id) rpc("browser.hide", { owner: props.id }).catch(() => {});
  }));
  useVisibleEvent("browser:tab", (t) => {
    if (t?.owner === props.id && aside() !== "browser") setBrowserPing(true);
  });
  useVisibleEvent("browser:agent_action", (act) => {
    if (act?.owner === props.id && aside() !== "browser") setBrowserPing(true);
  });
  const drag = (e) => {
    e.preventDefault();
    const x0 = e.clientX, w0 = asideW();
    const move = (ev) => setAsideW(Math.max(320, Math.min(window.innerWidth - 560, w0 + (x0 - ev.clientX))));
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); localStorage.setItem("agent.aside.w", String(asideW())); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const store = useStore();
  const [events, setEvents] = createSignal([]);
  // An agent that was just removed must not take the whole screen down with a failed read.
  const [info, { refetch }] = createResource(() => props.id, (id) => rpc("agents.get", { id }, { local: true }).catch(() => null));
  const models = store.models;
  const providers = store.providers;
  const [historyLoading, setHistoryLoading] = createSignal(false);
  const [historyError, setHistoryError] = createSignal("");
  const [historyCursor, setHistoryCursor] = createSignal(0);
  const [historyMore, setHistoryMore] = createSignal(false);
  let retryAfter = 0;
  let generation = 0, historyId;
  const merge = mergeEvents;
  const load = async (id, after = 0) => {
    const request = ++generation;
    let effectiveAfter = after;
    if (historyId !== id) {
      historyId = id;
      const cached = agentEventsCache.get(id);
      if (cached?.length && after === 0) {
        setEvents(cached);
        effectiveAfter = cached.at(-1)?.seq ?? 0;
        setHistoryCursor(effectiveAfter);
        setHistoryMore(false);
      } else {
        setEvents([]);
        setHistoryCursor(0);
        setHistoryMore(false);
      }
    }
    retryAfter = effectiveAfter;
    if (!agentEventsCache.has(id)) setHistoryLoading(true);
    setHistoryError("");
    try {
      const evs = await rpc("agents.events", { id, after: effectiveAfter, limit: 5000 }, { local: true });
      if (request !== generation || props.id !== id) return;
      if (!Array.isArray(evs)) throw new Error("Invalid history response");
      setEvents((current) => {
        const next = merge(current, evs);
        agentEventsCache.set(id, next);
        return next;
      });
      const next = evs.at(-1)?.seq ?? effectiveAfter;
      setHistoryCursor(next);
      setHistoryMore(evs.length === 5000 && next > effectiveAfter);
    } catch (e) {
      if (request === generation && props.id === id) setHistoryError(String(e));
    } finally {
      if (request === generation && props.id === id) setHistoryLoading(false);
    }
  };
  onCleanup(() => generation++);
  createEffect(on(() => props.id, (id) => load(id)));
  useVisibleEvent("agents:events", (p) => {
    if (p.id !== props.id || historyId !== props.id) return;
    setEvents((current) => {
      const next = merge(current, p.events ?? []);
      agentEventsCache.set(props.id, next);
      return next;
    });
  });
  useVisibleEvent("agents:changed", (p) => { if (p.id === props.id) refetch(); });
  useVisibleEvent("library", () => refetch(), { debounceMs: 250 });

  const a = () => info()?.id === props.id ? info() : props.agent;
  const [effort, setEffort] = createSignal("default");
  const [effortPending, setEffortPending] = createSignal(false);
  let effortRequest = 0;
  onCleanup(() => effortRequest++);
  createEffect(on(() => props.id, (id) => {
    const request = ++effortRequest;
    setEffort("default");
    setEffortPending(true);
    rpc("agents.get_effort", { id })
      .then((value) => { if (request === effortRequest && props.id === id) setEffort(value); })
      .catch(() => { if (request === effortRequest && props.id === id) setEffort("default"); })
      .finally(() => { if (request === effortRequest && props.id === id) setEffortPending(false); });
  }));
  const changeEffort = async (value) => {
    if (effortPending() || !live()) return;
    const id = props.id, request = ++effortRequest;
    setEffortPending(true);
    try {
      const saved = await rpc("agents.set_effort", { id, effort: speedEffort(value) });
      if (request === effortRequest && props.id === id) setEffort(saved);
    } catch (e) {
      if (request === effortRequest && props.id === id) toast(String(e), "danger");
    } finally {
      if (request === effortRequest && props.id === id) setEffortPending(false);
    }
  };
  const live = () => a()?.id === props.id ? a()?.live : props.agent?.live;
  const activeModel = createMemo(() => (models() ?? []).find((m) => m.ref === a()?.model_ref));

  const changeModel = async (newRef) => {
    if (!newRef || newRef === a()?.model_ref) return true;
    try {
      const before = effort();
      await rpc("agents.set_model", { id: props.id, model: newRef });
      // Effort levels differ by model. Say so when the chosen level did not survive the switch.
      rpc("agents.get_effort", { id: props.id }).then((v) => {
        const now = v || "default";
        setEffort(now);
        if (before !== "default" && now !== before) toast(`${newRef.split("/").pop()} has no ${before} effort level. Effort is now Auto.`, "info");
      }).catch(() => {});
      toast(`Switched ${a()?.name ?? "agent"} model to ${newRef} (0 tokens used)`, "secure");
      refetch();
      return true;
    } catch (e) {
      toast(String(e), "danger");
      return false;
    }
  };

  const [pickModel, setPickModel] = createSignal(0);
  // After /model the cursor belongs back in the message box. A model change can redraw or
  // briefly disable that box, so try again while nothing else has taken the focus.
  const backToComposer = () => {
    const focus = () => agentMain?.querySelector("textarea.composer-input:not(:disabled)")?.focus();
    focus();
    for (const ms of [150, 500, 1200]) setTimeout(() => { const el = document.activeElement; if (!el || el === document.body) focus(); }, ms);
  };
  const handleSlashModel = async (query) => {
    if (!query) {
      // Bare /model opens the picker; /model <name> still switches straight away.
      setPickModel((n) => n + 1);
      return true;
    }
    const q = query.toLowerCase();
    const provMap = Object.fromEntries((providers() ?? []).map((p) => [p.id, p]));
    const curProv = (a()?.model_ref || "").split("/")[0];
    const allowed = (models() ?? []).filter((m) => {
      if (m.enabled === 0) return false;
      const p = provMap[m.provider_id];
      if (!p || p.enabled === false) return false;
      if (a()?.strict) return m.provider_id === curProv;
      return !!p.trusted;
    });
    const match =
      allowed.find((m) => m.ref.toLowerCase() === q || m.model_id.toLowerCase() === q) ||
      allowed.find((m) => `${m.provider_name || ""} ${m.display || ""} ${m.ref}`.toLowerCase().includes(q));
    if (!match) {
      toast(`No accepted model matching "${query}" in this session's trust boundary.`, "danger");
      return false;
    }
    return await changeModel(match.ref);
  };

  let observedId, observedLive;
  createEffect(() => {
    const id = props.id, value = live();
    if (typeof value !== "boolean") return;
    if (observedId === id && observedLive !== value) load(id);
    observedId = id; observedLive = value;
  });
  const busy = () => ["running", "waiting"].includes(a()?.state);
  let agentMain;

  // What is typed while the agent works waits here, survives a quit, and is sent when the step ends.
  const [queued, { refetch: refetchQueue }] = createResource(() => props.id, (id) => rpc("agents.queue_list", { id }).catch(() => []));
  useVisibleEvent("agents:queue", (p) => { if (p.id === props.id || p.id === a()?.id) refetchQueue(); });
  const queue = async (text, images = []) => {
    try { await rpc("agents.queue_add", { id: props.id, text, images }, { action: true }); refetchQueue(); return true; }
    catch (e) { toast(String(e), "danger"); return false; }
  };
  const send = async (text, images = []) => {
    try { await rpc("agents.send", { id: props.id, text, images }, { action: true }); return true; }
    catch (e) {
      // It started working a moment before: queue instead of refusing.
      if (/session is busy|"type"\s*:\s*"busy"/i.test(String(e))) return queue(text, images);
      toast(sendError(e), "danger"); return false;
    }
  };
  const decide = (request_id, decision, scope) =>
    live() ? rpc("agents.permission", { id: props.id, request_id, decision, scope }, { action: true }).catch((e) => toast(String(e), "danger")) : Promise.resolve();

  const remove = async () => {
    const r = await rpc("agents.remove", { id: props.id }).catch((e) => ({ error: String(e) }));
    if (r?.error) return toast(r.error, "danger");
    if (r?.needs_confirmation) {
      const ok = await confirm(`Remove ${a().name}?`, `${r.reason ?? "This agent has unsaved work."} ${(r.items ?? []).map((i) => i.handle ?? i).join(", ")}`, { danger: true, ok: "Delete anyway" });
      if (!ok) return;
      const r2 = await rpc("agents.remove", { id: props.id, force: true }).catch((e) => ({ error: String(e) }));
      if (r2?.error) return toast(r2.error, "danger");
    }
    toast(`${a().name} removed`);
    go("agents", {});
  };

  const toggleAside = (panel) => setAside(aside() === panel ? "none" : panel);

  const handleSlashRemember = async (raw) => {
    if (!raw) {
      setAside("context");
      toast("Usage: /remember [fact_key=]decision text", "info");
      return false;
    }
    if (a()?.strict) {
      toast("Strict agents cannot write to personal or repository memory.", "danger");
      return false;
    }
    let factKey = null;
    let body = raw;
    const eq = raw.indexOf("=");
    if (eq > 0 && eq < 48 && /^[a-zA-Z0-9._:/-]+$/.test(raw.slice(0, eq).trim())) {
      factKey = raw.slice(0, eq).trim();
      body = raw.slice(eq + 1).trim();
    }
    if (!body) return false;
    try {
      const scope = a()?.repo_id ? `repo:${a().repo_id}` : "global";
      await rpc("library.save", {
        content: body,
        category: "preference",
        scope,
        fact_key: factKey,
        trust_tier: "user_direct",
        provenance: `slash:remember:${a()?.name || "agent"}`,
      });
      toast(`Saved direct decision${factKey ? ` (${factKey})` : ""} (0 tokens)`, "secure");
      setAside("context");
      return true;
    } catch (e) {
      toast(String(e), "danger");
      return false;
    }
  };

  const handleSlashFork = async (brief) => {
    try {
      const cur = a();
      const forked = await rpc("agents.spawn", {
        role: cur?.role || "worker",
        repo_id: cur?.repo_id || null,
        model: cur?.model_ref || null,
        strict: !!cur?.strict,
        base: cur?.branch || null,
        brief: brief || `Forked from ${cur?.name}. Check memory_slate and working_note_get for current state.`,
      });
      toast(`Forked ${cur?.name} -> ${forked.name}`, "secure");
      store.refetchAgents?.();
      go("agents", { id: forked.id });
      return true;
    } catch (e) {
      toast(String(e), "danger");
      return false;
    }
  };

  return (
    <div class="agent-view">
      <div class="agent-head">
        <Dot state={stateDot(a()?.state)} />
        <AgentChip name={a()?.name} />
        <RoleBadge role={a()?.role} />
        <span class="small dim">{stateLabel(a()?.state)}</span>
        <ModelPicker
          compact
          value={a()?.model_ref || ""}
          onValue={changeModel}
          models={models() ?? []}
          providers={providers() ?? []}
          strict={!!a()?.strict}
          currentProvider={(a()?.model_ref || "").split("/")[0]}
          openRequest={pickModel()}
          onDismiss={backToComposer}
        />
        <Show when={a()?.strict}><Badge tone="strict"><Icon name="shield" size={11} /> Strict</Badge></Show>
        <Show when={a()?.parent_id}>
          <span class="xs faint" style={{ cursor: "pointer" }} onClick={() => go("agents", { id: a().parent_id })}>
            ↳ coordinator: <b>{a()?.parent_name || "parent"}</b>
          </span>
        </Show>
        <Show when={a()?.branch}>
          <span class="mono xs faint ellipsis">{`${a()?.repo_name || ""}#${a()?.branch}`}</span>
        </Show>
        <div class="grow" />
        <Segmented value={tab()} onChange={setTab} options={[{ value: "chat", label: "Chat" }, { value: "terms", label: "Commands" }, { value: "changes", label: "Changes" }, { value: "info", label: "Details" }]} />
        <div class="segmented" role="group" aria-label="Inspector panel">
          <button type="button" class={aside() === "terminal" ? "on" : ""} onClick={() => toggleAside("terminal")} title="Toggle live shell alongside chat">
            <span class="row gap-4"><Icon name="terminal" size={12} /> Shell</span>
          </button>
          <button type="button" class={aside() === "browser" ? "on" : ""} onClick={() => toggleAside("browser")} title="Toggle isolated agent browser">
            <span class="row gap-4"><Icon name="globe" size={12} /> Browser<Show when={browserPing()}><span class="ping" /></Show></span>
          </button>
          <button type="button" class={aside() === "surfaces" ? "on" : ""} onClick={() => toggleAside("surfaces")} title="Isolated apps (KiCad, DBeaver, Chromium)">
            <span class="row gap-4"><Icon name="layers" size={12} /> Apps</span>
          </button>
          <button type="button" class={aside() === "context" ? "on" : ""} onClick={() => toggleAside("context")}
            title={memoryHint(a()) ? `Repo memory active: ${Number(a()?.memory_count) || 0} memories${a()?.has_working_note ? " + working note" : ""}, injected automatically` : "Workspace memory & notes"}>
            <span class="row gap-4"><Icon name="brain" size={12} /> Memory<Show when={memoryHint(a())}><span class="xs dim">· {memoryHint(a())}</span></Show></span>
          </button>
        </div>
        <Show when={live()} fallback={<Button size="sm" onClick={() => rpc("agents.resume", { id: props.id }, { action: true }).catch((e) => toast(String(e), "danger"))}><Icon name="play" size={11} /> Resume</Button>}>
          <Button size="sm" onClick={() => rpc("agents.stop", { id: props.id }, { action: true }).catch((e) => toast(String(e), "danger"))}><Icon name="stop" size={11} /> Stop</Button>
        </Show>
        <Button size="sm" variant="ghost" title="Remove agent and clean up" onClick={remove}><Icon name="trash" size={12} /></Button>
      </div>
      <Show when={tab() === "chat"}>
        <div style={{ padding: "6px 16px 0" }}>
          <KvTelemetryBar kv={a()?.kv} compact />
        </div>
      </Show>
      <Show when={(a()?.children ?? []).length}>
        <div class="cluster-subbar">
          <span class="xs faint">Helpers ({a().children.length}):</span>
          <For each={a().children}>
            {(c) => (
              <button type="button" class="cluster-subpill" onClick={() => go("agents", { id: c.id })}>
                <Dot state={stateDot(c.state)} />
                <span>{c.name}</span>
                <RoleBadge role={c.role} />
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={a()?.error}><div class="shield danger" style={{ margin: "8px 16px 0" }}><PlainError text={a().error} /></div></Show>
          <Show when={a()?.rate_limited && a()?.state === "idle"}><div class="shield" role="status" style={{ margin: "8px 16px 0" }}>{a().rate_limited.until ? `The model provider's rate limit stopped the last step. ${a().name} continues at ${new Date(a().rate_limited.until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}, or when you send a message.` : `Stopped at the rate limit after several tries. Send a message to continue.`}</div></Show>
      <Show when={calmWarnings(a()?.warnings).length}>
        <div style={{ padding: "6px 16px 0" }}><WarningPills warnings={calmWarnings(a()?.warnings)} max={4} /></div>
      </Show>
      <div class="agent-body">
        <div class="agent-main" ref={agentMain}>
          <Show when={tab() === "chat"}>
            <Show when={historyLoading()}><div class="history-loading small dim" role="status">Loading saved conversation…</div></Show>
            <Show when={historyError()}><div class="history-error shield danger" role="alert">Unable to load saved conversation: {historyError()} <Button size="sm" onClick={() => load(props.id, retryAfter)}>Retry history</Button></div></Show>
            <Show when={historyMore()}><div class="history-more small dim" role="status">More saved history is available. Showing loaded pages only. <Button size="sm" disabled={historyLoading()} onClick={() => load(props.id, historyCursor())}>Load more history</Button></div></Show>
            <AccessApprovalBar agentId={props.id} agentName={a()?.name} repoId={a()?.repo_id} strict={!!a()?.strict} />
            <Conversation agent={a()?.name} events={events()} busy={busy()} readOnly={!live()} loadAttachment={agentAttachmentLoader(props.id)} emptyText={historyLoading() ? "Loading saved conversation…" : historyError() ? "History could not be loaded. Retry above." : live() ? "Send the first message to start." : "No saved messages. This agent is stopped. Resume it to continue."}
              onApprove={(id, scope) => decide(id, "approve", scope)} onDeny={(id) => decide(id, "deny")} />
            <Composer draftKey={`agent:${props.id}`} dropTarget={() => agentMain} onSend={send} busy={busy()} disabled={!live()}
              onQueue={queue} queued={queued() ?? []} queueTarget={a()?.name}
              onTakeQueued={(q) => rpc("agents.queue_take", { id: props.id, item: q.id }).then((r) => r?.text).catch((e) => { toast(String(e), "danger"); return null; })}
              onRemoveQueued={(q) => rpc("agents.queue_remove", { id: props.id, item: q.id }).catch((e) => toast(String(e), "danger"))} onCancel={() => rpc("agents.cancel", { id: props.id }, { action: true }).catch((e) => toast(String(e), "danger"))}
              effort={effort()} speed={effortSpeed(effort())} effortOptions={activeModel()?.effort_details ?? []} effortPending={effortPending()} onEffort={changeEffort} onSpeed={changeEffort} onSlashModel={handleSlashModel}
              onSlashContext={() => setAside("context")} onSlashRemember={handleSlashRemember} onSlashFork={handleSlashFork} onSlashCompact={() => rpc("agents.compact", { id: props.id }, { action: true }).then(() => toast("Context compacted.", "success")).catch((e) => toast(String(e), "danger"))}
              placeholder={live() ? `Message ${a()?.name ?? ""}…` : "Stopped"} />
          </Show>
          <Show when={tab() === "terms"}><Terminals id={props.id} /></Show>
          <Show when={tab() === "changes"}><Changes id={props.id} agent={a()} onOpenShell={() => setAside("terminal")} /></Show>
          <Show when={tab() === "info" && info()}><Details agent={info()} onRefetch={refetch} /></Show>
        </div>
        <div class="agent-aside" classList={{ hidden: aside() === "none" }} style={{ width: `${asideW()}px` }}>
          <div class="aside-grip" onPointerDown={drag} title="Drag to resize" />
          <Show when={aside() === "terminal"}>
            <div class="aside-body">
              <Suspense fallback={<div class="small dim" role="status">Loading terminals…</div>}>
                <AgentTerminals id={props.id} name={a()?.name ?? ""} live={live()} active={screenVisible() && aside() === "terminal"} />
              </Suspense>
            </div>
          </Show>
          <Show when={aside() === "browser"}>
            <div class="aside-body">
              <Suspense fallback={<div class="small dim" role="status">Loading browser…</div>}>
                <AgentBrowser agent={props.id} name={a()?.name} strict={a()?.strict} provider={a()?.model_ref?.split("/")[0]} active={screenVisible() && aside() === "browser"} visible={screenVisible() && aside() === "browser"} onActivity={() => setBrowserPing(true)} />
              </Suspense>
            </div>
          </Show>
          <Show when={aside() === "surfaces"}>
            <div class="aside-body">
              <Suspense fallback={<div class="small dim" role="status">Loading isolated apps…</div>}>
                <AgentSurfaces agentId={props.id} repoId={a()?.repo_id} />
              </Suspense>
            </div>
          </Show>
          <Show when={aside() === "context"}>
            <div class="aside-body">
              <Suspense fallback={<div class="small dim" role="status">Loading context slate…</div>}>
                <AgentContext agentId={props.id} name={a()?.name} repoId={a()?.repo_id} repoName={a()?.repo_name} modelRef={a()?.model_ref} strict={!!a()?.strict} />
              </Suspense>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}

export function AgentGridCard(props) {
  const a = () => props.agent;
  const [events, setEvents] = createSignal([]);
  const [text, setText] = createSignal("");
  const [sending, setSending] = createSignal(false);
  let generation = 0;

  const load = async (id) => {
    const req = ++generation;
    const cached = agentEventsCache.get(id);
    const after = cached?.length ? (cached.at(-1)?.seq ?? 0) : 0;
    if (cached?.length) setEvents(cached);
    try {
      const evs = await rpc("agents.events", { id, after, limit: 5000 }, { local: true });
      if (req === generation && Array.isArray(evs)) {
        setEvents((cur) => {
          const next = mergeEvents(cur, evs);
          agentEventsCache.set(id, next);
          return next;
        });
      }
    } catch {}
  };

  onCleanup(() => generation++);
  createEffect(on(() => a()?.id, (id) => { if (id) load(id); }));
  useVisibleEvent("agents:events", (p) => {
    const id = a()?.id;
    if (!id || p.id !== id) return;
    setEvents((cur) => {
      const next = mergeEvents(cur, p.events ?? []);
      agentEventsCache.set(id, next);
      return next;
    });
  });

  const live = () => !!a()?.live;
  const busy = () => ["running", "waiting"].includes(a()?.state);
  const hasClash = () => (a()?.warnings ?? []).some((w) => w.kind === "clash");

  const send = async (e) => {
    e?.preventDefault();
    const msg = text().trim();
    if (!msg || !live() || sending()) return;
    setSending(true);
    try {
      if (busy()) {
        await rpc("agents.queue_add", { id: a().id, text: msg, images: [] }, { action: true });
        toast(`Queued for ${a().name}: sent when this step ends`, "info");
      } else await rpc("agents.send", { id: a().id, text: msg, images: [] }, { action: true });
      setText("");
    } catch (err) {
      toast(sendError(err), "danger");
    } finally {
      setSending(false);
    }
  };

  const decide = (request_id, decision, scope) =>
    live() ? rpc("agents.permission", { id: a().id, request_id, decision, scope }, { action: true }).catch((e) => toast(String(e), "danger")) : Promise.resolve();

  return (
    <div
      class={`agent-card role-card-${a()?.role || "worker"} ${a()?.state === "waiting" || (props.pendingAccess ?? 0) > 0 ? "needs-you" : hasClash() ? "has-clash" : busy() ? "active" : ""}`}
      data-agent-card={a()?.name}
    >
      <div class="agent-card-head" onClick={() => props.onOpen?.(a().id)}>
        <Dot state={stateDot(a()?.state)} title={stateLabel(a()?.state)} />
        <AgentChip name={a()?.name} />
        <RoleBadge role={a()?.role} />
        <span class="xs dim">{stateLabel(a()?.state)}</span>
        <KvBadge kv={a()?.kv} />
        <Show when={a()?.strict}><Badge tone="strict">Strict</Badge></Show>
        <Show when={(props.pendingAccess ?? 0) > 0}><Badge tone="strict">Access request</Badge></Show>
        <Show when={a()?.state === "waiting"}><Badge tone="strict">Needs you</Badge></Show>
        <Show when={a()?.parent_name}><span class="xs faint" title={`Helper of ${a().parent_name}`}>↳ {a().parent_name}</span></Show>
        <Show when={(a()?.children_count ?? 0) > 0}><span class="xs faint" title="Helpers">◈ {a().children_count}</span></Show>
        <span class="xs faint ellipsis grow" title={a()?.task_title || a()?.branch || a()?.model_ref}>
          {a()?.repo_name ? `${a().repo_name}${a().branch ? `@${a().branch}` : ""}` : (a()?.task_title || a()?.branch || a()?.model_ref)}
        </span>
        <span class="mono xs faint">{(a()?.model_ref ?? "").split("/").pop()}</span>
        <div class="row gap-4" onClick={(e) => e.stopPropagation()}>
          <Show when={a()?.role === "orchestrator"}>
            <Button size="sm" variant="ghost" onClick={() => props.onSpawnChild?.(a())} title="Add helper under this coordinator">
              <Icon name="plus" size={11} />
            </Button>
          </Show>
          <Show when={live()} fallback={
            <Button size="sm" onClick={() => rpc("agents.resume", { id: a().id }, { action: true }).catch((e) => toast(String(e), "danger"))} title="Resume agent">
              <Icon name="play" size={11} />
            </Button>
          }>
            <Show when={busy()}>
              <Button size="sm" onClick={() => rpc("agents.cancel", { id: a().id }, { action: true }).catch((e) => toast(String(e), "danger"))} title="Cancel current turn">
                Cancel
              </Button>
            </Show>
            <Button size="sm" onClick={() => rpc("agents.stop", { id: a().id }, { action: true }).catch((e) => toast(String(e), "danger"))} title="Stop agent">
              <Icon name="stop" size={11} />
            </Button>
          </Show>
          <Button size="sm" variant="ghost" onClick={() => props.onOpen?.(a().id)} title="Open full workspace (shell, browser, diff)">
            <Icon name="external" size={12} />
          </Button>
        </div>
      </div>
      <AgentProgressPanel agent={a()} />
      <Show when={calmWarnings(a()?.warnings).length}>
        <div class="agent-card-warnings">
          <WarningPills warnings={calmWarnings(a()?.warnings)} max={3} />
        </div>
      </Show>
      <Show when={a()?.error}><div class="shield danger xs" style={{ margin: "6px 10px 0" }}>{a().error}</div></Show>
      <div class="agent-card-body">
        <Conversation
          agent={a()?.name}
          events={events()}
          busy={busy()}
          readOnly={!live()}
          loadAttachment={agentAttachmentLoader(a().id)}
          emptyText={live() ? `Send a message to ${a()?.name} below.` : "Stopped. Press play to resume."}
          onApprove={(id, scope) => decide(id, "approve", scope)}
          onDeny={(id) => decide(id, "deny")}
        />
      </div>
      <form class="agent-card-foot" onSubmit={send}>
        <input
          type="text"
          class="input agent-card-input"
          value={text()}
          disabled={!live() || sending()}
          placeholder={live() ? `Message ${a()?.name}…` : `${a()?.name} is stopped`}
          onInput={(e) => setText(e.currentTarget.value)}
        />
        <Button size="sm" variant="primary" type="submit" disabled={!live() || sending() || !text().trim()}>
          <Icon name="send" size={12} />
        </Button>
      </form>
    </div>
  );
}

/** Ultra-dense tile for Fleet Heatmap view (100+ agents on screen). */
function FleetHeatmapTile(props) {
  const a = () => props.agent;
  const warnings = () => a()?.warnings ?? [];
  return (
    <div
      class={`fleet-tile state-${a()?.state} ${warnings().length ? "has-warning" : ""}`}
      onClick={() => props.onOpen?.(a().id)}
      title={`${a()?.name} (${roleLabel(a()?.role)}) · ${stateLabel(a()?.state)}${a()?.repo_name ? ` · ${a().repo_name}` : ""}${a()?.branch ? `@${a().branch}` : ""}`}
    >
      <div class="fleet-tile-top">
        <Dot state={stateDot(a()?.state)} />
        <AgentChip name={a()?.name} />
        <RoleBadge role={a()?.role} />
        <div class="grow" />
        <KvBadge kv={a()?.kv} />
      </div>
      <div class="fleet-tile-mid xs faint ellipsis">
        {a()?.repo_name ? `${a().repo_name}${a().branch ? `@${a().branch}` : ""}` : (a()?.task_title || (a()?.model_ref ?? "").split("/").pop())}
      </div>
      <AgentProgressLine agent={a()} />
      <Show when={warnings().length}>
        <WarningPills warnings={warnings()} compact max={2} />
      </Show>
      <div class="fleet-tile-foot" onClick={(e) => e.stopPropagation()}>
        <span class="xs faint">{stateLabel(a()?.state)}</span>
        <Show when={a()?.parent_name}><span class="xs faint">↳{a().parent_name}</span></Show>
        <div class="grow" />
        <Show when={a()?.live} fallback={
          <button type="button" class="fleet-mini-btn" onClick={() => rpc("agents.resume", { id: a().id }, { action: true }).catch((e) => toast(String(e), "danger"))} title="Resume">▶</button>
        }>
          <button type="button" class="fleet-mini-btn" onClick={() => rpc("agents.stop", { id: a().id }, { action: true }).catch((e) => toast(String(e), "danger"))} title="Stop">■</button>
        </Show>
      </div>
    </div>
  );
}

/** Quick Mission Dispatch Bar: 1-line launch for a Coordinator or Agent. */
function MissionDispatchBar(props) {
  const store = useStore();
  const repos = store.repos;
  const labels = createMemo(() => repoLabels(repos() ?? []));
  const [repo, setRepo] = createSignal(spawnRepoDefault(props.preset, activeRepoId()));
  createEffect(on(activeRepoId, (id) => setRepo(spawnRepoDefault(props.preset, id)), { defer: true }));
  const [role, setRole] = createSignal("orchestrator");
  const [brief, setBrief] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const dispatch = async (e) => {
    e?.preventDefault();
    const text = brief().trim();
    if (!text || busy()) return;
    setBusy(true);
    try {
      const a = await rpc("agents.spawn", {
        role: role(),
        repo_id: repo() || null,
        brief: text,
      });
      setBrief("");
      toast(`Dispatched ${roleLabel(role())} ${a.name}`, "secure");
      props.onSpawned?.(a);
    } catch (err) {
      toast(String(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="mission-dispatch-bar" onSubmit={dispatch}>
      <select class="select mission-select" value={role()} onChange={(e) => setRole(e.currentTarget.value)} aria-label="Agent role">
        <option value="orchestrator">Coordinator (Splits work)</option>
        <option value="worker">Agent (Single worker)</option>
        <option value="reviewer">Reviewer (Audits diffs &amp; PRs)</option>
      </select>
      <select class="select mission-select" value={repo()} onChange={(e) => setRepo(e.currentTarget.value)} aria-label="Target repository">
        <option value="">Any / Scratch Repo</option>
        <For each={repos() ?? []}>{(r) => <option value={r.id} selected={repo() === r.id}>{labels().get(r.id)}</option>}</For>
      </select>
      <input
        type="text"
        class="input mission-input"
        value={brief()}
        disabled={busy()}
        onInput={(e) => setBrief(e.currentTarget.value)}
        placeholder="Describe a task or goal…"
      />
      <Button size="sm" variant="primary" type="submit" busy={busy()} disabled={!brief().trim()}>
        <Icon name="send" size={12} /> Dispatch
      </Button>
    </form>
  );
}

export default function Agents() {
  const store = useStore();
  const list = store.agents;
  const refetch = store.refetchAgents;
  const screenVisible = typeof useScreenVisible === "function" ? useScreenVisible() : () => true;
  const [spawn, setSpawn] = createSignal(null);
  const [pendingAccess, pendingAccessCtrl] = typeof createResource === "function"
    ? createResource(() => rpc("access.pending", {}, { local: true }).catch(() => []))
    : [() => [], { refetch() {} }];
  if (typeof useVisibleEvent === "function") {
    useVisibleEvent("access:requested", () => pendingAccessCtrl.refetch(), { debounceMs: 120 });
    useVisibleEvent("access:changed", () => pendingAccessCtrl.refetch(), { debounceMs: 120 });
  }
  const pendingByAgent = createMemo(() => {
    if (typeof pendingAccessByAgent === "function") return pendingAccessByAgent(pendingAccess());
    const counts = new Map();
    for (const r of pendingAccess() ?? []) {
      if (r?.agent_id && (!r.status || r.status === "pending")) {
        counts.set(r.agent_id, (counts.get(r.agent_id) ?? 0) + 1);
      }
    }
    return counts;
  });
  const needsYou = (a) => (typeof agentNeedsUser === "function"
    ? agentNeedsUser(a, pendingByAgent())
    : a.state === "waiting" || (pendingByAgent().get(a.id) ?? 0) > 0);
  const scoped = createMemo(() => workspaceAgents(list() ?? [], activeRepoId(), store.repos() ?? []));
  // Stopped or failed agents in the active workspace scope: what they found stays in Review and Board after they go.
  const finished = createMemo(() => scoped().filter((a) => a.state === "stopped" || a.state === "failed"));
  async function removeFinished() {
    const targets = finished();
    const ids = targets.map((a) => a.id);
    const names = targets.map((a) => a.name);
    if (!ids.length) return;
    if (!(await confirm(`Remove ${names.length} finished agent${names.length === 1 ? "" : "s"}?`, `${names.join(", ")}. Their folders and saved history go; what they found stays in Review and Board. Agents that are working or waiting for you stay.`, { danger: true, ok: "Remove" }))) return;
    const r = await rpc("agents.remove_many", { scope: "finished", ids }, { action: true }).catch((e) => ({ error: String(e) }));
    if (r.error) return toast(r.error, "bad");
    let removed = r.removed?.length || 0;
    if (r.unsaved?.length) {
      const ok = await confirm(`${r.unsaved.length} of them have unsaved files`, `${r.unsaved.map((u) => u.name).join(", ")} still have changes in their folders that were never committed. Delete them anyway?`, { danger: true, ok: "Delete anyway" });
      if (ok) {
        const r2 = await rpc("agents.remove_many", { scope: "finished", force: true, ids: r.unsaved.map((u) => u.id) }, { action: true }).catch(() => ({}));
        removed += r2.removed?.length || 0;
      }
    }
    toast(`Removed ${removed} agent${removed === 1 ? "" : "s"}${r.kept?.length ? `; kept ${r.kept.join(", ")} (a helper is still working)` : ""}`, "good");
    refetch?.();
  }
  // Picked in the list with ⌘-click (or Ctrl), a range with Shift: removed together.
  const [picked, setPicked] = createSignal([]);
  let anchor = null;
  function pickRow(e, id) {
    const order = tree().map((x) => x.a.id);
    if (e.metaKey || e.ctrlKey) {
      setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
      anchor = id;
    } else if (e.shiftKey && anchor && order.includes(anchor)) {
      const [i, j] = [order.indexOf(anchor), order.indexOf(id)].sort((x, y) => x - y);
      setPicked((p) => [...new Set([...p, ...order.slice(i, j + 1)])]);
    } else {
      setPicked([]);
      anchor = id;
      go("agents", { id });
    }
  }
  async function removePicked() {
    const all = list() || [];
    const allIds = new Set(all.map((a) => a.id));
    const ids = picked().filter((id) => allIds.has(id));
    if (!ids.length) return;
    // Helpers go with their lead; say so before anything is removed.
    const going = new Set(ids);
    for (let grew = true; grew; ) {
      grew = false;
      for (const a of all) if (a.parent_id && going.has(a.parent_id) && !going.has(a.id)) { going.add(a.id); grew = true; }
    }
    const agents = all.filter((a) => going.has(a.id));
    const working = agents.filter((a) => ["running", "provisioning", "waiting"].includes(a.state));
    const helpers = agents.length - ids.length;
    const what = `${agents.map((a) => a.name).join(", ")}.${helpers ? ` That includes ${helpers} helper${helpers === 1 ? "" : "s"} of the ones you picked.` : ""}${working.length ? ` ${working.map((a) => a.name).join(", ")} ${working.length === 1 ? "is" : "are"} working and will be stopped.` : ""} Their folders and saved history go; what they found stays in Review and Board.`;
    if (!(await confirm(`Remove ${agents.length} agent${agents.length === 1 ? "" : "s"}?`, what, { danger: true, ok: "Remove" }))) return;
    const r = await rpc("agents.remove_many", { scope: "selected", ids }, { action: true }).catch((e) => ({ error: String(e) }));
    if (r.error) return toast(r.error, "bad");
    let removed = r.removed?.length || 0;
    if (r.unsaved?.length) {
      const ok = await confirm(`${r.unsaved.length} of them have unsaved files`, `${r.unsaved.map((u) => u.name).join(", ")} still have changes in their folders that were never committed. Delete them anyway?`, { danger: true, ok: "Delete anyway" });
      if (ok) {
        const r2 = await rpc("agents.remove_many", { scope: "selected", force: true, ids: r.unsaved.map((u) => u.id) }, { action: true }).catch(() => ({}));
        removed += r2.removed?.length || 0;
      }
    }
    toast(`Removed ${removed} agent${removed === 1 ? "" : "s"}${r.kept?.length ? `; kept ${r.kept.join(", ")}` : ""}`, "good");
    setPicked([]);
    if (selected() && !(list() || []).some((a) => a.id === selected())) go("agents", {});
    refetch?.();
  }
  const onKey = (e) => {
    if (!screenVisible() || (typeof route === "function" && route() !== "agents")) return;
    if (!picked().length || e.target?.closest?.("input, textarea, select, [contenteditable=true]")) return;
    if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); removePicked(); }
    else if (e.key === "Escape") setPicked([]);
  };
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  }
  const [filter, setFilter] = createSignal("all");
  const [search, setSearch] = createSignal("");
  const savedMode = localStorage.getItem("agents.viewMode");
  const initialMode = savedMode === "grid" || savedMode === "fleet" ? savedMode : "split";
  const [viewMode, setViewModeRaw] = createSignal(initialMode);
  const setViewMode = (m) => { setViewModeRaw(m); localStorage.setItem("agents.viewMode", m); };
  createEffect(() => {
    if (params().focusOwner && params().id) {
      setFilter('all'); setSearch(''); setViewModeRaw('split');
    }
    if (params().spawn) {
      setSpawn(typeof params().spawn === "object" ? params().spawn : {});
    }
  });

  let searchEl;
  const switchAgent = (dir) => {
    const ids = filtered().map((a) => a.id);
    const next = ids[cycle(ids.indexOf(selected()), ids.length, dir)];
    if (next) go("agents", { id: next });
  };
  const focusPane = (dir) => {
    const inputs = [...document.querySelectorAll(".agent-card-input, .conversation textarea")].filter((el) => !el.disabled);
    const el = inputs[cycle(inputs.indexOf(document.activeElement), inputs.length, dir)];
    el?.focus();
  };
  const shortcutActions = {
    "agent.new": () => setSpawn({}),
    "agent.next": () => switchAgent(1),
    "agent.prev": () => switchAgent(-1),
    "agent.split": () => setViewMode("split"),
    "agent.grid": () => setViewMode("grid"),
    "pane.next": () => focusPane(1),
    "pane.prev": () => focusPane(-1),
    "agent.search": () => searchEl?.focus(),
  };
  const onShortcut = (e) => {
    if (!screenVisible() || (typeof route === "function" && route() !== "agents") || spawn()) return;
    const run = shortcutActions[matchShortcut(e)];
    if (run) { e.preventDefault(); run(); }
  };
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onShortcut);
    onCleanup(() => window.removeEventListener("keydown", onShortcut));
  }

  const unregisterCommands = registerCommands([
    { id: "agent.new", label: "New agent or coordinator", kbd: shortcutLabel("agent.new"), run: () => setSpawn({}) },
    { id: "agent.next", label: "Agents: Next agent", kbd: shortcutLabel("agent.next"), run: () => switchAgent(1) },
    { id: "agent.prev", label: "Agents: Previous agent", kbd: shortcutLabel("agent.prev"), run: () => switchAgent(-1) },
    { id: "agent.search", label: "Agents: Search", kbd: shortcutLabel("agent.search"), run: () => searchEl?.focus() },
    { id: "agent.split", label: "Agents: Focus view", kbd: shortcutLabel("agent.split"), run: () => setViewMode("split") },
    { id: "agent.grid", label: "Agents: Grid view", kbd: shortcutLabel("agent.grid"), run: () => setViewMode("grid") },
    { id: "agent.fleet", label: "Agents: Fleet view", run: () => setViewMode("fleet") },
  ]);
  if (typeof onCleanup === "function" && unregisterCommands) onCleanup(unregisterCommands);

  const filtered = createMemo(() => {
    const q = search().trim().toLowerCase();
    return scoped().filter((a) => {
      if (filter() === "live" && !a.live) return false;
      if (filter() === "waiting" && !needsYou(a)) return false;
      if (filter() === "warnings" && !(a.warnings ?? []).length) return false;
      if (q) {
        const roleText = a.role === "orchestrator" ? "coordinator" : a.role === "reviewer" ? "reviewer" : "agent";
        const hay = `${a.name} ${roleText} ${a.role || ""} ${a.repo_name || ""} ${a.branch || ""} ${a.task_title || ""}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  });

  // Tree order: parents first, children indented beneath.
  const tree = createMemo(() => {
    const all = filtered();
    const ids = new Set(all.map((x) => x.id));
    const byParent = new Map();
    for (const a of all) {
      const p = a.parent_id && ids.has(a.parent_id) ? a.parent_id : null;
      if (!byParent.has(p)) byParent.set(p, []);
      byParent.get(p).push(a);
    }
    const out = [];
    const walk = (p, d) => (byParent.get(p) ?? []).forEach((a) => { out.push({ a, d }); walk(a.id, d + 1); });
    walk(null, 0);
    return out;
  });

  const selected = createMemo(() => selectedAgentId(tree().map(({ a }) => a), params().id));

  const telemetry = createMemo(() => {
    const l = scoped();
    let totalIn = 0, totalCached = 0, warnings = 0, clashes = 0;
    const allAlerts = [];
    for (const a of l) {
      totalIn += a.kv?.input_tokens || 0;
      totalCached += a.kv?.cached_tokens || 0;
      for (const w of a.warnings ?? []) {
        warnings++;
        if (w.kind === "clash") clashes++;
        allAlerts.push({ agent: a, warning: w });
      }
    }
    const kvPct = totalIn > 0 ? Math.round((totalCached * 100) / totalIn) : 0;
    return {
      total: l.length,
      live: l.filter((a) => a.live).length,
      waiting: l.filter((a) => needsYou(a)).length,
      strict: l.filter((a) => a.strict).length,
      orchestrators: l.filter((a) => a.role === "orchestrator").length,
      kvPct,
      totalIn,
      totalCached,
      warnings,
      clashes,
      allAlerts,
    };
  });

  const openAgentFocus = (id) => {
    setViewMode("split");
    go("agents", { id });
  };

  const approveAllWaiting = async (clusterId = null) => {
    const waitingAgents = scoped().filter(
      (a) => a.state === "waiting" && (!clusterId || a.cluster_id === clusterId || a.id === clusterId),
    );
    if (typeof confirm === "function") {
      const names = waitingAgents.map((a) => a.name);
      const count = waitingAgents.length || telemetry().waiting;
      const ok = await confirm(
        `Approve ${count} waiting agent${count === 1 ? "" : "s"}?`,
        `Approve pending permission request(s) once for: ${names.join(", ") || "waiting agents"}. Open an agent directly to review or grant session-wide permissions.`,
        { ok: `Approve once (${count})` },
      );
      if (!ok) return;
    }
    try {
      const res = await rpc("agents.batch_permission", {
        decision: "approve",
        scope: "once",
        cluster_id: clusterId,
        repo_id: activeRepoId() || null,
      });
      toast(`Approved ${res?.resolved ?? 0} waiting permission request(s)`, "secure");
      refetch();
    } catch (e) {
      toast(String(e), "danger");
    }
  };

  return (
    <Page title="Agents" flush
      sub={
        <Show when={scoped().length}>
          <span class="fleet-sub-telemetry">
            <span>{telemetry().live}/{telemetry().total} running</span>
            <Show when={telemetry().waiting > 0}>
              <span>· {telemetry().waiting} need you</span>
            </Show>
            <Show when={telemetry().warnings > 0}>
              <span class="warn-text">· {telemetry().warnings} warning{telemetry().warnings === 1 ? "" : "s"}</span>
            </Show>
          </span>
        </Show>
      }
      actions={<div class="row" style={{ gap: "8px", "flex-wrap": "wrap" }}>
        <Show when={finished().length}>
          <Button size="sm" onClick={removeFinished}>Remove finished ({finished().length})</Button>
        </Show>
        <Show when={scoped().length}>
          <input
            ref={searchEl}
            type="search"
            class="input fleet-search"
            placeholder="Filter agents…"
            value={search()}
            onInput={(e) => setSearch(e.currentTarget.value)}
          />
          <Segmented value={filter()} onChange={setFilter} options={[
            { value: "all", label: "All" },
            { value: "live", label: "Running" },
            { value: "waiting", label: `Need you${telemetry().waiting ? ` (${telemetry().waiting})` : ""}` },
            ...(telemetry().warnings > 0 || filter() === "warnings"
              ? [{ value: "warnings", label: `Warnings (${telemetry().warnings})` }]
              : []),
          ]} />
          <Segmented value={viewMode()} onChange={setViewMode} options={[
            { value: "split", label: "Focus" },
            { value: "grid", label: "Grid" },
            { value: "fleet", label: "Fleet" },
          ]} />
        </Show>
        <Button variant="primary" onClick={() => setSpawn({})}><Icon name="plus" size={12} /> New agent</Button>
      </div>}>

      <div class="agents-workspace" style={{ display: "flex", "flex-direction": "column", "min-height": "0" }}>
      <Show when={telemetry().allAlerts.length > 0}>
        <div class="fleet-triage-strip" style={{ flex: "none" }}>
          <Show when={telemetry().waiting > 0}>
            <Button size="sm" variant="primary" onClick={() => approveAllWaiting(null)}>
              ✓ Approve All Waiting ({telemetry().waiting})
            </Button>
          </Show>
          <div class="fleet-triage-alerts">
            <For each={telemetry().allAlerts.slice(0, 6)}>
              {({ agent, warning }) => (
                <button
                  type="button"
                  class={`triage-alert-chip sev-${warning.severity}`}
                  onClick={() => openAgentFocus(agent.id)}
                  title={warning.text}
                >
                  <b>{agent.name}</b>: {warning.text}
                </button>
              )}
            </For>
          </div>
        </div>
      </Show>

      <div class="agents-workspace-content" style={{ display: "flex", flex: "1", "min-height": "0", overflow: "hidden" }}>
      <Show when={scoped().length} fallback={
        <Empty icon={<Icon name="agents" size={28} />} title="No agents in this workspace"
          action={<Button variant="primary" onClick={() => setSpawn({})}>New agent</Button>}>
          Start an agent or coordinator for this workspace.
        </Empty>}>
        <Show when={viewMode() !== "split"} fallback={
          <div class="split" style={{ flex: "1", "min-width": "0" }}>
            <div class={`agent-list ${picked().length ? "picking" : ""}`} role="listbox" aria-multiselectable="true" aria-label="Agents">
              <Show when={picked().length}>
                <div class="picked-bar row" role="toolbar" aria-label="Picked agents">
                  <span class="grow small">{picked().length} picked</span>
                  <Show when={picked().length < tree().length}>
                    <Button size="sm" variant="ghost" onClick={() => setPicked(tree().map((x) => x.a.id))}>All {tree().length}</Button>
                  </Show>
                  <Button size="sm" variant="danger" onClick={removePicked}>Remove</Button>
                  <Button size="sm" variant="ghost" onClick={() => setPicked([])}>Clear</Button>
                </div>
              </Show>
              <For each={tree()}>{({ a, d }) => <AgentRow agent={a} depth={d} selected={a.id === selected()} picked={picked().includes(a.id)} pendingAccess={pendingByAgent().get(a.id) ?? 0} onPick={() => pickRow({ metaKey: true }, a.id)} onClick={(e) => pickRow(e, a.id)} />}</For>
            </div>
            <Show when={selected()}>{(id) => (
              <Show when={filtered().some((a) => a.id === id())} fallback={
                <NotFound title="Agent not found" action={<Button onClick={() => go("agents", {})}>Back to agents</Button>}>
                  This agent was removed or never existed.
                </NotFound>}>
                <AgentView id={id()} agent={filtered().find((a) => a.id === id())} />
              </Show>
            )}</Show>
          </div>
        }>
          <div class="agents-grid-wrap">
            <Show when={tree().length} fallback={<Empty title="No matching agents">Switch the filter above to see all agents.</Empty>}>
              <Show when={viewMode() === "grid"}>
                <div class={`agents-grid count-${Math.min(tree().length, 4)}`}>
                  <For each={tree()}>
                    {({ a }) => (
                      <AgentGridCard
                        agent={a}
                        pendingAccess={pendingByAgent().get(a.id) ?? 0}
                        onOpen={openAgentFocus}
                        onSpawnChild={(parent) => setSpawn({ parent_id: parent.id, role: "worker", repo_id: parent.repo_id })}
                      />
                    )}
                  </For>
                </div>
              </Show>

              <Show when={viewMode() === "fleet"}>
                <div class="fleet-heatmap-grid">
                  <For each={tree()}>
                    {({ a }) => <FleetHeatmapTile agent={a} onOpen={openAgentFocus} />}
                  </For>
                </div>
              </Show>
            </Show>
          </div>
        </Show>
      </Show>
      </div>
      </div>
      <Show when={spawn()}>
        <SpawnModal preset={spawn()} onClose={(a) => { setSpawn(null); if (a) { refetch(); go("agents", { id: a.id }); } }} />
      </Show>
    </Page>
  );
}
