import { Show, createSignal, createResource, createMemo, createEffect, on } from "solid-js";
import { Modal, Button, Field, Input, TextArea, Select, Toggle, ModelPicker, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { useStore, activeRepoId } from "../lib/store.js";
import { spawnRepoDefault } from "../lib/agent-workspace.js";
import { repoLabels } from "../lib/repo-labels.js";
import { useVisibleEvent } from "../lib/visibility.js";

/** Spawn dialog: name, role, model (live list), repo, task, brief, Strict. */
export function SpawnModal(props) {
  const store = useStore();
  const [names] = createResource(() => rpc("agents.names"));
  const models = store.models;
  const providers = store.providers;
  const repos = store.repos;
  const labels = createMemo(() => repoLabels(repos() ?? []));
  const [tasks, { refetch: refetchTasks }] = createResource(() => rpc("board.list", { status: "open", visible_only: true }).catch(() => []));
  useVisibleEvent("board:changed", () => refetchTasks(), { debounceMs: 200 });
  const def = store.defaultModel;
  const [name, setName] = createSignal(props.preset?.name ?? "");
  const [role, setRole] = createSignal(props.preset?.role ?? "worker");
  const [model, setModel] = createSignal("");
  const [repo, setRepo] = createSignal(spawnRepoDefault(props.preset, activeRepoId()));
  const [task, setTask] = createSignal(props.preset?.task_id ?? "");
  const [brief, setBrief] = createSignal(props.preset?.brief ?? "");
  const [strict, setStrict] = createSignal(false);
  const [showOptions, setShowOptions] = createSignal(
    Boolean(props.preset?.name || (props.preset?.role && props.preset.role !== "worker") || props.preset?.task_id),
  );
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  const provById = createMemo(() => Object.fromEntries((providers() ?? []).map((p) => [p.id, p])));
  const chosen = () => model() || def() || "";
  const chosenProvider = () => provById()[(chosen() || "").split("/")[0]];
  const forcedStrict = () => chosenProvider() && !chosenProvider().trusted;
  const isSub = () => ["chatgpt_oauth", "claude_oauth", "antigravity_oauth"].includes(chosenProvider()?.kind);

  createEffect(on(task, (t) => {
    const tk = (tasks() ?? []).find((x) => x.id === t);
    if (tk && !brief()) setBrief(`Work on board task "${tk.title}" (id ${tk.id}). Claim it first, follow the playbook, open a PR when done.\n\n${tk.detail ?? ""}`.trim());
    if (tk?.repo_id && !repo()) setRepo(tk.repo_id);
  }));

  const roleLabel = (r) => (r === "orchestrator" ? "Coordinator" : r === "reviewer" ? "Reviewer" : "Agent");

  const submit = async (e) => {
    e?.preventDefault?.();
    if (busy()) return;
    if (role() === "orchestrator" && !repo()) { setErr("Choose a workspace so the coordinator can create tasks and helpers."); return; }
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
      toast(`${a.name} (${roleLabel(role())}) is starting`, "secure");
      props.onClose(a);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title="New agent" onClose={() => props.onClose(null)} footer={<>
      <span class="xs faint grow">Press <span class="kbd">⌘↵</span> to start</span>
      <Button onClick={() => props.onClose(null)}>Cancel</Button>
      <Button variant="primary" busy={busy()} onClick={submit}>Start agent</Button>
    </>}>
      <form
        onSubmit={submit}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
        style={{ display: "grid", gap: "14px" }}
      >
        <Field label="Brief" hint="Describe the goal or task. Leave blank to start an interactive session.">
          <TextArea autofocus rows={4} value={brief()} onValue={setBrief} placeholder="What should this agent work on?" />
        </Field>
        <div class="grid-2">
          <Field label="Workspace" hint="Gets an isolated worktree and branch">
            <Select value={repo()} onValue={id => { setTask(""); setRepo(id); }} options={[{ value: "", label: "No repo (scratch only)" }, ...(repos() ?? []).map((r) => ({ value: r.id, label: labels().get(r.id) }))]} />
            <Show when={repos.state === "ready" && (repos() ?? []).length === 0}>
              <div class="row" style={{ "margin-top": "4px" }}>
                <Button size="sm" onClick={() => { props.onClose(null); go("repos"); }}>
                  <Icon name="repo" size={11} /> Add repo
                </Button>
              </div>
            </Show>
          </Field>
          <Field label="Model" hint="Discovered from enabled providers">
            <ModelPicker
              value={chosen()}
              onValue={setModel}
              models={models() ?? []}
              providers={providers() ?? []}
              allowUntrusted={true}
              placeholder="Choose a model"
            />
            <Show when={providers.state === "ready" && (providers() ?? []).length === 0}>
              <div class="row" style={{ "margin-top": "4px" }}>
                <Button size="sm" onClick={() => { props.onClose(null); go("providers"); }}>
                  <Icon name="key" size={11} /> Add provider
                </Button>
              </div>
            </Show>
          </Field>
        </div>
        <div style={{ "border-top": "1px solid var(--line)", "padding-top": "10px" }}>
          <button
            type="button"
            class="row"
            style={{ width: "100%", border: 0, background: "transparent", padding: "2px 0", cursor: "pointer", color: "var(--text-dim)" }}
            aria-expanded={showOptions() || forcedStrict()}
            onClick={() => setShowOptions((v) => !v)}
          >
            <Icon name={showOptions() || forcedStrict() ? "chevronDown" : "chevronRight"} size={12} />
            <span class="small" style={{ "font-weight": "500" }}>Options</span>
            <span class="xs faint grow" style={{ "text-align": "right" }}>
              {roleLabel(role())} · {name().trim() || names()?.[0] || "auto name"}{strict() || forcedStrict() ? " · Strict" : ""}
            </span>
          </button>
          <Show when={showOptions() || forcedStrict()}>
            <div style={{ display: "grid", gap: "12px", "margin-top": "12px" }}>
              <div class="grid-2">
                <Field label="Role" hint="Single agent, coordinator, or reviewer">
                  <Select value={role()} onValue={setRole} options={[
                    { value: "worker", label: "Agent (Single worker)" },
                    { value: "orchestrator", label: "Coordinator (Splits work across helpers)" },
                    { value: "reviewer", label: "Reviewer (Audits diffs & PRs)" },
                  ]} />
                </Field>
                <Field label="Name" hint={`Optional (defaults to ${names()?.[0] ?? "auto"})`}>
                  <Input value={name()} placeholder={names()?.[0] ?? "lion"} onValue={(v) => setName(v.toLowerCase().replace(/[^a-z0-9-]/g, ""))} />
                </Field>
              </div>
              <Field label="Board task" hint="Optional open task to attach">
                <Select value={task()} onValue={setTask} options={[{ value: "", label: "None" }, ...(tasks() ?? []).filter(t => (t.repo_id ?? "") === repo()).map((t) => ({ value: t.id, label: t.title }))]} />
              </Field>
              <div class="row">
                <Toggle tone="strict" on={strict() || forcedStrict()} disabled={forcedStrict() || isSub()} onChange={setStrict} label="Strict mode" />
                <span class="small dim" title="Strict mode confines the agent to its worktree and allows outbound network access only to approved hosts.">
                  <Show when={forcedStrict()} fallback={<Show when={isSub()} fallback="Sandboxed worktree with host allowlist.">Subscriptions run in Standard mode.</Show>}>
                    Required for untrusted providers.
                  </Show>
                </span>
              </div>
            </div>
          </Show>
        </div>
        <Show when={err()}><div class="err small">{err()}</div></Show>
      </form>
    </Modal>
  );
}
