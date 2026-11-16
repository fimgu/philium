import { createSignal, createEffect, createMemo, onMount, onCleanup, For, Show } from "solid-js";
import { Page, Button, Empty, Badge, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent } from "../lib/rpc.js";
import { route } from "../lib/nav.js";
import "../styles/browser.css";

// Small local glyphs (same 20px grid and stroke style as ui/icons.jsx).
const G = {
  back: "M12.5 4.5 7 10l5.5 5.5",
  fwd: "M7.5 4.5 13 10l-5.5 5.5",
  x: "M5.5 5.5l9 9m0-9-9 9",
  warn: "M10 3 18 17H2zM10 8v4m0 2.5v.5",
};
const Glyph = (p) => (
  <svg width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 20 20" fill="none" stroke="currentColor"
    stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={G[p.name]} /></svg>
);

// Anything that can paint above the view area. The native webview cannot be
// covered by DOM, so while one of these intersects the view we hide it.
const OVERLAYS = ".overlay, .palette, .board-drawer-backdrop, [role=dialog], [role=menu], [role=listbox], .menu, .popover, .toast";

const blank = (u) => !u || u === "about:blank";
const host = (u) => { try { return new URL(u).host; } catch { return u; } };

export default function Screen() {
  const [tabs, setTabs] = createSignal([]);
  const [active, setActive] = createSignal(null);
  const [addr, setAddr] = createSignal("");
  const [editing, setEditing] = createSignal(false);
  const [err, setErr] = createSignal("");
  const [covered, setCovered] = createSignal(false);
  const [notice, setNotice] = createSignal(null);
  let view, input;
  let alive = true;
  let shown = null; // tab id currently shown natively
  let last = "";    // last bounds sent

  const cur = createMemo(() => tabs().find((t) => t.id === active()));
  const upsert = (t) => setTabs((l) => (l.some((x) => x.id === t.id) ? l.map((x) => (x.id === t.id ? { ...x, ...t } : x)) : [...l, t]));
  const drop = (id) => {
    const rest = tabs().filter((t) => t.id !== id);
    setTabs(rest);
    if (active() === id) setActive(rest.length ? rest[rest.length - 1].id : null);
    if (shown === id) shown = null;
  };
  // Stale tab ids: the backend already forgot them, so just drop locally.
  const call = (m, a) => rpc(`browser.${m}`, a).catch((e) => {
    const s = String(e);
    if (/is closed|no such tab/.test(s) && a?.tab) drop(a.tab); else setErr(s);
  });

  // The native view should be visible only when all of these hold.
  const wanted = () => {
    const t = cur();
    return alive && route() === "browser" && t && !blank(t.url) && !t.error && !covered() && !document.hidden ? t.id : null;
  };

  const hideAll = () => { shown = null; last = ""; rpc("browser.hide", {}).catch(() => {}); };

  const sync = async () => {
    const want = wanted();
    if (shown && shown !== want) { const id = shown; shown = null; rpc("browser.hide", { tab: id }).catch(() => {}); }
    if (!want || !view) return;
    const r = view.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return;
    const b = `${want}:${r.left}:${r.top}:${r.width}:${r.height}`;
    if (b !== last) { last = b; await call("set_bounds", { tab: want, x: r.left + 1, y: r.top + 1, w: r.width - 2, h: r.height - 2 }); }
    if (shown !== want && wanted() === want) { shown = want; await call("show", { tab: want }); }
  };

  // rAF loop: catches position-only moves (sidebar collapse, page scroll) that ResizeObserver misses.
  let raf = 0;
  const tick = () => {
    if (!alive) return;
    if (view) {
      const r = view.getBoundingClientRect();
      let hit = false;
      for (const el of document.querySelectorAll(OVERLAYS)) {
        if (view.contains(el)) continue;
        const o = el.getBoundingClientRect();
        if (o.width && o.height && o.left < r.right && o.right > r.left && o.top < r.bottom && o.bottom > r.top) { hit = true; break; }
      }
      if (hit !== covered()) setCovered(hit);
      const b = `${wanted()}:${r.left}:${r.top}:${r.width}:${r.height}`;
      if (b !== last || (wanted() || null) !== shown) sync();
    }
    raf = requestAnimationFrame(tick);
  };

  const select = (id) => { setActive(id); setErr(""); };

  const open = async (url = "about:blank") => {
    setErr("");
    try {
      const t = await rpc("browser.open", { url });
      upsert(t);
      select(t.id);
      if (blank(url)) queueMicrotask(() => input?.focus());
    } catch (e) { setErr(String(e)); }
  };

  const close = async (id) => { drop(id); await rpc("browser.close", { tab: id }).catch(() => {}); };

  useEvent("browser:tab", (t) => { if (alive && !t.owner) upsert(t); });
  useEvent("browser:closed", ({ id }) => drop(id));
  useEvent("browser:opened", ({ id, opener }) => { if (opener === active() && tabs().some((t) => t.id === id)) select(id); });
  useEvent("browser:download", (d) => {
    const msg = d.state === "started" ? `Downloading ${d.name || "file"} to Downloads`
      : d.state === "done" ? `Saved ${d.name || "file"} to Downloads` : `Download failed${d.name ? `: ${d.name}` : ""}`;
    setNotice({ text: msg, tone: d.state === "failed" ? "bad" : d.state === "done" ? "good" : "" });
    if (d.state !== "started") toast(msg, d.state === "failed" ? "bad" : "good");
    clearTimeout(noticeT); noticeT = setTimeout(() => setNotice(null), 6000);
  });
  let noticeT;

  createEffect(() => { const t = cur(); if (!editing()) setAddr(t && !blank(t.url) ? t.url : ""); });

  const submit = (e) => {
    e.preventDefault();
    const v = addr().trim();
    if (!v) return;
    setEditing(false);
    input?.blur();
    const t = cur();
    t ? call("navigate", { tab: t.id, url: v }) : open(v);
  };
  const act = (m) => cur() && call(m, { tab: cur().id });

  const onKey = (e) => {
    if (route() !== "browser" || !(e.metaKey || e.ctrlKey)) return;
    const k = e.key.toLowerCase();
    if (k === "t") { e.preventDefault(); open(); }
    else if (k === "w" && cur()) { e.preventDefault(); close(cur().id); }
    else if (k === "l") { e.preventDefault(); input?.focus(); input?.select(); }
    else if (k === "r" && cur()) { e.preventDefault(); act(cur().loading ? "stop" : "reload"); }
  };
  const onVis = () => sync();

  onMount(async () => {
    // After an app reload native tabs may still be showing: start from a clean slate.
    hideAll();
    try {
      const l = await rpc("browser.tabs");
      if (!alive) return;
      setTabs(l);
      if (l.length && !active()) setActive(l[l.length - 1].id);
    } catch (e) { setErr(String(e)); }
    window.addEventListener("keydown", onKey);
    document.addEventListener("visibilitychange", onVis);
    raf = requestAnimationFrame(tick);
  });
  // The native webview floats above the DOM: hide every tab when this screen goes away.
  onCleanup(() => {
    alive = false;
    cancelAnimationFrame(raf);
    clearTimeout(noticeT);
    window.removeEventListener("keydown", onKey);
    document.removeEventListener("visibilitychange", onVis);
    for (const t of tabs()) rpc("browser.hide", { tab: t.id }).catch(() => {});
    hideAll();
  });
  // Leaving the screen without unmount (or a route change before cleanup) also hides.
  createEffect(() => { if (route() !== "browser") hideAll(); });

  const loading = () => !!cur()?.loading && !blank(cur()?.url);

  return (
    <Page title="Browser" sub="Shared browser for you and your agents. Pages cannot reach Philium." flush
      actions={<Button onClick={() => open()} title="New tab (Cmd+T)"><Icon name="plus" /> New tab</Button>}>
      <div class="br">
        <div class="br-tabs" classList={{ none: !tabs().length }} role="tablist">
          <For each={tabs()}>{(t) => (
            <div class="br-tab" role="tab" aria-selected={t.id === active()} classList={{ on: t.id === active(), agent: !!t.agent }}
              title={t.url} onClick={() => select(t.id)}
              onMouseDown={(e) => { if (e.button === 1) e.preventDefault(); }}
              onAuxClick={(e) => { if (e.button === 1) { e.preventDefault(); close(t.id); } }}>
              <Show when={t.loading && !blank(t.url)} fallback={<span class="br-dot" classList={{ err: !!t.error }} />}>
                <span class="br-spin" />
              </Show>
              <span class="br-title">{t.title || (blank(t.url) ? "New tab" : host(t.url))}</span>
              <Show when={t.agent}><Badge>{t.agent}</Badge></Show>
              <button class="br-x" title="Close tab (Cmd+W)" aria-label="Close tab" onClick={(e) => { e.stopPropagation(); close(t.id); }}><Glyph name="x" size={12} /></button>
            </div>
          )}</For>
          <button class="br-new" title="New tab (Cmd+T)" aria-label="New tab" onClick={() => open()}><Icon name="plus" /></button>
        </div>
        <form class="br-bar" onSubmit={submit}>
          <Button type="button" variant="ghost" class="icon" disabled={!cur() || blank(cur()?.url)} onClick={() => act("back")} title="Back"><Glyph name="back" /></Button>
          <Button type="button" variant="ghost" class="icon" disabled={!cur() || blank(cur()?.url)} onClick={() => act("forward")} title="Forward"><Glyph name="fwd" /></Button>
          <Show when={loading()} fallback={
            <Button type="button" variant="ghost" class="icon" disabled={!cur() || blank(cur()?.url)} onClick={() => act("reload")} title="Reload (Cmd+R)"><Icon name="refresh" /></Button>}>
            <Button type="button" variant="ghost" class="icon" onClick={() => act("stop")} title="Stop loading"><Glyph name="x" /></Button>
          </Show>
          <div class="br-addr" classList={{ focus: editing() }}>
            <Show when={cur() && !blank(cur().url)}>
              <span class="br-lock" classList={{ insecure: cur().url.startsWith("http:") }} title={cur().url.startsWith("https:") ? "Secure connection" : "Not encrypted"}>
                <Icon name={cur().url.startsWith("https:") ? "lock" : "shield"} size={13} />
              </span>
            </Show>
            <input ref={input} value={addr()} spellcheck={false} autocomplete="off" placeholder="Search or enter address"
              onFocus={(e) => { setEditing(true); const t = cur(); if (t && !blank(t.url)) setAddr(t.url); queueMicrotask(() => e.currentTarget.select()); }}
              onBlur={() => setEditing(false)}
              onInput={(e) => setAddr(e.currentTarget.value)}
              onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); const t = cur(); setAddr(t && !blank(t.url) ? t.url : ""); e.currentTarget.blur(); } }} />
            <Show when={loading()}><div class="br-progress" /></Show>
          </div>
          <Button type="button" variant="ghost" class="icon" disabled={!cur() || blank(cur()?.url)} title="Open in system browser" onClick={() => call("external", { url: cur().url })}><Icon name="external" /></Button>
        </form>
        <Show when={err()}><div class="br-note bad"><Glyph name="warn" size={13} /> {err()}<button onClick={() => setErr("")} aria-label="Dismiss"><Glyph name="x" size={11} /></button></div></Show>
        <Show when={notice()}><div class={`br-note ${notice().tone}`}>{notice().text}</div></Show>
        <div class="br-view" ref={view} classList={{ live: !!wanted() }}>
          <Show when={!cur()}>
            <div class="br-empty"><Empty icon={<Icon name="globe" size={28} />} title="No tab open"
              action={<Button variant="primary" onClick={() => open()}>New tab</Button>}
              body="Browse alongside your agents. Agents can open tabs here too, and pages never get access to Philium." /></div>
          </Show>
          <Show when={cur() && blank(cur().url) && !cur().error}>
            <div class="br-empty"><Empty icon={<Icon name="globe" size={28} />} title="New tab"
              body="Type an address or a search above. Links that open new windows become tabs here, and downloads go to your Downloads folder." />
              <div class="br-keys"><span><kbd>Cmd</kbd><kbd>L</kbd> address</span><span><kbd>Cmd</kbd><kbd>T</kbd> new tab</span><span><kbd>Cmd</kbd><kbd>W</kbd> close</span><span><kbd>Cmd</kbd><kbd>R</kbd> reload</span></div>
            </div>
          </Show>
          <Show when={cur()?.error}>
            <div class="br-empty"><Empty icon={<span class="br-warn"><Glyph name="warn" size={28} /></span>} title="This page could not be shown"
              body={cur().error} action={<div class="br-actions"><Button variant="primary" onClick={() => act("reload")}>Retry</Button><Button onClick={() => call("navigate", { tab: cur().id, url: "about:blank" })}>Blank tab</Button></div>} /></div>
          </Show>
          <Show when={covered() && cur() && !blank(cur().url) && !cur().error}>
            <div class="br-empty dim"><span class="br-veil">Page hidden while an overlay is open</span></div>
          </Show>
        </div>
      </div>
    </Page>
  );
}
