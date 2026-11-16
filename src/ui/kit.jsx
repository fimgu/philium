// Philium UI primitives. Screens compose these; no third-party UI kit.
// Rule: never render untrusted text as HTML. Solid's text binding is safe.
import { For, Show, createSignal, onCleanup, onMount, splitProps, createMemo } from "solid-js";
import { Portal } from "solid-js/web";

export function Button(props) {
  const [p, rest] = splitProps(props, ["variant", "size", "class", "children", "busy"]);
  return (
    <button
      {...rest}
      disabled={rest.disabled || p.busy}
      class={`btn ${p.variant ?? ""} ${p.size ?? ""} ${p.class ?? ""}`}
    >
      {p.busy ? "…" : p.children}
    </button>
  );
}

export function Field(props) {
  return (
    <div class="field">
      <Show when={props.label}><label>{props.label}</label></Show>
      {props.children}
      <Show when={props.error}><span class="err">{props.error}</span></Show>
      <Show when={props.hint && !props.error}><span class="hint">{props.hint}</span></Show>
    </div>
  );
}

export function Input(props) {
  const [p, rest] = splitProps(props, ["class", "onValue", "mono"]);
  return (
    <input
      {...rest}
      class={`input ${p.mono ? "mono" : ""} ${p.class ?? ""}`}
      onInput={(e) => p.onValue?.(e.currentTarget.value)}
      spellcheck={false}
      autocomplete="off"
    />
  );
}

export function TextArea(props) {
  const [p, rest] = splitProps(props, ["class", "onValue"]);
  return <textarea {...rest} class={`textarea ${p.class ?? ""}`} onInput={(e) => p.onValue?.(e.currentTarget.value)} />;
}

export function Select(props) {
  // options: [{value, label}] or strings
  return (
    <select class="select" value={props.value ?? ""} onChange={(e) => props.onValue?.(e.currentTarget.value)} disabled={props.disabled}>
      <Show when={props.placeholder}><option value="" disabled>{props.placeholder}</option></Show>
      <For each={props.options ?? []}>
        {(o) => {
          const v = typeof o === "string" ? o : o.value;
          const l = typeof o === "string" ? o : o.label;
          return <option value={v} selected={v === props.value}>{l}</option>;
        }}
      </For>
    </select>
  );
}

export function Toggle(props) {
  return (
    <label class={`toggle ${props.on ? "on" : ""} ${props.tone ?? ""}`} onClick={(e) => { e.preventDefault(); if (!props.disabled) props.onChange?.(!props.on); }}>
      <span class="track" />
      <span>{props.label}</span>
    </label>
  );
}

export function Segmented(props) {
  return (
    <div class="segmented">
      <For each={props.options}>
        {(o) => {
          const v = typeof o === "string" ? o : o.value;
          return (
            <button class={props.value === v ? "on" : ""} onClick={() => props.onChange?.(v)}>
              {typeof o === "string" ? o : o.label}
            </button>
          );
        }}
      </For>
    </div>
  );
}

export function Badge(props) {
  return <span class={`badge ${props.tone ?? ""}`} title={props.title}>{props.children}</span>;
}

export function Dot(props) {
  return <span class={`dot ${props.state ?? ""}`} title={props.title} />;
}

export function Card(props) {
  const [p, rest] = splitProps(props, ["class", "children", "selected"]);
  return <div {...rest} class={`card ${rest.onClick ? "click" : ""} ${p.selected ? "selected" : ""} ${p.class ?? ""}`}>{p.children}</div>;
}

export function Stat(props) {
  return (
    <div class="stat">
      <span class="label">{props.label}</span>
      <span class="value">{props.value}</span>
      <Show when={props.sub}><span class="delta">{props.sub}</span></Show>
    </div>
  );
}

export function Empty(props) {
  return (
    <div class="empty">
      <Show when={props.icon}><div class="empty-icon">{props.icon}</div></Show>
      <h3>{props.title}</h3>
      <Show when={props.children ?? props.body}><p>{props.children ?? props.body}</p></Show>
      <Show when={props.action}>{props.action}</Show>
    </div>
  );
}

export function Shield(props) {
  return <div class={`shield ${props.tone ?? ""}`}>{props.children}</div>;
}

