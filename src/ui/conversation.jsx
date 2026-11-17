// Conversation view built from HAL session events. Used by Agents and Chat.
// Events are folded into items: user / assistant (streamed) / tool / file /
// permission / error. Assistant Markdown is rendered without raw HTML.
import { For, Index, Show, Switch, Match, createSignal, createMemo, createEffect, on, onCleanup } from "solid-js";
import { Portal } from "solid-js/web";
import { Badge, Button, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { AssistantReply } from "./markdown.jsx";
import { ConversationActivity } from "./conversation-activity.jsx";
import { ViewToggle } from "./conversation-view.jsx";
import { conversationView } from "../lib/conversation-view.js";
import "../styles/composer.css";
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

function argSummary(tool, args) {
  if (!args || typeof args !== "object") return "";
  const t = short(tool);
  if (t === "shell_run") return args.command ?? "";
  if (args.path) return args.path;
  if (args.file_path) return args.file_path;
  if (args.pattern) return args.pattern;
  if (args.title) return args.title;
  if (args.to) return `to ${args.to}`;
  if (args.id) return args.id;
  const s = JSON.stringify(args);
  return s.length > 120 ? s.slice(0, 120) + "…" : s;
}

import { toolOutput, cleanTerminalText, shellStatus } from "../lib/tool-presentation.js";

function Tool(props) {
  const [open, setOpen] = createSignal(null);
  const expanded = () => open() ?? props.terminal;
  const t = () => props.item;
  const state = () => shellStatus(t().result);
  const resultText = () => {
    return cleanTerminalText(toolOutput(t().result, t().out));
  };
  const active = () => props.active && !t().inactive;
  const [elapsed, setElapsed] = createSignal(0);
  const [silent, setSilent] = createSignal(0);
  const activeCall = createMemo(() => active() && t().running ? t().seq : null);
  const output = createMemo(() => t().out);
  createEffect(() => {
    if (activeCall() == null) return;
    // Observation time, not execution time: history may have arrived late.
    const started = Date.now();
    setElapsed(0);
    const timer = setInterval(() => setElapsed(Math.floor((Date.now() - started) / 1000)), 1000);
    onCleanup(() => clearInterval(timer));
  });
  createEffect(() => {
    output();
    if (activeCall() == null) return;
    const updated = Date.now();
    setSilent(0);
    const timer = setInterval(() => setSilent(Math.floor((Date.now() - updated) / 1000)), 1000);
    onCleanup(() => clearInterval(timer));
  });
  return (
    <div class="msg tool conv-tool">
      <button type="button" class="row conv-tool-toggle" aria-expanded={!!expanded()} onClick={() => setOpen(!expanded())}>
        <span class={`dot ${t().running ? active() ? "busy" : "" : t().ok === false || (state()?.startsWith("Exit ") && state() !== "Exit 0") ? "bad" : state() === "Still running" ? "" : "live"}`} />
        <b>{short(t().tool) === "shell_run" ? "Shell" : short(t().tool) === "shell_read" ? "Shell output" : short(t().tool)}</b>
        <span class="ellipsis dim grow">{t().args?.description ?? t().args?.intent ?? (short(t().tool) === "shell_run" ? t().args?.shell ?? "Command" : argSummary(t().tool, t().args))}</span>
        <span class="xs dim">{t().running ? active() ? "Running" : "Unconfirmed" : t().ok === false ? "Failed" : (shellStatus(t().result) === "Still running" ? "Running at return" : shellStatus(t().result)) ?? "Returned"}</span>
        <Show when={t().ms != null}><span class="faint xs">{(t().ms / 1000).toFixed(1)}s</span></Show>
        <span class="dim" aria-hidden="true">{expanded() ? "▾" : "▸"}</span>
      </button>
      <Show when={short(t().tool) === "shell_run" && t().args?.command}><pre class="tool-command"><span class="dim">$ </span>{expanded() ? t().args.command : t().args.command.split("\n").slice(0, 2).join("\n").slice(0, 240)}</pre><Show when={!expanded() && (t().args.command.length > 240 || t().args.command.split("\n").length > 2)}><span class="tool-preview-label">Command shortened · expand to read all</span></Show></Show>
      <Show when={!expanded() && resultText()}><pre class="tool-output-preview">{resultText().split("\n").slice(-4).join("\n").slice(-1200)}</pre><span class="tool-preview-label">Output preview · expand for details</span></Show>
      <Show when={t().running}>
        <div class="xs dim" role="status">{active()
          ? `Awaiting tool completion · observed ${elapsed()}s${silent() >= 30 ? ` · No new output for ${silent()}s. It may be working or waiting for approval. This is not a completion signal.` : ""}`
          : "No tool completion recorded in loaded history."}</div>
      </Show>
      <Show when={expanded()}>
        <Show when={short(t().tool) !== "shell_run"}><pre class="conv-pre">{JSON.stringify(t().args, null, 2)}</pre></Show>
        <Show when={resultText()}><pre class="conv-pre">{resultText().slice(0, 20000)}</pre><Show when={resultText().length > 20000}><span class="xs dim">Showing the first 20,000 characters of tool output.</span></Show></Show>
        <details class="tool-raw"><summary>Raw tool data</summary><pre class="conv-pre">{JSON.stringify({ arguments: t().args, result: t().result }, null, 2)?.slice(0, 20000)}</pre></details>
      </Show>
    </div>
  );
}

/**
 * props: events (array, reactive), onApprove(id, scope), onDeny(id), busy
 */
export function Conversation(props) {
  let el;
  const items = createMemo(() => fold(props.events ?? []));
  const [stick, setStick] = createSignal(true);
  createEffect(on(() => [items().length, items().at(-1)?.text?.length], () => {
    if (stick() && el) queueMicrotask(() => (el.scrollTop = el.scrollHeight));
  }));
  return (
    <div class={`conv conv-${conversationView()}`} style={props.hidden ? { display: "none" } : undefined} ref={el} onScroll={(e) => {
      const d = e.currentTarget;
      setStick(d.scrollHeight - d.scrollTop - d.clientHeight < 60);
    }}>
      <ViewToggle />
      <Lightbox />
      <div class="timeline">
        <Show when={items().length === 0}>
          <div class="conv-empty">{props.emptyText ?? "No messages yet."}</div>
        </Show>
        <Index each={items()}>
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
              <Match when={it().kind === "tool"}><Tool item={it()} active={props.busy && !props.readOnly} terminal={conversationView() === "terminal"} /></Match>
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
              <Match when={it().kind === "error"}><div class="msg error"><Show when={String(it().text).includes("philium_guard")} fallback={it().text}><strong>Request blocked by secret guard</strong><p class="small">This conversation contains content the guard identified as a secret. This request was not sent to the provider. Start a fresh conversation without sensitive content.</p><details class="tool-raw"><summary>Error details</summary><pre class="conv-pre">{it().text}</pre></details></Show></div></Match>
              <Match when={it().kind === "note"}><div class="faint xs" style={{ "align-self": "center" }}>{it().text}</div></Match>
            </Switch>
          )}
        </Index>
        <ConversationActivity events={props.events ?? []} busy={props.busy} readOnly={props.readOnly || props.hidden} />
      </div>
    </div>
  );
}

