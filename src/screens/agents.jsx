// Agents: every named agent, its live conversation, approvals, terminals and
// changes. Untrusted-provider agents are marked Strict everywhere.
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup } from "solid-js";
import { NotFound, Page, Button, Badge, Dot, Empty, Modal, Field, Input, TextArea, Select, Toggle, Segmented, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { Conversation, Composer, speedEffort, effortSpeed } from "../ui/conversation.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { go, params, registerCommands } from "../lib/nav.js";
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

/** Spawn dialog: name, model (live list), repo, task, brief, Strict. */
export function SpawnModal(props) {
  const [names] = createResource(() => rpc("agents.names"));
  const [models] = createResource(() => rpc("providers.models", {}).catch(() => []));
  const [providers] = createResource(() => rpc("providers.list").catch(() => []));
  const [repos] = createResource(() => rpc("workspace.repos_list").catch(() => []));
  const [tasks] = createResource(() => rpc("board.list", { status: "open" }).catch(() => []));
  const [def] = createResource(() => rpc("providers.default").catch(() => null));
  const [name, setName] = createSignal(props.preset?.name ?? "");
  const [model, setModel] = createSignal("");
  const [repo, setRepo] = createSignal(props.preset?.repo_id ?? "");
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
        name: name().trim() || null, model: chosen() || null, repo_id: repo() || null,
        task_id: task() || null, brief: brief(), strict: strict() || forcedStrict(),
      });
      toast(`${a.name} is starting`, "secure");
      props.onClose(a);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title="New agent" onClose={() => props.onClose(null)} footer={<>
      <Button onClick={() => props.onClose(null)}>Cancel</Button>
      <Button variant="primary" busy={busy()} onClick={submit}>Start agent</Button>
    </>}>
      <div class="grid-2">
        <Field label="Name" hint={`Leave empty for ${names()?.[0] ?? "a free name"}`}>
          <Input value={name()} placeholder={names()?.[0] ?? "lion"} onValue={(v) => setName(v.toLowerCase().replace(/[^a-z0-9-]/g, ""))} />
        </Field>
        <Field label="Model" hint="From your providers, discovered live">
          <Select value={chosen()} onValue={setModel} options={modelOptions()} placeholder="Choose a model" />
        </Field>
        <Field label="Repo" hint="The agent gets its own worktree and branch">
          <Select value={repo()} onValue={setRepo} options={[{ value: "", label: "No repo (scratch only)" }, ...(repos() ?? []).map((r) => ({ value: r.id, label: r.name }))]} />
        </Field>
        <Field label="Task" hint="Optional: link a board task">
          <Select value={task()} onValue={setTask} options={[{ value: "", label: "None" }, ...(tasks() ?? []).map((t) => ({ value: t.id, label: t.title }))]} />
        </Field>
      </div>
      <Field label="Brief" hint="The first message. The agent starts working right away.">
        <TextArea rows={5} value={brief()} onValue={setBrief} placeholder="What should this agent do?" />
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
    <div class={`list-item agent-row ${props.selected ? "on" : ""}`} style={{ "padding-left": `${12 + props.depth * 16}px` }} onClick={props.onClick}>
      <Dot state={stateDot(a().state)} title={stateLabel(a().state)} />
      <div class="grow" style={{ "min-width": 0 }}>
        <div class="row" style={{ gap: "6px" }}>
          <AgentChip name={a().name} />
          <Show when={a().strict}><Badge tone="strict" title="Untrusted provider: sandboxed and fully observed">Strict</Badge></Show>
          <Show when={a().state === "waiting"}><Badge tone="strict">Needs you</Badge></Show>
        </div>
        <div class="xs faint ellipsis">{a().task_title || a().repo_name || a().model_ref}</div>
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
  return (
    <div class="page-body agent-details">
      <div class="grid-2">
        <div><div class="xs faint">Model</div><div class="mono small">{a().model_ref}</div></div>
        <div><div class="xs faint">Security</div>
          <Show when={a().strict} fallback={<Badge tone="secure">Standard: trusted provider</Badge>}>
            <Badge tone="strict">Strict: sandboxed, network allowlist</Badge>
          </Show>
        </div>
        <div><div class="xs faint">Workspace</div><div class="mono xs ellipsis" title={a().cwd ?? a().worktree}>{a().cwd ?? a().worktree ?? "not running"}</div></div>
        <div><div class="xs faint">Branch</div><div class="mono small">{a().branch ?? "none"}</div></div>
        <div><div class="xs faint">Ports</div><div class="mono small">{a().port_base}–{a().port_base + 19}</div></div>
        <div><div class="xs faint">Started</div><div class="small">{fmt.ago(a().created_at)}</div></div>
      </div>
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
        <h3 class="section-title">Helpers</h3>
        <div class="row" style={{ "flex-wrap": "wrap" }}><For each={a().children}>{(c) => <span onClick={() => go("agents", { id: c.id })} style={{ cursor: "pointer" }}><AgentChip name={c.name} /></span>}</For></div>
      </Show>
    </div>
  );
}

