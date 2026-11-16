import { createSignal, createEffect, onMount, onCleanup, For, Show } from "solid-js";
import { Page, Button, Input, Empty, Badge } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent } from "../lib/rpc.js";
import "../styles/browser.css";

export default function Screen() {
  const [tabs, setTabs] = createSignal([]);
  const [active, setActive] = createSignal(null);
  const [addr, setAddr] = createSignal("");
  const [err, setErr] = createSignal("");
  let view;
  let gone = false;

  const cur = () => tabs().find((t) => t.id === active());
  const upsert = (t) => setTabs((l) => (l.some((x) => x.id === t.id) ? l.map((x) => (x.id === t.id ? { ...x, ...t } : x)) : [...l, t]));

  const place = async () => {
    const t = cur();
    if (gone || !t || !view) return;
    const r = view.getBoundingClientRect();
    try {
      await rpc("browser.set_bounds", { tab: t.id, x: r.left, y: r.top, w: r.width, h: r.height });
      await rpc("browser.show", { tab: t.id });
    } catch (e) { setErr(String(e)); }
  };

  const select = async (id) => {
    const prev = active();
    if (prev && prev !== id) await rpc("browser.hide", { tab: prev }).catch(() => {});
    setActive(id);
  };

  const open = async (url) => {
    setErr("");
    try {
      const t = await rpc("browser.open", { url });
      upsert(t);
      await select(t.id);
    } catch (e) { setErr(String(e)); }
  };

  const close = async (id) => {
    await rpc("browser.close", { tab: id }).catch(() => {});
    const rest = tabs().filter((t) => t.id !== id);
    setTabs(rest);
    if (active() === id) setActive(rest.length ? rest[rest.length - 1].id : null);
  };

  useEvent("browser:tab", (t) => { upsert(t); if (t.id === active() && document.activeElement?.tagName !== "INPUT") setAddr(t.url); });
  useEvent("browser:closed", ({ id }) => setTabs((l) => l.filter((t) => t.id !== id)));

  createEffect(() => { const t = cur(); if (t) setAddr(t.url); place(); });

  onMount(async () => {
    try {
      const l = await rpc("browser.tabs");
      setTabs(l);
      if (l.length) setActive(l[0].id);
    } catch (e) { setErr(String(e)); }
    const ro = new ResizeObserver(place);
    ro.observe(view);
    window.addEventListener("resize", place);
    onCleanup(() => { ro.disconnect(); window.removeEventListener("resize", place); });
  });
  // The native webview floats above the DOM: hide it whenever this screen goes away.
  // Hide every known tab explicitly (the backend may ignore an empty tab id) and
  // once more with no id, so no native webview stays on screen after navigating away.
  onCleanup(() => {
    gone = true;
    for (const t of tabs()) rpc("browser.hide", { tab: t.id }).catch(() => {});
    rpc("browser.hide", {}).catch(() => {});
  });

  const go = (e) => { e.preventDefault(); const t = cur(); t ? rpc("browser.navigate", { tab: t.id, url: addr() }).catch((x) => setErr(String(x))) : open(addr()); };
  const act = (m) => cur() && rpc(`browser.${m}`, { tab: cur().id }).catch((x) => setErr(String(x)));

  return (
    <Page title="Browser" sub="Shared browser for you and your agents. Pages cannot reach Philium." flush
      actions={<Button onClick={() => open("https://duckduckgo.com")}><Icon name="plus" /> New tab</Button>}>
      <div class="br">
        <div class="br-tabs" classList={{ none: !tabs().length }}>
          <For each={tabs()}>{(t) => (
            <div class="br-tab" classList={{ on: t.id === active() }} onClick={() => select(t.id)}>
              <span>{t.title || t.url}</span>
              <Show when={t.agent}><Badge>{t.agent}</Badge></Show>
              <button title="Close tab" onClick={(e) => { e.stopPropagation(); close(t.id); }}>x</button>
            </div>
          )}</For>
        </div>
        <form class="br-bar" onSubmit={go}>
          <Button type="button" onClick={() => act("back")} title="Back"><span>&larr;</span></Button>
          <Button type="button" onClick={() => act("forward")} title="Forward"><span>&rarr;</span></Button>
          <Button type="button" onClick={() => act("reload")} title="Reload"><Icon name="refresh" /></Button>
          <Input value={addr()} onInput={(e) => setAddr(e.currentTarget.value)} placeholder="Search or enter address" />
          <Button type="button" disabled={!cur()} title="Open in system browser" onClick={() => rpc("browser.external", { url: cur().url })}><Icon name="external" /></Button>
        </form>
        <Show when={err()}><div class="br-note" style="color:#e5484d">{err()}</div></Show>
        <div class="br-view" ref={view}>
          <Show when={!cur()}>
            <div class="br-empty"><Empty icon={<Icon name="globe" size={28} />} title="No tab open" action={<Button onClick={() => open("https://duckduckgo.com")}>Open a tab</Button>} body="Open a page to browse alongside your agents. Agents can open tabs here too." /></div>
          </Show>
        </div>
      </div>
    </Page>
  );
}
