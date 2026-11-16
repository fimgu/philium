// <AgentBrowser agent={id} /> : the agent's own browser, shown live next to its chat.
// The agent drives it through MCP browser_* tools; the human can click, type and
// sign in at any time (both act on the same tabs). Profile is per agent and persistent.
import { createSignal, createMemo, createEffect, onMount, onCleanup, For, Show } from "solid-js";
import { Button, Badge } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc, useEvent } from "../lib/rpc.js";
import { nativeView, findLocalUrls } from "../lib/native-view.js";
import "../styles/agent-browser.css";

const G = {
  back: "M12.5 4.5 7 10l5.5 5.5",
  fwd: "M7.5 4.5 13 10l-5.5 5.5",
  x: "M5.5 5.5l9 9m0-9-9 9",
  warn: "M10 3 18 17H2zM10 8v4m0 2.5v.5",
  cursor: "M5 3l10 6-4.5 1.5L9 15z",
};
const Glyph = (p) => (
  <svg width={p.size ?? 16} height={p.size ?? 16} viewBox="0 0 20 20" fill="none" stroke="currentColor"
    stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={G[p.name]} /></svg>
);
const blank = (u) => !u || u === "about:blank";
const host = (u) => { try { return new URL(u).host; } catch { return u; } };
const b64 = (s) => { try { return atob(s); } catch { return ""; } };

/**
 * Props:
 * - agent: agent id (required). Tabs, profile and tools are scoped to it.
 * - name?, strict?, provider?: optional overrides; otherwise read via agents.get.
 * - autoOpen?: default true. Opens a localhost URL printed by the agent's terminals when the pane has no tab.
 * - visible?: default true. false hides the native webview (component stays mounted).
 * - onActivity?(): called when the agent acts or opens a tab while visible is false.
 * - class?: extra class on the root.
 * Bounds are re-synced every animation frame, so panel resizes and visible=true need no extra wiring.
 */
