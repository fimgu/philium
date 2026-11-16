// Code: structural index of a repo (symbols, references, graph) with per-worktree overlays.
import { For, Show, createResource, createSignal, createMemo, createEffect, onCleanup, on } from "solid-js";
import { Page, Button, Badge, Select, Input, Segmented, Toggle, Empty, VirtualList } from "../ui/kit.jsx";
import Graph from "../ui/graph.jsx";
import CodeInspector from "../ui/code-inspector.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { params } from "../lib/nav.js";
import { agentHue, AgentChip } from "./agents.jsx";
import "../styles/code.css";

const SEV = { same_symbol: ["danger", "same symbol"], dependent: ["strict", "dependent code"], same_file: ["strict", "same file"] };
const sevOf = (s) => SEV[s] ?? ["strict", String(s ?? "overlap").replace(/_/g, " ")];
const basename = (p) => p.split("/").pop();
const dirname = (p) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/") + 1) : "");

// Calm palette that reads well on a dark canvas, in legend order.
const PALETTE = ["#7aa2f7", "#9ece6a", "#e0af68", "#bb9af7", "#7dcfff", "#f7768e", "#73daca", "#ff9e64", "#c0caf5", "#b4f9f8", "#d7a6ff", "#a9b665"];
const OTHER = "#6b7280";
const groupOf = (nd) => {
  const p = String(nd.path ?? nd.id).split(":")[0].split("/");
  p.pop();
  if (!p.length) return "(root)";
  return p.slice(0, 2).join("/");
};
const SETTINGS_KEY = "philium.code.graph";
const DEFAULTS = { link: 1, repel: 1, center: 1, size: 1, thickness: 1, arrows: false, depth: 1, local: false, display: false };
const loadSettings = () => { try { return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(SETTINGS_KEY) || "{}") }; } catch { return { ...DEFAULTS }; } };

function Slider(props) {
  return (
    <label class="gslider">
      <span class="lab">{props.label}<b>{props.fmt ? props.fmt(props.value) : props.value.toFixed(1)}</b></span>
      <input type="range" min={props.min ?? 0.1} max={props.max ?? 2} step={props.step ?? 0.05} value={props.value} onInput={(e) => props.onValue(+e.currentTarget.value)} />
    </label>
  );
}

