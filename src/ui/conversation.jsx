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
import { draftFor, newDraft, clearSentDraft, attachImage, sendableImages } from "../lib/drafts.js";
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
      case "subagent.started": items.push({ kind: "note", text: `Subagent started: ${e.description ?? e.agent ?? ""}`, seq: e.seq }); break;
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
export function Conversation(props) {
  let el, timelineEl;
  const [store, setStore] = createStore({ items: [] });
  const [stick, setStick] = createSignal(true);
  let programmaticScroll = false;
  let smoothScrolling = false;
  let scrollRaf = 0;
  let programmaticTimer = 0;
  let lastScrollTop = 0;
  let expectedScrollTop = 0;
  let expectedScrollHeight = 0;
  const scrollToBottom = (smooth = false) => {
    if (!el) return;
    if (programmaticTimer) { clearTimeout(programmaticTimer); programmaticTimer = 0; }
    if (smooth && typeof el.scrollTo === "function") {
      if (el.scrollHeight - el.scrollTop - el.clientHeight < 2) {
        programmaticScroll = false;
        smoothScrolling = false;
        return;
      }
      programmaticScroll = true;
      smoothScrolling = true;
      expectedScrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
      expectedScrollHeight = el.scrollHeight;
      lastScrollTop = el.scrollTop;
      el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
      programmaticTimer = setTimeout(() => {
        programmaticScroll = false;
        smoothScrolling = false;
        programmaticTimer = 0;
      }, 450);
    } else {
      const before = el.scrollTop;
      el.scrollTop = el.scrollHeight;
      lastScrollTop = el.scrollTop;
      if (el.scrollTop !== before) {
        programmaticScroll = true;
        smoothScrolling = false;
        expectedScrollTop = el.scrollTop;
        expectedScrollHeight = el.scrollHeight;
        programmaticTimer = setTimeout(() => {
          programmaticScroll = false;
          smoothScrolling = false;
          programmaticTimer = 0;
        }, 40);
      } else {
        programmaticScroll = false;
        smoothScrolling = false;
      }
    }
  };
  const scheduleScrollToBottom = () => {
    if (!stick() || !el || scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = 0;
      if (stick()) scrollToBottom(false);
    });
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
        setStore("items", store.items.length, { kind: "note", text: `Subagent started: ${e.description ?? e.agent ?? ""}`, seq: e.seq });
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
            nextItems.push({ kind: "note", text: `Subagent started: ${e.description ?? e.agent ?? ""}`, seq: e.seq });
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
    if (timelineEl && typeof ResizeObserver !== "undefined") {
      const ro = new ResizeObserver(() => {
        if (stick()) scheduleScrollToBottom();
      });
      ro.observe(timelineEl);
      onCleanup(() => ro.disconnect());
    }
    if (stick()) scheduleScrollToBottom();
  });
  onCleanup(() => {
    if (scrollRaf) cancelAnimationFrame(scrollRaf);
    if (programmaticTimer) clearTimeout(programmaticTimer);
  });

  createEffect(on(() => {
    const last = store.items.at(-1);
    return [store.items.length, last?.text?.length, last?.out?.length, last?.result];
  }, () => {
    if (stick()) scheduleScrollToBottom();
  }));
  return (
    <div
      class={`conv conv-${conversationView()}`}
      style={props.hidden ? { display: "none" } : undefined}
      ref={el}
      onWheel={(e) => { if (e.deltaY < 0) { programmaticScroll = false; smoothScrolling = false; } }}
      onPointerDown={() => { programmaticScroll = false; smoothScrolling = false; }}
      onScroll={(e) => {
        const d = e.currentTarget;
        const atBottom = d.scrollHeight - d.scrollTop - d.clientHeight < 60;
        const scrolledUp = d.scrollTop < lastScrollTop - 2;
        if (programmaticScroll) {
          const layoutGrewAfterSnap = !smoothScrolling && expectedScrollTop > 0 && Math.abs(d.scrollTop - expectedScrollTop) <= 4 && d.scrollHeight > expectedScrollHeight;
          const userInterrupted = smoothScrolling
            ? scrolledUp
            : !atBottom && !layoutGrewAfterSnap;
          if (userInterrupted) {
            programmaticScroll = false;
            smoothScrolling = false;
            lastScrollTop = d.scrollTop;
            setStick(false);
          } else {
            lastScrollTop = d.scrollTop;
            if (atBottom) {
              programmaticScroll = false;
              smoothScrolling = false;
            }
          }
          return;
        }
        lastScrollTop = d.scrollTop;
        setStick(atBottom);
      }}
    >
      <ViewToggle />
      <Lightbox />
      <div class="timeline" ref={timelineEl}>
        <Show when={store.items.length === 0}>
          <div class="conv-empty">{props.emptyText ?? "No messages yet."}</div>
        </Show>
        <Index each={store.items}>
          {(it) => (
            <Switch>
              <Match when={it().kind === "user"}><div class="msg user"><SentImages item={it()} load={props.loadAttachment} />{it().text}</div></Match>
              <Match when={it().kind === "assistant"}>
                <div class={`msg assistant ${it().live && props.busy && !props.readOnly ? "live" : ""}`}>
                  <AssistantReply text={it().text} live={it().live && props.busy && !props.readOnly} />
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
                <div class={`perm ${it().open ? "" : "done"}`} style={{ margin: 0, "flex-wrap": "wrap", "align-items": "flex-start" }}>
                  <Icon name="shield" size={14} />
                  <div class="grow" style={{ "min-width": 0, "flex-basis": "calc(100% - 28px)" }}>
                    <Show when={it().action === "mcp" && it().resource?.startsWith("philium/browser_")} fallback={<>Wants to <b>{it().action}</b> <code class="mono">{it().resource}</code></>}>
                      <b>Browser permission</b>: <code class="mono">{it().resource?.replace("philium/", "")}</code>
                      <Show when={it().args}><pre class="conv-pre">{JSON.stringify(it().args, null, 2)}</pre></Show>
                      <span class="dim xs">Page content may be sent to this agent's provider. Session approval applies only to this tool.</span>
                    </Show>
                  </div>
                  <Show when={it().open && !props.readOnly} fallback={<Badge tone={it().open ? "" : /^(appr|allow)/.test(it().decision ?? "") ? "secure" : "danger"}>{it().open ? "Historical request · agent stopped" : it().decision ?? "resolved"}</Badge>}>
                    <div class="row" style={{ width: "100%", "justify-content": "flex-end", "flex-wrap": "wrap" }}>
                      <Button size="sm" onClick={() => props.onDeny?.(it().id)}>Deny</Button>
                      <Button size="sm" onClick={() => props.onApprove?.(it().id, "once")}>Allow once</Button>
                      <Button size="sm" variant="primary" onClick={() => props.onApprove?.(it().id, "session")}>Allow for session</Button>
                    </div>
                  </Show>
                </div>
              </Match>
              <Match when={it().kind === "error"}><div class="msg error"><Show when={String(it().text).includes("philium_guard")} fallback={<PlainError text={it().text} />}><strong>Request blocked by secret guard</strong><p class="small">This conversation contains content the guard identified as a secret. This request was not sent to the provider. Start a fresh conversation without sensitive content.</p><details class="tool-raw"><summary>Error details</summary><pre class="conv-pre">{it().text}</pre></details></Show></div></Match>
              <Match when={it().kind === "note"}><div class="faint xs" style={{ "align-self": "center" }}>{it().text}</div></Match>
            </Switch>
          )}
        </Index>
        <ConversationActivity events={props.events ?? []} busy={props.busy} readOnly={props.readOnly || props.hidden} />
      </div>
      <Show when={!stick() && store.items.length > 0}>
        <div class="conv-jump-wrap">
          <button type="button" class="conv-jump" onClick={() => { setStick(true); scrollToBottom(true); }}>
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

/**
 * AffectUI-style live KV Cache & Context Token Bar.
 * Visualizes Cached Prefix (emerald) vs Fresh Uncached Input (blue) vs Output (violet),
 * plus Hit %, TTFT (ms), and request count. 100% out-of-band (0 LLM tokens used).
 */
export function KvTelemetryBar(props) {
  const kv = () => props.kv || {};
  const reqs = () => Number(kv().requests || 0);
  const inp = () => Number(kv().input_tokens || kv().tokens_in || 0);
  const cached = () => Math.min(inp(), Number(kv().cached_tokens || kv().tokens_cached || 0));
  const fresh = () => Math.max(0, inp() - cached());
  const out = () => Number(kv().output_tokens || kv().tokens_out || 0);
  const total = () => Math.max(1, cached() + fresh() + out());
  const hitPct = () => (inp() > 0 ? Math.round((cached() / inp()) * 100) : 0);
  const ttft = () => kv().avg_ttft_ms ?? kv().p50_ttft_ms;
  const tone = () => (hitPct() >= 60 ? "hot" : hitPct() >= 25 ? "warm" : reqs() > 0 ? "cold" : "idle");
  const fmtTok = (n) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(2)}M` : n >= 1_000 ? `${(n / 1_000).toFixed(1)}k` : String(Math.round(n)));

  return (
    <div class={`kv-telemetry-bar ${props.compact ? "compact" : ""}`} title="Out-of-band Gateway KV Cache & Token Telemetry (0 extra prompt tokens)">
      <div class="kv-telemetry-top">
        <span class={`kv-hit-pill ${tone()}`}>
          <span class="kv-hit-dot" />
          KV {reqs() > 0 ? `${hitPct()}%` : "—"}
        </span>
        <span class="kv-legend-item cached" title="Cached prefix tokens (shared repo map + conversation prefix)">
          <i /> Cached <b>{fmtTok(cached())}</b>
        </span>
        <span class="kv-legend-item fresh" title="Uncached fresh input tokens">
          <i /> Fresh <b>{fmtTok(fresh())}</b>
        </span>
        <span class="kv-legend-item out" title="Generated output tokens">
          <i /> Out <b>{fmtTok(out())}</b>
        </span>
        <div class="grow" />
        <Show when={ttft() != null}>
          <span class="mono xs faint" title="Average Time To First Token">TTFT {Math.round(ttft())}ms</span>
        </Show>
        <span class="mono xs faint">{reqs()} req{reqs() === 1 ? "" : "s"}</span>
      </div>
      <div class="kv-bar-track" role="img" aria-label={`KV cache hit rate ${hitPct()}%`}>
        <div class="kv-bar-seg cached" style={{ width: `${reqs() > 0 ? (cached() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg fresh" style={{ width: `${reqs() > 0 ? (fresh() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg out" style={{ width: `${reqs() > 0 ? (out() / total()) * 100 : 0}%` }} />
      </div>
    </div>
  );
}

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
  const syncHeight = () => {
    if (!input) return;
    if (!text()) {
      input.style.height = "";
      input.style.overflowY = "hidden";
      return;
    }
    input.style.height = "auto";
    const sh = input.scrollHeight;
    input.style.height = sh > 0 ? `${Math.min(sh, 200)}px` : "";
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
  const slashHint = createMemo(() => {
    const t = text().trimStart();
    if (!t.startsWith("/")) return null;
    if (t.startsWith("/effort")) {
      const opts = effortOptions();
      const summary = opts.length
        ? opts.map((o) => o.value).join(" | ")
        : "not supported by the active model";
      return { kind: "effort", label: `Zero-token local command: /effort ${summary}` };
    }
    if (t.startsWith("/model")) {
      return { kind: "model", label: "Press Enter to choose from a list, or type a name: /model sonnet. Switching costs no tokens." };
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
  createEffect(on(() => props.draftKey, () => { setNotice(""); setMenu(false); }));
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
      await props.onSlashCompact();
      clearSentDraft(target, draft, attached);
      return;
    }
    target.setSending(true);
    try {
      // While the agent works, a message is queued and delivered when the current step ends.
      const deliver = props.busy && props.onQueue ? props.onQueue : props.onSend;
      const accepted = await deliver(t, await sendableImages(attached));
      if (accepted !== false) clearSentDraft(target, draft, attached);
    } catch (e) { toast(String(e), "danger"); }
    finally { target.setSending(false); }
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
      <textarea
        ref={input}
        class="textarea composer-input"
        rows="1"
        placeholder={props.placeholder ?? "Message (or /effort, /model)"}
        value={text()}
        disabled={props.disabled || !record().ready()}
        onInput={(e) => { setText(e.currentTarget.value); syncHeight(); }}
        onPaste={(e) => { const f = [...(e.clipboardData?.files ?? [])]; if (f.length) { e.preventDefault(); add(f); } }}
        onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); } }}
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
