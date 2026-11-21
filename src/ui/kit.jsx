// Philium UI primitives. Screens compose these; no third-party UI kit.
// Rule: never render untrusted text as HTML. Solid's text binding is safe.
import { For, Show, ErrorBoundary, createSignal, createEffect, on, onCleanup, onMount, splitProps, createMemo } from "solid-js";
import { splitLatest } from "../lib/model-list.js";
import { Portal } from "solid-js/web";

/** Readable text for any thrown value: strips "Error: " and Rust/Tauri noise. */
export function cleanError(e) {
  let m = e instanceof Error ? e.message : typeof e === "string" ? e : e && typeof e === "object" ? (e.message ?? e.error ?? safeJson(e)) : String(e ?? "Something went wrong");
  m = String(m).replace(/^(Uncaught )?(\w*Error|error)(\([^)]*\))?:\s*/i, "").replace(/^Os \{[^}]*message: "([^"]+)"[^}]*\}/, "$1").replace(/\s+at\s+\S+:\d+:\d+.*$/s, "").trim();
  if (!m || m === "[object Object]") m = "Something went wrong";
  return m.length > 300 ? m.slice(0, 300) + "…" : m;
}
function safeJson(o) { try { return JSON.stringify(o); } catch { return "Something went wrong"; } }

export function Button(props) {
  const [p, rest] = splitProps(props, ["variant", "size", "class", "children", "busy", "onClick"]);
  const [auto, setAuto] = createSignal(false);
  const isBusy = () => Boolean(p.busy || auto());
  const click = (e) => {
    if (auto()) return;
    let r;
    try { r = p.onClick?.(e); } catch (x) { toast(cleanError(x), "bad"); return; }
    if (r && typeof r.then === "function") {
      setAuto(true);
      r.catch((x) => toast(cleanError(x), "bad")).finally(() => setAuto(false));
    }
  };
  return (
    <button
      {...rest}
      onClick={p.onClick ? click : undefined}
      aria-busy={isBusy() || undefined}
      disabled={rest.disabled || isBusy()}
      class={`btn ${p.variant ?? ""} ${p.size ?? ""} ${p.class ?? ""}`}
    >
      <Show when={isBusy()} fallback={p.children}>
        <span class="btn-label-busy" style={{ visibility: "hidden", display: "inline-flex", "align-items": "center", gap: "6px" }}>{p.children}</span>
        <span class="btn-busy-overlay" aria-hidden="true" style={{ position: "absolute", inset: 0, display: "inline-flex", "align-items": "center", "justify-content": "center" }}>…</span>
      </Show>
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
export function dismissToast(id) {
  setToasts((t) => t.filter((x) => x.id !== id));
}
export function toast(text, tone = "") {
  const id = ++toastId;
  setToasts((t) => [...t, { id, text: cleanError(text), tone }]);
  setTimeout(() => dismissToast(id), tone === "bad" || tone === "danger" ? 7000 : 3500);
}
export function ToastHost() {
  return (
    <Portal>
      <div class="toasts">
        <For each={toasts()}>
          {(t) => (
            <div class={`toast ${t.tone}`}>
              <span class="toast-text">{t.text}</span>
              <button
                type="button"
                class="toast-close"
                aria-label="Close notification"
                onClick={() => dismissToast(t.id)}
              >
                ×
              </button>
            </div>
          )}
        </For>
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
    if (el && el.clientHeight) setHeight(el.clientHeight);
    const ro = new ResizeObserver(() => el && setHeight(el.clientHeight));
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  });
  const range = createMemo(() => {
    const h = props.rowHeight;
    const len = props.items?.length ?? 0;
    const start = Math.max(0, Math.floor(scroll() / h) - 5);
    const end = Math.min(len, Math.ceil((scroll() + height()) / h) + 5);
    return { start, end };
  });
  const visible = createMemo(() => (props.items ?? []).slice(range().start, range().end));
  return (
    <div
      class={`vlist ${props.class ?? ""}`}
      ref={(node) => {
        el = node;
        if (typeof props.ref === "function") props.ref(node);
      }}
      onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
    >
      <div class="vlist-inner" style={{ height: `${(props.items?.length ?? 0) * props.rowHeight}px` }}>
        <For each={visible()}>
          {(item, i) => {
            const idx = () => range().start + i();
            return (
              <div style={{ position: "absolute", top: `${idx() * props.rowHeight}px`, left: 0, right: 0, height: `${props.rowHeight}px` }}>
                {props.children(item, idx())}
              </div>
            );
          }}
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

/** Per-screen error boundary: one broken screen never blanks the app. */
export function Guard(props) {
  return (
    <ErrorBoundary fallback={(err, reset) => (
      <div class="page-body flush">
        <Empty title={props.title ?? "This screen hit a problem"}
          action={<Button onClick={reset}>Retry</Button>}>
          {cleanError(err)}
        </Empty>
      </div>
    )}>
      {props.children}
    </ErrorBoundary>
  );
}

/** Friendly state for a deleted or unknown id in the route params. */
export function NotFound(props) {
  return <Empty title={props.title ?? "Not found"} action={props.action}>{props.children ?? "It may have been removed."}</Empty>;
}

/** Compact overflow menu. items: [{label, onClick, danger?, disabled?}]. Rendered in a portal so lists never clip it. */
export function Menu(props) {
  const [pos, setPos] = createSignal(null);
  let btn;
  const close = () => setPos(null);
  const open = (e) => {
    e.stopPropagation();
    if (pos()) return close();
    const r = btn.getBoundingClientRect();
    setPos({ top: Math.min(r.bottom + 4, window.innerHeight - 40 - (props.items?.length ?? 0) * 30), right: Math.max(8, window.innerWidth - r.right) });
  };
  // The opening click is delegated to the document and reaches this listener too: ignore it.
  const onDoc = (e) => { if (e?.type !== "click" || !btn?.contains(e.target)) close(); };
  createEffect(() => {
    if (!pos()) return;
    document.addEventListener("click", onDoc);
    window.addEventListener("resize", onDoc);
    onCleanup(() => {
      document.removeEventListener("click", onDoc);
      window.removeEventListener("resize", onDoc);
    });
  });
  return (
    <>
      <button ref={btn} type="button" class="btn ghost sm menu-btn" title={props.title ?? "More actions"} aria-haspopup="menu" aria-expanded={!!pos()} onClick={open}>
        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><circle cx="3" cy="8" r="1.4" /><circle cx="8" cy="8" r="1.4" /><circle cx="13" cy="8" r="1.4" /></svg>
      </button>
      <Show when={pos()}>
        <Portal>
          <div class="menu" role="menu" style={{ top: `${pos().top}px`, right: `${pos().right}px` }} onClick={(e) => e.stopPropagation()}>
            <For each={props.items}>{(it) => (
              <button type="button" role="menuitem" class={`menu-item ${it.danger ? "danger" : ""}`} disabled={it.disabled}
                onClick={() => { close(); try { const r = it.onClick?.(); r?.catch?.((x) => toast(cleanError(x), "bad")); } catch (x) { toast(cleanError(x), "bad"); } }}>{it.label}</button>
            )}</For>
          </div>
        </Portal>
      </Show>
    </>
  );
}

/**
 * Model picker: a provider switch, the latest model of each family first, older ones one click
 * away, and a search across every provider. Opens from its button, or when `openRequest` changes
 * (typing `/model`).
 * Respects session trust boundaries:
 * - Standard session (`strict === false` and `!allowUntrusted`): only accepted trusted providers.
 * - Strict session (`strict === true` and `lockedProvider`): only models on that locked provider.
 * - Spawn / New Chat (`allowUntrusted === true`): all enabled providers, clearly badged.
 */
export function ModelPicker(props) {
  const [open, setOpen] = createSignal(false);
  const [q, setQ] = createSignal("");
  const [prov, setProv] = createSignal("");
  const [older, setOlder] = createSignal(false);
  const [pos, setPos] = createSignal({ top: 60, left: 60, width: 360 });
  let btn, searchEl, requested = false;

  const provById = createMemo(() =>
    Object.fromEntries((props.providers ?? []).map((p) => [p.id, p]))
  );
  const currentProvId = () => (props.value || "").split("/")[0] || "";

  const normalized = createMemo(() => {
    const raw = props.models ?? [];
    const pmap = provById();
    return raw
      .filter((m) => { const p = pmap[m.provider_id || (m.ref || m.value || "").split("/")[0]]; return p && ![false,0].includes(m.enabled) && ![false,0].includes(p.enabled); })
      .map((m) => {
        const pid = m.provider_id || (m.ref || m.value || "").split("/")[0] || "provider";
        const p = pmap[pid] || {};
        const ref = m.ref || m.value || `${pid}/${m.model_id || m.id}`;
        const modelId = m.model_id || ref.split("/").slice(1).join("/") || ref;
        const display = m.display || m.label || modelId;
        const provName = m.provider_name || p.name || pid;
        const trusted = p.trusted !== false;
        const kind = p.kind || "";
        return {
          ref,
          pid,
          provName,
          modelId,
          display,
          trusted,
          kind,
          priceIn: m.price_in,
          priceOut: m.price_out,
        };
      });
  });

  const accepted = createMemo(() => {
    const locked = props.lockedProvider || (props.strict ? currentProvId() : "");
    return normalized().filter((m) => {
      if (locked) return m.pid === locked;
      if (!props.allowUntrusted && !m.trusted) return false;
      return true;
    });
  });

  const hiddenUntrustedCount = createMemo(() => {
    if (props.allowUntrusted || props.strict) return 0;
    return normalized().filter((m) => !m.trusted).length;
  });

  // One entry per provider that has a model this session may use, in the order they arrive.
  const provList = createMemo(() => {
    const map = new Map();
    for (const m of accepted()) {
      if (!map.has(m.pid)) map.set(m.pid, { pid: m.pid, name: m.provName, trusted: m.trusted, kind: m.kind, items: [] });
      map.get(m.pid).items.push(m);
    }
    // In the order the providers were added, as the Providers screen lists them.
    const at = (pid) => { const i = (props.providers ?? []).findIndex((p) => p.id === pid); return i < 0 ? 1e6 : i; };
    return [...map.values()].sort((x, y) => at(x.pid) - at(y.pid));
  });
  const trustLabel = (g) => (g.trusted ? (["chatgpt_oauth", "claude_oauth"].includes(g.kind) ? "Subscription · Trusted" : "API · Trusted") : "Untrusted · Strict");
  const searching = () => Boolean(q().trim());

  // What the list shows: every match grouped by provider while searching, otherwise the chosen
  // provider's latest models with the older ones behind one button.
  const sections = createMemo(() => {
    const needle = q().trim().toLowerCase();
    if (needle) {
      return provList()
        .map((g) => ({ ...g, more: 0, items: g.items.filter((m) =>
          m.display.toLowerCase().includes(needle) || m.modelId.toLowerCase().includes(needle) ||
          m.provName.toLowerCase().includes(needle) || m.ref.toLowerCase().includes(needle)) }))
        .filter((g) => g.items.length);
    }
    const g = provList().find((x) => x.pid === prov()) || provList()[0];
    if (!g) return [];
    const { latest, older: rest } = splitLatest(g.items, { keep: props.value ? [props.value] : [] });
    return [{ ...g, items: older() ? [...latest, ...rest] : latest, more: rest.length }];
  });
  const shown = () => sections().flatMap((g) => g.items);

  const activeItem = createMemo(() =>
    normalized().find((m) => m.ref === props.value) || (props.value ? {
      ref: props.value,
      pid: currentProvId(),
      provName: currentProvId(),
      display: props.value.split("/").slice(1).join("/") || props.value,
      trusted: provById()[currentProvId()]?.trusted !== false,
    } : null)
  );

  const show = () => {
    if (props.disabled || !btn) return;
    const r = btn.getBoundingClientRect();
    const w = Math.min(window.innerWidth - 24, Math.max(360, Math.min(440, r.width)));
    const left = Math.max(12, Math.min(r.left, window.innerWidth - w - 12));
    const top = r.bottom + 6 + 400 > window.innerHeight ? Math.max(12, r.top - 400) : r.bottom + 6;
    setPos({ top, left, width: w });
    setQ("");
    setOlder(false);
    setProv(provList().some((g) => g.pid === currentProvId()) ? currentProvId() : (provList()[0]?.pid || ""));
    setOpen(true);
    queueMicrotask(() => searchEl?.focus());
  };
  const toggle = (e) => {
    e.stopPropagation();
    requested = false;
    if (open()) setOpen(false);
    else show();
  };
  const close = (restoreFocus = false) => {
    if (!open()) return;
    setOpen(false);
    if (requested) { requested = false; props.onDismiss?.(); }
    else if (restoreFocus === true) btn?.focus();
  };
  const pick = (ref) => { props.onValue?.(ref); close(true); };
  // `/model` asks for the list without a click on the button.
  // Opened after the current event has finished: when the request comes from a click (the Send
  // button), that same click would otherwise reach the listener below and close the list at once.
  createEffect(on(() => props.openRequest, (n) => { if (n) setTimeout(() => { requested = true; show(); }, 0); }, { defer: true }));

  // Clicks are delegated to the document, so the click that opens the list, and every click
  // inside it, also reaches this listener. Only a click elsewhere closes the list.
  // A click inside can redraw the row it landed on ("Show older models"), which detaches the
  // clicked element before this listener runs. The event's own path still records where it was.
  const outside = (e) => {
    const path = e.composedPath?.() ?? [];
    const inside = (n) => n === btn || n?.classList?.contains?.("model-picker-pop");
    if (!path.some(inside) && !btn?.contains(e.target) && !e.target?.closest?.(".model-picker-pop")) close();
  };
  const onResize = () => close();
  createEffect(() => {
    if (!open()) return;
    document.addEventListener("click", outside);
    window.addEventListener("resize", onResize);
    onCleanup(() => {
      document.removeEventListener("click", outside);
      window.removeEventListener("resize", onResize);
    });
  });

  // Arrow keys walk the models; from the search box the first Down lands on the first model.
  const onKeys = (e) => {
    const pop = e.currentTarget;
    if (e.key === "Escape") { e.preventDefault(); close(true); return; }
    if (e.key === "Enter" && e.target === searchEl && shown().length && (searching() || shown().length === 1)) {
      e.preventDefault(); pick(shown()[0].ref); return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const items = Array.from(pop.querySelectorAll("button.model-picker-item"));
    if (!items.length) return;
    e.preventDefault();
    const i = items.indexOf(document.activeElement);
    if (e.key === "ArrowUp" && i <= 0) { searchEl?.focus(); return; }
    items[e.key === "ArrowDown" ? Math.min(items.length - 1, i + 1) : i - 1]?.focus();
  };

  return (
    <>
      <button
        ref={btn}
        type="button"
        disabled={props.disabled}
        class={`model-picker-btn ${props.size ?? ""}`}
        aria-haspopup="dialog"
        aria-expanded={open()}
        aria-label="Choose model"
        onClick={toggle}
        title={props.title ?? (activeItem() ? `Model: ${activeItem().ref} (click, or type /model, to switch)` : "Choose a model")}
      >
        <Show when={activeItem()} fallback={<span class="dim">{props.placeholder ?? "Choose model…"}</span>}>
          {(it) => (
            <>
              <span class={`model-picker-prov ${it().trusted ? "trusted" : "strict"}`}>{it().provName}</span>
              <span class="model-picker-name ellipsis">{it().display}</span>
            </>
          )}
        </Show>
        <span class="model-picker-caret" aria-hidden="true">▾</span>
      </button>
      <Show when={open()}>
        <Portal>
          <div
            class="model-picker-pop"
            role="dialog"
            aria-label="Choose a model"
            style={{ top: `${pos().top}px`, left: `${pos().left}px`, width: `${pos().width}px` }}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={onKeys}
          >
            <div class="model-picker-head">
              <input
                ref={searchEl}
                type="text"
                class="input sm model-picker-search"
                aria-label="Search models or providers"
                placeholder="Search every provider…"
                value={q()}
                onInput={(e) => setQ(e.currentTarget.value)}
                spellcheck={false}
              />
            </div>
            <Show when={!searching() && provList().length > 1}>
              <div class="model-picker-providers" role="tablist" aria-label="Provider">
                <For each={provList()}>
                  {(g) => (
                    <button type="button" role="tab" aria-selected={g.pid === prov()}
                      class={`model-picker-prov-chip ${g.pid === prov() ? "on" : ""} ${g.trusted ? "" : "untrusted"}`}
                      title={`${g.items.length} models · ${trustLabel(g)}`}
                      onClick={() => { setProv(g.pid); setOlder(false); }}>
                      {g.name}
                    </button>
                  )}
                </For>
              </div>
            </Show>
            <div class="model-picker-list">
              <Show when={sections().length} fallback={<div class="xs dim" style={{ padding: "14px" }}>No matching models in accepted providers.</div>}>
                <For each={sections()}>
                  {(g) => (
                    <div class="model-picker-group">
                      <div class="model-picker-group-label">
                        <span>{searching() ? g.name : (older() ? `All ${g.name} models` : `Latest from ${g.name}`)}</span>
                        <span class={`model-picker-trust ${g.trusted ? "trusted" : "untrusted"}`}>{trustLabel(g)}</span>
                      </div>
                      <div role="group" aria-label={`${g.name} models`}>
                        <For each={g.items}>
                          {(m) => {
                            const active = () => m.ref === props.value;
                            return (
                              <button type="button" aria-pressed={active()} class={`model-picker-item ${active() ? "on" : ""}`} onClick={() => pick(m.ref)}>
                                <div class="grow" style={{ "min-width": 0 }}>
                                  <div class="row gap-4">
                                    <span class="model-item-title ellipsis">{m.display}</span>
                                    <Show when={m.display !== m.modelId}>
                                      <span class="mono xs faint ellipsis">{m.modelId}</span>
                                    </Show>
                                  </div>
                                </div>
                                <Show when={m.priceIn != null && m.priceOut != null}>
                                  <span class="mono xs faint">${m.priceIn}/${m.priceOut}</span>
                                </Show>
                                <Show when={active()}>
                                  <span class="model-item-check">✓</span>
                                </Show>
                              </button>
                            );
                          }}
                        </For>
                      </div>
                      <Show when={!searching() && g.more > 0}>
                        <button type="button" class="model-picker-more" aria-expanded={older()} onClick={() => setOlder(!older())}>
                          {older() ? "Show only the latest" : `Show ${g.more} older model${g.more === 1 ? "" : "s"}`}
                        </button>
                      </Show>
                    </div>
                  )}
                </For>
              </Show>
            </div>
            <div class="model-picker-foot xs faint">
              <Show when={props.strict}>
                <span>Strict session: locked to accepted provider <b>{currentProvId()}</b>.</span>
              </Show>
              <Show when={!props.strict && hiddenUntrustedCount() > 0}>
                <span>{hiddenUntrustedCount()} untrusted-provider model(s) hidden (spawn a Strict agent to use them).</span>
              </Show>
              <Show when={!props.strict && hiddenUntrustedCount() === 0}>
                <span>↑↓ to move, Enter to choose, Esc to close.</span>
              </Show>
            </div>
          </div>
        </Portal>
      </Show>
    </>
  );
}
