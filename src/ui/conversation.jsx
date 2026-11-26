// Conversation view built from HAL session events. Used by Agents and Chat.
// Events are folded into items: user / assistant (streamed) / tool / file /
// permission / error. Assistant Markdown is rendered without raw HTML.
import { For, Index, Show, Switch, Match, batch, createSignal, createMemo, createComputed, createEffect, on, onMount, onCleanup } from "solid-js";
import { createStore } from "solid-js/store";
import { Portal } from "solid-js/web";
import { Badge, Button, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { AssistantReply } from "./markdown.jsx";
import { ConversationActivity } from "./conversation-activity.jsx";
import { ViewToggle } from "./conversation-view.jsx";
import { conversationView } from "../lib/conversation-view.js";
import "../styles/composer.css";
import { plainError } from "../lib/plain-error.js";
import { rpc } from "../lib/rpc.js";
import { activeRepoId } from "../lib/store.js";
import { draftFor, newDraft, clearSentDraft, attachImage, sendableImages } from "../lib/drafts.js";
import { compactFocus } from "../lib/compact.js";
import { mentionAt, insertMention, rankSuggestions, symbolSlice, contextBlock } from "../lib/mentions.js";
import { validateImages, normalizeImage, createImageCache, MAX_IMAGES } from "../lib/images.js";

/** Fold a list of HAL events into display items. Pure, incremental-safe. */
export function fold(events) {
  const items = [];
  const tools = new Map();
  let streaming = null;
  let turnText = false;
  const endPendingTools = () => { for (const t of tools.values()) if (t.running) t.inactive = true; if (streaming) streaming.live = false; };
  for (const e of events) {
    switch (e.type) {
      case "message.created":
        if (e.role === "user") { items.push({ kind: "user", text: e.content, seq: e.seq, attachments: Array.isArray(e.attachments) ? e.attachments.filter((a) => /^[0-9a-f]{64}$/.test(a?.sha256 ?? "")) : [], imageCount: typeof e.images === "number" ? e.images : 0 }); turnText = false; }
        break;
      case "model.delta":
        if (!streaming) { streaming = { kind: "assistant", text: "", seq: e.seq, live: true }; items.push(streaming); }
        streaming.text += e.delta;
        turnText = true;
        break;
      case "model.finished":
        if (streaming) { streaming.live = false; streaming.usage = e.usage; }
        streaming = null;
        break;
      case "assistant.completed":
        endPendingTools();
        if (streaming) { streaming.live = false; streaming = null; }
        else if (!turnText && e.text) items.push({ kind: "assistant", text: e.text, seq: e.seq });
        turnText = false;
        break;
      case "tool.started": {
        if (streaming) streaming.live = false;
        const t = { kind: "tool", tool: e.tool, args: e.arguments, out: "", seq: e.seq, running: true };
        tools.set(e.call_id, t); items.push(t); streaming = null;
        break;
      }
      case "tool.output": { const t = tools.get(e.call_id); if (t) t.out += e.data; break; }
      case "tool.finished": {
        const t = tools.get(e.call_id);
        if (t) { t.running = false; t.ok = e.ok; t.ms = e.duration_ms; t.result = e.result; }
        break;
      }
      case "file.changed": items.push({ kind: "file", path: e.path, diff: e.diff, seq: e.seq }); break;
      case "permission.requested":
        items.push({ kind: "perm", id: e.request_id, action: e.action, resource: e.resource, args: tools.get(e.call_id)?.args, seq: e.seq, open: true });
        break;
      case "permission.resolved": {
        const p = items.find((i) => i.kind === "perm" && i.id === e.request_id);
        if (p) { p.open = false; p.decision = e.decision; }
        break;
      }
      case "turn.failed": endPendingTools(); items.push({ kind: "error", text: e.error, seq: e.seq }); streaming = null; break;
      case "turn.cancelled": endPendingTools(); items.push({ kind: "note", text: "Stopped.", seq: e.seq }); streaming = null; break;
      case "context.compacted": items.push({ kind: "note", text: "Context compacted to save tokens.", seq: e.seq }); break;
      case "loop.detected": items.push({ kind: "error", text: `Loop detected: the agent repeated ${e.tool ?? "a tool"} ${e.repeats ?? ""} times.`, seq: e.seq }); break;
      case "subagent.started": items.push({ kind: "note", text: `Session helper started: ${e.description ?? e.agent ?? ""}`, seq: e.seq }); break;
    }
  }
  return items;
}

// MCP tool names are mcp_philium_shell_run etc.; show the short form.
const short = (t) => (t ?? "").replace(/^mcp_philium_/, "").replace(/^mcp_/, "");

import { toolOutput, cleanTerminalText, summarizeTool } from "../lib/tool-presentation.js";

function Tool(props) {
  const [open, setOpen] = createSignal(false);
  const [rawOpen, setRawOpen] = createSignal(false);
  const expanded = () => !!open();
  const t = () => props.item;
  const active = () => props.active && !t().inactive;
  const info = createMemo(() => (typeof summarizeTool === "function"
    ? summarizeTool(t().tool, t().args, t().result, t().out, { running: t().running, ok: t().ok, active: active() })
    : { label: "", summary: "", outcome: "", tone: "", errorHint: "" }));
  const resultText = createMemo(() => (typeof cleanTerminalText === "function" ? cleanTerminalText(toolOutput(t().result, t().out)) : ""));
  const [elapsed, setElapsed] = createSignal(0);
  const [silent, setSilent] = createSignal(0);
  const activeCall = createMemo(() => (active() && !props.hidden && t().running ? t().seq : null));
  const output = createMemo(() => t().out);
  let updatedAt = 0;
  createEffect(() => {
    output();
    if (activeCall() == null) return;
    updatedAt = Date.now();
    setSilent(0);
  });
  createEffect(() => {
    if (activeCall() == null) return;
    // Observation time, not execution time: history may have arrived late.
    const started = Date.now();
    updatedAt = started;
    setElapsed(0);
    setSilent(0);
    const timer = setInterval(() => {
      const now = Date.now();
      setElapsed(Math.floor((now - started) / 1000));
      setSilent(Math.floor((now - updatedAt) / 1000));
    }, 1000);
    onCleanup(() => clearInterval(timer));
  });
  const dotTone = () => (info().tone === "busy" ? (active() ? "busy" : "") : info().tone === "bad" ? "bad" : info().tone === "ok" ? "live" : "");
  const outcomeLabel = () => (info().outcome ? info().outcome.split(" · ")[0] : "");
  const isShellLike = () => {
    const n = typeof short === "function" ? short(t().tool) : "";
    return n === "shell_run" || n === "surface_exec";
  };
  return (
    <div class={`msg tool conv-tool ${expanded() ? "is-open" : ""} ${info().tone === "bad" ? "is-bad" : ""}`}>
      <button type="button" class="row conv-tool-toggle" aria-expanded={expanded()} onClick={() => {
        const next = !expanded();
        if (!next) setRawOpen(false);
        setOpen(next);
      }}>
        <span class={`dot ${dotTone()}`} />
        <b class="tool-label">{info().label}</b>
        <span class="ellipsis dim grow tool-summary" title={info().summary}>{info().summary}</span>
        <span class={`xs tool-outcome ${info().tone === "bad" ? "bad" : "dim"}`}>{outcomeLabel()}</span>
        <Show when={t().ms != null}><span class="faint xs tool-dur">{(t().ms / 1000).toFixed(1)}s</span></Show>
        <span class="dim tool-chevron" aria-hidden="true">{expanded() ? "▾" : "▸"}</span>
      </button>
      <Show when={t().running}>
        <div class="xs dim tool-status-line ellipsis" role="status">{active()
          ? `Awaiting tool completion · observed ${elapsed()}s${silent() >= 30 ? ` · No new output for ${silent()}s. It may be working or waiting for approval. This is not a completion signal.` : ""}`
          : "No tool completion recorded in loaded history."}</div>
      </Show>
      <Show when={!expanded() && !t().running && info().tone === "bad" && info().errorHint}>
        <div class="xs tool-error-line ellipsis" title={info().errorHint}>{info().errorHint}</div>
      </Show>
      <Show when={expanded()}>
        <Show when={isShellLike() && t().args?.command}>
          <pre class="tool-command"><span class="dim">$ </span>{t().args.command}</pre>
        </Show>
        <Show when={short(t().tool) !== "shell_run" && t().args && Object.keys(t().args).length > 0}>
          <pre class="conv-pre">{JSON.stringify(t().args, null, 2)}</pre>
        </Show>
        <Show when={resultText()}>
          <pre class="conv-pre">{resultText().slice(0, 20000)}</pre>
          <Show when={resultText().length > 20000}><span class="xs dim">Showing the first 20,000 characters of tool output.</span></Show>
        </Show>
        <details class="tool-raw" onToggle={(e) => setRawOpen(e.currentTarget.open)}><summary>Raw tool data</summary><Show when={rawOpen()}><pre class="conv-pre">{JSON.stringify({ arguments: t().args, result: t().result }, null, 2)?.slice(0, 20000)}</pre></Show></details>
      </Show>
    </div>
  );
}

/**
 * props: events (array, reactive), onApprove(id, scope), onDeny(id), busy
 */
/** Quiet per-message action: start a new agent from the conversation up to here. */
function ForkHere(props) {
  return (
    <Show when={props.onFork && props.item.seq}>
      <button type="button" class="link fork-here" title="Start a new agent with this conversation up to here. Files are not rewound." onClick={() => props.onFork(props.item.seq)}>Fork from here</button>
    </Show>
  );
}

function UserMessage(props) {
  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [copied, setCopied] = createSignal(false);
  let ta;

  const startEdit = () => {
    setDraft(props.item.text || "");
    setEditing(true);
    queueMicrotask(() => {
      if (ta) {
        ta.focus({ preventScroll: true });
        ta.setSelectionRange(ta.value.length, ta.value.length);
      }
    });
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.item.text || "");
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {}
  };

  const submit = async (mode) => {
    const next = draft().trim();
    if (!next || busy()) return;
    setBusy(true);
    try {
      const ok = await props.onEdit?.(props.item.seq, next, mode);
      if (ok !== false) setEditing(false);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class={`msg user ${editing() ? "is-editing" : ""}`} data-seq={props.item.seq}>
      <SentImages item={props.item} load={props.load} />
      <Show
        when={editing()}
        fallback={
          <>
            <div class="user-msg-text">{props.item.text}</div>
            <div class="user-msg-actions">
              <Show when={props.onEdit && props.item.seq}>
                <button type="button" class="link" onClick={startEdit} title="Edit and resend or fork from this message">Edit</button>
              </Show>
              <button type="button" class="link" onClick={copy}>{copied() ? "Copied" : "Copy"}</button>
              <ForkHere item={props.item} onFork={props.onFork} />
            </div>
          </>
        }
      >
        <div class="user-msg-editor">
          <textarea
            ref={ta}
            class="textarea user-msg-edit-input"
            rows={Math.min(8, Math.max(2, (draft().match(/\n/g)?.length ?? 0) + 2))}
            value={draft()}
            disabled={busy()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") { e.preventDefault(); setEditing(false); }
              else if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); submit("fork"); }
            }}
          />
          <div class="row" style={{ "justify-content": "flex-end", gap: "6px", "flex-wrap": "wrap" }}>
            <Button size="sm" variant="ghost" type="button" disabled={busy()} onClick={() => setEditing(false)}>Cancel</Button>
            <Show when={!props.readOnly}>
              <Button size="sm" variant="ghost" type="button" disabled={busy() || !draft().trim()} onClick={() => submit("resend")} title="Send the edited prompt in this session">Resend here</Button>
            </Show>
            <Button size="sm" variant="primary" type="button" busy={busy()} disabled={!draft().trim()} onClick={() => submit("fork")} title="Fork from before this message and run with the edited prompt (⌘↵)">Fork with edit</Button>
          </div>
        </div>
      </Show>
    </div>
  );
}

