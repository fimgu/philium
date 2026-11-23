// Commit chosen files, push the branch, open a pull request. Three separate,
// confirmed steps that run the user's own git and gh on this machine.
import { createResource, createSignal, For, Show } from "solid-js";
import { Modal, Button, Input, Field, toast } from "./kit.jsx";
import { rpc } from "../lib/rpc.js";
import { publishSteps } from "../lib/publish-steps.js";

export default function PublishModal(props) {
  const id = () => props.worktree.id;
  const [state, { refetch }] = createResource(id, (wid) => rpc("workspace.publish_state", { worktree_id: wid }, { fresh: true }).catch((e) => ({ error: String(e) })));
  const [picked, setPicked] = createSignal([]);
  const [message, setMessage] = createSignal("");
  const [base, setBase] = createSignal(props.base || "");
  const [title, setTitle] = createSignal("");
  const [body, setBody] = createSignal("");
  const [busy, setBusy] = createSignal("");
  const [prUrl, setPrUrl] = createSignal("");
  const steps = () => publishSteps(state(), picked());
  const toggle = (p) => setPicked((l) => (l.includes(p) ? l.filter((x) => x !== p) : [...l, p]));
  const step = async (name, method, args, done) => {
    setBusy(name);
    try {
      const r = await rpc(`workspace.${method}`, { worktree_id: id(), ...args }, { action: true });
      done?.(r);
      refetch();
    } catch (e) { toast(String(e), "bad"); }
    finally { setBusy(""); }
  };
  return (
    <Modal title={`Publish ${props.worktree.branch}`} onClose={props.onClose} wide>
      <Show when={!state()?.error} fallback={<p class="small">{state().error}</p>}>
        <h3 class="small">1. Commit</h3>
        <Show when={(state()?.files || []).length} fallback={<p class="small dim">No uncommitted changes.</p>}>
          <div style={{ "max-height": "180px", overflow: "auto" }}>
            <For each={state()?.files || []}>{(f) => (
              <label class="row small" style={{ gap: "6px" }}>
                <input type="checkbox" checked={picked().includes(f.path)} onChange={() => toggle(f.path)} />
                <code class="dim">{f.status}</code> <span class="ellipsis">{f.path}</span>
              </label>
            )}</For>
          </div>
          <Field label="Message"><Input value={message()} onValue={setMessage} placeholder="Describe the change" /></Field>
          <Button size="sm" variant="primary" disabled={!steps().canCommit || !message().trim() || !!busy()}
            onClick={() => step("commit", "commit", { paths: picked(), message: message(), expect_head: state()?.head }, () => { setPicked([]); setMessage(""); toast("Committed.", "good"); })}>
            Commit {picked().length || ""} file{picked().length === 1 ? "" : "s"}
          </Button>
        </Show>
        <h3 class="small">2. Push</h3>
        <p class="small dim">{state()?.remote ? `To ${state().remote}/${state()?.branch}${state()?.ahead != null ? `, ${state().ahead} commit(s) ahead` : ", not pushed yet"}.` : "No remote found for this branch."}</p>
        <Button size="sm" disabled={!steps().canPush || !!busy()} onClick={() => step("push", "push", {}, () => toast("Pushed.", "good"))}>Push branch</Button>
        <h3 class="small">3. Pull request</h3>
        <Field label="Base branch"><Input value={base()} onValue={setBase} /></Field>
        <Field label="Title"><Input value={title()} onValue={setTitle} /></Field>
        <Field label="Description"><textarea class="textarea" rows="4" value={body()} onInput={(e) => setBody(e.currentTarget.value)} /></Field>
        <Button size="sm" disabled={!steps().canPr || !title().trim() || !base().trim() || !!busy()}
          onClick={() => step("pr", "pr_create", { base: base(), title: title(), body: body() }, (r) => setPrUrl(r?.url || ""))}>
          Open pull request
        </Button>
        <Show when={prUrl()}><p class="small">Pull request: <code>{prUrl()}</code></p></Show>
      </Show>
    </Modal>
  );
}
