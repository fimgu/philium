// Conversation view built from HAL session events. Used by Agents and Chat.
// Events are folded into items: user / assistant (streamed) / tool / file /
// permission / error. Text is always rendered as text, never HTML.
import { For, Index, Show, Switch, Match, createSignal, createMemo, createEffect, on } from "solid-js";
import { Badge, Button, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { draftFor, newDraft } from "../lib/drafts.js";

/** Fold a list of HAL events into display items. Pure, incremental-safe. */
export function fold(events) {
  const items = [];
  const tools = new Map();
  let streaming = null;
  let turnText = false;
  for (const e of events) {
    switch (e.type) {
      case "message.created":
        if (e.role === "user") { items.push({ kind: "user", text: e.content, seq: e.seq }); turnText = false; }
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
        if (streaming) { streaming.live = false; streaming = null; }
        else if (!turnText && e.text) items.push({ kind: "assistant", text: e.text, seq: e.seq });
        turnText = false;
        break;
      case "tool.started": {
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
      case "turn.failed": items.push({ kind: "error", text: e.error, seq: e.seq }); streaming = null; break;
      case "turn.cancelled": items.push({ kind: "note", text: "Stopped.", seq: e.seq }); streaming = null; break;
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

function Tool(props) {
  const [open, setOpen] = createSignal(false);
  const t = () => props.item;
  return (
    <div class="msg tool conv-tool">
      <div class="row" style={{ cursor: "pointer" }} onClick={() => setOpen(!open())}>
        <span class={`dot ${t().running ? "busy" : t().ok === false ? "bad" : "live"}`} />
        <b>{short(t().tool)}</b>
        <span class="ellipsis dim grow">{argSummary(t().tool, t().args)}</span>
        <Show when={t().ms != null}><span class="faint xs">{(t().ms / 1000).toFixed(1)}s</span></Show>
      </div>
      <Show when={open()}>
        <pre class="conv-pre">{JSON.stringify(t().args, null, 2)}</pre>
        <Show when={t().out || t().result}><pre class="conv-pre">{(t().result || t().out || "").slice(0, 20000)}</pre></Show>
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
    <div class="conv" style={props.hidden ? { display: "none" } : undefined} ref={el} onScroll={(e) => {
      const d = e.currentTarget;
      setStick(d.scrollHeight - d.scrollTop - d.clientHeight < 60);
    }}>
      <div class="timeline">
        <Show when={items().length === 0}>
          <div class="conv-empty">{props.emptyText ?? "No messages yet."}</div>
        </Show>
        <Index each={items()}>
          {(it) => (
            <Switch>
              <Match when={it().kind === "user"}><div class="msg user">{it().text}</div></Match>
              <Match when={it().kind === "assistant"}>
                <div class={`msg ${it().live ? "live" : ""}`}>{it().text}<Show when={it().live}><span class="caret" /></Show></div>
              </Match>
              <Match when={it().kind === "tool"}><Tool item={it()} /></Match>
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
              <Match when={it().kind === "error"}><div class="msg error">{it().text}</div></Match>
              <Match when={it().kind === "note"}><div class="faint xs" style={{ "align-self": "center" }}>{it().text}</div></Match>
            </Switch>
          )}
        </Index>
      </div>
    </div>
  );
}

/** Composer: Enter sends, Shift+Enter newline. Images can be pasted or dropped;
 * onSend(text, images) gets HAL image parts: [{ data: base64, media_type, name }]. */
const readImage = (file) => new Promise((resolve, reject) => {
  const r = new FileReader();
  r.onload = () => resolve({ data: String(r.result).split(",")[1], media_type: file.type, name: file.name || "image", preview: r.result });
  r.onerror = reject;
  r.readAsDataURL(file);
});

/** Speed presets mapped to HAL reasoning effort. `normal` leaves it to the model. */
export const SPEEDS = [
  { value: "fast", label: "Fast", effort: "low", hint: "Low reasoning effort: quick answers" },
  { value: "normal", label: "Normal", effort: "default", hint: "The model's default effort" },
  { value: "deep", label: "Deep", effort: "high", hint: "High reasoning effort: slower, more thorough" },
];
export const speedEffort = (v) => SPEEDS.find((s) => s.value === v)?.effort ?? "default";
export const effortSpeed = (e) => (e === "low" || e === "minimal" || e === "none" ? "fast" : e === "high" || e === "xhigh" || e === "max" ? "deep" : "normal");

export function Composer(props) {
  const localDraft = newDraft();
  const record = () => props.draftKey ? draftFor(props.draftKey) : localDraft;
  const text = () => record().text(), setText = (value) => record().setText(value);
  const images = () => record().images(), setImages = (value) => record().setImages(value);
  const sending = () => record().sending();
  const [over, setOver] = createSignal(false);
  const add = async (files) => {
    const target = record();
    const imgs = [...files].filter((f) => f.type.startsWith("image/"));
    if (imgs.length) {
      const loaded = await Promise.all(imgs.map(readImage));
      target.setImages((current) => [...current, ...loaded].slice(0, 8));
    }
  };
  const send = async () => {
    const target = record();
    const draft = target.text(), attached = target.images();
    const t = draft.trim();
    if ((!t && !attached.length) || props.disabled || props.busy || sending()) return;
    target.setSending(true);
    try {
      const accepted = await props.onSend(t, attached.map(({ data, media_type, name }) => ({ data, media_type, name })));
      if (accepted !== false) {
        if (target.text() === draft) target.setText("");
        target.setImages((current) => current.filter((im) => !attached.includes(im)));
      }
    } catch (e) { toast(String(e), "danger"); }
    finally { target.setSending(false); }
  };
  return (
    <div class="composer" style={{ "flex-wrap": "wrap", outline: over() ? "1px dashed var(--accent, #7aa2ff)" : "none" }}
      onDragOver={(e) => { e.preventDefault(); setOver(true); }} onDragLeave={() => setOver(false)}
      onDrop={(e) => { e.preventDefault(); setOver(false); add(e.dataTransfer.files); }}>
      <Show when={images().length}>
        <div class="row" style={{ width: "100%", gap: "8px" }}>
          <For each={images()}>
            {(im, i) => (
              <div style={{ position: "relative" }}>
                <img src={im.preview} alt={im.name} style={{ height: "56px", "border-radius": "6px" }} />
                <button class="btn ghost sm" title="Remove image" style={{ position: "absolute", top: "-6px", right: "-6px" }}
                  onClick={() => setImages(images().filter((_, j) => j !== i()))}>x</button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <textarea
        class="textarea grow"
        placeholder={props.placeholder ?? "Message"}
        value={text()}
        disabled={props.disabled}
        onInput={(e) => setText(e.currentTarget.value)}
        onPaste={(e) => { const f = [...e.clipboardData.files]; if (f.some((x) => x.type.startsWith("image/"))) { e.preventDefault(); add(f); } }}
        onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }}
      />
      <Show when={props.onSpeed}>
        <div class="speed" title="How hard the model thinks before answering. Fast is quickest and cheapest.">
          <For each={SPEEDS}>{(s) => (
            <button type="button" classList={{ on: (props.speed || "normal") === s.value }} onClick={() => props.onSpeed(s.value)} title={s.hint}>{s.label}</button>
          )}</For>
        </div>
      </Show>
      <Show when={props.busy} fallback={<Button variant="primary" onClick={send} disabled={props.disabled || sending()}>Send</Button>}>
        <Button onClick={props.onCancel}><Icon name="stop" size={12} /> Stop</Button>
      </Show>
      <Show when={props.busy}><div class="xs dim" role="status" style={{ width: "100%" }}>Working or waiting for approval. Your draft stays here. Use Stop to interrupt.</div></Show>
    </div>
  );
}