function AgentView(props) {
  const [tab, setTab] = createSignal("chat");
  const [events, setEvents] = createSignal([]);
  const [info, { refetch }] = createResource(() => props.id, (id) => rpc("agents.get", { id }));
  let lastSeq = 0;

  const load = async (id) => {
    setEvents([]); lastSeq = 0;
    try {
      const evs = await rpc("agents.events", { id, after: 0, limit: 5000 });
      if (props.id !== id) return;
      setEvents(evs);
      lastSeq = evs.at(-1)?.seq ?? 0;
    } catch { /* not running: empty */ }
  };
  createEffect(on(() => props.id, load));
  useEvent("agents:events", (p) => {
    if (p.id !== props.id) return;
    const fresh = p.events.filter((e) => (e.seq ?? 0) > lastSeq);
    if (!fresh.length) return;
    lastSeq = fresh.at(-1).seq ?? lastSeq;
    setEvents((xs) => xs.concat(fresh));
  });
  useEvent("agents:changed", (p) => { if (p.id === props.id) refetch(); });

  const a = () => info() ?? props.agent;
  const [effort, setEffort] = createSignal("default");
  createEffect(on(() => props.id, (id) => rpc("agents.get_effort", { id }).then(setEffort).catch(() => setEffort("default"))));
  const live = () => a()?.live;
  const busy = () => ["running", "waiting"].includes(a()?.state);

  const send = async (text, images = []) => {
    try { await rpc("agents.send", { id: props.id, text, images }); } catch (e) { toast(String(e), "danger"); }
  };
  const decide = (request_id, decision, scope) =>
    rpc("agents.permission", { id: props.id, request_id, decision, scope }).catch((e) => toast(String(e), "danger"));

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
        <span class="small dim">{stateLabel(a()?.state)}</span>
        <Show when={a()?.strict}><Badge tone="strict"><Icon name="shield" size={11} /> Strict</Badge></Show>
        <span class="mono xs faint ellipsis">{a()?.model_ref}</span>
        <div class="grow" />
        <Segmented value={tab()} onChange={setTab} options={[{ value: "chat", label: "Conversation" }, { value: "terms", label: "Terminals" }, { value: "changes", label: "Changes" }, { value: "info", label: "Details" }]} />
        <Show when={live()} fallback={<Button size="sm" onClick={() => rpc("agents.resume", { id: props.id }).catch((e) => toast(String(e), "danger"))}><Icon name="play" size={11} /> Resume</Button>}>
          <Button size="sm" onClick={() => rpc("agents.stop", { id: props.id })}><Icon name="stop" size={11} /> Stop</Button>
        </Show>
        <Button size="sm" variant="ghost" title="Remove agent and clean up" onClick={remove}><Icon name="trash" size={12} /></Button>
      </div>
      <Show when={a()?.error}><div class="shield danger" style={{ margin: "8px 16px 0" }}>{a().error}</div></Show>
      <Show when={tab() === "chat"}>
        <Conversation events={events()} emptyText={live() ? "Send the first message to start." : "This agent is stopped. Resume it to continue."}
          onApprove={(id, scope) => decide(id, "approve", scope)} onDeny={(id) => decide(id, "deny")} />
        <Composer onSend={send} busy={busy()} disabled={!live()} onCancel={() => rpc("agents.cancel", { id: props.id })}
          speed={effortSpeed(effort())} onSpeed={(v) => rpc("agents.set_effort", { id: props.id, effort: speedEffort(v) }).then(setEffort).catch((e) => toast(String(e), "danger"))}
          placeholder={live() ? `Message ${a()?.name ?? ""}` : "Agent is stopped"} />
      </Show>
      <Show when={tab() === "terms"}><Terminals id={props.id} /></Show>
      <Show when={tab() === "changes"}><Changes id={props.id} /></Show>
      <Show when={tab() === "info" && info()}><Details agent={info()} /></Show>
    </div>
  );
}

