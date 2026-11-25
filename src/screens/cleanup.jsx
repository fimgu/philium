// What agents left on this machine, and the actions to clean it up. One list, one place:
// Environments shows it, Home links to it.
import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { Button, Badge, Dot, confirm, toast } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";

const KIND = { worktree: "Worktree", process: "Process", terminal: "Terminal", port: "Port", container: "Container", cluster: "Cluster", colima: "Colima", scratch_dir: "Scratch folder", other: "Note" };
const kind = (k) => KIND[k] || k;

function Group(props) {
  return (
    <section class="ws-section cleanup-group">
      <div class="ws-row">
        <div>
          <h2>{props.title}</h2>
          <Show when={props.sub}><p class="ws-muted small">{props.sub}</p></Show>
        </div>
        <Show when={props.action}>{props.action}</Show>
      </div>
      <Show when={props.rows?.length} fallback={<p class="ws-muted small">{props.empty}</p>}>
        <div class="cleanup-list">
          <For each={props.rows}>{props.children}</For>
        </div>
      </Show>
    </section>
  );
}

function Row(props) {
  return (
    <div class="cleanup-row">
      <Dot state={props.dot} />
      <div class="cleanup-main">
        <div class="cleanup-title">
          <strong title={props.owner || props.scoped ? "" : "Not tied to a workspace, so it shows in every workspace"}>{props.owner || (props.scoped ? "Removed agent" : "Machine-wide")}</strong>
          <span class="ws-muted">· {kind(props.kind)}</span>
          <Show when={props.badge}><Badge tone={props.tone}>{props.badge}</Badge></Show>
        </div>
        <code class="ws-path" title={props.handle}>{props.handle}</code>
        <Show when={props.note}><small class="ws-muted">{props.note}</small></Show>
      </div>
      <div class="ws-actions">{props.children}</div>
    </div>
  );
}

export function Cleanup(props) {
  const [busy, setBusy] = createSignal(false);
  const [orphans, setOrphans] = createSignal(null);
  const [ledger, lg] = createResource(() => rpc("reaper.ledger", {}).then((r) => r.filter((x) => x.state !== "released")).catch(() => []));
  const [proposals, pr] = createResource(() => rpc("reaper.proposals").catch(() => []));
  const reload = () => { lg.refetch(); pr.refetch(); };
  for (const t of ["reaper:changed", "reaper:proposals", "agents:changed"]) useVisibleEvent(t, reload, { debounceMs: 300 });

  // Follows the workspace picker: one workspace, or everything.
  const scoped = (list) => {
    const rid = activeRepoId();
    return rid ? (list ?? []).filter((r) => !r.repo_id || r.repo_id === rid) : list ?? [];
  };
  const leaked = createMemo(() => scoped(ledger()).filter((r) => r.state === "leaked"));
  const live = createMemo(() => scoped(ledger()).filter((r) => r.state !== "leaked"));
  const pending = createMemo(() => scoped(proposals()));

  async function act(fn) {
    if (busy()) return;
    setBusy(true);
    try { await fn(); } catch (e) { toast(String(e), "danger"); } finally { setBusy(false); reload(); }
  }
  const forget = (ids) => act(async () => {
    const r = await rpc("reaper.forget", { ids }, { action: true });
    toast(`Forgot ${r.forgot.length} record${r.forgot.length === 1 ? "" : "s"}`, "good");
  });
  const decide = (p, approve) => act(async () => {
    if (approve && !(await confirm(
      p.unsaved ? "Permanently discard unsaved work?" : "Clean this up?",
      `${kind(p.kind)}: ${p.handle}\n${p.reason}\n${p.unsaved ? "This can delete dirty files and unpushed or unmerged commits. There is no undo." : "It is removed for good."}`,
      { danger: true, ok: p.unsaved ? "Discard and delete" : "Clean up" },
    ))) return;
    const [res] = await rpc(approve ? "reaper.approve" : "reaper.dismiss", { ids: [p.id] });
    if (res?.error) toast(res.error, "danger");
    else if (res?.needs_confirmation) toast("Work changed since this was proposed. Check it and try again.", "danger");
  });

  return (
    <div class="ws-settings">
      <Show when={pending().length}>
        <Group title="Ready to clean up" sub="Philium found these idle. Nothing is removed until you say so." rows={pending()}>
          {(p) => (
            <Row dot={p.unsaved ? "warn" : ""} owner={p.agent_name} scoped={!!p.repo_id} kind={p.kind} handle={p.handle} note={p.reason} badge={p.unsaved ? "Unsaved work" : null} tone="strict">
              <Button size="sm" disabled={busy()} onClick={() => decide(p, false)}>Keep</Button>
              <Button size="sm" variant="primary" disabled={busy()} onClick={() => decide(p, true)}>{p.unsaved ? "Review and discard" : "Clean up"}</Button>
            </Row>
          )}
        </Group>
      </Show>
      <Show when={leaked().length}>
        <Group
          title="Could not stop"
          sub="Philium could not stop these safely. Stop them yourself (the note says how), then forget them here."
          rows={leaked()}
          action={<Show when={leaked().length > 1}><Button size="sm" disabled={busy()} onClick={() => forget(leaked().map((r) => r.id))}>Forget all</Button></Show>}
        >
          {(r) => (
            <Row dot="bad" owner={r.agent_name} scoped={!!r.repo_id} kind={r.kind} handle={r.handle} note={r.meta?.note}>
              <Button size="sm" disabled={busy()} title="Nothing is stopped or deleted. Philium just stops tracking it." onClick={() => forget([r.id])}>Forget</Button>
            </Row>
          )}
        </Group>
      </Show>
      <Group
        title="Still running"
        sub="Started by agents. Removed with the agent that owns it."
        rows={live()}
        empty={ledger.loading ? "Loading…" : "Nothing. Agents have not left anything running."}
      >
        {(r) => <Row dot="live" owner={r.agent_name} scoped={!!r.repo_id} kind={r.kind} handle={r.handle} note={`Since ${fmt.ago(r.created_at)}`} />}
      </Group>
      <Show when={!props.pilot}>
        <Group
          title="Not tracked"
          sub="Philium worktree folders, labelled containers and philium-* clusters that no agent owns any more."
          rows={orphans()?.resources}
          empty={orphans() ? (orphans().errors?.length ? `Some checks could not run: ${orphans().errors.join("; ")}` : "Nothing found.") : "Not scanned yet."}
          action={<Button size="sm" busy={busy()} onClick={() => act(async () => setOrphans(await rpc("reaper.orphans")))}>Scan</Button>}
        >
          {(r) => <Row dot="warn" owner={r.agent} kind={r.kind} handle={r.handle} note={r.action || "Check who owns it before you delete it."} />}
        </Group>
      </Show>
    </div>
  );
}
