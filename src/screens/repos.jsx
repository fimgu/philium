import { createSignal, createMemo, onMount, Show } from "solid-js";
import { Page, Empty, Button, Input, Field, Badge, Modal, Select, VirtualList, confirm, toast } from "../ui/kit.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import "../styles/workspace.css";

export default function Repos() {
  const [repos, setRepos] = createSignal([]), [selected, setSelected] = createSignal(null);
  const [trees, setTrees] = createSignal([]), [branches, setBranches] = createSignal([]);
  const [error, setError] = createSignal(""), [busy, setBusy] = createSignal(false), [loading, setLoading] = createSignal(true);
  const [dialog, setDialog] = createSignal(null), [path, setPath] = createSignal(""), [name, setName] = createSignal("");
  const [diff, setDiff] = createSignal(null), [treeName, setTreeName] = createSignal("");
  let detailRequest = 0;
  const current = createMemo(() => repos().find(r => r.id === selected()));
  async function details(id) {
    const request = ++detailRequest;
    if (!id) { setTrees([]); setBranches([]); return; }
    const [w, b] = await Promise.all([rpc("workspace.worktrees", { repo_id: id }), rpc("workspace.branches", { repo_id: id })]);
    if (request === detailRequest && selected() === id) { setTrees(w); setBranches(b); }
  }
  async function refresh() {
    try { const list = await rpc("workspace.repos_list"); setRepos(list); if (!list.some(r => r.id === selected())) setSelected(list[0]?.id ?? null); await details(selected()); setError(""); }
    catch (e) { setError(String(e)); } finally { setLoading(false); }
  }
  async function act(fn) { if (busy()) return; setBusy(true); setError(""); try { await fn(); await refresh(); } catch (e) { setError(String(e)); toast(String(e), "danger"); } finally { setBusy(false); } }
  function choose(id) { setSelected(id); setTrees([]); setBranches([]); details(id).catch(e => setError(String(e))); }
  function add(mode) { setPath(""); setName(""); setDialog(mode); }
  async function submit() { await act(async () => { const r = await rpc(dialog() === "local" ? "workspace.repo_add" : "workspace.repo_clone", dialog() === "local" ? { path: path() } : { url: path(), ...(name().trim() ? { name: name().trim() } : {}) }); setSelected(r.id); setDialog(null); }); }
  async function remove(w) {
    await act(async () => {
      const status = await rpc("workspace.worktree_status", { id: w.id }).catch(e => ({ unsaved: true, dirty_files: [], unpushed_commits: "unknown", unmerged_commits: "unknown", error: String(e) }));
      const unsaved = status.unsaved;
      if (!await confirm(unsaved ? "Discard unsaved work?" : "Remove worktree?", `${w.branch}\n${w.path}\n${unsaved ? `${status.error ? `Status could not be inspected: ${status.error}. ` : ""}${status.dirty_files.length} dirty entries, ${status.unpushed_commits} unpushed commits and ${status.unmerged_commits} unmerged commits. This permanently deletes this worktree and its local branch.` : "The clean worktree and its local branch will be deleted. The repository stays."}`, { danger: true, ok: unsaved ? "Discard and delete" : "Delete worktree" })) return;
      await rpc("workspace.worktree_remove", { id: w.id, force: unsaved });
    });
  }
  function open(path, app) { act(() => rpc("workspace.open_in", { path, app })); }
  onMount(refresh); useEvent("workspace:changed", refresh);
  return <Page title="Repositories" sub="Each agent works in its own branch and worktree" actions={<><Button onClick={() => add("local")}>Add local repo</Button><Button variant="primary" onClick={() => add("clone")}>Clone repo</Button><Button busy={busy()} onClick={refresh}>Refresh</Button></>}>
    <Show when={error()}><div class="ws-alert" role="alert">{error()}</div></Show>
    <Show when={!loading()} fallback={<p>Loading repositories…</p>}>
      <Show when={repos().length} fallback={<Empty title="Give agents a repository" children="Add an existing local Git repository without moving it, or clone one into your Philium folder." action={<Button onClick={() => add("local")}>Add your first repository</Button>} />}>
        <div class="ws-layout">
          <aside class="ws-repos"><VirtualList items={repos()} rowHeight={110}>{r => <button class={`ws-repo ${selected() === r.id ? "selected" : ""}`} onClick={() => choose(r.id)}><strong>{r.name}</strong><span class="ws-mono">{r.branch || "Detached HEAD"}</span><span><Badge tone={r.dirty ? "strict" : "secure"}>{r.error ? "Unavailable" : r.dirty ? "Dirty" : "Clean"}</Badge> {r.worktree_count} worktrees</span><small title={r.path}>{r.path}</small></button>}</VirtualList></aside>
          <Show when={current()}>{r => <section class="ws-detail">
            <header class="ws-row"><div><h2>{r().name}</h2><code>{r().path}</code></div><div class="ws-actions"><Button onClick={() => open(r().path, "finder")}>Finder</Button><Button onClick={() => open(r().path, "terminal")}>Terminal</Button><Button onClick={() => act(async () => { if (await confirm("Unregister repository?", "This only unregisters the repository. No user folder is deleted. Managed worktrees must be removed first.")) { await rpc("workspace.repo_remove", { id: r().id }); setSelected(null); } })}>Unregister</Button></div></header>
            <div class="ws-row"><Field label="Default branch"><Select value={r().default_branch} options={branches().filter(b => !b.name.startsWith("origin/")).map(b => b.name)} onValue={value => act(() => rpc("workspace.repo_set", { id: r().id, default_branch: value }))} /></Field><Field label="Integration branch" hint="New worktrees start here, or from the default branch"><Select value={r().integration_branch ?? ""} options={[{ value: "", label: "Use default branch" }, ...branches().map(b => ({ value: b.name, label: b.name }))]} onValue={value => act(() => rpc("workspace.repo_set", { id: r().id, integration_branch: value || null }))} /></Field></div>
            <div class="ws-row"><h3>Managed worktrees</h3><div class="ws-actions"><Input aria-label="Worktree name" placeholder="Agent or worktree name" value={treeName()} onValue={setTreeName} /><Button busy={busy()} disabled={!treeName().trim()} onClick={() => act(async () => { await rpc("workspace.worktree_create", { repo_id: r().id, name: treeName().trim() }); setTreeName(""); })}>Create worktree</Button></div></div>
            <Show when={trees().length} fallback={<p class="ws-muted">No managed worktrees yet. Create one here or assign an agent to this repository. Your main checkout remains untouched.</p>}>
              <div class="ws-tree-list"><VirtualList items={trees()} rowHeight={188}>{w => <article class="ws-tree"><div class="ws-row"><strong class="ws-mono">{w.branch}</strong><Badge tone={w.status.unsaved ? "strict" : "secure"}>{w.status.error ? "Status unavailable, keep safe" : w.status.unsaved ? "Unsaved work" : "Clean and landed"}</Badge></div><p>Owner: <strong>{w.agent_name ?? "Manual worktree"}</strong> · Created {fmt.ago(w.created_at)} · {w.status.ahead ?? "?"} ahead / {w.status.behind ?? "?"} behind</p><p>{w.status.dirty_files?.length ?? "?"} dirty entries · {w.status.unpushed_commits ?? "?"} unpushed · {w.status.unmerged_commits ?? "?"} unmerged</p><code class="ws-path">{w.path}</code><div class="ws-actions"><Button onClick={() => act(async () => { const d = await rpc("workspace.diff", { worktree_id: w.id }); setDiff({ ...d, branch: w.branch, files: w.status.dirty_files }); })}>View diff</Button><Button onClick={() => open(w.path, "finder")}>Finder</Button><Button onClick={() => open(w.path, "terminal")}>Terminal</Button><Button onClick={() => open(w.path, "editor")}>Editor</Button><Button disabled={busy()} onClick={() => remove(w)}>Remove</Button></div></article>}</VirtualList></div>
            </Show>
            <h3>Branches</h3><div class="ws-branch-list"><VirtualList items={branches()} rowHeight={34}>{b => <div class="ws-row ws-branch"><code>{b.name}</code><span>{b.current ? "Current checkout" : b.upstream || b.commit?.slice(0, 8)}</span></div>}</VirtualList></div>
          </section>}</Show>
        </div>
      </Show>
    </Show>
    <Show when={dialog()}><Modal title={dialog() === "local" ? "Add existing local repository" : "Clone repository"} onClose={() => !busy() && setDialog(null)} footer={<Button busy={busy()} disabled={!path().trim()} onClick={submit}>{dialog() === "local" ? "Register repository" : "Clone into Philium"}</Button>}><Field label={dialog() === "local" ? "Absolute folder path" : "Repository URL"} hint={dialog() === "local" ? "The repository stays where it is. Philium never deletes this folder." : "Uses your existing Git authentication. No interactive password prompts."}><Input value={path()} onValue={setPath} placeholder={dialog() === "local" ? "/Users/you/Developer/project" : "https://github.com/owner/repo.git"} /></Field><Show when={dialog() === "clone"}><Field label="Folder name (optional)"><Input value={name()} onValue={setName} placeholder="Inferred from URL" /></Field></Show><Show when={error()}><p class="ws-alert">{error()}</p></Show></Modal></Show>
    <Show when={diff()}>{d => <Modal title={`Diff: ${d().branch}`} wide onClose={() => setDiff(null)}><Show when={d().truncated}><div class="ws-alert">Diff capped at 256 KiB. Open the worktree to inspect all changes.</div></Show><p>{d().note}</p><pre class="ws-diff">{d().stat || "No tracked changes"}</pre><pre class="ws-diff">{d().patch}</pre><h3>Dirty entries, including untracked files</h3><pre class="ws-diff">{d().files?.join("\n") || "None"}</pre></Modal>}</Show>
  </Page>;
}
