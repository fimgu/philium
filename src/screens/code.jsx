// Code: structural index of a repo (symbols, references, graph) with per-worktree overlays.
import { For, Show, createResource, createSignal, createMemo, createEffect, onCleanup, on } from "solid-js";
import { Page, Button, Badge, Select, Input, Segmented, Empty, VirtualList } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { params } from "../lib/nav.js";
import { agentHue, AgentChip } from "./agents.jsx";
import "../styles/code.css";

const SEV = { same_symbol: ["danger", "same symbol"], dependent: ["strict", "dependent code"], same_file: ["strict", "same file"] };
const sevOf = (s) => SEV[s] ?? ["strict", String(s).replace(/_/g, " ")];
const basename = (p) => p.split("/").pop();
const dirname = (p) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/") + 1) : "");

/** Deterministic force layout, computed once. Falls back to grouped grid when huge. */
export function layout(nodes, edges) {
  const n = nodes.length;
  const pos = new Map();
  const seed = (s) => { let h = 2166136261; for (const c of s) h = Math.imul(h ^ c.charCodeAt(0), 16777619); return ((h >>> 0) % 10000) / 10000; };
  const R = Math.sqrt(n) * 38 + 40;
  nodes.forEach((nd) => pos.set(nd.id, { x: (seed(nd.id) - 0.5) * 2 * R, y: (seed(nd.id + "y") - 0.5) * 2 * R, vx: 0, vy: 0 }));
  if (n > 450) {
    const groups = [...new Set(nodes.map((x) => x.group))];
    const gc = Math.ceil(Math.sqrt(groups.length));
    nodes.forEach((nd, i) => {
      const gi = groups.indexOf(nd.group);
      const p = pos.get(nd.id);
      p.x = (gi % gc) * 260 + (seed(nd.id) - 0.5) * 220;
      p.y = Math.floor(gi / gc) * 260 + (seed(nd.id + "y") - 0.5) * 220;
    });
    return pos;
  }
  const list = nodes.map((nd) => pos.get(nd.id));
  const idx = new Map(nodes.map((nd, i) => [nd.id, i]));
  const es = edges.map((e) => [idx.get(e.source), idx.get(e.target)]).filter(([a, b]) => a != null && b != null && a !== b);
  const grp = nodes.map((nd) => nd.group);
  for (let it = 0; it < 220; it++) {
    const cool = 1 - it / 220;
    for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) {
      const a = list[i], b = list[j];
      let dx = a.x - b.x, dy = a.y - b.y;
      let d2 = dx * dx + dy * dy + 0.01;
      if (d2 > 160000) continue;
      const f = 2200 / d2, d = Math.sqrt(d2);
      dx = (dx / d) * f; dy = (dy / d) * f;
      a.vx += dx; a.vy += dy; b.vx -= dx; b.vy -= dy;
    }
    for (const [i, j] of es) {
      const a = list[i], b = list[j];
      const dx = b.x - a.x, dy = b.y - a.y, d = Math.sqrt(dx * dx + dy * dy) + 0.01;
      const f = (d - (grp[i] === grp[j] ? 40 : 80)) * 0.04;
      a.vx += (dx / d) * f; a.vy += (dy / d) * f; b.vx -= (dx / d) * f; b.vy -= (dy / d) * f;
    }
    for (const p of list) {
      p.vx -= p.x * 0.004; p.vy -= p.y * 0.004;
      p.x += Math.max(-30, Math.min(30, p.vx)) * cool; p.y += Math.max(-30, Math.min(30, p.vy)) * cool;
      p.vx *= 0.6; p.vy *= 0.6;
    }
  }
  return pos;
}