export function Page(props) {
  return (
    <>
      <header class="page-head">
        <h1>{props.title}</h1>
        <Show when={props.sub}><span class="sub">{props.sub}</span></Show>
        <div class="actions">{props.actions}</div>
      </header>
      <div class={`page-body ${props.flush ? "flush" : ""}`}>{props.children}</div>
    </>
  );
}

export function Modal(props) {
  const onKey = (e) => e.key === "Escape" && props.onClose?.();
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));
  return (
    <Portal>
      <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && props.onClose?.()}>
        <div class={`modal ${props.wide ? "wide" : ""}`} role="dialog">
          <div class="modal-head">
            <h2>{props.title}</h2>
            <Show when={props.badge}>{props.badge}</Show>
            <div style={{ "margin-left": "auto" }}>
              <Button variant="ghost" class="icon" onClick={() => props.onClose?.()} aria-label="Close">✕</Button>
            </div>
          </div>
          <div class="modal-body">{props.children}</div>
          <Show when={props.footer}><div class="modal-foot">{props.footer}</div></Show>
        </div>
      </div>
    </Portal>
  );
}

/** Ask a yes/no question. Returns a promise of boolean. */
const [confirmState, setConfirmState] = createSignal(null);
export function confirm(title, body, { danger = false, ok = "Confirm" } = {}) {
  return new Promise((resolve) => setConfirmState({ title, body, danger, ok, resolve }));
}
export function ConfirmHost() {
  const done = (v) => { confirmState()?.resolve(v); setConfirmState(null); };
  return (
    <Show when={confirmState()}>
      {(c) => (
        <Modal title={c().title} onClose={() => done(false)}
          footer={<><Button onClick={() => done(false)}>Cancel</Button>
            <Button variant={c().danger ? "danger" : "primary"} onClick={() => done(true)}>{c().ok}</Button></>}>
          <div class="dim" style={{ "white-space": "pre-wrap" }}>{c().body}</div>
        </Modal>
      )}
    </Show>
  );
}

const [toasts, setToasts] = createSignal([]);
let toastId = 0;
export function toast(text, tone = "") {
  const id = ++toastId;
  setToasts((t) => [...t, { id, text: String(text), tone }]);
  setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), tone === "bad" ? 7000 : 3500);
}
export function ToastHost() {
  return (
    <Portal>
      <div class="toasts">
        <For each={toasts()}>{(t) => <div class={`toast ${t.tone}`}>{t.text}</div>}</For>
      </div>
    </Portal>
  );
}

/**
 * Virtualized list: renders only visible rows. `rowHeight` fixed px.
 * Use for anything that can grow past a few hundred rows.
 */
export function VirtualList(props) {
  let el;
  const [scroll, setScroll] = createSignal(0);
  const [height, setHeight] = createSignal(600);
  onMount(() => {
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  });
  const range = createMemo(() => {
    const h = props.rowHeight;
    const start = Math.max(0, Math.floor(scroll() / h) - 5);
    const end = Math.min(props.items.length, Math.ceil((scroll() + height()) / h) + 5);
    return { start, end };
  });
  const visible = createMemo(() => props.items.slice(range().start, range().end).map((item, i) => ({ item, index: range().start + i })));
  return (
    <div class="vlist" ref={el} onScroll={(e) => setScroll(e.currentTarget.scrollTop)}>
      <div class="vlist-inner" style={{ height: `${props.items.length * props.rowHeight}px` }}>
        <For each={visible()}>
          {(v) => (
            <div style={{ position: "absolute", top: `${v.index * props.rowHeight}px`, left: 0, right: 0, height: `${props.rowHeight}px` }}>
              {props.children(v.item, v.index)}
            </div>
          )}
        </For>
      </div>
    </div>
  );
}

export function Meter(props) {
  return <div class={`meter ${props.tone ?? ""}`}><div style={{ width: `${Math.min(100, Math.max(0, props.value * 100))}%` }} /></div>;
}

export function Bars(props) {
  // props.values: [{label, value}]
  const max = createMemo(() => Math.max(1, ...props.values.map((v) => v.value)));
  return (
    <div class="bars">
      <For each={props.values}>{(v) => <div title={`${v.label}: ${props.format ? props.format(v.value) : v.value}`} style={{ height: `${(v.value / max()) * 100}%` }} />}</For>
    </div>
  );
}
