// "Improve Philium": ask for a change from any screen, an agent builds it in
// a worktree of the Philium source, you test, preview, accept or roll back.
import { For, Show, createSignal, createResource, onCleanup } from "solid-js";
import { Modal, Button, Field, Input, TextArea, Badge, Dot, Empty, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { improveOpen, closeImprove, go } from "../lib/nav.js";

const STATES = {
  building: ["busy", "Agent is building"],
  preview: ["", "Preview status unavailable"],
  accepted: ["live", "Accepted"],
  rejected: ["", "Rejected"],
  rolled_back: ["warn", "Rolled back"],
};

export function previewLabel(status) {
  return ({ active: "Preview process active", stopped: "Preview stopped", failed: "Preview failed", unavailable: "Preview status unavailable" })[status] ?? "Preview status unavailable";
}

// Recent errors are captured from the console so the agent knows what broke.
const recentErrors = [];
window.addEventListener("error", (e) => { recentErrors.push(String(e.message).slice(0, 300)); if (recentErrors.length > 5) recentErrors.shift(); });
window.addEventListener("unhandledrejection", (e) => { recentErrors.push(String(e.reason).slice(0, 300)); if (recentErrors.length > 5) recentErrors.shift(); });

function Item(props) {
  const [st, { refetch }] = createResource(() => props.id, (id) => rpc("selfdev.get", { id }));
  useEvent("selfdev:changed", (p) => p.id === props.id && refetch());
  useEvent("terminals:exit", () => refetch());
  const [busy, setBusy] = createSignal("");
  const run = async (what, fn) => {
    setBusy(what);
    try { await fn(); refetch(); } catch (e) { toast(String(e), "danger"); } finally { setBusy(""); }
  };
  const accept = () => run("accept", async () => {
    let r = await rpc("selfdev.accept", { id: props.id });
    if (r?.needs_security_approval) {
      const ok = await confirm("Security files changed",
        `This change touches: ${r.flagged.join(", ")}. These control keys, the gateway or the sandbox. Only accept if you have read the diff.`,
        { danger: true, ok: "I reviewed it, accept" });
      if (!ok) return;
      r = await rpc("selfdev.accept", { id: props.id, approve_security: true });
    }
    toast("Merged. Rebuilding Philium in a terminal.", "secure");
  });
  const reject = () => run("reject", async () => {
    let r = await rpc("selfdev.reject", { id: props.id });
    if (r?.needs_confirmation) {
      if (!(await confirm("Discard this change?", "The agent's work has not been merged and will be deleted.", { danger: true, ok: "Discard" }))) return;
      await rpc("selfdev.reject", { id: props.id, force: true });
    }
  });
  const s = () => st() ?? {};
  const [showDiff, setShowDiff] = createSignal(false);
  const [diff, { refetch: refetchDiff }] = createResource(() => showDiff() && props.id, (id) => rpc("selfdev.diff", { id }).catch((e) => ({ error: String(e) })));
  useEvent("selfdev:changed", (p) => p.id === props.id && showDiff() && refetchDiff());
  // Covers an exit arriving before the preview RPC has stored its terminal ID.
  const poll = setInterval(() => { if (s().preview === "active") refetch(); }, 2000);
  onCleanup(() => clearInterval(poll));
  const testTone = () => ({ passed: "secure", failed: "danger", running: "" })[s().tests] ?? "";
  return (
    <div class="card" style={{ display: "grid", gap: "8px" }}>
      <div class="row">
        <Dot state={s().state === "preview" ? (s().preview === "active" ? "busy" : s().preview === "failed" ? "warn" : "") : STATES[s().state]?.[0]} />
        <b class="grow ellipsis">{(s().request ?? "").split("\n")[0]}</b>
        <span class="xs faint">{fmt.ago(s().created_at)}</span>
      </div>
      <div class="row small dim" style={{ "flex-wrap": "wrap", gap: "6px" }}>
        <span>{s().state === "preview" ? previewLabel(s().preview) : STATES[s().state]?.[1] ?? s().state}</span>
        <Show when={s().state === "preview" && s().preview_exit_code != null}><span>exit {s().preview_exit_code}</span></Show>
        <Show when={s().agent_name}><span>by <a href="#" onClick={(e) => { e.preventDefault(); closeImprove(); go("agents", { id: s().agent_id }); }}>{s().agent_name}</a> ({s().agent_state})</span></Show>
        <Show when={s().files?.length}><Badge>{s().files.length} files</Badge></Show>
        <Show when={s().tests}><Badge tone={testTone()}>tests {s().tests}</Badge></Show>
        <Show when={s().tests === "passed" && s().tested_commit !== s().head}><Badge tone="strict">changed since tests</Badge></Show>
        <Show when={s().flagged?.length}><Badge tone="strict" title={s().flagged.join("\n")}><Icon name="shield" size={11} /> security files</Badge></Show>
        <Show when={s().build}><Badge tone={s().build === "passed" ? "secure" : s().build === "failed" ? "danger" : ""}>build {s().build}</Badge></Show>
      </div>
      <div class="row" style={{ "flex-wrap": "wrap" }}>
        <Show when={["building", "preview"].includes(s().state)}>
          <Button size="sm" busy={busy() === "test"} onClick={() => run("test", () => rpc("selfdev.test", { id: props.id }))}>Run tests</Button>
          <Button size="sm" busy={busy() === "preview"} disabled={s().preview === "active"} onClick={() => run("preview", () => rpc("selfdev.preview", { id: props.id }))}>Open preview</Button>
          <Button size="sm" variant="primary" busy={busy() === "accept"} disabled={s().tests !== "passed" || s().tested_commit !== s().head} title="Tests must pass on the latest commit" onClick={accept}>Accept</Button>
          <Button size="sm" variant="ghost" busy={busy() === "reject"} onClick={reject}>Reject</Button>
        </Show>
        <Show when={s().state === "accepted"}>
          <Show when={s().build === "passed"}><Button size="sm" variant="primary" onClick={() => rpc("selfdev.restart")}>Restart Philium</Button></Show>
          <Button size="sm" busy={busy() === "rb"} onClick={() => run("rb", async () => {
            if (await confirm("Roll back?", "This adds a revert commit and rebuilds Philium.", { ok: "Roll back" })) await rpc("selfdev.rollback", { id: props.id });
          })}>Roll back</Button>
        </Show>
        <Show when={s().state === "rolled_back" && s().build === "passed"}><Button size="sm" onClick={() => rpc("selfdev.restart")}>Restart Philium</Button></Show>
        <Show when={s().files?.length || s().agent_id}><Button size="sm" variant="ghost" onClick={() => setShowDiff(!showDiff())}>{showDiff() ? "Hide diff" : "View diff"}</Button></Show>
        <For each={[["test_terminal", "Test output"], ["preview_terminal", "Preview output"], ["build_terminal", "Build output"]]}>{([key, label]) =>
          <Show when={s()[key]}><Button size="sm" variant="ghost" onClick={() => { closeImprove(); go("terminals", { id: s()[key] }); }}><Icon name="terminal" size={12} /> {label}</Button></Show>
        }</For>
      </div>
      <Show when={showDiff()}><DiffView data={diff()} files={s().files} /></Show>
    </div>
  );
}

function lineStyle(l) {
  if (l.startsWith("+++") || l.startsWith("---")) return { color: "var(--fg-dim, inherit)", "font-weight": "600" };
  if (l.startsWith("diff ")) return { "font-weight": "700", "border-top": "1px solid var(--border, #8884)", "padding-top": "4px", "margin-top": "4px" };
  if (l.startsWith("@@")) return { color: "var(--accent, #58a6ff)" };
  if (l.startsWith("+")) return { background: "rgba(46,160,67,.15)" };
  if (l.startsWith("-")) return { background: "rgba(248,81,73,.15)" };
  return {};
}

function DiffView(props) {
  const lines = () => (props.data?.patch ?? "").split("\n");
  return (
    <div style={{ display: "grid", gap: "6px" }}>
      <Show when={props.data} fallback={<span class="small dim">Loading diff...</span>}>
        <Show when={!props.data.error} fallback={<span class="small danger">{props.data.error}</span>}>
          <Show when={props.files?.length}>
            <div class="small dim">Files changed: <For each={props.files}>{(f) => <code style={{ "margin-right": "6px" }}>{f}</code>}</For></div>
          </Show>
          <Show when={props.data.stat}><pre class="xs" style={{ margin: 0, "white-space": "pre-wrap" }}>{props.data.stat}</pre></Show>
          <Show when={props.data.patch} fallback={<span class="small dim">No changes yet.</span>}>
            <pre class="xs" style={{ margin: 0, "max-height": "420px", overflow: "auto", "font-family": "var(--mono, ui-monospace, monospace)" }}>
              <For each={lines()}>{(l) => <div style={{ ...lineStyle(l), "white-space": "pre" }}>{l || " "}</div>}</For>
            </pre>
          </Show>
        </Show>
      </Show>
    </div>
  );
}

export function ImproveHost() {
  const [text, setText] = createSignal("");
  const [sourcePath, setSourcePath] = createSignal("");
  const [savingSource, setSavingSource] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [list, { refetch }] = createResource(() => improveOpen(), () => rpc("selfdev.list").catch(() => []));
  const [source, { refetch: refetchSource }] = createResource(() => improveOpen(), () => rpc("selfdev.source").catch(() => null));
  useEvent("selfdev:changed", () => improveOpen() && refetch());

  const saveSourcePath = async (e) => {
    e?.preventDefault();
    const val = sourcePath().trim();
    if (!val || savingSource()) return;
    setSavingSource(true);
    try {
      await rpc("app.settings_set", { key: "selfdev.source", value: val });
      const resolved = await refetchSource();
      if (resolved) {
        setSourcePath("");
        toast(`Philium source set to ${resolved}`, "secure");
      } else {
        toast("Folder must be a Philium git checkout containing src-tauri/", "danger");
      }
    } catch (err) {
      toast(String(err), "danger");
    } finally {
      setSavingSource(false);
    }
  };

  const submit = async () => {
    setBusy(true);
    try {
      const ctx = { ...improveOpen(), errors: [...recentErrors], selection: String(window.getSelection() ?? "").slice(0, 500) };
      await rpc("selfdev.request", { text: text(), context: ctx });
      setText("");
      toast("An agent is working on it", "secure");
      refetch();
    } catch (e) { toast(String(e), "danger"); } finally { setBusy(false); }
  };

  return (
    <Show when={improveOpen()}>
      <Modal wide title="Improve Philium" onClose={closeImprove}>
        <p class="small dim" style={{ "margin-top": 0 }}>
          Describe a change. An agent builds it in a separate copy of the Philium source, so this app keeps working. You run the tests, try a preview with no access to your keys, then accept or reject. Accepted changes can be rolled back.
        </p>
        <Show when={source() === null}>
          <div class="shield strict small" style={{ display: "grid", gap: "8px" }}>
            <div>
              Philium source checkout not found. Clone Philium into <code class="mono">~/Philium/repos/philium</code> (or add it under <a href="#" onClick={(e) => { e.preventDefault(); closeImprove(); go("repos"); }}>Repos</a>), or enter the local checkout path below:
            </div>
            <form class="row" onSubmit={saveSourcePath}>
              <Input value={sourcePath()} onValue={setSourcePath} placeholder="/path/to/philium (containing .git and src-tauri)" />
              <Button size="sm" variant="primary" type="submit" busy={savingSource()} disabled={!sourcePath().trim()}>Use folder</Button>
            </form>
          </div>
        </Show>
        <Field label={`Change for the ${improveOpen()?.screen ?? ""} screen`}>
          <TextArea rows={4} value={text()} onValue={setText} placeholder="For example: show token cost next to each agent in the list" />
        </Field>
        <div class="row" style={{ "justify-content": "flex-end", margin: "8px 0 16px" }}>
          <Button variant="primary" busy={busy()} disabled={!text().trim() || !source()} onClick={submit}><Icon name="spark" size={12} /> Start</Button>
        </div>
        <Show when={(list() ?? []).length} fallback={<Empty title="No improvements yet" />}>
          <div style={{ display: "grid", gap: "8px", "max-height": "45vh", overflow: "auto" }}>
            <For each={list()}>{(r) => <Item id={r.id} />}</For>
          </div>
        </Show>
      </Modal>
    </Show>
  );
}