const formatSize = (n) => n >= 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;

/** Full-size image overlay. Esc or click closes. */
const [lightbox, setLightbox] = createSignal(null);
export function Lightbox() {
  const close = () => setLightbox(null);
  const key = (e) => { if (e.key === "Escape" && lightbox()) close(); };
  window.addEventListener("keydown", key);
  onCleanup(() => window.removeEventListener("keydown", key));
  return (
    <Show when={lightbox()}><Portal>
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

/** Effort presets. Names are not guarantees of latency, cost or quality. */
export const SPEEDS = [
  { value: "fast", label: "Fast", effort: "low", hint: "Requests low reasoning effort, not guaranteed speed" },
  { value: "normal", label: "Normal", effort: "default", hint: "The model's default effort" },
  { value: "deep", label: "Deep", effort: "high", hint: "Requests high reasoning effort, not guaranteed quality" },
];
export const EFFORTS = [
  { value: "none", label: "None", budget: "0t", hint: "Requests no extended thinking (0 thinking tokens)" },
  { value: "low", label: "Low", budget: "2k", hint: "Requests low reasoning effort (~2,048 thinking token budget on Claude)" },
  { value: "default", label: "Auto", budget: "def", hint: "The model's default effort" },
  { value: "medium", label: "Med", budget: "8k", hint: "Requests medium reasoning effort (~8,192 thinking token budget on Claude)" },
  { value: "high", label: "High", budget: "16k", hint: "Requests high reasoning effort (~16,384 thinking token budget on Claude)" },
  { value: "xhigh", label: "Max", budget: "32k", hint: "Requests maximum reasoning effort (~32,000 thinking token budget on Claude)" },
];
export const speedEffort = (v) => {
  if (EFFORTS.some((e) => e.value === v) || v === "minimal" || v === "max") return v === "max" ? "xhigh" : v === "minimal" ? "low" : v;
  return SPEEDS.find((s) => s.value === v)?.effort ?? "default";
};
export const effortSpeed = (e) => (e === "low" || e === "minimal" || e === "none" ? "fast" : e === "high" || e === "xhigh" || e === "max" ? "deep" : "normal");

/** Composer card: attachments, text, then one toolbar row.
 * Images arrive by paste, drop (anywhere over the agent pane via props.dropTarget),
 * Attach menu file picker, or native screenshot. onSend(text, images) receives HAL
 * image parts [{ data: base64, media_type, name }] built only at send time. */
export function Composer(props) {
  const localDraft = newDraft();
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
  let input, picker;
  const canAttach = () => record().ready() && !props.disabled;
  const currentEffort = () => {
    const raw = props.effort || speedEffort(props.speed || "normal");
    return raw === "max" ? "xhigh" : raw === "minimal" ? "low" : raw;
  };
  const selectEffort = (val) => {
    if (props.onEffort) props.onEffort(val);
    else if (props.onSpeed) props.onSpeed(val);
  };
  const slashHint = createMemo(() => {
    const t = text().trimStart();
    if (!t.startsWith("/")) return null;
    if (t.startsWith("/effort")) {
      return { kind: "effort", label: "Zero-token local command: /effort none | low | default | medium | high | max" };
    }
    if (t.startsWith("/model")) {
      return { kind: "model", label: "Zero-token in-session model switch: /model <provider/model or substring>" };
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
    try {
      const imgs = await Promise.all(list.map((f) => normalizeImage(f)));
      validateImages(imgs, target.images().length);
      const stored = [];
      for (const f of imgs) stored.push(props.draftKey ? await attachImage(f, f.name || "image.png") : { sha256: "", media_type: f.type, name: f.name || "image.png", size: f.size, blob: f, preview: URL.createObjectURL(f) });
      if (target.images().length + stored.length > MAX_IMAGES) throw new Error(`Up to ${MAX_IMAGES} images per message.`);
      target.setImages((current) => [...current, ...stored]);
    } catch (e) { setNotice(String(e?.message ?? e).replace(/^Error: /, "")); }
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
    if (!target.ready() || (!t && !attached.length) || props.disabled || props.busy || sending() || adding()) return;
    // Intercept /effort and /model locally with 0 LLM tokens spent
    if (!attached.length && /^\/effort(\s+.*)?$/i.test(t)) {
      const arg = t.replace(/^\/effort\s*/i, "").trim().toLowerCase();
      const alias = { auto: "default", normal: "default", fast: "low", deep: "high", med: "medium", max: "xhigh", off: "none" };
      const resolved = alias[arg] || arg;
      if (!resolved) {
        toast(`Current effort: ${currentEffort()} (use /effort none|low|default|medium|high|max)`, "info");
        clearSentDraft(target, draft, attached);
        return;
      }
      if (["default", "none", "minimal", "low", "medium", "high", "xhigh", "max"].includes(resolved)) {
        selectEffort(resolved === "max" ? "xhigh" : resolved);
        toast(`Reasoning effort set to ${resolved} (0 tokens used)`, "secure");
        clearSentDraft(target, draft, attached);
        return;
      }
      toast(`Unknown effort "${arg}". Choose: none, low, default, medium, high, max`, "danger");
      return;
    }
    if (!attached.length && /^\/model(\s+.*)?$/i.test(t) && props.onSlashModel) {
      const arg = t.replace(/^\/model\s*/i, "").trim();
      const ok = await props.onSlashModel(arg);
      if (ok !== false) clearSentDraft(target, draft, attached);
      return;
    }
    target.setSending(true);
    try {
      const accepted = await props.onSend(t, await sendableImages(attached));
      if (accepted !== false) clearSentDraft(target, draft, attached);
    } catch (e) { toast(String(e), "danger"); }
    finally { target.setSending(false); }
  };
  const remove = (image) => { setImages(images().filter((x) => x !== image)); input?.focus(); };
  const empty = () => !text().trim() && !images().length;
  return (
    <div class="composer composer-card" classList={{ dragging: over() }}
      onDragOver={(e) => { if (!props.dropTarget && [...(e.dataTransfer?.types ?? [])].includes("Files")) { e.preventDefault(); setOver(true); } }}
      onDragLeave={(e) => { if (!props.dropTarget && !e.currentTarget.contains(e.relatedTarget)) setOver(false); }}
      onDrop={(e) => { if (props.dropTarget) return; e.preventDefault(); setOver(false); if (canAttach()) add(e.dataTransfer.files); }}>
      <Show when={over()}><div class="composer-drop" aria-hidden="true"><Icon name="image" size={16} /> Drop images to attach</div></Show>
      <Show when={slashHint()}>
        {(h) => (
          <div class="composer-slash-bar xs">
            <span class="mono">{h().label}</span>
            <Show when={h().kind === "effort"}>
              <div class="row gap-4">
                <For each={EFFORTS}>
                  {(ef) => (
                    <button type="button" class="btn sm ghost" onClick={() => { selectEffort(ef.value); setText(""); }}>
                      {ef.label} ({ef.budget})
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
        onInput={(e) => setText(e.currentTarget.value)}
        onPaste={(e) => { const f = [...(e.clipboardData?.files ?? [])]; if (f.length) { e.preventDefault(); add(f); } }}
        onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); } }}
      />
      <div class="composer-bar">
        <div class="composer-attach">
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
        <Show when={props.onEffort || props.onSpeed}>
          <div class="speed" role="group" aria-label="Reasoning effort" aria-describedby="effort-caveat" title="Reasoning effort (or type /effort in composer). Support varies by provider and model, and speed, cost and quality are not guaranteed.">
            <span id="effort-caveat" class="sr-only">Requests a reasoning effort level. Support varies by provider and model, and speed, cost and quality are not guaranteed.</span>
            <For each={EFFORTS}>{(s) => (
              <button
                type="button"
                disabled={props.disabled || props.effortPending}
                aria-pressed={currentEffort() === s.value}
                classList={{ on: currentEffort() === s.value }}
                onClick={() => selectEffort(s.value)}
                title={`${s.hint} (/effort ${s.value})`}
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
            props.busy ? "Working. Your draft is kept. Stop interrupts." : ""}>
            <span class="composer-notice" role="alert">{notice()} <button type="button" class="link" onClick={() => setNotice("")}>Dismiss</button></span>
          </Show>
          <Show when={permission()}>
            <span class="composer-notice composer-permission" role="alert">
              Screenshots need Screen Recording for Philium. <button type="button" class="link" onClick={openScreenSettings}>Open Settings</button>, turn on Philium, then reopen it. Or press <kbd>⌃⇧⌘4</kbd> and paste with <kbd>⌘V</kbd>, no permission needed. <button type="button" class="link" onClick={() => setPermission(false)}>Dismiss</button>
            </span>
          </Show>
          <Show when={record().error()}><span class="composer-notice" role="alert">{record().error()} <button type="button" class="link" disabled={record().loading() || record().pending()} onClick={() => record().retry?.()}>Retry</button></span></Show>
        </div>
        <Show when={props.busy} fallback={
          <button type="button" class="composer-send" aria-label="Send" title="Send (Enter) · Shift+Enter for a new line" onClick={send} disabled={props.disabled || !record().ready() || sending() || adding() > 0 || empty()}><Icon name="send" size={14} /></button>}>
          <button type="button" class="composer-send stop" aria-label="Stop" title="Stop the current turn" onClick={props.onCancel}><Icon name="stop" size={12} /></button>
        </Show>
      </div>
    </div>
  );
}
