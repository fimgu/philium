// Per-hunk stage, unstage and revert for an agent worktree. The backend issues
// hunk ids and rejects any id whose content changed since it was listed.
import { createResource, createSignal, For, Show } from "solid-js";
import { rpc } from "../lib/rpc.js";
import { Button, confirm } from "./kit.jsx";
import { hunkActions } from "../lib/hunk-actions.js";

export default function HunkReview(props) {
  const [layer, setLayer] = createSignal("unstaged");
  const [note, setNote] = createSignal("");
  const [data, { refetch }] = createResource(
    () => props.worktreeId && [props.worktreeId, layer()],
    ([id, l]) => rpc("workspace.hunks", { worktree_id: id, layer: l }).catch((e) => ({ error: String(e) })),
  );
  const act = async (op, id, label) => {
    if (op === "revert" && !(await confirm(`Discard ${label}?`, "This removes the change from the file on disk.", { danger: true, ok: "Discard" }))) return;
    setNote("");
    try {
      await rpc("workspace.hunk_apply", { worktree_id: props.worktreeId, layer: layer(), op, id }, { action: true });
      refetch();
      props.onChanged?.();
    } catch (e) { setNote(String(e)); refetch(); }
  };
  return (
    <div class="hunk-review">
      <div class="row small" style={{ gap: "6px", "margin-bottom": "6px" }}>
        <span class="grow dim">Review by hunk</span>
        <Button size="sm" variant={layer() === "unstaged" ? "primary" : "ghost"} onClick={() => setLayer("unstaged")}>Not staged</Button>
        <Button size="sm" variant={layer() === "staged" ? "primary" : "ghost"} onClick={() => setLayer("staged")}>Staged</Button>
      </div>
      <Show when={data()?.error}><p class="small dim">{data().error}</p></Show>
      <Show when={note()}><p class="small">{note()}</p></Show>
      <Show when={data() && !data().error && !(data().files || []).length}><p class="small dim">Nothing here.</p></Show>
      <For each={data()?.files || []}>{(f) => (
        <div class="hunk-file">
          <div class="row small">
            <strong class="grow ellipsis">{f.path}</strong>
            <For each={hunkActions(layer())}>{(x) => (
              <Button size="sm" variant="ghost" aria-label={`${x.label} file ${f.path}`} onClick={() => act(x.op, f.id, `all changes in ${f.path}`)}>{x.label} file</Button>
            )}</For>
          </div>
          <Show when={!f.binary} fallback={<p class="small dim">Binary file. Use the file actions.</p>}>
            <For each={f.hunks}>{(h) => (
              <div class="hunk">
                <div class="row small">
                  <code class="grow ellipsis dim">{h.header}</code>
                  <For each={hunkActions(layer())}>{(x) => (
                    <Button size="sm" variant="ghost" aria-label={`${x.label} hunk ${h.header}`} onClick={() => act(x.op, h.id, "this hunk")}>{x.label}</Button>
                  )}</For>
                </div>
                <pre class="conv-pre diff">{h.text}</pre>
              </div>
            )}</For>
          </Show>
        </div>
      )}</For>
    </div>
  );
}