function GraphView(props) {
  const [cfg, setCfg] = createSignal(loadSettings());
  const set = (k, v) => setCfg((c) => { const n = { ...c, [k]: v }; try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(n)); } catch {} return n; });
  const [hidden, setHidden] = createSignal(new Set());
  const [gq, setGq] = createSignal("");
  const [hover, setHover] = createSignal(null);
  let api;

  const all = createMemo(() => {
    const nodes = (props.data?.nodes ?? []).filter((n) => !props.changesOnly || props.changedPaths?.has(n.path ?? n.id)).map((n) => ({ ...n, group: groupOf(n), dir: n.group, weight: Math.max(n.weight ?? 0, props.level === "file" ? Math.sqrt(n.syms ?? 0) : 0) }));
    const ids = new Set(nodes.map((n) => n.id));
    return { nodes, edges: (props.data?.edges ?? []).filter((e) => ids.has(e.source) && ids.has(e.target)) };
  });
  const groups = createMemo(() => {
    const m = new Map();
    for (const n of all().nodes) m.set(n.group, (m.get(n.group) ?? 0) + 1);
    const list = [...m.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
    return list.map(([g, c], i) => ({ g, c, color: i < PALETTE.length ? PALETTE[i] : OTHER }));
  });
  const colorOf = createMemo(() => { const m = new Map(groups().map((x) => [x.g, x.color])); return (n) => m.get(n.group) ?? OTHER; });

  // Local mode: selected node plus N hops.
  const view = createMemo(() => {
    const { nodes, edges } = all();
    const c = cfg();
    const withRing = (ns) => ns.map((n) => props.showChanges && props.changedPaths?.has(n.path ?? n.id) ? { ...n, ring: "#e0af68" } : (n.changed_by?.length ? { ...n, ring: `hsl(${agentHue(n.changed_by[0].agent)} 80% 66%)` } : n));
    if (!c.local || !props.selected) return { nodes: withRing(nodes), edges };
    const adj = new Map();
    for (const e of edges) { (adj.get(e.source) ?? adj.set(e.source, []).get(e.source)).push(e.target); (adj.get(e.target) ?? adj.set(e.target, []).get(e.target)).push(e.source); }
    const keep = new Set([props.selected]);
    let front = [props.selected];
    for (let d = 0; d < c.depth; d++) { const nx = []; for (const id of front) for (const o of adj.get(id) ?? []) if (!keep.has(o)) { keep.add(o); nx.push(o); } front = nx; }
    return { nodes: withRing(nodes.filter((n) => keep.has(n.id))), edges: edges.filter((e) => keep.has(e.source) && keep.has(e.target)) };
  });
  const matches = createMemo(() => {
    const t = gq().trim().toLowerCase();
    if (!t) return new Set();
    return new Set(view().nodes.filter((n) => String(n.label ?? "").toLowerCase().includes(t) || String(n.path ?? n.id).toLowerCase().includes(t)).map((n) => n.id));
  });
  createEffect(on(() => [cfg().local, cfg().depth, props.selected], () => cfg().local && queueMicrotask(() => setTimeout(() => api?.fit(), 450)), { defer: true }));
  createEffect(on(() => props.level, () => setHidden(new Set()), { defer: true }));
  const toggleGroup = (g, solo) => setHidden((h) => {
    if (solo) { const only = groups().map((x) => x.g).filter((x) => x !== g); return h.size === only.length && !h.has(g) ? new Set() : new Set(only); }
    const n = new Set(h); n.has(g) ? n.delete(g) : n.add(g); return n;
  });
  const settings = createMemo(() => { const c = cfg(); return { link: c.link, repel: c.repel, center: c.center, size: c.size, thickness: c.thickness, arrows: c.arrows }; });
  const nNodes = () => view().nodes.filter((n) => !hidden().has(n.group)).length;

  return (
    <div class="code-graph">
      <Graph nodes={view().nodes} edges={view().edges} hidden={hidden()} color={colorOf()} selected={props.selected} match={matches()} settings={settings()}
        onSelect={(n) => props.onSelect(n)} onOpen={(n) => props.onOpen?.(n)} onHover={setHover} api={(a) => (api = a)} />
      <div class="gtools">
        <Segmented value={props.level} onChange={props.onLevel} options={[{ value: "file", label: "Files" }, { value: "symbol", label: "Symbols" }]} />
        <Segmented value={cfg().local ? "local" : "global"} onChange={(v) => set("local", v === "local")} options={[{ value: "global", label: "Global" }, { value: "local", label: "Local" }]} />
        <div class="gsearch">
          <Icon name="search" size={12} />
          <input placeholder="Find in graph" value={gq()} onInput={(e) => setGq(e.currentTarget.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && matches().size) api?.fit([...matches()]); if (e.key === "Escape") setGq(""); }} />
          <Show when={gq()}><span class="cnt">{matches().size}</span></Show>
        </div>
      </div>
      <div class="gzoom">
        <button title="Zoom in" onClick={() => api?.zoom(1.3)}>+</button>
        <button title="Zoom out" onClick={() => api?.zoom(1 / 1.3)}>-</button>
        <button title="Fit to view" onClick={() => api?.fit()}><svg width="12" height="12" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M3 7V3h4M13 3h4v4M17 13v4h-4M7 17H3v-4" /></svg></button>
        <button title="Display settings" class={cfg().display ? "on" : ""} onClick={() => set("display", !cfg().display)}><Icon name="settings" size={12} /></button>
      </div>
      <Show when={cfg().display}>
        <div class="gpanel">
          <div class="gph">Display</div>
          <Show when={cfg().local}>
            <Slider label="Depth" min={1} max={4} step={1} value={cfg().depth} fmt={(v) => String(v)} onValue={(v) => set("depth", v)} />
            <Show when={!props.selected}><div class="ghint">Select a node to see its neighbourhood.</div></Show>
          </Show>
          <Slider label="Node size" value={cfg().size} min={0.4} max={2.5} onValue={(v) => set("size", v)} />
          <Slider label="Link thickness" value={cfg().thickness} min={0.2} max={3} onValue={(v) => set("thickness", v)} />
          <Toggle on={cfg().arrows} label="Arrows" onChange={(v) => set("arrows", v)} />
          <div class="gph">Forces</div>
          <Slider label="Center force" value={cfg().center} min={0} max={3} onValue={(v) => set("center", v)} />
          <Slider label="Repel force" value={cfg().repel} min={0.1} max={4} onValue={(v) => set("repel", v)} />
          <Slider label="Link force" value={cfg().link} min={0.05} max={2} onValue={(v) => set("link", v)} />
          <button class="greset" onClick={() => { const d = cfg().display, l = cfg().local; setCfg({ ...DEFAULTS, display: d, local: l }); localStorage.removeItem(SETTINGS_KEY); api?.reheat(); }}>Restore defaults</button>
        </div>
      </Show>
      <Show when={hover()}>
        <div class="ghover">
          <b>{hover().label}</b>
          <span class="mono">{hover().path ?? hover().id}</span>
          <span>{hover().refs_in ?? 0} in, {hover().refs_out ?? 0} out{props.level === "file" && hover().syms != null ? `, ${hover().syms} symbols` : ""}</span>
        </div>
      </Show>
      <Show when={props.data?.truncated}><div class="note">Showing the most connected part of a larger graph</div></Show>
      <Show when={groups().length}>
        <div class="legend">
          <For each={groups()}>{(x) => (
            <button class={hidden().has(x.g) ? "off" : ""} title="Click to hide, shift click to show only this group" onClick={(e) => toggleGroup(x.g, e.shiftKey || e.altKey)}>
              <i style={{ background: x.color }} />{x.g}<span class="c">{x.c}</span>
            </button>
          )}</For>
          <span class="meta">{nNodes()} nodes, size = references</span>
        </div>
      </Show>
      <Show when={!(props.data?.nodes ?? []).length}><div class="code-empty center">Nothing to draw yet. The index fills in after the first scan.</div></Show>
    </div>
  );
}

