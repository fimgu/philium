import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { rpc } from "../lib/rpc.js";
import { openCodeDraft } from "../lib/code-context.js";
import { VirtualList } from "./kit.jsx";
import { AnsiDiff } from "./ansi-diff.jsx";

// Tokens remain ordinary JSX text nodes, never HTML. Covers Python and common C-like languages.
const tokens = (text) => text.split(/("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\/\/.*$|#.*$|\b(?:def|class|import|from|as|return|if|else|elif|for|while|in|is|not|and|or|None|True|False|async|await|try|except|finally|with|yield|raise|pass|const|let|var|function|export|default|new|throw|catch|public|private|fn|pub|use|struct|impl|match)\b|\b\d+(?:\.\d+)?\b)/g).map((s) => ({ text: s, kind: /^["']/.test(s) ? "string" : /^(#|\/\/)/.test(s) ? "comment" : /^\d/.test(s) ? "number" : /^(def|class|import|from|as|return|if|else|elif|for|while|in|is|not|and|or|None|True|False|async|await|try|except|finally|with|yield|raise|pass|const|let|var|function|export|default|new|throw|catch|public|private|fn|pub|use|struct|impl|match)$/.test(s) ? "keyword" : "" }));

const LINE_HEIGHT = 20;

// Emphasize the modified part of a paired line while preserving the syntax colors.
function changedSpans(before, after) {
  const a = before.match(/\w+|[^\w\s]|\s+/gu) ?? [];
  const b = after.match(/\w+|[^\w\s]|\s+/gu) ?? [];
  let start = 0, end = 0;
  while (start < Math.min(a.length, b.length) && a[start] === b[start]) start++;
  while (end < Math.min(a.length, b.length) - start && a.at(-1 - end) === b.at(-1 - end)) end++;
  return [a, b].map((parts) => [
    parts.slice(0, start).join(""),
    parts.slice(start, parts.length - end).join(""),
    parts.slice(parts.length - end).join(""),
  ]);
}

export default function CodeInspector(props) {
  const [data, setData] = createSignal(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [selection, setSelection] = createSignal(null);
  const [agents, setAgents] = createSignal([]);
  const [chooser, setChooser] = createSignal(null);
  const [agent, setAgent] = createSignal("");
  const [draftError, setDraftError] = createSignal("");
  const [layout, setLayout] = createSignal("inline");
  const [moreContext, setMoreContext] = createSignal(false);
  const [deltaMode, setDeltaMode] = createSignal(false);
  const [currentHunk, setCurrentHunk] = createSignal(-1);
  let generation = 0, body, vlist;
  const path = () => props.selected?.file;
  createEffect(() => {
    const target = props.target, file = path(), tab = props.tab;
    const options = tab === "diff" ? { comparison: props.comparison, more_context: moreContext(), ...(deltaMode() ? { render: "delta" } : {}) } : {};
    void props.tick;
    const request = ++generation;
    setData(null); setError(""); setSelection(null); setChooser(null); setLoading(false); setCurrentHunk(-1);
    if (!file || tab === "references") return;
    setLoading(true);
    rpc(tab === "diff" ? "codeindex.file_diff" : "codeindex.source", { ...target, path: file, ...options }).then((value) => {
      if (request === generation) setData(value);
    }).catch((e) => { if (request === generation) setError(String(e)); }).finally(() => {
      if (request === generation) setLoading(false);
    });
  });
  onCleanup(() => generation++);
  createEffect(() => {
    const target = props.target, repoId = props.repoId, targetAgentId = props.targetAgentId;
    let active = true;
    setAgents([]);
    rpc("agents.list").then((list) => { if (active) setAgents((list ?? []).filter((a) => target?.worktree_id ? a.worktree_id === target.worktree_id || a.id === targetAgentId : a.repo_id === repoId)); }).catch(() => {});
    onCleanup(() => { active = false; });
  });
  const lines = createMemo(() => String(props.tab === "diff" ? data()?.patch ?? "" : data()?.text ?? "").split("\n"));
  const diffLines = createMemo(() => {
    let old = 0, current = 0, oldRemaining = 0, newRemaining = 0, inHunk = false;
    return lines().map((text, index) => {
      const h = text.match(/^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/);
      if (h) {
        old = +h[1]; current = +h[3]; oldRemaining = +(h[2] ?? 1); newRemaining = +(h[4] ?? 1);
        inHunk = oldRemaining > 0 || newRemaining > 0;
        return { text, index, hunk: true };
      }
      // Header-looking content (--- or +++) inside a hunk is still a code line.
      // No-newline markers never consume either side's line count.
      if (text.startsWith("\\") || !inHunk) return { text, index };
      let row;
      if (text.startsWith("-")) { row = { text, index, old: old++, deletion: true }; oldRemaining--; }
      else if (text.startsWith("+")) { row = { text, index, line: current++, addition: true }; newRemaining--; }
      else if (text.startsWith(" ")) { row = { text, index, old: old++, line: current++ }; oldRemaining--; newRemaining--; }
      else return { text, index };
      if (oldRemaining <= 0 && newRemaining <= 0) inHunk = false;
      return row;
    });
  });
  const sourceStart = createMemo(() => Math.max(0, Math.min(lines().length - 3000, (props.selected?.line ?? 1) - 500)));
  const visibleSource = createMemo(() => lines().slice(sourceStart(), sourceStart() + 3000).map((text, i) => ({ text, index: sourceStart() + i, line: sourceStart() + i + 1 })));
  const displayedSource = createMemo(() => visibleSource().map((row) => ({ both: row })));
  const deltaText = createMemo(() => deltaMode() && props.tab === "diff" && data()?.delta ? data().delta : null);
  const visibleDiff = createMemo(() => diffLines().slice(0, 3000));
  const paired = createMemo(() => {
    const inline = new Map();
    const rows = visibleDiff();
    const split = [];
    for (let i = 0; i < rows.length;) {
      const row = rows[i];
      if (!row.addition && !row.deletion) {
        split.push({ both: row });
        i++;
        continue;
      }
      const removed = [], added = [];
      while (rows[i]?.deletion) removed.push(rows[i++]);
      while (rows[i]?.addition) added.push(rows[i++]);
      for (let j = 0; j < Math.max(removed.length, added.length); j++) {
        const old = removed[j], next = added[j];
        if (old && next && old.text.length + next.text.length < 2000) {
          const [left, right] = changedSpans(old.text.slice(1), next.text.slice(1));
          inline.set(old.index, left);
          inline.set(next.index, right);
        }
        split.push({ old, next });
      }
    }
    return { inline, split };
  });
  const displayedDiff = createMemo(() => layout() === "split" ? paired().split : visibleDiff().map((row) => ({ both: row })));
  const hunkRows = createMemo(() => displayedDiff().flatMap((row, i) => row.both?.hunk ? [i] : []));
  const jumpHunk = (delta) => {
    const hunks = hunkRows();
    if (!vlist || !hunks.length) return;
    const next = Math.max(0, Math.min(hunks.length - 1, currentHunk() + delta));
    setCurrentHunk(next);
    vlist.scrollTop = hunks[next] * LINE_HEIGHT;
    vlist.dispatchEvent(new Event("scroll"));
  };
  createEffect(() => {
    const line = props.selected?.line, ready = data();
    if (ready && line && props.tab === "source") {
      queueMicrotask(() => {
        if (vlist) {
          const offset = Math.max(0, (Number(line) - 1 - sourceStart()) * LINE_HEIGHT);
          vlist.scrollTop = Math.max(0, offset - (vlist.clientHeight || 400) / 2);
          vlist.dispatchEvent?.(new Event("scroll"));
        }
        body?.querySelector(`[data-line="${Number(line)}"]`)?.scrollIntoView?.({ block: "center" });
      });
    }
  });
  const selectLines = (index, extend, side) => {
    const start = extend && selection()?.anchor != null ? selection().anchor : index;
    const lo = Math.min(start, index), hi = Math.max(start, index);
    const items = props.tab === "diff" ? diffLines().slice(lo, hi + 1) : [];
    const newNums = items.map((l) => l.line).filter((n) => n != null);
    const onNew = side !== "old" && newNums.length > 0;
    const nums = onNew ? newNums : items.map((l) => l.old).filter((n) => n != null);
    setSelection({ anchor: start, lo, hi, line_side: props.tab === "diff" ? (onNew ? "new" : "old") : "source", text: lines().slice(lo, hi + 1).join("\n"), start: props.tab === "diff" ? nums[0] : lo + 1, end: props.tab === "diff" ? nums.at(-1) : hi + 1 });
  };
  const selectHunk = (index) => {
    let end = diffLines().findIndex((l, i) => i > index && l.hunk);
    if (end < 0) end = Math.min(lines().length, 3000);
    end = Math.min(end, 3000);
    selectLines(index, false); selectLines(end - 1, true);
  };
  const captureText = () => {
    const selected = window.getSelection();
    if (!selected || selected.isCollapsed || !body?.contains(selected.anchorNode) || !body?.contains(selected.focusNode)) return;
    const row = (n) => (n?.nodeType === 1 ? n : n?.parentElement)?.closest("[data-index]");
    const a = row(selected.anchorNode), b = row(selected.focusNode);
    if (!a || !b) return;
    const lo = Math.min(+a.dataset.index, +b.dataset.index), hi = Math.max(+a.dataset.index, +b.dataset.index);
    selectLines(lo, false); selectLines(hi, true);
    setSelection((s) => ({ ...s, text: selected.toString() }));
  };
  const draft = async () => {
    if (!agent() || !selection()?.text.trim() || !agents().some((a) => a.id === agent())) return;
    setDraftError("");
    try {
      const s = selection(), d = data() ?? {};
      await openCodeDraft(agent(), { intent: chooser(), path: path(), old_path: d.old_path, line_side: s.line_side, selection: s.text.slice(0, 12000) + (s.text.length > 12000 ? "\n[Selection truncated to 12000 characters]" : ""), start_line: s.start, end_line: s.end, base_commit: d.base_commit, revision: d.revision, worktree_id: props.target?.worktree_id ?? d.worktree_id, repo_id: props.repoId, kind: props.tab === "diff" ? "diff" : "source" });
      setChooser(null);
    } catch (e) { setDraftError(String(e)); }
  };
  const drawLine = (l, side) => {
    const inline = () => props.tab === "diff" ? paired().inline.get(l.index) : undefined;
    const split = () => props.tab === "diff" && layout() === "split";
    return <div class={`code-line ${props.tab === "diff" ? "code-diff-line" : "code-source-line"}`} title={split() ? l.text : undefined}
      classList={{ selected: selection() && l.index >= selection().lo && l.index <= selection().hi, addition: !!l.addition, deletion: !!l.deletion, jump: props.tab === "source" && l.line === props.selected?.line }}
      data-index={l.index} data-line={l.line}>
      <button class="line-number" title="Select line, shift-click for range" onClick={(e) => selectLines(l.index, e.shiftKey, side)}>
        {props.tab === "source" ? l.line : side === "old" ? l.old : side === "new" ? l.line : `${l.old ?? ""} ${l.line ?? ""}`}
      </button>
      <code>
        <Show when={inline()} fallback={<For each={tokens(split() && side ? l.text.slice(1) : l.text)}>{(t) => <span class={`syntax-${t.kind}`}>{t.text}</span>}</For>}>
          {(parts) => <><Show when={!split()}>{l.text[0]}</Show><For each={parts()}>{(part, i) =>
            <span classList={{ "diff-word-change": i() === 1 }}><For each={tokens(part)}>{(t) => <span class={`syntax-${t.kind}`}>{t.text}</span>}</For></span>
          }</For></>}
        </Show>{"\n"}
      </code>
      <Show when={l.hunk}><button class="hunk-select" onClick={() => selectHunk(l.index)}>Select hunk</button></Show>
    </div>;
  };
  return <aside class="code-inspector">
    <header class="inspector-head">
      <div class="row"><strong class="mono ellipsis grow" title={path()}>{path()}</strong><button onClick={props.onExpand}>{props.expanded ? "Restore graph" : "Expand"}</button><button onClick={props.onClose}>Close</button></div>
      <div class="inspector-target">{props.targetLabel}<Show when={props.target?.worktree_id}> · {props.target.worktree_id}</Show> · base {(data()?.base_commit ?? props.baseCommit ?? "unknown").slice(0, 12)}<Show when={data()?.revision}> · revision {String(data().revision).slice(0, 12)}</Show></div>
      <nav aria-label="Code inspector tabs"><For each={["source", "diff", "references"]}>{(tab) => <button classList={{ active: props.tab === tab }} onClick={() => props.onTab(tab)}>{tab[0].toUpperCase() + tab.slice(1)}</button>}</For></nav>
      <Show when={props.tab === "diff" && data()?.patch && !data()?.binary}>
        <div class="diff-controls row gap-4" aria-label="Diff controls">
          <button aria-pressed={!deltaMode() && layout() === "inline"} onClick={() => { setDeltaMode(false); setLayout("inline"); setCurrentHunk(-1); }}>Inline</button>
          <button aria-pressed={!deltaMode() && layout() === "split"} onClick={() => { setDeltaMode(false); setLayout("split"); setCurrentHunk(-1); }}>Side by side</button>
          <button disabled={!!deltaText() || currentHunk() <= 0} onClick={() => jumpHunk(-1)}>Previous hunk</button>
          <button disabled={!!deltaText() || currentHunk() >= hunkRows().length - 1} onClick={() => jumpHunk(1)}>Next hunk</button>
          <button aria-pressed={deltaMode()} title="Render with the Delta executable" onClick={() => { setDeltaMode((v) => !v); setCurrentHunk(-1); }}>Delta</button>
          <button aria-pressed={moreContext()} onClick={() => setMoreContext((value) => !value)}>{moreContext() ? "Less context" : "More context"}</button>
        </div>
      </Show>
    </header>
    <Show when={props.tab !== "references"} fallback={<div class="inspector-body">{props.children}</div>}>
      <Show when={selection()?.text.trim()}><div class="code-selection"><span>{props.tab === "diff" ? `Diff (${selection().line_side} side)` : "Lines"} {selection().start ?? "header"}–{selection().end ?? "header"}{selection().text.length > 12000 ? " · snippet will be truncated" : ""}</span><button title="Explain selected code in an agent draft" onClick={() => { setChooser("explain"); setAgent(""); }}>Explain</button><button title="Request a change to selected code in an agent draft" onClick={() => { setChooser("change"); setAgent(""); }}>Request a change</button></div></Show>
      <Show when={chooser()}><div class="code-agent-chooser"><label>Choose an agent for a draft (never sends)<select value={agent()} onChange={(e) => setAgent(e.currentTarget.value)}><option value="">Choose agent…</option><For each={agents()}>{(a) => <option value={a.id}>{a.name || a.id}</option>}</For></select></label><Show when={!agents().length}><span>No agents belong to this target. Create one on the Agents page.</span></Show><button disabled={!agent()} onClick={draft}>Open draft</button><button onClick={() => setChooser(null)}>Cancel</button><Show when={draftError()}><div class="err">{draftError()}</div></Show></div></Show>
      <div class="inspector-body" style={{ display: "flex", "flex-direction": "column" }} ref={body} onMouseUp={captureText}>
        <Show when={loading()}><div class="code-empty">Loading {props.tab}…</div></Show>
        <Show when={error()}><div class="code-empty err" role="alert">Unable to load {props.tab}: {error()}</div></Show>
        <Show when={data()}>
          <Show when={data().note}><div class="inspector-note">{data().note}</div></Show>
          <Show when={data().truncated}><div class="inspector-note">Large file: this is a truncated preview, not the full file.</div></Show>
          <Show when={deltaMode() && data().delta_notice}><div class="inspector-note" role="status">{data().delta_notice}</div></Show>
          <Show when={deltaText() && data().delta_truncated}><div class="inspector-note">Delta output truncated.</div></Show>
          <Show when={data().binary}><div class="code-empty">Binary file. Text preview is unavailable.</div></Show>
          <Show when={props.tab === "source" && data().exists === false}><div class="code-empty">File is missing or deleted in this target. Open Diff to inspect its removal.</div></Show>
          <Show when={!data().binary && !(props.tab === "source" && data().exists === false)}>
            <Show when={props.tab === "source" || data().patch} fallback={<div class="code-empty">No textual diff for this file against the selected base.</div>}>
              <Show when={lines().length > 3000}><div class="inspector-note">Rendering a bounded preview: {props.tab === "source" ? `lines ${sourceStart() + 1}–${Math.min(sourceStart() + 3000, lines().length)} of ${lines().length}. Select another symbol to jump to its region.` : `first 3000 of ${lines().length} diff lines.`}</div></Show>
              <div class="code-text" style={{ flex: 1, "min-height": 0, height: "100%", padding: 0 }} aria-label={props.tab === "source" ? `Read-only ${data().language ?? "source"}` : "Read-only diff"}>
                <Show when={deltaText()} fallback={<VirtualList ref={(el) => { vlist = el; }} items={props.tab === "diff" ? displayedDiff() : displayedSource()} rowHeight={LINE_HEIGHT}>
                  {(row) => <Show when={row.both && !(props.tab === "diff" && layout() === "split" && row.both.old != null) && row.both} fallback={
                    <div class="code-diff-split">
                      <div>{(row.old ?? row.both) && drawLine(row.old ?? row.both, "old")}</div>
                      <div>{(row.next ?? row.both) && drawLine(row.next ?? row.both, "new")}</div>
                    </div>
                  }>{(line) => drawLine(line())}</Show>}
                </VirtualList>}>
                  <AnsiDiff text={deltaText()} />
                </Show>
              </div>
            </Show>
          </Show>
        </Show>
      </div>
      <footer class="inspector-note">Read only · select text or click line numbers, shift-click for a range</footer>
    </Show>
  </aside>;
}
