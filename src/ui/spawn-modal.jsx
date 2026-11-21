import { Show, createSignal, createResource, createMemo, createEffect, on } from "solid-js";
import { Modal, Button, Field, Input, TextArea, Select, Toggle, ModelPicker, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { useStore, activeRepoId } from "../lib/store.js";
import { spawnRepoDefault } from "../lib/agent-workspace.js";
import { repoLabels } from "../lib/repo-labels.js";

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
  const chosen = () => model() || def() || "";
  const chosenProvider = () => provById()[(chosen() || "").split("/")[0]];
  const forcedStrict = () => chosenProvider() && !chosenProvider().trusted;
  const isSub = () => ["chatgpt_oauth", "claude_oauth", "antigravity_oauth"].includes(chosenProvider()?.kind);

  createEffect(on(task, (t) => {
    const tk = (tasks() ?? []).find((x) => x.id === t);
    if (tk && !brief()) setBrief(`Work on board task "${tk.title}" (id ${tk.id}). Claim it first, follow the playbook, open a PR when done.\n\n${tk.detail ?? ""}`.trim());
    if (tk?.repo_id && !repo()) setRepo(tk.repo_id);
  }));

  const submit = async (e) => {
    e?.preventDefault?.();
    if (busy()) return;
    if (role() === "orchestrator" && !repo()) { setErr("Choose a repo. A coordinator without a checkout cannot create tasks or start helpers."); return; }
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
      <form
        onSubmit={submit}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
        style={{ display: "grid", gap: "16px" }}
      >
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
            <Show when={providers.state === "ready" && (providers() ?? []).length === 0}>
              <div class="row" style={{ "margin-top": "4px" }}>
                <Button size="sm" onClick={() => { props.onClose(null); go("providers"); }}>
                  <Icon name="key" size={11} /> Add provider
                </Button>
              </div>
            </Show>
          </Field>
          <Field label="Repo" hint="The agent gets its own worktree and branch">
            <Select value={repo()} onValue={id => { setTask(""); setRepo(id); }} options={[{ value: "", label: "No repo (scratch only)" }, ...(repos() ?? []).map((r) => ({ value: r.id, label: labels().get(r.id) }))]} />
            <Show when={repos.state === "ready" && (repos() ?? []).length === 0}>
              <div class="row" style={{ "margin-top": "4px" }}>
                <Button size="sm" onClick={() => { props.onClose(null); go("repos"); }}>
                  <Icon name="repo" size={11} /> Add repo
                </Button>
              </div>
            </Show>
          </Field>
          <Field label="Task" hint="Optional: link a board task">
            <Select value={task()} onValue={setTask} options={[{ value: "", label: "None" }, ...(tasks() ?? []).filter(t => (t.repo_id ?? "") === repo()).map((t) => ({ value: t.id, label: t.title }))]} />
          </Field>
        </div>
        <Field label="Mission Brief" hint="The first instruction. Orchestrators will automatically plan, spawn workers on disjoint files, and synthesize results. (⌘Enter to start)">
          <TextArea rows={5} value={brief()} onValue={setBrief} placeholder="Describe the mission, target PR, or task..." />
        </Field>
        <div class="row">
          <Toggle tone="strict" on={strict() || forcedStrict()} disabled={forcedStrict() || isSub()} onChange={setStrict} label="Strict mode" />
          <span class="small dim">
            <Show when={forcedStrict()} fallback={<Show when={isSub()} fallback="Sandboxed: own worktree only, network only to allowed hosts.">Subscriptions are trusted and run in Standard mode.</Show>}>
              Required: this provider is untrusted. Sandboxed, every request recorded.
            </Show>
          </span>
        </div>
        <Show when={err()}><div class="err small">{err()}</div></Show>
      </form>
    </Modal>
  );
}