function RefList(props) {
  const grouped = (list) => {
    const m = new Map();
    for (const r of list) { if (!m.has(r.path)) m.set(r.path, []); m.get(r.path).push(r); }
    return [...m.entries()];
  };
  const exact = () => (props.data?.refs ?? []).filter((r) => r.exact);
  const possible = () => (props.data?.refs ?? []).filter((r) => !r.exact);
  const Group = (g) => (
    <For each={grouped(g.items)}>
      {([path, rs]) => (
        <>
          <button class="ref-file" onClick={() => props.onJump?.({ path, line: 1 })}>{path}</button>
          <For each={rs}>{(r) => (
            <div class={`ref ${g.possible ? "possible" : ""}`} role="button" tabindex="0" onClick={() => props.onJump?.(r)} onKeyDown={(e) => e.key === "Enter" && props.onJump?.(r)}>
              <span class="loc">{r.path}:{r.line}</span>
              <span class="small faint"> {r.kind}{r.from ? ` in ${r.from}` : ""}</span>
              <Show when={g.possible}><span class="tag">name match</span></Show>
              <div class="src">{(r.text ?? "").trim()}</div>
            </div>
          )}</For>
        </>
      )}
    </For>
  );
  return (
    <>
      <div class="dsec">Definition</div>
      <Show when={(props.data?.defs ?? []).length} fallback={<div class="code-empty">No definition found in this view.</div>}>
        <For each={props.data.defs}>{(d) => <div class="ref" role="button" tabindex="0" onClick={() => props.onJump?.(d)} onKeyDown={(e) => e.key === "Enter" && props.onJump?.(d)}><span class="loc">{d.path}:{d.line}</span><span class="small faint"> {d.kind}</span><div class="src">{d.sig || d.name}</div></div>}</For>
      </Show>
      <div class="dsec">Used here <Badge tone="secure">{exact().length} confirmed</Badge></div>
      <Show when={exact().length} fallback={<div class="code-empty">No confirmed uses.</div>}>{Group({ items: exact() })}</Show>
      <Show when={possible().length}>
        <div class="dsec">Possible <Badge tone="strict">{possible().length} name match</Badge></div>
        <div class="code-empty" style={{ padding: "0 14px 6px" }}>Same name only. Not confirmed to refer to this symbol.</div>
        {Group({ items: possible(), possible: true })}
      </Show>
    </>
  );
}