export function AgentBrowser(props) {
  const [tabs, setTabs] = createSignal([]);
  const [active, setActive] = createSignal(null);
  const [addr, setAddr] = createSignal("");
  const [editing, setEditing] = createSignal(false);
  const [err, setErr] = createSignal("");
  const [action, setAction] = createSignal(null);
  const [offers, setOffers] = createSignal([]);
  const [covered, setCovered] = createSignal(false);
  const [info, setInfo] = createSignal(null);
  let view, input, actionT;
  const myTerms = new Set();

  const name = () => props.name ?? info()?.name ?? "agent";
  const strict = () => props.strict ?? !!info()?.strict;
  const provider = () => props.provider ?? ((info()?.model_ref || info()?.model || "").split("/")[0] || "the model provider");

  const cur = createMemo(() => tabs().find((t) => t.id === active()));
  const upsert = (t) => setTabs((l) => (l.some((x) => x.id === t.id) ? l.map((x) => (x.id === t.id ? { ...x, ...t } : x)) : [...l, t]));
  const drop = (id) => {
    const rest = tabs().filter((t) => t.id !== id);
    setTabs(rest);
    if (active() === id) setActive(rest.length ? rest[rest.length - 1].id : null);
  };
  const select = (id) => { setActive(id); setErr(""); rpc("browser.select", { tab: id }).catch(() => {}); };
  const call = (m, a) => rpc(`browser.${m}`, a).catch((e) => {
    const s = String(e);
    if (/is closed|no such tab/.test(s) && a?.tab) drop(a.tab); else setErr(s);
  });

  const load = async () => {
    const id = props.agent;
    setTabs([]); setActive(null); setOffers([]); myTerms.clear();
    if (!id) return;
    // Start clean: no stale native view from a previous mount or app reload stays on screen.
    rpc("browser.hide", { owner: id }).catch(() => {});
    rpc("agents.get", { id }).then((r) => props.agent === id && setInfo(r)).catch(() => {});
    rpc("terminals.list").then((l) => { for (const t of l || []) if (t.agent_id === id) myTerms.add(t.id); }).catch(() => {});
    try {
      const l = await rpc("browser.tabs", { owner: id });
      if (props.agent !== id) return;
      setTabs(l || []);
      if (l?.length) setActive(l[l.length - 1].id);
    } catch (e) { setErr(String(e)); }
  };
  let prevAgent;
  createEffect(() => {
    if (prevAgent && prevAgent !== props.agent) rpc("browser.hide", { owner: prevAgent }).catch(() => {});
    prevAgent = props.agent;
    load();
  });

  const open = async (url = "about:blank") => {
    setErr("");
    try {
      const t = await rpc("browser.open", { url, owner: props.agent });
      upsert(t); select(t.id);
      if (blank(url)) queueMicrotask(() => input?.focus());
      return t;
    } catch (e) { setErr(String(e)); }
  };
  const go = (url) => { const t = cur(); t ? call("navigate", { tab: t.id, url }) : open(url); setOffers((o) => o.filter((x) => x !== url)); };
  const close = async (id) => { drop(id); await rpc("browser.close", { tab: id }).catch(() => {}); };

  const ping = () => { if (props.visible === false) props.onActivity?.(); };
  useEvent("browser:tab", (t) => {
    if (t.owner !== props.agent) return;
    if (!tabs().some((x) => x.id === t.id)) ping();
    upsert(t);
    if (!active()) setActive(t.id);
  });
  useEvent("browser:closed", ({ id }) => drop(id));
  useEvent("browser:opened", ({ id, opener }) => { if (tabs().some((t) => t.id === opener)) setActive(id); });
  useEvent("browser:agent_action", (a) => {
    if (a.owner !== props.agent) return;
    ping();
    if (a.tab && a.tab !== active() && tabs().some((t) => t.id === a.tab)) setActive(a.tab);
    setAction(a.text);
    clearTimeout(actionT); actionT = setTimeout(() => setAction(null), 2500);
  });
  // Dev servers: offer localhost URLs printed by this agent's terminals.
  useEvent("terminals:changed", () => rpc("terminals.list").then((l) => { for (const t of l || []) if (t.agent_id === props.agent) myTerms.add(t.id); }).catch(() => {}));
  useEvent("terminals:data", (d) => {
    if (!myTerms.has(d.id)) return;
    const found = findLocalUrls(b64(d.data));
    if (!found.length) return;
    if (!tabs().length && props.autoOpen !== false) { open(found[0]); found.shift(); }
    const known = new Set(tabs().map((t) => t.url.replace(/\/$/, "")));
    const fresh = found.filter((u) => !known.has(u.replace(/\/$/, "")));
    if (fresh.length) setOffers((o) => [...new Set([...fresh, ...o])].slice(0, 3));
  });

  createEffect(() => { const t = cur(); if (!editing()) setAddr(t && !blank(t.url) ? t.url : ""); });

  onMount(() => {
    const stop = nativeView(() => view, () => {
      if (props.visible === false) return null;
      const t = cur();
      return t && !blank(t.url) && !t.error ? t.id : null;
    }, setCovered, (e, id) => { if (/is closed|no such tab/.test(e)) drop(id); });
    onCleanup(() => { stop(); clearTimeout(actionT); });
  });

  const submit = (e) => {
    e.preventDefault();
    const v = addr().trim();
    if (!v) return;
    setEditing(false); input?.blur();
    go(v);
  };
  const act = (m) => cur() && call(m, { tab: cur().id });
  const loading = () => !!cur()?.loading && !blank(cur()?.url);
  const live = () => cur() && !blank(cur().url) && !cur().error;

  return (
    <div class={`ab ${props.class ?? ""}`} classList={{ acting: !!action() }}>
      <div class="ab-head">
        <div class="ab-tabs" role="tablist">
          <For each={tabs()}>{(t) => (
            <div class="ab-tab" role="tab" aria-selected={t.id === active()} classList={{ on: t.id === active() }} title={t.url}
              onClick={() => select(t.id)} onMouseDown={(e) => e.button === 1 && e.preventDefault()}
              onAuxClick={(e) => { if (e.button === 1) { e.preventDefault(); close(t.id); } }}>
              <Show when={t.loading && !blank(t.url)} fallback={<span class="ab-dot" classList={{ err: !!t.error }} />}><span class="ab-spin" /></Show>
              <span class="ab-title">{t.title || (blank(t.url) ? "New tab" : host(t.url))}</span>
              <button class="ab-x" aria-label="Close tab" title="Close tab" onClick={(e) => { e.stopPropagation(); close(t.id); }}><Glyph name="x" size={11} /></button>
            </div>
          )}</For>
          <button class="ab-new" aria-label="New tab" title="New tab" onClick={() => open()}><Icon name="plus" size={14} /></button>
        </div>
        <form class="ab-bar" onSubmit={submit}>
          <Button type="button" variant="ghost" size="sm" class="icon" disabled={!live()} onClick={() => act("back")} title="Back"><Glyph name="back" size={14} /></Button>
          <Button type="button" variant="ghost" size="sm" class="icon" disabled={!live()} onClick={() => act("forward")} title="Forward"><Glyph name="fwd" size={14} /></Button>
          <Show when={loading()} fallback={<Button type="button" variant="ghost" size="sm" class="icon" disabled={!cur() || blank(cur()?.url)} onClick={() => act("reload")} title="Reload"><Icon name="refresh" size={14} /></Button>}>
            <Button type="button" variant="ghost" size="sm" class="icon" onClick={() => act("stop")} title="Stop loading"><Glyph name="x" size={14} /></Button>
          </Show>
          <div class="ab-addr" classList={{ focus: editing() }}>
            <Show when={cur() && !blank(cur().url)}>
              <span class="ab-lock" classList={{ insecure: cur().url.startsWith("http:") }}><Icon name={cur().url.startsWith("https:") ? "lock" : "shield"} size={12} /></span>
            </Show>
            <input ref={input} value={addr()} spellcheck={false} autocomplete="off" placeholder={`Address for ${name()}'s browser`}
              onFocus={(e) => { setEditing(true); const t = cur(); if (t && !blank(t.url)) setAddr(t.url); queueMicrotask(() => e.currentTarget.select()); }}
              onBlur={() => setEditing(false)} onInput={(e) => setAddr(e.currentTarget.value)}
              onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); const t = cur(); setAddr(t && !blank(t.url) ? t.url : ""); e.currentTarget.blur(); } }} />
            <Show when={loading()}><div class="ab-progress" /></Show>
          </div>
          <Button type="button" variant="ghost" size="sm" class="icon" disabled={!live()} title="Open in system browser" onClick={() => call("external", { url: cur().url })}><Icon name="external" size={14} /></Button>
        </form>
        <Show when={strict()}>
          <div class="ab-note warn"><Icon name="shield" size={12} /> Pages you open here, including signed-in ones, are visible to {provider()}.</div>
        </Show>
        <Show when={offers().length}>
          <div class="ab-offers">
            <span>Dev server</span>
            <For each={offers()}>{(u) => <button class="ab-offer" onClick={() => go(u)}>{u.replace(/^https?:\/\//, "")}</button>}</For>
            <button class="ab-offer-x" aria-label="Dismiss" onClick={() => setOffers([])}><Glyph name="x" size={10} /></button>
          </div>
        </Show>
        <Show when={err()}><div class="ab-note bad"><Glyph name="warn" size={12} /> {err()}<button aria-label="Dismiss" onClick={() => setErr("")}><Glyph name="x" size={10} /></button></div></Show>
      </div>
      <div class="ab-view" ref={view} classList={{ live: !!live() && !covered() }}>
        <Show when={!cur()}>
          <div class="ab-empty">
            <div class="ab-empty-icon"><Icon name="globe" size={22} /></div>
            <h4>{name()}'s browser</h4>
            <p>When {name()} opens a page you will see every step here, live. You can click, type and sign in too; logins stay in this agent's private profile.</p>
            <Button size="sm" onClick={() => open()}><Icon name="plus" size={13} /> Open a page</Button>
          </div>
        </Show>
        <Show when={cur() && blank(cur().url) && !cur().error}>
          <div class="ab-empty"><h4>New tab</h4><p>Type an address above, or ask {name()} to open something.</p></div>
        </Show>
        <Show when={cur()?.error}>
          <div class="ab-empty">
            <div class="ab-empty-icon warn"><Glyph name="warn" size={22} /></div>
            <h4>This page could not be shown</h4><p>{cur().error}</p>
            <Button size="sm" variant="primary" onClick={() => act("reload")}>Retry</Button>
          </div>
        </Show>
        <Show when={covered() && live()}><div class="ab-empty dim"><p>Page hidden while an overlay is open</p></div></Show>
      </div>
      <div class="ab-status" classList={{ on: !!action() }} aria-live="polite">
        <span class="ab-pulse" /><Glyph name="cursor" size={12} /><span>{action() || " "}</span>
        <Show when={tabs().length}><Badge>{tabs().length} {tabs().length === 1 ? "tab" : "tabs"}</Badge></Show>
      </div>
    </div>
  );
}

export default AgentBrowser;