export function Conversation(props) {
  let el;
  const [store, setStore] = createStore({ items: [] });
  const [stick, setStick] = createSignal(true);
  // `.conv` uses native `flex-direction: column-reverse`, so `scrollTop === 0` IS the bottom
  // of the conversation and the browser compositor anchors streaming content automatically
  // without JS ResizeObserver fighting or accidental scroll-to-top jumps.
  const pinBottom = (smooth = false) => {
    if (!el) return;
    if (smooth && typeof el.scrollTo === "function") {
      el.scrollTo({ top: 0, behavior: "smooth" });
    } else {
      el.scrollTop = 0;
    }
  };
  const scheduleScrollToBottom = () => {
    if (!stick() || !el) return;
    if (Math.abs(el.scrollTop) > 1) el.scrollTop = 0;
  };
  const tools = new Map();
  let streamingIdx = -1;
  let turnText = false;
  let cursor = 0;
  let firstEvent = null;
  let lastEvent = null;

  const endPendingTools = () => {
    for (const idx of tools.values()) {
      if (store.items[idx]?.running) setStore("items", idx, "inactive", true);
    }
    if (streamingIdx >= 0) setStore("items", streamingIdx, "live", false);
  };

  const applyEvent = (e) => {
    switch (e.type) {
      case "message.created":
        if (e.role === "user") {
          setStore("items", store.items.length, {
            kind: "user",
            text: e.content,
            seq: e.seq,
            attachments: Array.isArray(e.attachments) ? e.attachments.filter((a) => /^[0-9a-f]{64}$/.test(a?.sha256 ?? "")) : [],
            imageCount: typeof e.images === "number" ? e.images : 0,
          });
          turnText = false;
        }
        break;
      case "model.delta":
        if (streamingIdx < 0) {
          streamingIdx = store.items.length;
          setStore("items", streamingIdx, { kind: "assistant", text: e.delta ?? "", seq: e.seq, live: true, usage: undefined });
        } else {
          setStore("items", streamingIdx, "text", (t) => t + e.delta);
        }
        turnText = true;
        break;
      case "model.finished":
        if (streamingIdx >= 0) setStore("items", streamingIdx, { live: false, usage: e.usage });
        streamingIdx = -1;
        break;
      case "assistant.completed":
        endPendingTools();
        if (streamingIdx >= 0) {
          setStore("items", streamingIdx, "live", false);
          streamingIdx = -1;
        } else if (!turnText && e.text) {
          setStore("items", store.items.length, { kind: "assistant", text: e.text, seq: e.seq, live: false, usage: undefined });
        }
        turnText = false;
        break;
      case "tool.started": {
        if (streamingIdx >= 0) setStore("items", streamingIdx, "live", false);
        const idx = store.items.length;
        setStore("items", idx, {
          kind: "tool",
          tool: e.tool,
          args: e.arguments,
          out: "",
          seq: e.seq,
          running: true,
          inactive: undefined,
          ok: undefined,
          ms: undefined,
          result: undefined,
        });
        tools.set(e.call_id, idx);
        streamingIdx = -1;
        break;
      }
      case "tool.output": {
        const idx = tools.get(e.call_id);
        if (idx != null) setStore("items", idx, "out", (out) => out + e.data);
        break;
      }
      case "tool.finished": {
        const idx = tools.get(e.call_id);
        if (idx != null) setStore("items", idx, { running: false, ok: e.ok, ms: e.duration_ms, result: e.result });
        break;
      }
      case "file.changed":
        setStore("items", store.items.length, { kind: "file", path: e.path, diff: e.diff, seq: e.seq });
        break;
      case "permission.requested": {
        const toolIdx = tools.get(e.call_id);
        const args = toolIdx != null ? store.items[toolIdx]?.args : undefined;
        setStore("items", store.items.length, {
          kind: "perm",
          id: e.request_id,
          action: e.action,
          resource: e.resource,
          args,
          seq: e.seq,
          open: true,
          decision: undefined,
        });
        break;
      }
      case "permission.resolved": {
        const pIdx = store.items.findIndex((i) => i.kind === "perm" && i.id === e.request_id);
        if (pIdx >= 0) setStore("items", pIdx, { open: false, decision: e.decision });
        break;
      }
      case "turn.failed":
        endPendingTools();
        setStore("items", store.items.length, { kind: "error", text: e.error, seq: e.seq });
        streamingIdx = -1;
        break;
      case "turn.cancelled":
        endPendingTools();
        setStore("items", store.items.length, { kind: "note", text: "Stopped.", seq: e.seq });
        streamingIdx = -1;
        break;
      case "context.compacted":
        setStore("items", store.items.length, { kind: "note", text: "Context compacted to save tokens.", seq: e.seq });
        break;
      case "loop.detected":
        setStore("items", store.items.length, { kind: "error", text: `Loop detected: the agent repeated ${e.tool ?? "a tool"} ${e.repeats ?? ""} times.`, seq: e.seq });
        break;
      case "subagent.started":
        setStore("items", store.items.length, { kind: "note", text: `Session helper started: ${e.description ?? e.agent ?? ""}`, seq: e.seq });
        break;
    }
  };

  createComputed(() => {
    const evs = props.events ?? [];
    batch(() => {
      const canAppend =
        cursor === 0
          ? store.items.length === 0
          : evs.length >= cursor && evs[0] === firstEvent && evs[cursor - 1] === lastEvent;
      if (canAppend) {
        for (let i = cursor; i < evs.length; i++) applyEvent(evs[i]);
        cursor = evs.length;
        firstEvent = cursor > 0 ? evs[0] : null;
        lastEvent = cursor > 0 ? evs[cursor - 1] : null;
        return;
      }
      // Full reset fallback (e.g. switched session, array shrunk, or first seq / boundary event changed)
      tools.clear();
      streamingIdx = -1;
      turnText = false;
      const nextItems = [];
      const endPendingNext = () => {
        for (const idx of tools.values()) if (nextItems[idx]?.running) nextItems[idx].inactive = true;
        if (streamingIdx >= 0) nextItems[streamingIdx].live = false;
      };
      for (const e of evs) {
        switch (e.type) {
          case "message.created":
            if (e.role === "user") {
              nextItems.push({ kind: "user", text: e.content, seq: e.seq, attachments: Array.isArray(e.attachments) ? e.attachments.filter((a) => /^[0-9a-f]{64}$/.test(a?.sha256 ?? "")) : [], imageCount: typeof e.images === "number" ? e.images : 0 });
              turnText = false;
            }
            break;
          case "model.delta":
            if (streamingIdx < 0) {
              streamingIdx = nextItems.length;
              nextItems.push({ kind: "assistant", text: e.delta ?? "", seq: e.seq, live: true, usage: undefined });
            } else {
              nextItems[streamingIdx].text += e.delta;
            }
            turnText = true;
            break;
          case "model.finished":
            if (streamingIdx >= 0) { nextItems[streamingIdx].live = false; nextItems[streamingIdx].usage = e.usage; }
            streamingIdx = -1;
            break;
          case "assistant.completed":
            endPendingNext();
            if (streamingIdx >= 0) { nextItems[streamingIdx].live = false; streamingIdx = -1; }
            else if (!turnText && e.text) nextItems.push({ kind: "assistant", text: e.text, seq: e.seq, live: false, usage: undefined });
            turnText = false;
            break;
          case "tool.started": {
            if (streamingIdx >= 0) nextItems[streamingIdx].live = false;
            const idx = nextItems.length;
            nextItems.push({ kind: "tool", tool: e.tool, args: e.arguments, out: "", seq: e.seq, running: true, inactive: undefined, ok: undefined, ms: undefined, result: undefined });
            tools.set(e.call_id, idx);
            streamingIdx = -1;
            break;
          }
          case "tool.output": {
            const idx = tools.get(e.call_id);
            if (idx != null) nextItems[idx].out += e.data;
            break;
          }
          case "tool.finished": {
            const idx = tools.get(e.call_id);
            if (idx != null) { nextItems[idx].running = false; nextItems[idx].ok = e.ok; nextItems[idx].ms = e.duration_ms; nextItems[idx].result = e.result; }
            break;
          }
          case "file.changed":
            nextItems.push({ kind: "file", path: e.path, diff: e.diff, seq: e.seq });
            break;
          case "permission.requested":
            nextItems.push({ kind: "perm", id: e.request_id, action: e.action, resource: e.resource, args: nextItems[tools.get(e.call_id)]?.args, seq: e.seq, open: true, decision: undefined });
            break;
          case "permission.resolved": {
            const p = nextItems.find((i) => i.kind === "perm" && i.id === e.request_id);
            if (p) { p.open = false; p.decision = e.decision; }
            break;
          }
          case "turn.failed":
            endPendingNext();
            nextItems.push({ kind: "error", text: e.error, seq: e.seq });
            streamingIdx = -1;
            break;
          case "turn.cancelled":
            endPendingNext();
            nextItems.push({ kind: "note", text: "Stopped.", seq: e.seq });
            streamingIdx = -1;
            break;
          case "context.compacted":
            nextItems.push({ kind: "note", text: "Context compacted to save tokens.", seq: e.seq });
            break;
          case "loop.detected":
            nextItems.push({ kind: "error", text: `Loop detected: the agent repeated ${e.tool ?? "a tool"} ${e.repeats ?? ""} times.`, seq: e.seq });
            break;
          case "subagent.started":
            nextItems.push({ kind: "note", text: `Session helper started: ${e.description ?? e.agent ?? ""}`, seq: e.seq });
            break;
        }
      }
      if (store.items.length > nextItems.length) {
        setStore("items", (prev) => prev.slice(0, nextItems.length));
      }
      for (let k = 0; k < nextItems.length; k++) {
        const cur = store.items[k];
        const nxt = nextItems[k];
        if (cur && cur.kind === nxt.kind && cur.seq === nxt.seq) {
          for (const key of Object.keys(nxt)) {
            if (cur[key] === nxt[key]) continue;
            if (
              key === "attachments" &&
              Array.isArray(cur.attachments) &&
              Array.isArray(nxt.attachments) &&
              cur.attachments.length === nxt.attachments.length &&
              cur.attachments.every((a, idx) => a?.sha256 === nxt.attachments[idx]?.sha256)
            ) {
              continue;
            }
            setStore("items", k, key, nxt[key]);
          }
        } else {
          setStore("items", k, nxt);
        }
      }
      cursor = evs.length;
      firstEvent = cursor > 0 ? evs[0] : null;
      lastEvent = cursor > 0 ? evs[cursor - 1] : null;
      setStick(true);
      scheduleScrollToBottom();
    });
  });

  onMount(() => {
    pinBottom(false);
  });

  return (
    <div class="conv-shell" style={props.hidden ? { display: "none" } : undefined}>
      <ViewToggle />
      <Lightbox />
      <div
        class={`conv conv-${conversationView()}`}
        ref={el}
        onScroll={(e) => {
          setStick(Math.abs(e.currentTarget.scrollTop) <= 24);
        }}
      >
        <div class="timeline">
        <Show when={store.items.length === 0}>
          <div class="conv-empty">{props.emptyText ?? "No messages yet."}</div>
        </Show>
        <Index each={store.items}>
          {(it) => (
            <Switch>
              <Match when={it().kind === "user"}><UserMessage item={it()} load={props.loadAttachment} onFork={props.onForkAt} onEdit={props.onEditAt} readOnly={props.readOnly} /></Match>
              <Match when={it().kind === "assistant"}>
                <div class={`msg assistant ${it().live && props.busy && !props.readOnly ? "live" : ""}`} data-seq={it().seq}>
                  <AssistantReply text={it().text} live={it().live && props.busy && !props.readOnly} />
                  <Show when={!it().live}><ForkHere item={it()} onFork={props.onForkAt} /></Show>
                  <Show when={it().usage && (it().usage.input_tokens || it().usage.output_tokens)}>
                    {(() => {
                      const u = it().usage || {};
                      const inp = Number(u.input_tokens || u.prompt_tokens || 0);
                      const cached = Number(u.cached_tokens || u.cache_read_input_tokens || 0);
                      const out = Number(u.output_tokens || u.completion_tokens || 0);
                      const pct = inp > 0 ? Math.round((cached / inp) * 100) : 0;
                      return (
                        <div class="turn-kv-meta mono xs faint">
                          <Show when={inp > 0}>
                            <span class={`turn-kv-hit ${pct >= 60 ? "hot" : pct >= 25 ? "warm" : "cold"}`}>
                              ⚡ {pct}% cached
                            </span>
                            <span>· {(cached / 1000).toFixed(1)}k cached / {(inp / 1000).toFixed(1)}k in</span>
                          </Show>
                          <Show when={out > 0}>
                            <span>· {out >= 1000 ? `${(out / 1000).toFixed(1)}k` : out} out</span>
                          </Show>
                        </div>
                      );
                    })()}
                  </Show>
                </div>
              </Match>
              <Match when={it().kind === "tool"}><Tool item={it()} active={props.busy && !props.readOnly} hidden={props.hidden} terminal={conversationView() === "terminal"} /></Match>
              <Match when={it().kind === "file"}>
                <details class="msg tool">
                  <summary><Icon name="repo" size={12} /> edited <b>{it().path}</b></summary>
                  <pre class="conv-pre diff">{(it().diff ?? "").slice(0, 20000)}</pre>
                </details>
              </Match>
              <Match when={it().kind === "perm"}>
                {(() => {
                  const isBrowser = it().action === "mcp" && it().resource?.startsWith("philium/browser_");
                  const shortRes = (it().resource ?? "").replace(/^philium\//, "");
                  const obj = it().args && typeof it().args === "object" && !Array.isArray(it().args) ? it().args : null;
                  const keys = obj ? Object.keys(obj) : [];
                  const inlineVal = obj?.url || obj?.command || obj?.path || (keys.length === 1 && typeof obj[keys[0]] === "string" ? obj[keys[0]] : null);
                  const hasExtra = keys.length > (inlineVal ? 1 : 0);
                  return (
                    <div class={`perm perm-card ${it().open ? "" : "done"}`}>
                      <div class="perm-body">
                        <div class="perm-head">
                          <span class="perm-icon"><Icon name={isBrowser ? "globe" : "shield"} size={13} /></span>
                          <span class="perm-title">{isBrowser ? shortRes.replace(/^browser_/, "Browser ").replace(/_/g, " ") : (it().action || "Permission")}</span>
                          <code class="perm-tag">{shortRes}</code>
                          <Show when={inlineVal}><code class="perm-target ellipsis" title={inlineVal}>{inlineVal}</code></Show>
                          <div class="grow" />
                          <Show when={!(it().open && !props.readOnly && !props.externalApprovals)}>
                            <Badge tone={it().open ? "" : /^(appr|allow)/.test(it().decision ?? "") ? "secure" : "danger"}>{it().open ? (props.readOnly ? "Historical · stopped" : "Requested") : it().decision ?? "resolved"}</Badge>
                          </Show>
                        </div>
                        <Show when={hasExtra}>
                          <details class="perm-details">
                            <summary class="xs faint">Arguments</summary>
                            <pre class="conv-pre">{JSON.stringify(it().args, null, 2)}</pre>
                          </details>
                        </Show>
                      </div>
                      <Show when={it().open && !props.readOnly && !props.externalApprovals}>
                        <div class="perm-foot">
                          <span class="xs faint grow">
                            <Show when={isBrowser} fallback="Tool approval requested">
                              Page content may be sent to this agent’s provider
                            </Show>
                          </span>
                          <div class="row gap-4" style={{ "flex-wrap": "wrap" }}>
                            <Button size="sm" variant="ghost" onClick={() => props.onDeny?.(it().id)}>Deny</Button>
                            <Button size="sm" onClick={() => props.onApprove?.(it().id, "session")}>Allow for session</Button>
                            <Show when={props.onAlwaysAllowSession}>
                              <Button size="sm" onClick={() => props.onAlwaysAllowSession?.(it().id)}>Always allow for this session</Button>
                            </Show>
                            <Button size="sm" variant="primary" onClick={() => props.onApprove?.(it().id, "once")}>Allow once</Button>
                          </div>
                        </div>
                      </Show>
                    </div>
                  );
                })()}
              </Match>
              <Match when={it().kind === "error"}><div class="msg error"><Show when={String(it().text).includes("philium_guard")} fallback={<PlainError text={it().text} />}><strong>Request blocked by secret guard</strong><p class="small">This conversation contains content the guard identified as a secret. This request was not sent to the provider. Start a fresh conversation without sensitive content.</p><details class="tool-raw"><summary>Error details</summary><pre class="conv-pre">{it().text}</pre></details></Show></div></Match>
              <Match when={it().kind === "note"}><div class="faint xs" style={{ "align-self": "center" }}>{it().text}</div></Match>
            </Switch>
          )}
        </Index>
        <ConversationActivity agent={props.agent} events={props.events ?? []} busy={props.busy} readOnly={props.readOnly || props.hidden} />
      </div>
      </div>
      <Show when={!stick() && store.items.length > 0}>
        <div class="conv-jump-wrap">
          <button type="button" class="conv-jump" onClick={() => { setStick(true); pinBottom(true); }}>
            ↓ Jump to latest
          </button>
        </div>
      </Show>
    </div>
  );
}

const formatSize = (n) => n >= 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;

/** Full-size image overlay. Esc or click closes. Only the first mounted instance is active. */
const [lightbox, setLightbox] = createSignal(null);
const [lightboxHosts, setLightboxHosts] = createSignal([]);
let lightboxNextId = 0;
export function Lightbox() {
  const id = ++lightboxNextId;
  setLightboxHosts((list) => [...list, id]);
  onCleanup(() => setLightboxHosts((list) => list.filter((x) => x !== id)));
  const isHost = createMemo(() => lightboxHosts()[0] === id);
  const close = () => setLightbox(null);
  createEffect(() => {
    if (!isHost() || !lightbox()) return;
    const key = (e) => { if (e.key === "Escape") close(); };
    window.addEventListener("keydown", key);
    onCleanup(() => window.removeEventListener("keydown", key));
  });
  return (
    <Show when={isHost() && lightbox()}><Portal>
      <div class="lightbox" role="dialog" aria-modal="true" aria-label={lightbox().name} onClick={close}>
        <img src={lightbox().src} alt={lightbox().name} />
        <div class="lightbox-caption">{lightbox().name} · Esc to close</div>
      </div>
    </Portal></Show>
  );
}

/** Images the user sent, loaded lazily from the agent's own saved session. */
function SentImages(props) {
  const list = () => props.item.attachments ?? [];
  const missing = () => Math.max(0, (props.item.imageCount ?? 0) - list().length);
  return (
    <Show when={list().length || missing()}>
      <div class="sent-images">
        <For each={list()}>{(a) => <SentImage attachment={a} load={props.load} />}</For>
        <Show when={missing()}><span class="image-chip">{missing()} image{missing() > 1 ? "s" : ""}</span></Show>
      </div>
    </Show>
  );
}
function SentImage(props) {
  const [src, setSrc] = createSignal("");
  const [failed, setFailed] = createSignal(false);
  createEffect(on(() => [props.attachment.sha256, props.load], () => {
    setSrc(""); setFailed(false);
    if (!props.load) return;
    props.load(props.attachment).then(setSrc, () => setFailed(true));
  }));
  const name = () => props.attachment.name || "image";
  return (
    <Show when={src()} fallback={<span class="image-chip" title={failed() ? "Image could not be loaded from saved history" : name()}><Icon name="image" size={12} /> {name()}{failed() ? " · unavailable" : ""}</span>}>
      <button type="button" class="sent-image" title={`${name()} · click to enlarge`} onClick={() => setLightbox({ src: src(), name: name() })}>
        <img src={src()} alt={name()} loading="lazy" />
      </button>
    </Show>
  );
}
/** One bounded cache per agent id. Object URLs are revoked on eviction. */
const agentCaches = new Map();
export function agentAttachmentLoader(id) {
  if (!id) return undefined;
  if (!agentCaches.has(id)) {
    if (agentCaches.size >= 4) { const [oldest, entry] = agentCaches.entries().next().value; entry.cache.clear(); agentCaches.delete(oldest); }
    const cache = createImageCache(async (key) => {
      const [sha256, media_type] = key.split("|");
      const { invoke } = await import("@tauri-apps/api/core");
      const bytes = await invoke("agent_attachment", { id, sha256 });
      return new Blob([bytes], { type: media_type || "image/png" });
    });
    agentCaches.set(id, { cache, load: (a) => cache.get(`${a.sha256}|${a.media_type || ""}`) });
  }
  return agentCaches.get(id).load;
}

export { KvTelemetryBar } from "./kv-bar.jsx";

/** An error as one plain sentence with its fix; the original text under Error details. */
export function PlainError(props) {
  const e = () => plainError(props.text);
  return <>{e().plain}<Show when={e().raw}><details class="tool-raw"><summary>Error details</summary><pre class="conv-pre">{e().raw}</pre></details></Show></>;
}

/** Effort presets. Names are not guarantees of latency, cost or quality. */
export const SPEEDS = [
  { value: "fast", label: "Fast", effort: "low", hint: "Requests low reasoning effort, not guaranteed speed" },
  { value: "normal", label: "Normal", effort: "default", hint: "The model's default effort" },
  { value: "deep", label: "Deep", effort: "high", hint: "Requests high reasoning effort, not guaranteed quality" },
];
export const speedEffort = (v) => SPEEDS.find((s) => s.value === v)?.effort ?? (v || "default");
export const effortSpeed = (e) => (e === "low" || e === "minimal" || e === "none" ? "fast" : e === "high" || e === "xhigh" || e === "max" ? "deep" : "normal");

/** Composer card: attachments, text, then one toolbar row.
 * Images arrive by paste, drop (anywhere over the agent pane via props.dropTarget),
 * Attach menu file picker, or native screenshot. onSend(text, images) receives HAL
 * image parts [{ data: base64, media_type, name }] built only at send time. */
export function Composer(props) {
  const localDraft = newDraft();
  const revokePreview = (im) => {
    if (im?.preview && typeof im.preview === "string" && im.preview.startsWith("blob:")) {
      try { URL.revokeObjectURL(im.preview); } catch {}
    }
  };
  const rawSetLocalImages = localDraft.setImages;
  localDraft.setImages = (value) => {
    const before = localDraft.images();
    const res = rawSetLocalImages(value);
    const after = new Set(localDraft.images());
    for (const im of before) if (!after.has(im)) revokePreview(im);
    return res;
  };
  onCleanup(() => {
    for (const im of localDraft.images()) revokePreview(im);
  });
  const record = () => props.draftKey ? draftFor(props.draftKey) : localDraft;
  const text = () => record().text(), setText = (value) => record().setText(value);
  const images = () => record().images(), setImages = (value) => record().setImages(value);
  const sending = () => record().sending();
  const [over, setOver] = createSignal(false);
  const [notice, setNotice] = createSignal("");
  // Screen Recording not granted: macOS gives capture rights to Philium itself, never to its screencapture child.
  const [permission, setPermission] = createSignal(false);
  const [adding, setAdding] = createSignal(0);
  const [menu, setMenu] = createSignal(false);
  let input, picker, attachEl;
  // @-mentions: files and symbols from the code index of this composer's worktree.
  const [mention, setMention] = createSignal(null);
  const [suggest, setSuggest] = createSignal([]);
  const [suggestIdx, setSuggestIdx] = createSignal(0);
  const [picked, setPicked] = createSignal([]);
  let mentionTimer, mentionGen = 0, treeCache = null;
  const updateMention = () => {
    const target = props.mentionTarget;
    const at = target && input ? mentionAt(input.value, input.selectionStart ?? input.value.length) : null;
    setMention(at);
    clearTimeout(mentionTimer);
    if (!at) { setSuggest([]); return; }
    const gen = ++mentionGen;
    mentionTimer = setTimeout(async () => {
      try {
        treeCache ??= await rpc("codeindex.tree", target).then((r) => r?.files || []);
        const syms = at.query.length >= 2 ? await rpc("codeindex.find", { ...target, query: at.query }).then((r) => r?.items || []).catch(() => []) : [];
        if (gen === mentionGen) { setSuggest(rankSuggestions(treeCache, syms, at.query)); setSuggestIdx(0); }
      } catch { if (gen === mentionGen) setSuggest([]); }
    }, 120);
  };
  const pickMention = (item) => {
    const at = mention();
    if (!at || !item) return;
    const r = insertMention(input.value, at, item.label);
    setText(r.text);
    setPicked((p) => [...p.filter((x) => x.label !== item.label), item]);
    setMention(null); setSuggest([]);
    queueMicrotask(() => { input.focus(); input.setSelectionRange(r.caret, r.caret); });
  };
  const resolveMentions = async (t) => {
    const used = picked().filter((m) => t.includes(`@${m.label}`));
    if (!used.length || !props.mentionTarget) return "";
    const resolved = await Promise.all(used.map(async (m) => {
      try {
        const src = await rpc("codeindex.source", { ...props.mentionTarget, path: m.path });
        if (src?.text == null) return { ...m, error: "not readable as text" };
        if (m.kind === "symbol") return { ...m, ...symbolSlice(src.text, m.line) };
        return { ...m, text: src.text, truncated: !!src.truncated };
      } catch (e) { return { ...m, error: String(e).slice(0, 120) }; }
    }));
    return contextBlock(resolved);
  };
  let lastTextLen = 0;
  const syncHeight = () => {
    if (!input) return;
    const val = text();
    if (!val) {
      lastTextLen = 0;
      if (input.style.height) input.style.height = "";
      input.style.overflowY = "hidden";
      return;
    }
    const shrinking = val.length < lastTextLen;
    lastTextLen = val.length;
    if (shrinking) {
      input.style.height = "auto";
    }
    const sh = input.scrollHeight;
    const nextH = sh > 0 ? `${Math.min(sh, 200)}px` : "";
    if (input.style.height !== nextH) input.style.height = nextH;
    input.style.overflowY = sh > 200 ? "auto" : "hidden";
  };
  createEffect(on(text, () => syncHeight()));
  createEffect(() => {
    if (!menu()) return;
    const onDown = (e) => { if (attachEl && !attachEl.contains(e.target)) setMenu(false); };
    const onKey = (e) => { if (e.key === "Escape") setMenu(false); };
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
    });
  });
  const canAttach = () => record().ready() && !props.disabled;
  const effortOptions = createMemo(() => {
    const raw = props.effortOptions;
    if (!Array.isArray(raw)) return [];
    const cur = props.effort || "default";
    if (raw.length > 0 && cur && !raw.some((o) => o.value === cur)) {
      return [...raw, { value: cur, label: cur, hint: `Custom effort: ${cur}` }];
    }
    return raw;
  });
  const currentEffort = () => props.effort || speedEffort(props.speed || "normal");
  const selectEffort = (val) => {
    if (props.onEffort) props.onEffort(val);
    else if (props.onSpeed) props.onSpeed(val);
  };
  const [slashIdx, setSlashIdx] = createSignal(0);
  const slashCommands = createMemo(() => {
    const list = [
      { cmd: "/model", desc: "Switch model (0 tokens)", immediate: true, show: !!props.onSlashModel },
      { cmd: "/effort", desc: "Change reasoning effort (0 tokens)", args: true, show: !!(props.onEffort || props.onSpeed) },
      { cmd: "/context", desc: "Open live Context & Memory inspector", immediate: true, show: !!props.onSlashContext },
      { cmd: "/remember", desc: "Save a decision to memory (0 tokens)", args: true, show: !!props.onSlashRemember },
      { cmd: "/fork", desc: "Fork a new agent from this workspace", args: true, show: !!props.onSlashFork },
      { cmd: "/compact", desc: "Compact conversation context", immediate: true, show: !!props.onSlashCompact },
      { cmd: "/skill", desc: "Run a saved skill: /skill <name> [task]", args: true, show: true },
    ];
    return list.filter((x) => x.show);
  });
  const slashMenu = createMemo(() => {
    const t = text();
    if (!t.startsWith("/") || /\s/.test(t)) return [];
    const q = t.toLowerCase();
    return slashCommands().filter((c) => c.cmd.startsWith(q));
  });
  createEffect(() => {
    slashMenu();
    setSlashIdx(0);
  });
  const pickSlash = (item) => {
    if (!item) return;
    if (item.immediate) {
      setText(item.cmd);
      queueMicrotask(() => send());
    } else {
      setText(`${item.cmd} `);
      queueMicrotask(() => { input?.focus(); syncHeight(); });
    }
  };
  const slashHint = createMemo(() => {
    const t = text().trimStart();
    if (!t.startsWith("/") || slashMenu().length > 0) return null;
    if (t.startsWith("/effort")) {
      const opts = effortOptions();
      const summary = opts.length
        ? opts.map((o) => o.value).join(" | ")
        : "not supported by the active model";
      return { kind: "effort", label: `Zero-token local command: /effort ${summary}` };
    }
    if (t.startsWith("/model")) {
      return { kind: "model", label: "Press Enter to choose from a list, or type a name: /model <name>. Switching costs no tokens." };
    }
    if (t.startsWith("/context") && props.onSlashContext) {
      return { kind: "context", label: "Open the live Context Slate Inspector (budget, active facts, working note). 0 tokens." };
    }
    if (t.startsWith("/remember") && props.onSlashRemember) {
      return { kind: "remember", label: "Save a direct decision to the fact ledger: /remember [key=]decision text. 0 tokens." };
    }
    if (t.startsWith("/fork") && props.onSlashFork) {
      return { kind: "fork", label: "Fork a new agent on this workspace with the current working handoff: /fork [brief]" };
    }
    if (t.startsWith("/skill")) {
      return { kind: "skill", label: "Send with a skill from the Skills screen: /skill <name> [task]" };
    }
    if (t.startsWith("/compact") && props.onSlashCompact) {
      return { kind: "compact", label: "Open the working handoff note so future turns stay lean." };
    }
    return null;
  });
  const add = async (files) => {
    const target = record();
    if (!target.ready()) return;
    const list = [...files].filter((f) => f && f.size !== 0);
    if (!list.length) return;
    setNotice("");
    setAdding((n) => n + list.length);
    const stored = [];
    try {
      const imgs = await Promise.all(list.map((f) => normalizeImage(f)));
      validateImages(imgs, target.images().length);
      for (const f of imgs) stored.push(props.draftKey ? await attachImage(f, f.name || "image.png") : { sha256: "", media_type: f.type, name: f.name || "image.png", size: f.size, blob: f, preview: URL.createObjectURL(f) });
      if (target.images().length + stored.length > MAX_IMAGES) throw new Error(`Up to ${MAX_IMAGES} images per message.`);
      target.setImages((current) => [...current, ...stored]);
    } catch (e) {
      if (!props.draftKey) for (const im of stored) revokePreview(im);
      setNotice(String(e?.message ?? e).replace(/^Error: /, ""));
    }
    finally { setAdding((n) => Math.max(0, n - list.length)); input?.focus(); }
  };
  const capture = async () => {
    setMenu(false);
    if (!canAttach()) return;
    setNotice("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const bytes = await invoke("capture_screenshot");
      const size = bytes?.byteLength ?? bytes?.length ?? 0;
      if (!size) return; // Cancelled.
      const stamp = new Date().toTimeString().slice(0, 8).replaceAll(":", ".");
      await add([new File([bytes], `Screenshot ${stamp}.png`, { type: "image/png" })]);
    } catch (e) {
      const msg = String(e?.message ?? e);
      if (msg === "screen-permission") setPermission(true);
      else setNotice(msg);
    }
  };
  const openScreenSettings = async () => {
    try { const { invoke } = await import("@tauri-apps/api/core"); await invoke("open_screen_settings"); }
    catch (e) { setNotice(String(e?.message ?? e)); }
  };
  createEffect(() => {
    const el = props.dropTarget?.();
    if (!el) return;
    const has = (e) => [...(e.dataTransfer?.types ?? [])].includes("Files");
    const enter = (e) => { if (has(e) && canAttach()) { e.preventDefault(); setOver(true); } };
    const leave = (e) => { if (!el.contains(e.relatedTarget)) setOver(false); };
    const drop = (e) => { if (!has(e)) return; e.preventDefault(); setOver(false); if (canAttach()) add(e.dataTransfer.files); };
    el.addEventListener("dragover", enter); el.addEventListener("dragleave", leave); el.addEventListener("drop", drop);
    onCleanup(() => { el.removeEventListener("dragover", enter); el.removeEventListener("dragleave", leave); el.removeEventListener("drop", drop); });
  });
  createEffect(on(() => props.draftKey, () => { setNotice(""); setMenu(false); record().setSending(false); }));
  onMount(() => {
    const onType = (e) => {
      if (!input || input.disabled || input.offsetParent === null) return;
      if (e.defaultPrevented || e.isComposing || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key.length !== 1) return;
      if (document.activeElement === input) return;
      if (document.querySelector(".overlay, .model-picker-pop")) return;
      if (e.target?.closest?.("input, textarea, select, [contenteditable=true], .xterm")) return;
      // Don't hijack space/enter on focused buttons/links.
      if (e.key === " " && e.target?.closest?.("button, a, [role=button], summary")) return;
      input.focus({ preventScroll: true });
    };
    window.addEventListener("keydown", onType);
    onCleanup(() => window.removeEventListener("keydown", onType));
  });
  const send = async () => {
    const target = record();
    const draft = target.text(), attached = target.images();
    const t = draft.trim();
    if (!target.ready() || (!t && !attached.length) || props.disabled || (props.busy && !props.onQueue) || sending() || adding()) return;
    // Intercept /effort and /model locally with 0 LLM tokens spent
    if (!attached.length && /^\/effort(\s+.*)?$/i.test(t)) {
      const arg = t.replace(/^\/effort\s*/i, "").trim().toLowerCase();
      const opts = effortOptions();
      if (!opts.length) {
        toast("The active provider/model does not accept reasoning effort parameters", "info");
        clearSentDraft(target, draft, attached);
        return;
      }
      const resolved = arg === "auto" || arg === "normal" ? "default" : arg;
      if (!resolved) {
        const list = opts.map((o) => o.value).join(" | ");
        toast(`Current effort: ${currentEffort()} (supported: ${list})`, "info");
        clearSentDraft(target, draft, attached);
        return;
      }
      const match = opts.find((o) => o.value.toLowerCase() === resolved || o.label.toLowerCase() === resolved);
      const nextVal = match ? match.value : resolved;
      selectEffort(nextVal);
      toast(`Reasoning effort set to ${nextVal} (0 tokens used)`, "secure");
      clearSentDraft(target, draft, attached);
      return;
    }
    if (!attached.length && /^\/model(\s+.*)?$/i.test(t) && props.onSlashModel) {
      const arg = t.replace(/^\/model\s*/i, "").trim();
      const ok = await props.onSlashModel(arg);
      if (ok !== false) clearSentDraft(target, draft, attached);
      return;
    }
    if (!attached.length && /^\/context(\s+.*)?$/i.test(t) && props.onSlashContext) {
      await props.onSlashContext();
      clearSentDraft(target, draft, attached);
      return;
    }
    if (!attached.length && /^\/remember(\s+.*)?$/i.test(t) && props.onSlashRemember) {
      const arg = t.replace(/^\/remember\s*/i, "").trim();
      const ok = await props.onSlashRemember(arg);
      if (ok !== false) clearSentDraft(target, draft, attached);
      return;
    }
    if (!attached.length && /^\/fork(\s+.*)?$/i.test(t) && props.onSlashFork) {
      const arg = t.replace(/^\/fork\s*/i, "").trim();
      const ok = await props.onSlashFork(arg);
      if (ok !== false) clearSentDraft(target, draft, attached);
      return;
    }
    if (!attached.length && /^\/compact(\s+.*)?$/i.test(t) && props.onSlashCompact) {
      const ok = await props.onSlashCompact(compactFocus(t));
      if (ok !== false) clearSentDraft(target, draft, attached);
      return;
    }
    target.setSending(true);
    try {
      let out = t;
      const sk = /^\/skill\s+([A-Za-z0-9_-]+)\s*([\s\S]*)$/.exec(t);
      if (sk) {
        const s = await rpc("skills.get", { name: sk[1], repo_id: activeRepoId() || null }, { fresh: true });
        out = `Use the "${s.name}" skill below.\n\n<skill name="${s.name}">\n${s.body}\n</skill>${sk[2].trim() ? `\n\n${sk[2].trim()}` : ""}`;
      }
      // While the agent works, a message is queued and delivered when the current step ends.
      out += await resolveMentions(out);
      const deliver = props.busy && props.onQueue ? props.onQueue : props.onSend;
      const accepted = await deliver(out, await sendableImages(attached));
      if (accepted !== false) { clearSentDraft(target, draft, attached); setPicked([]); }
    } catch (e) { toast(String(e), "danger"); }
    finally {
      target.setSending(false);
      queueMicrotask(() => { if (input && !input.disabled && input.offsetParent !== null) input.focus({ preventScroll: true }); });
    }
  };
  const remove = (image) => { setImages(images().filter((x) => x !== image)); input?.focus(); };
  // A queued message taken back into the box, in front of whatever is being typed.
  const editQueued = async (q) => {
    const back = await props.onTakeQueued?.(q);
    if (typeof back === "string") record().setText((old) => old?.trim() ? `${back}\n\n${old}` : back);
    input?.focus();
  };
  const empty = () => !text().trim() && !images().length;
  return (
    <div class="composer composer-card" classList={{ dragging: over() }}
      onDragOver={(e) => { if (!props.dropTarget && [...(e.dataTransfer?.types ?? [])].includes("Files")) { e.preventDefault(); setOver(true); } }}
      onDragLeave={(e) => { if (!props.dropTarget && !e.currentTarget.contains(e.relatedTarget)) setOver(false); }}
      onDrop={(e) => { if (props.dropTarget) return; e.preventDefault(); setOver(false); if (canAttach()) add(e.dataTransfer.files); }}>
      <Show when={over()}><div class="composer-drop" aria-hidden="true"><Icon name="image" size={16} /> Drop images to attach</div></Show>
      <Show when={props.queued?.length}>
        <div class="composer-queue" aria-label="Queued messages">
          <div class="xs dim">{props.busy ? `Queued · sent when ${props.queueTarget || "the agent"} finishes this step` : "Queued · sending now"}</div>
          <For each={props.queued}>{(q) => (
            <div class="composer-queued" data-queued={q.id}>
              <span class="composer-queued-text">{q.text}{q.images ? ` · ${q.images} image${q.images > 1 ? "s" : ""}` : ""}</span>
              <Show when={!q.images}><button type="button" class="link xs" onClick={() => editQueued(q)}>Edit</button></Show>
              <button type="button" class="link xs" onClick={() => props.onRemoveQueued?.(q)}>Remove</button>
            </div>
          )}</For>
        </div>
      </Show>
      <Show when={slashHint()}>
        {(h) => (
          <div class="composer-slash-bar xs">
            <span class="mono">{h().label}</span>
            <Show when={h().kind === "effort" && effortOptions().length > 0}>
              <div class="row gap-4">
                <For each={effortOptions()}>
                  {(ef) => (
                    <button type="button" class="btn sm ghost" title={ef.hint || ef.value} onClick={() => { selectEffort(ef.value); setText(""); }}>
                      {ef.label}
                    </button>
                  )}
                </For>
              </div>
            </Show>
          </div>
        )}
      </Show>
      <Show when={images().length || adding()}>
        <div class="composer-images" role="list" aria-label="Attached images">
          <For each={images()}>
            {(im) => (
              <div class="composer-image" role="listitem">
                <button type="button" class="composer-thumb" title={`${im.name} · ${formatSize(im.size || 0)} · click to preview`} onClick={() => setLightbox({ src: im.preview, name: im.name })}>
                  <img src={im.preview} alt={im.name} />
                </button>
                <button type="button" class="composer-image-remove" aria-label={`Remove ${im.name}`} title="Remove image" onClick={() => remove(im)}>×</button>
              </div>
            )}
          </For>
          <Show when={adding()}><div class="composer-image pending" role="status" aria-label="Adding image"><span class="spinner" /></div></Show>
        </div>
      </Show>
      <Show when={slashMenu().length}>
        <div class="composer-slashes" role="listbox" aria-label="Slash commands">
          <For each={slashMenu()}>{(c, i) => (
            <div class="opt" role="option" aria-selected={i() === slashIdx()} classList={{ on: i() === slashIdx() }}
              onMouseEnter={() => setSlashIdx(i())}
              onMouseDown={(e) => { e.preventDefault(); pickSlash(c); }}>
              <span class="mono slash-cmd">{c.cmd}</span>
              <span class="xs dim grow ellipsis">{c.desc}</span>
              <span class="kbd">{c.immediate ? "↵" : "Tab"}</span>
            </div>
          )}</For>
        </div>
      </Show>
      <Show when={suggest().length}>
        <div class="composer-mentions" id="composer-mentions" role="listbox" aria-label="Mention a file or symbol">
          <For each={suggest()}>{(m, i) => (
            <div class="opt" role="option" aria-selected={i() === suggestIdx()} classList={{ on: i() === suggestIdx() }}
              onMouseDown={(e) => { e.preventDefault(); pickMention(m); }}>
              <span class="xs dim">{m.kind === "symbol" ? "symbol" : "file"}</span> <span class="ellipsis">{m.label}</span>
              <Show when={m.detail}><span class="xs dim"> {m.detail}</span></Show>
            </div>
          )}</For>
        </div>
      </Show>
      <textarea
        ref={input}
        class="textarea composer-input"
        rows="1"
        placeholder={props.placeholder ?? "Message… (/ for commands, @ for files)"}
        value={text()}
        disabled={props.disabled || !record().ready()}
        onInput={(e) => { setText(e.currentTarget.value); syncHeight(); updateMention(); }}
        onBlur={() => setTimeout(() => setSuggest([]), 150)}
        onPaste={(e) => { const f = [...(e.clipboardData?.files ?? [])]; if (f.length) { e.preventDefault(); add(f); } }}
        role="combobox" aria-expanded={suggest().length > 0 || slashMenu().length > 0} aria-controls="composer-mentions" aria-autocomplete="list"
        onKeyDown={(e) => {
          if (slashMenu().length && !e.isComposing) {
            if (e.key === "ArrowDown") { e.preventDefault(); setSlashIdx((i) => (i + 1) % slashMenu().length); return; }
            if (e.key === "ArrowUp") { e.preventDefault(); setSlashIdx((i) => (i - 1 + slashMenu().length) % slashMenu().length); return; }
            if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) { e.preventDefault(); pickSlash(slashMenu()[slashIdx()]); return; }
            if (e.key === "Escape") { e.preventDefault(); setText(""); return; }
          }
          if (suggest().length && !e.isComposing) {
            if (e.key === "ArrowDown") { e.preventDefault(); setSuggestIdx((i) => (i + 1) % suggest().length); return; }
            if (e.key === "ArrowUp") { e.preventDefault(); setSuggestIdx((i) => (i - 1 + suggest().length) % suggest().length); return; }
            if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); pickMention(suggest()[suggestIdx()]); return; }
            if (e.key === "Escape") { e.preventDefault(); setSuggest([]); return; }
          }
          if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
        }}
      />
      <div class="composer-bar">
        <div class="composer-attach" ref={attachEl}>
          <button type="button" class="composer-icon" aria-label="Attach" aria-haspopup="menu" aria-expanded={menu()} title="Attach images or a screenshot" disabled={!canAttach()} onClick={() => setMenu(!menu())}><Icon name="plus" size={14} /></button>
          <Show when={menu()}>
            <div class="composer-menu" role="menu" onKeyDown={(e) => e.key === "Escape" && setMenu(false)}>
              <button type="button" role="menuitem" onClick={() => { setMenu(false); picker.click(); }}><Icon name="image" size={13} /> Choose images…</button>
              <button type="button" role="menuitem" onClick={capture}><Icon name="camera" size={13} /> Take screenshot…</button>
              <div class="composer-menu-hint">Paste or drop images anywhere · PNG, JPEG, GIF, WebP · up to {MAX_IMAGES}<br />No permission needed: press <kbd>⌃⇧⌘4</kbd>, then <kbd>⌘V</kbd> here</div>
            </div>
          </Show>
          <input ref={picker} type="file" accept="image/png,image/jpeg,image/gif,image/webp" multiple hidden onChange={(e) => { add(e.currentTarget.files); e.currentTarget.value = ""; }} />
        </div>
        <Show when={(props.onEffort || props.onSpeed) && effortOptions().length > 0}>
          <div class="speed" role="group" aria-label="Reasoning effort" aria-describedby="effort-caveat" title="Reasoning effort (or type /effort in composer). Support varies by provider and model, and speed, cost and quality are not guaranteed.">
            <span id="effort-caveat" class="sr-only">Requests a reasoning effort level. Support varies by provider and model, and speed, cost and quality are not guaranteed.</span>
            <For each={effortOptions()}>{(s) => (
              <button
                type="button"
                disabled={props.disabled || props.effortPending}
                aria-pressed={currentEffort() === s.value}
                classList={{ on: currentEffort() === s.value }}
                onClick={() => selectEffort(s.value)}
                title={s.hint ? `${s.hint} (/effort ${s.value})` : `/effort ${s.value}`}
              >
                {s.label}
              </button>
            )}</For>
          </div>
        </Show>
        <div class="composer-status xs dim grow" role="status">
          <Show when={notice()} fallback={
            record().loading() ? "Restoring draft…" :
            props.effortPending ? "Updating effort…" :
            props.busy ? (props.onQueue ? "Working. Enter queues your message for when this step ends. Stop interrupts." : "Working. Your draft is kept. Stop interrupts.") : ""}>
            <span class="composer-notice" role="alert">{notice()} <button type="button" class="link" onClick={() => setNotice("")}>Dismiss</button></span>
          </Show>
          <Show when={permission()}>
            <span class="composer-notice composer-permission" role="alert">
              Screenshots need Screen Recording for Philium. <button type="button" class="link" onClick={openScreenSettings}>Open Settings</button>, turn on Philium, then reopen it. Or press <kbd>⌃⇧⌘4</kbd> and paste with <kbd>⌘V</kbd>, no permission needed. <button type="button" class="link" onClick={() => setPermission(false)}>Dismiss</button>
            </span>
          </Show>
          <Show when={record().error()}><span class="composer-notice" role="alert">{record().error()} <button type="button" class="link" disabled={record().loading() || record().pending()} onClick={() => record().retry?.()}>Retry</button></span></Show>
        </div>
        <Show when={props.busy && props.onSteer && !empty() && !images().length}>
          <button type="button" class="btn sm ghost" title="Add this to the current turn after the running tool finishes. If the agent cannot take it mid-turn, it is queued instead."
            disabled={props.disabled || !record().ready() || sending()}
            onClick={async () => {
              const target = record(), draft = target.text();
              target.setSending(true);
              try { if ((await props.onSteer(draft.trim())) !== false) clearSentDraft(target, draft, []); }
              finally { target.setSending(false); }
            }}>Steer now</button>
        </Show>
        <Show when={props.busy && props.onQueue && !empty()}>
          <button type="button" class="composer-send" aria-label="Queue message" title="Queue (Enter): sent when this step ends" onClick={send} disabled={props.disabled || !record().ready() || sending() || adding() > 0}><Icon name="send" size={13} /></button>
        </Show>
        <Show when={props.busy} fallback={
          <button type="button" class="composer-send" aria-label="Send" title="Send (Enter) · Shift+Enter for a new line" onClick={send} disabled={props.disabled || !record().ready() || sending() || adding() > 0 || empty()}><Icon name="send" size={14} /></button>}>
          <button type="button" class="composer-send stop" aria-label="Stop" title="Stop the current turn" onClick={props.onCancel}><Icon name="stop" size={12} /></button>
        </Show>
      </div>
    </div>
  );
}