export default function Code() {
  const [repos] = createResource(() => rpc("workspace.repos_list").catch(() => []));
  const [repoId, setRepoId] = createSignal(params()?.repo_id ?? "");
  const repo = () => repoId() || repos()?.[0]?.id || "";
  const [wts] = createResource(repo, (r) => (r ? rpc("workspace.worktrees", { repo_id: r }).catch(() => []) : []));
  const [viewId, setViewId] = createSignal("main");
  const [level, setLevel] = createSignal("file");
  const [q, setQ] = createSignal("");
  const [open, setOpen] = createSignal(new Set());
  const [sel, setSel] = createSignal(null); // {id, symbol, label}
  const [busy, setBusy] = createSignal(false);
  const [tab, setTab] = createSignal("source");
  const [showChanges, setShowChanges] = createSignal(false);
  const [changesOnly, setChangesOnly] = createSignal(false);
  const [expanded, setExpanded] = createSignal(false);
  const [inspectorSize, setInspectorSize] = createSignal(360);
  let resizeCleanup;
  onCleanup(() => resizeCleanup?.());
  const resize = (event) => {
    event.preventDefault();
    const vertical = window.innerWidth <= 1450;
    const start = vertical ? event.clientY : event.clientX, initial = inspectorSize();
    const move = (e) => setInspectorSize(Math.max(vertical ? 180 : 300, Math.min(vertical ? window.innerHeight * .65 : window.innerWidth * .7, initial + start - (vertical ? e.clientY : e.clientX))));
    const stop = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", stop); };
    resizeCleanup?.(); resizeCleanup = stop;
    window.addEventListener("pointermove", move); window.addEventListener("pointerup", stop, { once: true });
  };

  const target = () => {
    const r = repo();
    if (!r) return null;
    return viewId() === "main" ? { repo_id: r } : viewId() === "local" ? { repo_id: r, working_tree: true } : { worktree_id: viewId() };
  };
  const key = () => { const t = target(); return t ? JSON.stringify([t, tick()]) : null; };
  const [tick, setTick] = createSignal(0);
  const [status] = createResource(key, () => rpc("codeindex.status", target()).catch((e) => ({ error: String(e) })));
  const [tree] = createResource(key, () => rpc("codeindex.tree", target()).catch(() => ({ files: [] })));
  const gkey = () => { const k = key(); return k ? k + level() : null; };
  const [graph] = createResource(gkey, () => rpc("codeindex.graph", { ...target(), level: level(), worktrees: true }).catch(() => ({ nodes: [], edges: [] })));
  const [changes] = createResource(key, () => rpc("codeindex.changes", target()).catch((e) => ({ files: [], error: String(e) })));
  const changedPaths = createMemo(() => new Set((changes()?.files ?? []).flatMap((f) => [f.path, f.old_path].filter(Boolean))));
  const targetLabel = () => viewId() === "main" ? "Indexed base (fixed commit)" : viewId() === "local" ? "Local checkout (working snapshot)" : `${agentName((wts() ?? []).find((w) => w.id === viewId()) ?? {})} worktree (working snapshot)`;
  const [clashes, clr] = createResource(repo, (r) => (r ? rpc("codeindex.clashes", { repo_id: r }).catch(() => ({ clashes: [] })) : { clashes: [] }));
  const [refs] = createResource(() => (sel()?.symbol ? JSON.stringify([target(), sel().symbol, tick()]) : null), () => rpc("codeindex.refs", { ...target(), symbol: sel().symbol }).catch((e) => ({ error: String(e), defs: [], refs: [] })));
  useEvent("codeindex:clashes", () => clr.refetch());
  createEffect(on(repo, () => { setViewId("main"); setSel(null); }, { defer: true }));
  createEffect(on(viewId, () => { setSel(null); setExpanded(false); setTab("source"); }, { defer: true }));

  const agentName = (w) => w.agent_name || w.branch;
  const rows = createMemo(() => {
    const t = q().trim().toLowerCase();
    const out = [];
    for (const f of tree()?.files ?? []) {
      const syms = f.syms ?? [];
      const symHits = t ? syms.filter((s) => (s.name ?? "").toLowerCase().includes(t)) : syms;
      const fileHit = t && f.path.toLowerCase().includes(t);
      if (t && !fileHit && !symHits.length) continue;
      out.push({ t: "file", f });
      if (open().has(f.path) || (t && symHits.length && !fileHit)) for (const s of symHits) out.push({ t: "sym", f, s });
    }
    return out;
  });
  const toggle = (p) => setOpen((o) => { const n = new Set(o); n.has(p) ? n.delete(p) : n.add(p); return n; });
  const pickNode = (nd) => { setTab("source"); const file = nd.path ?? nd.id; const indexed = (tree()?.files ?? []).find((f) => f.path === file)?.syms?.find((s) => s.name === nd.label); setSel(nd.kind === "file" ? { id: nd.id, file, label: nd.label } : { id: nd.id, symbol: nd.label, file, label: nd.label, line: nd.line ?? indexed?.line }); };
  const reindex = async () => { setBusy(true); try { await rpc("codeindex.reindex", target()); setTick((t) => t + 1); } finally { setBusy(false); } };
  const upToDate = () => !!status() && !status().error;

  return (
    <Page title="Code" sub="Symbols and references, indexed locally" flush>
      <div class="code-wrap">
        <div class="code-bar" style={repo() ? undefined : { display: "none" }}>
          <Select value={repo()} onValue={setRepoId} options={(repos() ?? []).map((r) => ({ value: r.id, label: r.name }))} />
          <Select value={viewId()} onValue={(v) => { setViewId(v); setSel(null); }}
            options={[{ value: "main", label: "Indexed base commit" }, { value: "local", label: "Local checkout" }, ...(wts() ?? []).filter((w) => w.state === "active").map((w) => ({ value: w.id, label: `${agentName(w)} (worktree)` }))]} />
          <Show when={viewId() !== "main" && viewId() !== "local"}><AgentChip name={agentName((wts() ?? []).find((w) => w.id === viewId()) ?? {})} /></Show>
          <div class="code-status">
            <Show when={status()?.error}><span class="err">{status().error}</span></Show>
            <Show when={upToDate()}>
              <span>base <span class="mono">{(status().base_commit ?? "").slice(0, 8)}</span></span>
              <span>{status().files} files, {status().syms} symbols</span>
              <Show when={status().worktree}><span>{status().worktree.delta_files} changed files</span></Show>
              <span class="row" style={{ color: "var(--secure)", gap: "4px" }}><span class="dot live" />up to date{status().built_at ? `, ${fmt.ago(status().built_at)}` : ""}</span>
            </Show>
          </div>
          <Toggle on={showChanges()} label={`Changes (${changes()?.files?.length ?? 0})`} onChange={setShowChanges} />
          <Show when={showChanges()}><Toggle on={changesOnly()} label="Changed nodes only" onChange={setChangesOnly} /></Show>
          <Button size="sm" disabled={busy()} onClick={reindex}><Icon name="refresh" size={12} /> {busy() ? "Indexing" : "Reindex"}</Button>
        </div>
        <Show when={(clashes()?.clashes ?? []).length}>
          <div class="code-clashes">
            <For each={clashes().clashes}>{(c) => (
              <div class="code-clash">
                <Badge tone={sevOf(c.severity)[0]}>{sevOf(c.severity)[1]}</Badge>
                <AgentChip name={c.a ?? "?"} /> and <AgentChip name={c.b ?? "?"} /> both change code that overlaps
                <span class="detail" title={(c.detail ?? []).join("\n")}>{(c.detail ?? []).slice(0, 2).join(", ")}</span>
              </div>
            )}</For>
          </div>
        </Show>
        <Show when={repo()} fallback={<Empty title="No repositories yet">Add a repository on the Repos screen. Philium indexes its symbols and references so you and your agents can see what depends on what.</Empty>}>
          <div class={`code-main ${sel() ? "has-inspector" : "no-drawer"} ${expanded() && sel() ? "inspector-expanded" : ""}`} style={{ "--inspector-size": `${inspectorSize()}px` }}>
            <div class="code-side">
              <div class="search"><Input placeholder="Search files and symbols" value={q()} onValue={setQ} /></div>
              <Show when={showChanges()}><section class="changed-files"><div class="dsec">Changes against base {(changes()?.base_commit ?? "").slice(0, 8)}</div><Show when={changes.loading}><div class="inspector-note">Loading changes…</div></Show><Show when={changes()?.error}><div class="inspector-note err">{changes().error}</div></Show><Show when={changes()?.note}><div class="inspector-note">{changes().note}</div></Show><Show when={changes()?.truncated}><div class="inspector-note">Changed file list truncated.</div></Show><For each={changes()?.files ?? []}>{(f) => <button class="changed-file" title={f.old_path ? `${f.old_path} → ${f.path}` : f.path} onClick={() => { setSel({ id: f.path, file: f.path, label: basename(f.path) }); setTab("diff"); }}><b>{f.status}</b><span>{f.old_path ? `${f.old_path} → ` : ""}{f.path}</span><small>+{f.added ?? 0} −{f.removed ?? 0}</small></button>}</For><Show when={!changes.loading && !changes()?.files?.length && !changes()?.note}><div class="inspector-note">No changes in this target.</div></Show></section></Show>
              <Show when={rows().length} fallback={<div class="code-empty">{tree.loading ? "Loading index" : q() ? "No matches." : "No indexed files in this view."}</div>}>
                <VirtualList items={rows()} rowHeight={24}>
                  {(r) => r.t === "file" ? (
                    <div class={`code-row file ${sel()?.id === r.f.path ? "on" : ""}`} onClick={() => { toggle(r.f.path); setTab("source"); setSel({ id: r.f.path, file: r.f.path, label: basename(r.f.path) }); }}>
                      <span class="cv">{open().has(r.f.path) ? "v" : ">"}</span>
                      <span class="name" title={r.f.path}><span class="dir">{dirname(r.f.path)}</span>{basename(r.f.path)}</span>
                      <span class="xs faint">{(r.f.syms ?? []).length}</span>
                    </div>
                  ) : (
                    <div class={`code-row sym ${sel()?.id === `${r.f.path}:${r.s.name}` ? "on" : ""}`} style={{ "padding-left": "28px" }} onClick={() => { setTab("source"); setSel({ id: `${r.f.path}:${r.s.name}`, symbol: r.s.name, file: r.f.path, label: r.s.name, line: r.s.line }); }}>
                      <span class="kind">{(r.s.kind ?? "sym").slice(0, 3)}</span>
                      <span class="name">{r.s.parent ? `${r.s.parent}.` : ""}{r.s.name}</span>
                      <span class="xs faint">{r.s.line}</span>
                    </div>
                  )}
                </VirtualList>
              </Show>
              <Show when={tree()?.truncated}><div class="xs faint" style={{ padding: "6px 10px" }}>Showing top-ranked files only. Search to narrow.</div></Show>
            </div>
            <GraphView showChanges={showChanges()} changesOnly={showChanges() && changesOnly()} changedPaths={changedPaths()} data={graph()} level={level()} onLevel={setLevel} selected={sel()?.id} onSelect={(n) => (n ? pickNode(n) : setSel(null))} onOpen={(n) => { if (n.kind === "file") { setOpen((o) => new Set(o).add(n.id)); setLevel("symbol"); } }} />
            <Show when={sel()}>
              <div class="inspector-slot">
                <div class="inspector-resize" role="separator" tabindex="0" aria-label="Resize code inspector" onPointerDown={resize} onKeyDown={(e) => { if (["ArrowLeft", "ArrowUp", "ArrowRight", "ArrowDown"].includes(e.key)) { e.preventDefault(); setInspectorSize((s) => Math.max(180, s + (["ArrowLeft", "ArrowUp"].includes(e.key) ? 30 : -30))); } }} />
                <CodeInspector selected={sel()} target={target()} targetAgentId={(wts() ?? []).find((w) => w.id === viewId())?.agent_id} targetLabel={targetLabel()} repoId={repo()} baseCommit={status()?.base_commit} tab={tab()} onTab={setTab} expanded={expanded()} onExpand={() => setExpanded(!expanded())} onClose={() => { setSel(null); setExpanded(false); }} tick={tick()}>
                  <Show when={sel()?.symbol} fallback={<div class="code-empty">Select a symbol to inspect its references. File contents are in Source.</div>}>
                    <Show when={!refs.loading} fallback={<div class="code-empty">Loading references…</div>}>
                      <Show when={!refs()?.error} fallback={<div class="code-empty err">{refs().error}</div>}><RefList data={refs()} onJump={(r) => { setSel({ id: r.path, file: r.path, label: basename(r.path), line: r.line }); setTab("source"); }} /></Show>
                    </Show>
                  </Show>
                </CodeInspector>
              </div>
            </Show>
          </div>
        </Show>
      </div>
    </Page>
  );
}