export default function Agents() {
  const [list, { refetch }] = createResource(() => rpc("agents.list").catch(() => []));
  const [spawn, setSpawn] = createSignal(null);
  const [filter, setFilter] = createSignal("all");
  let t;
  useEvent("agents:changed", () => { clearTimeout(t); t = setTimeout(refetch, 150); });
  onCleanup(() => clearTimeout(t));

  registerCommands([{ id: "agent.new", label: "New agent", run: () => setSpawn({}) }]);

  // Tree order: parents first, children indented beneath.
  const tree = createMemo(() => {
    const all = (list() ?? []).filter((a) =>
      filter() === "all" ? true : filter() === "live" ? a.live : a.state === "waiting");
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
  const selected = () => params().id ?? tree()[0]?.a.id;
  const counts = createMemo(() => {
    const l = list() ?? [];
    return { live: l.filter((a) => a.live).length, waiting: l.filter((a) => a.state === "waiting").length, strict: l.filter((a) => a.strict).length };
  });

  return (
    <Page title="Agents" flush
      sub={<Show when={(list() ?? []).length}>{`${counts().live} running · ${counts().waiting} need you · ${counts().strict} strict`}</Show>}
      actions={<Button variant="primary" onClick={() => setSpawn({})}><Icon name="plus" size={12} /> New agent</Button>}>
      <Show when={(list() ?? []).length} fallback={
        <Empty icon={<Icon name="agents" size={28} />} title="No agents yet"
          action={<Button variant="primary" onClick={() => setSpawn({})}>Start your first agent</Button>}>
          An agent is a named worker with its own model session, worktree and terminals. Give it a brief and it works on its own, coordinating with other agents through the board.
        </Empty>}>
        <div class="split">
          <div class="agent-list">
            <div style={{ padding: "8px" }}>
              <Segmented value={filter()} onChange={setFilter} options={[{ value: "all", label: "All" }, { value: "live", label: "Running" }, { value: "waiting", label: "Need you" }]} />
            </div>
            <For each={tree()}>{({ a, d }) => <AgentRow agent={a} depth={d} selected={a.id === selected()} onClick={() => go("agents", { id: a.id })} />}</For>
          </div>
          <Show when={selected()} keyed>{(id) => (
            <Show when={list.loading || (list() ?? []).some((a) => a.id === id)} fallback={
              <NotFound title="Agent not found" action={<Button onClick={() => go("agents", {})}>Back to agents</Button>}>
                This agent was removed or never existed.
              </NotFound>}>
              <AgentView id={id} agent={(list() ?? []).find((a) => a.id === id)} />
            </Show>
          )}</Show>
        </div>
      </Show>
      <Show when={spawn()}>
        <SpawnModal preset={spawn()} onClose={(a) => { setSpawn(null); if (a) { refetch(); go("agents", { id: a.id }); } }} />
      </Show>
    </Page>
  );
}
