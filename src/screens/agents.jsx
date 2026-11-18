// Agents: every named agent, its live conversation, approvals, terminals and
// changes. Supports Human -> Orchestrator -> Worker/Reviewer hierarchies,
// shared-prefix KV cache telemetry, AST clash/health warnings, and Fleet Command Grid.
import { workspaceAgents, selectedAgentId, spawnRepoDefault } from "../lib/agent-workspace.js";
import { repoLabels } from "../lib/repo-labels.js";
import { kindResources } from "../lib/kind-resources.js";
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup, Suspense } from "solid-js";
import { NotFound, Page, Button, Badge, Dot, Empty, Modal, Field, Input, TextArea, Select, Toggle, Segmented, ModelPicker, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { Conversation, Composer, KvTelemetryBar, speedEffort, effortSpeed, agentAttachmentLoader } from "../ui/conversation.jsx";
import { AgentTerminals } from "../ui/agent-terminals.jsx";
import { AgentBrowser } from "../ui/agent-browser.jsx";
import { rpc, useEvent, fmt, sendError } from "../lib/rpc.js";
import { go, params, registerCommands } from "../lib/nav.js";
import { useStore, activeRepoId, agentEventsCache } from "../lib/store.js";
import "../styles/agents.css";
import "../styles/code.css";

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

// Stable color per agent name, used for chips across the app.
export function agentHue(name = "") {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}
export function AgentChip(props) {
  return <span class="agent-chip" style={{ "--h": agentHue(props.name) }}>{props.name}</span>;
}

export function RoleBadge(props) {
  const role = () => (props.role || "worker").toLowerCase();
  return (
    <span class={`role-badge role-${role()}`} title={`Role: ${role()}`}>
      {role() === "orchestrator" ? "ORCH" : role() === "reviewer" ? "REV" : "WORK"}
    </span>
  );
}

export function KvBadge(props) {
  const kv = () => props.kv || {};
  const reqs = () => kv().requests || 0;
  const hit = () => kv().cache_hit_pct ?? 0;
  const inp = () => kv().input_tokens || 0;
  const cached = () => kv().cached_tokens || 0;
  const ttft = () => kv().avg_ttft_ms;
  const tone = () => (hit() >= 60 ? "ok" : hit() >= 25 ? "warn" : inp() >= 8000 ? "bad" : "neutral");
  return (
    <Show when={reqs() > 0}>
      <span
        class={`kv-badge kv-${tone()}`}
        title={`KV Cache: ${hit()}% hit (${(cached() / 1000).toFixed(1)}k cached / ${(inp() / 1000).toFixed(1)}k input across ${reqs()} reqs)${ttft() ? ` · avg TTFT ${ttft()}ms` : ""}`}
      >
        KV {hit()}%
      </span>
    </Show>
  );
}

export function WarningPills(props) {
  const warnings = () => props.warnings ?? [];
  return (
    <Show when={warnings().length}>
      <div class="agent-warnings">
        <For each={warnings().slice(0, props.max ?? 3)}>
          {(w) => (
            <span class={`warn-pill warn-${w.severity || "med"} warn-kind-${w.kind}`} title={w.text}>
              {w.kind === "clash" ? "⚡ Clash" :
               w.kind === "kv_cache" ? "❄ Cold KV" :
               w.kind === "guard" ? "🛡 Guard" :
               w.kind === "rate_limit" ? "⏳ 429" :
               w.kind === "waiting" ? "✋ Approval" :
               "⚠ Alert"}
              <Show when={!props.compact}><span class="warn-pill-detail">{w.text}</span></Show>
            </span>
          )}
        </For>
      </div>
    </Show>
  );
}

/** Spawn dialog: name, role, model (live list), repo, task, brief, Strict. */
export function SpawnModal(props) {
  const store = useStore();
  const [names] = createResource(() => rpc("agents.names"));
  const models = store.models;
  const providers = store.providers;
  const repos = store.repos;
  const labels = createMemo(() => repoLabels(repos() ?? []));
  const [tasks] = createResource(() => rpc("board.list", { status: "open" }).catch(() => []));
  const def = store.defaultModel;
  const [name, setName] = createSignal(props.preset?.name ?? "");
  const [role, setRole] = createSignal(props.preset?.role ?? "worker");
  const [model, setModel] = createSignal("");
  const [repo, setRepo] = createSignal(spawnRepoDefault(props.preset, activeRepoId()));
  const [task, setTask] = createSignal(props.preset?.task_id ?? "");
  const [brief, setBrief] = createSignal(props.preset?.brief ?? "");
  const [strict, setStrict] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  const provById = createMemo(() => Object.fromEntries((providers() ?? []).map((p) => [p.id, p])));
  const modelOptions = createMemo(() =>
    (models() ?? [])
      .filter((m) => m.enabled !== 0 && provById()[m.provider_id]?.enabled !== false)
      .map((m) => {
        const p = provById()[m.provider_id];
        const tag = p && !p.trusted ? "  (untrusted, Strict)" : "";
        return { value: m.ref, label: `${m.provider_name ?? m.provider_id} / ${m.display || m.model_id}${tag}` };
      }));
  const chosen = () => model() || def() || "";
  const chosenProvider = () => provById()[(chosen() || "").split("/")[0]];
  const forcedStrict = () => chosenProvider() && !chosenProvider().trusted;
  const isSub = () => ["chatgpt_oauth", "claude_oauth"].includes(chosenProvider()?.kind);

  createEffect(on(task, (t) => {
    const tk = (tasks() ?? []).find((x) => x.id === t);
    if (tk && !brief()) setBrief(`Work on board task "${tk.title}" (id ${tk.id}). Claim it first, follow the playbook, open a PR when done.\n\n${tk.detail ?? ""}`.trim());
    if (tk?.repo_id && !repo()) setRepo(tk.repo_id);
  }));

  const submit = async () => {
    setBusy(true); setErr("");
    try {
      const a = await rpc("agents.spawn", {
        name: name().trim() || null,
        role: role() || "worker",
        parent_id: props.preset?.parent_id || null,
        model: chosen() || null,
        repo_id: repo() || null,
        task_id: task() || null,
        brief: brief(),
        strict: strict() || forcedStrict(),
      });
      toast(`${a.name} (${role()}) is starting`, "secure");
      props.onClose(a);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title="New agent" onClose={() => props.onClose(null)} footer={<>
      <Button onClick={() => props.onClose(null)}>Cancel</Button>
      <Button variant="primary" busy={busy()} onClick={submit}>Start Agent</Button>
    </>}>
      <div class="grid-2">
        <Field label="Name" hint={`Leave empty for ${names()?.[0] ?? "a free name"}`}>
          <Input value={name()} placeholder={names()?.[0] ?? "lion"} onValue={(v) => setName(v.toLowerCase().replace(/[^a-z0-9-]/g, ""))} />
        </Field>
        <Field label="Role" hint="Standard agent or multi-agent orchestrator">
          <Select value={role()} onValue={setRole} options={[
            { value: "worker", label: "Agent (Standard)" },
            { value: "orchestrator", label: "Orchestrator (Spawns sub-agents)" },
            { value: "reviewer", label: "Reviewer (Audits diffs & PRs)" },
          ]} />
        </Field>
        <Field label="Model" hint="From your providers, discovered live">
          <ModelPicker
            value={chosen()}
            onValue={setModel}
            models={models() ?? []}
            providers={providers() ?? []}
            allowUntrusted={true}
            placeholder="Choose a model"
          />
        </Field>
        <Field label="Repo" hint="The agent gets its own worktree and branch">
          <Select value={repo()} onValue={id => { setTask(""); setRepo(id); }} options={[{ value: "", label: "No repo (scratch only)" }, ...(repos() ?? []).map((r) => ({ value: r.id, label: labels().get(r.id) }))]} />
        </Field>
        <Field label="Task" hint="Optional: link a board task">
          <Select value={task()} onValue={setTask} options={[{ value: "", label: "None" }, ...(tasks() ?? []).filter(t => (t.repo_id ?? "") === repo()).map((t) => ({ value: t.id, label: t.title }))]} />
        </Field>
      </div>
      <Field label="Mission Brief" hint="The first instruction. Orchestrators will automatically plan, spawn workers on disjoint files, and synthesize results.">
        <TextArea rows={5} value={brief()} onValue={setBrief} placeholder="Describe the mission, target PR, or task..." />
      </Field>
      <div class="row" style={{ "margin-top": "8px" }}>
        <Toggle tone="strict" on={strict() || forcedStrict()} disabled={forcedStrict() || isSub()} onChange={setStrict} label="Strict mode" />
        <span class="small dim">
          <Show when={forcedStrict()} fallback={<Show when={isSub()} fallback="Sandboxed: own worktree only, network only to allowed hosts.">Subscriptions are trusted and run in Standard mode.</Show>}>
            Required: this provider is untrusted. Sandboxed, every request recorded.
          </Show>
        </span>
      </div>
      <Show when={err()}><div class="err small" style={{ "margin-top": "8px" }}>{err()}</div></Show>
    </Modal>
  );
}

function AgentRow(props) {
  const a = () => props.agent;
  return (
    <div class={`list-item agent-row ${props.selected ? "on" : ""}`} style={{ "padding-left": `${12 + props.depth * 14}px` }} onClick={props.onClick}>
      <Dot state={stateDot(a().state)} title={stateLabel(a().state)} />
      <div class="grow" style={{ "min-width": 0 }}>
        <div class="row" style={{ gap: "6px" }}>
          <AgentChip name={a().name} />
          <Show when={a().state === "waiting"}><Badge tone="strict">Needs you</Badge></Show>
          <Show when={(a().warnings ?? []).some((w) => w.kind === "clash")}><Badge tone="danger">Clash</Badge></Show>
        </div>
        <div class="xs faint ellipsis">{a().task_title || a().repo_name || a().branch || (a().model_ref ?? "").split("/").pop()}</div>
      </div>
      <span class="xs faint">{fmt.ago(a().last_active_at)}</span>
    </div>
  );
}

function Terminals(props) {
  const [list, { refetch }] = createResource(() => props.id, (id) => rpc("terminals.list").then((t) => t.filter((x) => x.agent_id === id)).catch(() => []));
  useEvent("terminals:exit", () => refetch());
  useEvent("terminals:changed", () => refetch());
  return (
    <div class="page-body">
      <Show when={(list() ?? []).length} fallback={<Empty title="No commands yet">Every command this agent runs opens a visible terminal here.</Empty>}>
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
        <div class="xs faint">Symbols this agent changed, and code that uses them. "name match" means same name only, not confirmed.</div>
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
  return (
    <div class="page-body">
      <div class="row" style={{ "margin-bottom": "8px" }}>
        <span class="small dim grow">Changes in this agent's worktree, compared with where it started.</span>
        <Button size="sm" onClick={refetch}><Icon name="refresh" size={12} /> Refresh</Button>
      </div>
      <Impact id={props.id} />
      <Show when={diff()?.error}><div class="err small">{diff().error}</div></Show>
      <Show when={diff() && !diff().error && !(diff().patch || diff().stat)}><Empty title="No changes yet" /></Show>
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
        <div><div class="xs faint">Role & Cluster</div><div class="row gap-4"><RoleBadge role={a().role} /><span class="small">{a().cluster_name ? `Cluster ${a().cluster_name}` : "Root"}</span></div></div>
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

      <h3 class="section-title">Kubernetes-in-Docker (kind) Test Clusters</h3>
      <div class="card" style={{ padding: "12px", "margin-bottom": "12px" }}>
        <div class="row" style={{ "margin-bottom": "8px", "flex-wrap": "wrap" }}>
          <span class="small dim grow">
            Spin up an isolated <code class="mono">kind</code> Kubernetes cluster pre-mapped to this agent's port range (<code class="mono">:{a().port_base + 10}</code> HTTP, <code class="mono">:{a().port_base + 11}</code> HTTPS) and registered with the Reaper for automatic teardown.
          </span>
          <Button size="sm" variant="primary" disabled={!canChangeClusters()} busy={kindBusy()} onClick={createKind}>
            <Icon name="plus" size={11} /> Create kind Cluster
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
        <h3 class="section-title">Sub-Agents ({a().children.length})</h3>
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
  const [tab, setTab] = createSignal("chat");
  const [aside, setAsideRaw] = createSignal(localStorage.getItem("agent.aside") ?? "none");
  const setAside = (v) => { setAsideRaw(v); localStorage.setItem("agent.aside", v); };
  const [asideW, setAsideW] = createSignal(Number(localStorage.getItem("agent.aside.w")) || 520);
  const [browserPing, setBrowserPing] = createSignal(false);
  createEffect(on(aside, (v) => { if (v === "browser") setBrowserPing(false); }));
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
  const [info, { refetch }] = createResource(() => props.id, (id) => rpc("agents.get", { id }));
  const models = store.models;
  const providers = store.providers;
  const [historyLoading, setHistoryLoading] = createSignal(false);
  const [historyError, setHistoryError] = createSignal("");
  const [historyCursor, setHistoryCursor] = createSignal(0);
  const [historyMore, setHistoryMore] = createSignal(false);
  let retryAfter = 0;
  let generation = 0, historyId;
  const merge = (previous, incoming) => {
    const bySeq = new Map(previous.map((event) => [event.seq, event]));
    for (const event of incoming) if (!bySeq.has(event.seq)) bySeq.set(event.seq, event);
    return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  };
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
  useEvent("agents:events", (p) => {
    if (p.id !== props.id || historyId !== props.id) return;
    setEvents((current) => {
      const next = merge(current, p.events ?? []);
      agentEventsCache.set(props.id, next);
      return next;
    });
  });
  useEvent("agents:changed", (p) => { if (p.id === props.id) refetch(); });

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
      await rpc("agents.set_model", { id: props.id, model: newRef });
      rpc("agents.get_effort", { id: props.id }).then((v) => setEffort(v || "default")).catch(() => {});
      toast(`Switched ${a()?.name ?? "agent"} model to ${newRef} (0 tokens used)`, "secure");
      refetch();
      return true;
    } catch (e) {
      toast(String(e), "danger");
      return false;
    }
  };

  const handleSlashModel = async (query) => {
    if (!query) {
      toast(`Current model: ${a()?.model_ref} (use /model <name> or click the model picker in the header)`, "info");
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

  const send = async (text, images = []) => {
    try { await rpc("agents.send", { id: props.id, text, images }, { action: true }); return true; }
    catch (e) { toast(sendError(e), "danger"); return false; }
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
        />
        <Show when={a()?.strict}><Badge tone="strict"><Icon name="shield" size={11} /> Strict</Badge></Show>
        <Show when={a()?.parent_id}>
          <span class="xs faint" style={{ cursor: "pointer" }} onClick={() => go("agents", { id: a().parent_id })}>
            ↳ lead: <b>{a()?.parent_name || "parent"}</b>
          </span>
        </Show>
        <Show when={a()?.branch}>
          <span class="mono xs faint ellipsis">{`${a()?.repo_name || ""}#${a()?.branch}`}</span>
        </Show>
        <div class="grow" />
        <Segmented value={tab()} onChange={setTab} options={[{ value: "chat", label: "Conversation" }, { value: "terms", label: "Commands" }, { value: "changes", label: "Changes" }, { value: "info", label: "Details" }]} />
        <Segmented value={aside()} onChange={setAside} options={[
          { value: "terminal", label: <span class="row gap-4" title="Open the agent's live terminal alongside its conversation"><Icon name="terminal" size={12} /> Live shell</span> },
          { value: "browser", label: <span class="row gap-4" title="The agent's live browser"><Icon name="globe" size={12} /> Browser<Show when={browserPing()}><span class="ping" /></Show></span> },
          { value: "none", label: <span title="Focus on the conversation and inline command output">Focus</span> },
        ]} />
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
          <span class="xs faint">Sub-agents ({a().children.length}):</span>
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
      <Show when={a()?.error}><div class="shield danger" style={{ margin: "8px 16px 0" }}>{a().error}</div></Show>
      <Show when={(a()?.warnings ?? []).length}>
        <div style={{ padding: "6px 16px 0" }}><WarningPills warnings={a().warnings} max={6} /></div>
      </Show>
      <div class="agent-body">
        <div class="agent-main" ref={agentMain}>
          <Show when={tab() === "chat"}>
            <Show when={historyLoading()}><div class="history-loading small dim" role="status">Loading saved conversation…</div></Show>
            <Show when={historyError()}><div class="history-error shield danger" role="alert">Unable to load saved conversation: {historyError()} <Button size="sm" onClick={() => load(props.id, retryAfter)}>Retry history</Button></div></Show>
            <Show when={historyMore()}><div class="history-more small dim" role="status">More saved history is available. Showing loaded pages only. <Button size="sm" disabled={historyLoading()} onClick={() => load(props.id, historyCursor())}>Load more history</Button></div></Show>
            <Conversation events={events()} busy={busy()} readOnly={!live()} loadAttachment={agentAttachmentLoader(props.id)} emptyText={historyLoading() ? "Loading saved conversation…" : historyError() ? "History could not be loaded. Retry above." : live() ? "Send the first message to start." : "No saved messages. This agent is stopped. Resume it to continue."}
              onApprove={(id, scope) => decide(id, "approve", scope)} onDeny={(id) => decide(id, "deny")} />
            <Composer draftKey={`agent:${props.id}`} dropTarget={() => agentMain} onSend={send} busy={busy()} disabled={!live()} onCancel={() => rpc("agents.cancel", { id: props.id }, { action: true }).catch((e) => toast(String(e), "danger"))}
              effort={effort()} speed={effortSpeed(effort())} effortOptions={activeModel()?.effort_details ?? []} effortPending={effortPending()} onEffort={changeEffort} onSpeed={changeEffort} onSlashModel={handleSlashModel}
              placeholder={live() ? `Message ${a()?.name ?? ""} (or /effort, /model)` : "Agent is stopped"} />
          </Show>
          <Show when={tab() === "terms"}><Terminals id={props.id} /></Show>
          <Show when={tab() === "changes"}><Changes id={props.id} /></Show>
          <Show when={tab() === "info" && info()}><Details agent={info()} onRefetch={refetch} /></Show>
        </div>
        <div class="agent-aside" classList={{ hidden: aside() === "none" }} style={{ width: `${asideW()}px` }}>
            <div class="aside-grip" onPointerDown={drag} title="Drag to resize" />
            <div class="aside-body" classList={{ hidden: aside() !== "terminal" }}>
              <Suspense fallback={<div class="small dim" role="status">Loading terminals…</div>}>
                <AgentTerminals id={props.id} name={a()?.name ?? ""} live={live()} />
              </Suspense>
            </div>
            <div class="aside-body" classList={{ hidden: aside() !== "browser" }}>
              <Suspense fallback={<div class="small dim" role="status">Loading browser…</div>}>
                <AgentBrowser agent={props.id} name={a()?.name} strict={a()?.strict} provider={a()?.model_ref?.split("/")[0]} visible={aside() === "browser"} onActivity={() => setBrowserPing(true)} />
              </Suspense>
            </div>
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

  const merge = (previous, incoming) => {
    const bySeq = new Map(previous.map((event) => [event.seq, event]));
    for (const event of incoming) if (!bySeq.has(event.seq)) bySeq.set(event.seq, event);
    return [...bySeq.values()].sort((x, y) => x.seq - y.seq);
  };

  const load = async (id) => {
    const req = ++generation;
    try {
      const evs = await rpc("agents.events", { id, after: 0, limit: 250 }, { local: true });
      if (req === generation && Array.isArray(evs)) setEvents((cur) => merge(cur, evs));
    } catch {}
  };

  onCleanup(() => generation++);
  createEffect(on(() => a()?.id, (id) => { if (id) load(id); }));
  useEvent("agents:events", (p) => {
    if (p.id !== a()?.id) return;
    setEvents((cur) => merge(cur, p.events ?? []));
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
      await rpc("agents.send", { id: a().id, text: msg, images: [] }, { action: true });
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
      class={`agent-card role-card-${a()?.role || "worker"} ${a()?.state === "waiting" ? "needs-you" : hasClash() ? "has-clash" : busy() ? "active" : ""}`}
      data-agent-card={a()?.name}
    >
      <div class="agent-card-head" onClick={() => props.onOpen?.(a().id)}>
        <Dot state={stateDot(a()?.state)} title={stateLabel(a()?.state)} />
        <AgentChip name={a()?.name} />
        <RoleBadge role={a()?.role} />
        <span class="xs dim">{stateLabel(a()?.state)}</span>
        <KvBadge kv={a()?.kv} />
        <Show when={a()?.strict}><Badge tone="strict">Strict</Badge></Show>
        <Show when={a()?.state === "waiting"}><Badge tone="strict">Needs you</Badge></Show>
        <Show when={a()?.parent_name}><span class="xs faint" title={`Sub-agent of ${a().parent_name}`}>↳ {a().parent_name}</span></Show>
        <Show when={(a()?.children_count ?? 0) > 0}><span class="xs faint" title="Spawned sub-agents">◈ {a().children_count}</span></Show>
        <span class="xs faint ellipsis grow" title={a()?.task_title || a()?.branch || a()?.model_ref}>
          {a()?.repo_name ? `${a().repo_name}${a().branch ? `@${a().branch}` : ""}` : (a()?.task_title || a()?.branch || a()?.model_ref)}
        </span>
        <span class="mono xs faint">{(a()?.model_ref ?? "").split("/").pop()}</span>
        <div class="row gap-4" onClick={(e) => e.stopPropagation()}>
          <Show when={a()?.role === "orchestrator"}>
            <Button size="sm" variant="ghost" onClick={() => props.onSpawnChild?.(a())} title="Spawn worker under this orchestrator">
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
      <Show when={(a()?.warnings ?? []).length}>
        <div class="agent-card-warnings">
          <WarningPills warnings={a().warnings} max={3} />
        </div>
      </Show>
      <Show when={a()?.error}><div class="shield danger xs" style={{ margin: "6px 10px 0" }}>{a().error}</div></Show>
      <div class="agent-card-body">
        <Conversation
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
      title={`${a()?.name} (${a()?.role}) · ${stateLabel(a()?.state)}${a()?.repo_name ? ` · ${a().repo_name}` : ""}${a()?.branch ? `@${a().branch}` : ""}`}
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

/** Quick Mission Dispatch Bar: 1-line launch for an Orchestrator cluster or Worker. */
function MissionDispatchBar(props) {
  const [repos] = createResource(() => rpc("workspace.repos_list").catch(() => []));
  const labels = createMemo(() => repoLabels(repos() ?? []));
  const [repoId, setRepoId] = createSignal(activeRepoId());
  createEffect(on(activeRepoId, id => setRepoId(id), { defer: true }));
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
        repo_id: repoId() || null,
        brief: text,
      });
      setBrief("");
      toast(`Dispatched ${role()} ${a.name}`, "secure");
      props.onSpawned?.(a);
    } catch (err) {
      toast(String(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="mission-dispatch-bar" onSubmit={dispatch}>
      <span class="mission-dispatch-tag">HUMAN → ORCHESTRATOR</span>
      <select class="select mission-select" value={role()} onChange={(e) => setRole(e.currentTarget.value)} aria-label="Agent hierarchy role">
        <option value="orchestrator">Orchestrator (Cluster Lead)</option>
        <option value="worker">Worker (Single Task)</option>
        <option value="reviewer">Reviewer (PR / Diff Audit)</option>
      </select>
      <select class="select mission-select" value={repoId()} onChange={(e) => setRepoId(e.currentTarget.value)} aria-label="Target repository">
        <option value="">Any / Scratch Repo</option>
        <For each={repos() ?? []}>{(r) => <option value={r.id} selected={repoId() === r.id}>{labels().get(r.id)}</option>}</For>
      </select>
      <input
        type="text"
        class="input mission-input"
        value={brief()}
        disabled={busy()}
        onInput={(e) => setBrief(e.currentTarget.value)}
        placeholder="Give a high-level directive (e.g. 'Decompose auth refactor across workers, verify tests, and render a Mermaid architecture diagram')…"
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
  const [spawn, setSpawn] = createSignal(null);
  const [filter, setFilter] = createSignal("all");
  const [search, setSearch] = createSignal("");
  const [viewMode, setViewModeRaw] = createSignal(localStorage.getItem("agents.viewMode") || "split");
  const setViewMode = (m) => { setViewModeRaw(m); localStorage.setItem("agents.viewMode", m); };
  createEffect(() => {
    if (params().focusOwner && params().id) {
      setFilter('all'); setSearch(''); setViewModeRaw('split');
    }
  });

  registerCommands([
    { id: "agent.new", label: "New agent / orchestrator", run: () => setSpawn({}) },
    { id: "agent.clusters", label: "Agents: Cluster hierarchy view (Orchestrator -> Workers)", run: () => setViewMode("clusters") },
    { id: "agent.grid", label: "Agents: Grid view (all agents side-by-side)", run: () => setViewMode("grid") },
    { id: "agent.fleet", label: "Agents: Fleet heatmap view (100+ agents)", run: () => setViewMode("fleet") },
    { id: "agent.split", label: "Agents: Focus view (single agent + shell/browser)", run: () => setViewMode("split") },
  ]);

  const scoped = createMemo(() => workspaceAgents(list() ?? [], activeRepoId(), store.repos() ?? []));
  const filtered = createMemo(() => {
    const q = search().trim().toLowerCase();
    return scoped().filter((a) => {
      if (filter() === "live" && !a.live) return false;
      if (filter() === "waiting" && a.state !== "waiting") return false;
      if (filter() === "warnings" && !(a.warnings ?? []).length) return false;
      if (filter() === "orchestrators" && a.role !== "orchestrator") return false;
      if (q) {
        const hay = `${a.name} ${a.role} ${a.repo_name || ""} ${a.branch || ""} ${a.task_title || ""} ${a.cluster_name || ""}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  });

  // Tree order: parents first, children indented beneath.
  const tree = createMemo(() => {
    const all = filtered();
    const byParent = new Map();
    for (const a of all) {
      const p = all.some((x) => x.id === a.parent_id) ? a.parent_id : null;
      if (!byParent.has(p)) byParent.set(p, []);
      byParent.get(p).push(a);
    }
    const out = [];
    const walk = (p, d) => (byParent.get(p) ?? []).forEach((a) => { out.push({ a, d }); walk(a.id, d + 1); });
    walk(null, 0);
    return out;
  });

  // Group into Clusters (each root Orchestrator or standalone agent forms a cluster)
  const clusters = createMemo(() => {
    const groups = new Map();
    for (const { a, d } of tree()) {
      const cid = a.cluster_id || a.id;
      if (!groups.has(cid)) {
        groups.set(cid, {
          id: cid,
          name: a.cluster_name || a.name,
          repo: a.repo_name,
          lead: a,
          members: [],
        });
      }
      const g = groups.get(cid);
      if (a.id === cid) g.lead = a;
      g.members.push({ a, d });
    }
    return [...groups.values()];
  });

  const selected = () => selectedAgentId(tree().map(({ a }) => a), params().id);

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
      waiting: l.filter((a) => a.state === "waiting").length,
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
    try {
      const res = await rpc("agents.batch_permission", {
        decision: "approve",
        scope: "session",
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
        <Show when={scoped().length}>
          <input
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
          ]} />
          <Segmented value={viewMode()} onChange={setViewMode} options={[
            { value: "split", label: <span class="row gap-4" title="Single agent with live terminal & browser"><Icon name="agents" size={12} /> Focus</span> },
            { value: "grid", label: <span class="row gap-4" title="Watch and control all agents side-by-side"><Icon name="grid" size={12} /> Grid</span> },
            { value: "clusters", label: <span class="row gap-4" title="Grouped by cluster"><Icon name="board" size={12} /> Clusters</span> },
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
          action={<Button variant="primary" onClick={() => setSpawn({})}>Start your first agent</Button>}>
          Create an agent for the selected workspace, or switch workspaces to see its agents.
        </Empty>}>
        <Show when={viewMode() !== "split"} fallback={
          <div class="split" style={{ flex: "1", "min-width": "0" }}>
            <div class="agent-list">
              <For each={tree()}>{({ a, d }) => <AgentRow agent={a} depth={d} selected={a.id === selected()} onClick={() => go("agents", { id: a.id })} />}</For>
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
              <Show when={viewMode() === "clusters"}>
                <div class="clusters-stack">
                  <For each={clusters()}>
                    {(cl) => (
                      <section class="cluster-section">
                        <header class="cluster-header">
                          <span class="cluster-badge">CLUSTER</span>
                          <AgentChip name={cl.name} />
                          <RoleBadge role={cl.lead?.role} />
                          <Show when={cl.repo}><span class="mono xs dim">{cl.repo}</span></Show>
                          <span class="xs faint">{cl.members.filter((m) => m.a.live).length}/{cl.members.length} live</span>
                          <KvBadge kv={cl.lead?.kv} />
                          <div class="grow" />
                          <Show when={cl.members.some((m) => m.a.state === "waiting")}>
                            <Button size="sm" variant="primary" onClick={() => approveAllWaiting(cl.id)}>
                              Approve Cluster
                            </Button>
                          </Show>
                          <Button size="sm" variant="ghost" onClick={() => setSpawn({ parent_id: cl.id, role: "worker", repo_id: cl.lead?.repo_id })}>
                            <Icon name="plus" size={11} /> Add Worker
                          </Button>
                          <Show when={cl.members.some((m) => m.a.live)}>
                            <Button size="sm" variant="ghost" onClick={() => rpc("agents.stop_cluster", { cluster_id: cl.id }).then(refetch)}>
                              Stop Cluster
                            </Button>
                          </Show>
                        </header>
                        <div class={`agents-grid count-${Math.min(cl.members.length, 4)}`}>
                          <For each={cl.members}>
                            {({ a }) => (
                              <AgentGridCard
                                agent={a}
                                onOpen={openAgentFocus}
                                onSpawnChild={(parent) => setSpawn({ parent_id: parent.id, role: "worker", repo_id: parent.repo_id })}
                              />
                            )}
                          </For>
                        </div>
                      </section>
                    )}
                  </For>
                </div>
              </Show>

              <Show when={viewMode() === "grid"}>
                <div class={`agents-grid count-${Math.min(tree().length, 4)}`}>
                  <For each={tree()}>
                    {({ a }) => (
                      <AgentGridCard
                        agent={a}
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