function Graph(props) {
  const [view, setView] = createSignal({ x: 0, y: 0, k: 1 });
  let svg, drag;
  const pos = createMemo(() => layout(props.data?.nodes ?? [], props.data?.edges ?? []));
  const maxRank = createMemo(() => Math.max(1e-9, ...(props.data?.nodes ?? []).map((x) => x.rank ?? 0)));
  const fit = () => {
    const ps = [...pos().values()];
    if (!ps.length || !svg) return;
    const xs = ps.map((p) => p.x), ys = ps.map((p) => p.y);
    const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
    const w = svg.clientWidth || 600, h = svg.clientHeight || 400;
    const k = Math.min(2, 0.9 * Math.min(w / Math.max(60, x1 - x0), h / Math.max(60, y1 - y0)));
    setView({ k, x: w / 2 - ((x0 + x1) / 2) * k, y: h / 2 - ((y0 + y1) / 2) * k });
  };
  createEffect(on(pos, () => queueMicrotask(fit)));
  const hot = createMemo(() => {
    const s = props.selected; const set = new Set();
    if (!s) return set;
    for (const e of props.data?.edges ?? []) { if (e.source === s) set.add(e.target); if (e.target === s) set.add(e.source); }
    return set;
  });
  const radius = (nd) => 4 + 7 * Math.sqrt((nd.rank ?? 0) / maxRank());
  const fill = (nd) => (nd.changed_by?.length ? `hsl(${agentHue(nd.changed_by[0].agent)} 75% 62%)` : null);
  const agents = createMemo(() => {
    const m = new Map();
    for (const nd of props.data?.nodes ?? []) for (const c of nd.changed_by ?? []) m.set(c.agent, (m.get(c.agent) ?? 0) + 1);
    return [...m.entries()];
  });
  const zoomAt = (f, cx, cy) => setView((v) => { const k = Math.max(0.05, Math.min(6, v.k * f)); const r = k / v.k; return { k, x: cx - (cx - v.x) * r, y: cy - (cy - v.y) * r }; });
  const showLabel = (nd) => view().k > 1.4 || props.selected === nd.id || hot().has(nd.id) || (nd.rank ?? 0) / maxRank() > (view().k > 0.9 ? 0.5 : 0.75) || nd.changed_by?.length;
  return (
    <div class="code-graph">
      <svg ref={svg}
        onWheel={(e) => { e.preventDefault(); const r = svg.getBoundingClientRect(); zoomAt(e.deltaY < 0 ? 1.12 : 1 / 1.12, e.clientX - r.left, e.clientY - r.top); }}
        onPointerDown={(e) => { if (e.target.closest(".gnode")) return; drag = { x: e.clientX, y: e.clientY, v: view() }; svg.setPointerCapture(e.pointerId); }}
        onPointerMove={(e) => { if (drag) setView({ ...drag.v, x: drag.v.x + e.clientX - drag.x, y: drag.v.y + e.clientY - drag.y }); }}
        onPointerUp={() => (drag = null)}>
        <g transform={`translate(${view().x} ${view().y}) scale(${view().k})`}>
          <For each={props.data?.edges ?? []}>
            {(e) => {
              const a = () => pos().get(e.source), b = () => pos().get(e.target);
              return <Show when={a() && b()}><line class={`gedge ${props.selected && (e.source === props.selected || e.target === props.selected) ? "hot" : ""}`} x1={a().x} y1={a().y} x2={b().x} y2={b().y} stroke-width={(1 + Math.min(3, Math.log2(1 + (e.weight ?? 1)))) / view().k ** 0.5} /></Show>;
            }}
          </For>
          <For each={props.data?.nodes ?? []}>
            {(nd) => {
              const p = () => pos().get(nd.id);
              return (
                <g onClick={() => props.onSelect(nd)}>
                  <circle class={`gnode ${props.selected === nd.id ? "sel" : ""}`} cx={p().x} cy={p().y} r={radius(nd)} style={fill(nd) ? { fill: fill(nd) } : {}}>
                    <title>{nd.id}{nd.changed_by?.length ? ` (changed by ${nd.changed_by.map((c) => c.agent).join(", ")})` : ""}</title>
                  </circle>
                  <Show when={showLabel(nd)}><text class="glabel" x={p().x + radius(nd) + 3} y={p().y + 3}>{nd.label}</text></Show>
                </g>
              );
            }}
          </For>
        </g>
      </svg>
      <div class="gtools">
        <Segmented value={props.level} onChange={props.onLevel} options={[{ value: "file", label: "Files" }, { value: "symbol", label: "Symbols" }]} />
        <Button size="sm" onClick={() => zoomAt(1.3, svg.clientWidth / 2, svg.clientHeight / 2)}>+</Button>
        <Button size="sm" onClick={() => zoomAt(1 / 1.3, svg.clientWidth / 2, svg.clientHeight / 2)}>-</Button>
        <Button size="sm" onClick={fit}>Fit</Button>
      </div>
      <Show when={props.data?.truncated}><div class="note">Showing the most connected part of a larger graph</div></Show>
      <div class="legend">
        <span><i style={{ background: "#3a4050" }} />unchanged, size = how much depends on it</span>
        <For each={agents()}>{([a, c]) => <span><i style={{ background: `hsl(${agentHue(a)} 75% 62%)` }} />{a} changed {c}</span>}</For>
      </div>
      <Show when={!(props.data?.nodes ?? []).length}><div class="code-empty">Nothing to draw yet. The index fills in after the first scan.</div></Show>
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
          <div class="ref-file">{path}</div>
          <For each={rs}>{(r) => (
            <div class={`ref ${g.possible ? "possible" : ""}`}>
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
        <For each={props.data.defs}>{(d) => <div class="ref"><span class="loc">{d.path}:{d.line}</span><span class="small faint"> {d.kind}</span><div class="src">{d.sig || d.name}</div></div>}</For>
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

  const target = () => {
    const r = repo();
    if (!r) return null;
    return viewId() === "main" ? { repo_id: r } : { worktree_id: viewId() };
  };
  const key = () => { const t = target(); return t ? JSON.stringify([t, tick()]) : null; };
  const [tick, setTick] = createSignal(0);
  const [status] = createResource(key, () => rpc("codeindex.status", target()).catch((e) => ({ error: String(e) })));
  const [tree] = createResource(key, () => rpc("codeindex.tree", target()).catch(() => ({ files: [] })));
  const gkey = () => { const k = key(); return k ? k + level() : null; };
  const [graph] = createResource(gkey, () => rpc("codeindex.graph", { ...target(), level: level(), worktrees: true }).catch(() => ({ nodes: [], edges: [] })));
  const [clashes, clr] = createResource(repo, (r) => (r ? rpc("codeindex.clashes", { repo_id: r }).catch(() => ({ clashes: [] })) : { clashes: [] }));
  const [refs] = createResource(() => (sel()?.symbol ? JSON.stringify([target(), sel().symbol, tick()]) : null), () => rpc("codeindex.refs", { ...target(), symbol: sel().symbol }).catch((e) => ({ error: String(e), defs: [], refs: [] })));
  useEvent("codeindex:clashes", () => clr.refetch());
  createEffect(on(repo, () => { setViewId("main"); setSel(null); }, { defer: true }));

  const agentName = (w) => w.agent_name || w.branch;
  const rows = createMemo(() => {
    const t = q().trim().toLowerCase();
    const out = [];
    for (const f of tree()?.files ?? []) {
      const symHits = t ? f.syms.filter((s) => s.name.toLowerCase().includes(t)) : f.syms;
      const fileHit = t && f.path.toLowerCase().includes(t);
      if (t && !fileHit && !symHits.length) continue;
      out.push({ t: "file", f });
      if (open().has(f.path) || (t && symHits.length && !fileHit)) for (const s of symHits) out.push({ t: "sym", f, s });
    }
    return out;
  });
  const toggle = (p) => setOpen((o) => { const n = new Set(o); n.has(p) ? n.delete(p) : n.add(p); return n; });
  const pickNode = (nd) => setSel(nd.kind === "file" ? { id: nd.id, file: nd.id, label: nd.label } : { id: nd.id, symbol: nd.label, file: nd.path, label: nd.label });
  const reindex = async () => { setBusy(true); try { await rpc("codeindex.reindex", target()); setTick((t) => t + 1); } finally { setBusy(false); } };
  const upToDate = () => !!status() && !status().error;

  return (
    <Page title="Code" sub="Symbols and references, indexed locally" flush>
      <div class="code-wrap">
        <div class="code-bar" style={repo() ? undefined : { display: "none" }}>
          <Select value={repo()} onValue={setRepoId} options={(repos() ?? []).map((r) => ({ value: r.id, label: r.name }))} />
          <Select value={viewId()} onValue={(v) => { setViewId(v); setSel(null); }}
            options={[{ value: "main", label: "Main (base commit)" }, ...(wts() ?? []).filter((w) => w.state === "active").map((w) => ({ value: w.id, label: `${agentName(w)} (worktree)` }))]} />
          <Show when={viewId() !== "main"}><AgentChip name={agentName((wts() ?? []).find((w) => w.id === viewId()) ?? {})} /></Show>
          <div class="code-status">
            <Show when={status()?.error}><span class="err">{status().error}</span></Show>
            <Show when={upToDate()}>
              <span>base <span class="mono">{(status().base_commit ?? "").slice(0, 8)}</span></span>
              <span>{status().files} files, {status().syms} symbols</span>
              <Show when={status().worktree}><span>{status().worktree.delta_files} changed files</span></Show>
              <span class="row" style={{ color: "var(--secure)", gap: "4px" }}><span class="dot live" />up to date{status().built_at ? `, ${fmt.ago(status().built_at)}` : ""}</span>
            </Show>
          </div>
          <Button size="sm" disabled={busy()} onClick={reindex}><Icon name="refresh" size={12} /> {busy() ? "Indexing" : "Reindex"}</Button>
        </div>
        <Show when={(clashes()?.clashes ?? []).length}>
          <div class="code-clashes">
            <For each={clashes().clashes}>{(c) => (
              <div class="code-clash">
                <Badge tone={sevOf(c.severity)[0]}>{sevOf(c.severity)[1]}</Badge>
                <AgentChip name={c.a} /> and <AgentChip name={c.b} /> both change code that overlaps
                <span class="detail" title={(c.detail ?? []).join("\n")}>{(c.detail ?? []).slice(0, 2).join(", ")}</span>
              </div>
            )}</For>
          </div>
        </Show>
        <Show when={repo()} fallback={<Empty title="No repositories yet">Add a repository on the Repos screen. Philium indexes its symbols and references so you and your agents can see what depends on what.</Empty>}>
          <div class={`code-main ${sel() ? "" : "no-drawer"}`}>
            <div class="code-side">
              <div class="search"><Input placeholder="Search files and symbols" value={q()} onValue={setQ} /></div>
              <Show when={rows().length} fallback={<div class="code-empty">{tree.loading ? "Loading index" : q() ? "No matches." : "No indexed files in this view."}</div>}>
                <VirtualList items={rows()} rowHeight={24}>
                  {(r) => r.t === "file" ? (
                    <div class={`code-row file ${sel()?.id === r.f.path ? "on" : ""}`} onClick={() => { toggle(r.f.path); setSel({ id: r.f.path, file: r.f.path, label: basename(r.f.path) }); }}>
                      <span class="cv">{open().has(r.f.path) ? "v" : ">"}</span>
                      <span class="name" title={r.f.path}><span class="dir">{dirname(r.f.path)}</span>{basename(r.f.path)}</span>
                      <span class="xs faint">{r.f.syms.length}</span>
                    </div>
                  ) : (
                    <div class={`code-row sym ${sel()?.id === `${r.f.path}:${r.s.name}` ? "on" : ""}`} style={{ "padding-left": "28px" }} onClick={() => setSel({ id: `${r.f.path}:${r.s.name}`, symbol: r.s.name, file: r.f.path, label: r.s.name })}>
                      <span class="kind">{r.s.kind.slice(0, 3)}</span>
                      <span class="name">{r.s.parent ? `${r.s.parent}.` : ""}{r.s.name}</span>
                      <span class="xs faint">{r.s.line}</span>
                    </div>
                  )}
                </VirtualList>
              </Show>
              <Show when={tree()?.truncated}><div class="xs faint" style={{ padding: "6px 10px" }}>Showing top-ranked files only. Search to narrow.</div></Show>
            </div>
            <Graph data={graph()} level={level()} onLevel={setLevel} selected={sel()?.id} onSelect={pickNode} />
            <Show when={sel()}>
              <aside class="code-drawer">
                <div class="dhead">
                  <div class="row"><b class="mono grow ellipsis">{sel().symbol ?? sel().label}</b><Button size="sm" variant="ghost" onClick={() => setSel(null)}>Close</Button></div>
                  <div class="xs faint mono ellipsis">{sel().file}</div>
                </div>
                <Show when={sel().symbol} fallback={
                  <div class="code-empty">This is a file. Pick one of its symbols in the tree, or a symbol node in the symbol graph, to see where it is used.
                    <div style={{ "margin-top": "10px" }}><Button size="sm" onClick={() => { setOpen((o) => new Set(o).add(sel().file)); }}>Show its symbols</Button></div></div>}>
                  <Show when={!refs.loading} fallback={<div class="code-empty">Loading</div>}>
                    <Show when={!refs()?.error} fallback={<div class="code-empty err">{refs().error}</div>}><RefList data={refs()} /></Show>
                  </Show>
                </Show>
              </aside>
            </Show>
          </div>
        </Show>
      </div>
    </Page>
  );
}
