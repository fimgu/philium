import { createSignal, createMemo, createEffect, on, onMount, Show, For } from "solid-js";
import { repoLabels } from "../lib/repo-labels.js";
import { Page, Empty, Button, Input, Field, Badge, Modal, Select, Segmented, VirtualList, Menu, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import "../styles/workspace.css";

function Grants(props) {
  const [list, setList] = createSignal([]);
  const [err, setErr] = createSignal("");
  const load = (id) => rpc("sandbox.grants_get", { repo_id: id }).then((r) => setList(r.grants)).catch((e) => setErr(String(e)));
  createEffect(on(() => props.repoId, load));
  const save = async (next) => {
    setErr("");
    try { const r = await rpc("sandbox.grants_set", { repo_id: props.repoId, grants: next }); setList(r.grants); } catch (e) { setErr(String(e)); }
  };
  const addFolder = async () => {
    try {
      const r = await rpc("app.pick_folder", { prompt: "Choose a folder agents may use for this repository" });
      if (r.path && !list().some((g) => g.path === r.path)) save([...list(), { path: r.path, write: false }]);
    } catch (e) { setErr(String(e)); }
  };
  return <section class="ws-grants">
    <div class="ws-row"><div><h3>Shared with strict agents</h3><p class="ws-muted">Strict agents can read and run every app and tool on this Mac, but never your keys, logins, mail, messages or personal folders. Add folders this repository needs from those places, such as a parts library in Documents.</p></div><Button onClick={addFolder}><Icon name="plus" size={12} /> Add folder</Button></div>
    <Show when={err()}><p class="ws-alert">{err()}</p></Show>
    <For each={list()} fallback={<p class="ws-muted">Nothing extra shared. The agent's own worktree is always writable.</p>}>{(g, i) =>
      <div class="ws-row ws-grant"><code title={g.path}>{g.path}</code><div class="ws-actions">
        <Segmented value={g.write ? "rw" : "ro"} options={[{ value: "ro", label: "Read" }, { value: "rw", label: "Read and write" }]} onChange={(v) => save(list().map((x, j) => (j === i() ? { ...x, write: v === "rw" } : x)))} />
        <Button variant="ghost" title="Stop sharing" onClick={() => save(list().filter((_, j) => j !== i()))}><Icon name="trash" size={12} /></Button>
      </div></div>}</For>
  </section>;
}

const num = (x) => (typeof x === "number" ? x : null);
const when = (v) => { const t = typeof v === "number" ? (v < 1e12 ? v * 1000 : v) : Date.parse(v); return Number.isFinite(t) ? fmt.ago(t) : null; };

function Pill(props) {
  return <span class={`pill ${props.tone ?? ""}`} title={props.title}>{props.children}</span>;
}

function Worktree(props) {
  const w = () => props.w, st = () => props.w.status ?? {};
  const tone = () => (st().error ? "bad" : st().unsaved ? "warn" : "ok");
  return <article class={`wt ${tone()}`}>
    <span class={`wt-bar ${tone()}`} />
    <div class="wt-main">
      <div class="wt-top">
        <strong class="ws-mono wt-branch" title={w().branch}>{w().branch}</strong>
        <Show when={w().agent_id} fallback={<span class="xs faint">Manual</span>}>
          <button class="wt-agent" title="Open agent" onClick={() => go("agents", { id: w().agent_id })}>{w().agent_name ?? "agent"}</button>
        </Show>
        <span class="xs faint">{when(w().created_at) ?? ""}</span>
      </div>
      <div class="wt-pills">
        <Show when={st().error} fallback={<>
          <Show when={num(st().ahead)}><Pill title="Commits ahead of base">{"\u2191"}{st().ahead}</Pill></Show>
          <Show when={num(st().behind)}><Pill title="Commits behind base">{"\u2193"}{st().behind}</Pill></Show>
          <Show when={st().dirty_files?.length}><Pill tone="warn" title="Uncommitted entries">{st().dirty_files.length} dirty</Pill></Show>
          <Show when={num(st().unpushed_commits)}><Pill tone="warn" title="Commits not pushed">{st().unpushed_commits} unpushed</Pill></Show>
          <Show when={!st().unsaved}><Pill tone="ok">Clean and landed</Pill></Show>
        </>}><Pill tone="bad" title={st().error}>Status unavailable</Pill></Show>
      </div>
      <code class="ws-path" title={w().path}>{w().path}</code>
    </div>
    <div class="wt-actions">
      <Button size="sm" onClick={() => props.onDiff(w())}>View diff</Button>
      <Menu items={[
        { label: "Open in Finder", onClick: () => props.onOpen(w().path, "finder") },
        { label: "Open in Terminal", onClick: () => props.onOpen(w().path, "terminal") },
        { label: "Open in editor", onClick: () => props.onOpen(w().path, "editor") },
        { label: "Remove worktree", danger: true, disabled: props.busy, onClick: () => props.onRemove(w()) },
      ]} />
    </div>
  </article>;
}

function Branches(props) {
  const [q, setQ] = createSignal("");
  const usedBy = createMemo(() => { const m = {}; for (const w of props.trees) (m[w.branch] ??= []).push(w.agent_name ?? "manual"); return m; });
  const rows = createMemo(() => {
    const t = q().trim().toLowerCase();
    const all = (props.branches ?? []).filter((b) => b?.name && !b.name.endsWith("/HEAD") && (!t || b.name.toLowerCase().includes(t)));
    const local = all.filter((b) => !b.name.startsWith("origin/") && !b.name.includes("/remotes/"));
    const remote = all.filter((b) => !local.includes(b));
    const out = [];
    if (local.length) { out.push({ h: "Local", n: local.length }); local.forEach((b) => out.push({ b })); }
    if (remote.length) { out.push({ h: "Remote", n: remote.length }); remote.forEach((b) => out.push({ b })); }
    return out;
  });
  return <section class="rp-sec">
    <div class="rp-sec-head"><h3>Branches</h3><span class="xs faint">{(props.branches ?? []).length}</span><div class="grow" /><div class="rp-search"><Input aria-label="Search branches" placeholder="Search branches" value={q()} onValue={setQ} /></div></div>
    <Show when={rows().length} fallback={<p class="ws-muted rp-pad">{q() ? "No branches match." : "No branches yet."}</p>}>
      <div class="ws-branch-list" style={{ height: `${Math.min(rows().length, 9) * 34 + 2}px` }}><VirtualList items={rows()} rowHeight={34}>{(r) => r.h
        ? <div class="br-group"><span>{r.h}</span><span class="faint">{r.n}</span></div>
        : <div class="br-row">
            <Icon name="branch" size={12} />
            <code class="ellipsis grow" title={r.b.name}>{r.b.name}</code>
            <Show when={r.b.current}><Pill tone="ok">Current</Pill></Show>
            <For each={usedBy()[r.b.name] ?? []}>{(a) => <Pill title="Worktree in use">{a}</Pill>}</For>
            <span class="xs faint br-meta">{when(r.b.date ?? r.b.updated_at) ?? (r.b.commit ? String(r.b.commit).slice(0, 7) : "")}</span>
          </div>}</VirtualList></div>
    </Show>
  </section>;
}

export default function Repos() {
  const [repos, setRepos] = createSignal([]), [selected, setSelected] = createSignal(null);
  const labels = createMemo(() => repoLabels(repos()));
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
    if (request === detailRequest && selected() === id) { setTrees(w ?? []); setBranches(b ?? []); }
  }
  async function refresh() {
    try { const list = (await rpc("workspace.repos_list")) ?? []; setRepos(list); if (!list.some(r => r.id === selected())) setSelected(list[0]?.id ?? null); await details(selected()); setError(""); }
    catch (e) { setError(cleanErr(e)); } finally { setLoading(false); }
  }
  const cleanErr = (e) => String(e?.message ?? e).replace(/^Error:\s*/, "");
  async function act(fn) { if (busy()) return; setBusy(true); setError(""); try { await fn(); await refresh(); } catch (e) { setError(cleanErr(e)); toast(cleanErr(e), "danger"); } finally { setBusy(false); } }
  function choose(id) { setSelected(id); setTrees([]); setBranches([]); details(id).catch(e => setError(cleanErr(e))); }
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
  const viewDiff = (w) => act(async () => { const d = await rpc("workspace.diff", { worktree_id: w.id }); setDiff({ ...d, branch: w.branch, files: w.status?.dirty_files }); });
  const unregister = (r) => act(async () => { if (await confirm("Unregister repository?", "This only unregisters the repository. No user folder is deleted. Managed worktrees must be removed first.")) { await rpc("workspace.repo_remove", { id: r.id }); setSelected(null); } });
  onMount(refresh); useEvent("workspace:changed", refresh);
  return <Page title="Repositories" sub="Each agent works in its own branch and worktree" actions={<><Button onClick={() => add("local")}>Add local repo</Button><Button variant="primary" onClick={() => add("clone")}>Clone repo</Button><Button class="ghost" busy={busy()} onClick={refresh} title="Refresh"><Icon name="refresh" size={12} /></Button></>}>
    <Show when={error()}><div class="ws-alert" role="alert">{error()}</div></Show>
    <Show when={!loading()} fallback={<div class="loading"><span class="spinner" />Loading repositories</div>}>
      <Show when={repos().length} fallback={<Empty icon={<Icon name="repo" size={28} />} title="Give agents a repository" action={<Button variant="primary" onClick={() => add("local")}>Add your first repository</Button>}>Add an existing local Git repository without moving it, or clone one into your Philium folder.</Empty>}>
        <div class="ws-layout">
          <aside class="ws-repos"><VirtualList items={repos()} rowHeight={84}>{r => <button class={`ws-repo ${selected() === r.id ? "selected" : ""}`} onClick={() => choose(r.id)}>
            <strong class="ellipsis" title={r.path}>{labels().get(r.id)}</strong>
            <span class="ws-mono xs faint"><Icon name="branch" size={11} /> {r.branch || "Detached HEAD"}</span>
            <span class="rp-line"><Pill tone={r.error ? "bad" : r.dirty ? "warn" : "ok"}>{r.error ? "Unavailable" : r.dirty ? "Dirty" : "Clean"}</Pill><span class="xs faint">{r.worktree_count ?? 0} worktree{r.worktree_count === 1 ? "" : "s"}</span></span>
          </button>}</VirtualList></aside>
          <Show when={current()} fallback={<Empty title="Select a repository">Pick one on the left to see its worktrees and branches.</Empty>}>{r => <section class="ws-detail">
            <header class="rp-head">
              <div class="rp-title">
                <h2 class="ellipsis" title={r().name}>{r().name}</h2>
                <Pill tone={r().error ? "bad" : r().dirty ? "warn" : "ok"}>{r().error ? "Unavailable" : r().dirty ? "Uncommitted changes" : "Clean"}</Pill>
                <div class="grow" />
                <Button size="sm" onClick={() => open(r().path, "finder")}>Finder</Button>
                <Button size="sm" onClick={() => open(r().path, "terminal")}>Terminal</Button>
                <Menu items={[{ label: "Unregister repository", danger: true, onClick: () => unregister(r()) }]} />
              </div>
              <code class="ws-path" title={r().path}>{r().path}</code>
              <div class="rp-meta">
                <span class="rp-chip"><Icon name="branch" size={11} /> {r().branch || "Detached HEAD"}</span>
                <Show when={r().remote_url}><span class="rp-chip" title={r().remote_url}><Icon name="external" size={11} /> <span class="ellipsis">{r().remote_url}</span></span></Show>
                <Show when={!r().remote_url}><span class="rp-chip faint">No remote</span></Show>
              </div>
            </header>
            <section class="rp-sec">
              <div class="rp-sec-head"><h3>Branch settings</h3></div>
              <div class="rp-settings">
                <Field label="Default branch"><Select value={r().default_branch} options={branches().filter(b => b?.name && !b.name.startsWith("origin/")).map(b => b.name)} onValue={value => act(() => rpc("workspace.repo_set", { id: r().id, default_branch: value }))} /></Field>
                <Field label="Integration branch" hint="New worktrees start here, or from the default branch"><Select value={r().integration_branch ?? ""} options={[{ value: "", label: "Use default branch" }, ...branches().filter(b => b?.name).map(b => ({ value: b.name, label: b.name }))]} onValue={value => act(() => rpc("workspace.repo_set", { id: r().id, integration_branch: value || null }))} /></Field>
              </div>
            </section>
            <section class="rp-sec">
              <div class="rp-sec-head"><h3>Worktrees</h3><span class="xs faint">{trees().length}</span><div class="grow" />
                <div class="rp-create"><Input aria-label="Worktree name" placeholder="Agent or worktree name" value={treeName()} onValue={setTreeName} onKeyDown={(e) => e.key === "Enter" && treeName().trim() && act(async () => { await rpc("workspace.worktree_create", { repo_id: r().id, name: treeName().trim() }); setTreeName(""); })} />
                <Button busy={busy()} disabled={!treeName().trim()} onClick={() => act(async () => { await rpc("workspace.worktree_create", { repo_id: r().id, name: treeName().trim() }); setTreeName(""); })}>Create</Button></div></div>
              <Show when={trees().length} fallback={<p class="ws-muted rp-pad">No managed worktrees yet. Create one here or assign an agent to this repository. Your main checkout stays untouched.</p>}>
                <div class="ws-tree-list" style={{ height: `${Math.min(trees().length, 5) * 104 + 4}px` }}><VirtualList items={trees()} rowHeight={104}>{w => <Worktree w={w} busy={busy()} onDiff={viewDiff} onOpen={open} onRemove={remove} />}</VirtualList></div>
              </Show>
            </section>
            <Branches branches={branches()} trees={trees()} />
            <Grants repoId={r().id} />
          </section>}</Show>
        </div>
      </Show>
    </Show>
    <Show when={dialog()}><Modal title={dialog() === "local" ? "Add existing local repository" : "Clone repository"} onClose={() => !busy() && setDialog(null)} footer={<Button busy={busy()} disabled={!path().trim()} onClick={submit}>{dialog() === "local" ? "Register repository" : "Clone into Philium"}</Button>}><Field label={dialog() === "local" ? "Folder" : "Repository URL"} hint={dialog() === "local" ? "The repository stays where it is. Philium never deletes this folder." : "Uses your existing Git authentication. No interactive password prompts."}><div class="row" style={{ gap: "8px" }}><Input value={path()} onValue={setPath} placeholder={dialog() === "local" ? "~/Developer/project" : "https://github.com/owner/repo.git"} /><Show when={dialog() === "local"}><Button onClick={async () => { try { const r = await rpc("app.pick_folder", { prompt: "Choose a Git repository" }); if (r.path) setPath(r.path); } catch (e) { toast(String(e), "danger"); } }}>Choose…</Button></Show></div></Field><Show when={dialog() === "clone"}><Field label="Folder name (optional)"><Input value={name()} onValue={setName} placeholder="Inferred from URL" /></Field></Show><Show when={error()}><p class="ws-alert">{error()}</p></Show></Modal></Show>
    <Show when={diff()}>{d => <Modal title={`Diff: ${d().branch}`} wide onClose={() => setDiff(null)}><Show when={d().truncated}><div class="ws-alert">Diff capped at 256 KiB. Open the worktree to inspect all changes.</div></Show><p>{d().note}</p><pre class="ws-diff">{d().stat || "No tracked changes"}</pre><pre class="ws-diff">{d().patch}</pre><h3>Dirty entries, including untracked files</h3><pre class="ws-diff">{d().files?.join("\n") || "None"}</pre></Modal>}</Show>
  </Page>;
}
