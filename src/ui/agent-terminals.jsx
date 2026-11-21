// Right-hand side panel of an agent: its live browser and its terminals.
// The human sees exactly what the agent does and can take over at any time.
import { For, Show, createSignal, createResource, createMemo, createEffect, on } from "solid-js";
import { Button, Empty, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { TermView } from "./termview.jsx";
import { rpc, useEvent } from "../lib/rpc.js";

const LOCAL = /\bhttps?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})(\/[^\s"'<>\x1b]*)?/g;

/** Local dev-server URLs printed in terminal output, newest last. */
export function localUrls(text) {
  const out = [];
  for (const m of text.matchAll(LOCAL)) {
    const url = m[0].replace("0.0.0.0", "localhost").replace(/[.,;:)\]]+$/, "");
    if (!out.includes(url)) out.push(url);
  }
  return out;
}

function termLabel(t, agentName) {
  const title = t.title ?? "";
  if (title === `${agentName} shell`) return "Shell";
  const p = `${agentName}: `;
  return title.startsWith(p) ? title.slice(p.length) : title;
}

export function AgentTerminals(props) {
  const isActive = () => props.active !== false;
  const [activated, setActivated] = createSignal(isActive());
  let dirtyList = false, dirtyMain = false;
  const [list, { refetch }] = createResource(() => (activated() ? props.id : null), (id) => {
    dirtyList = false;
    return rpc("terminals.list").then((t) => t.filter((x) => x.agent_id === id)).catch(() => []);
  });
  const [mainId, { refetch: refetchMain }] = createResource(() => (activated() && props.id ? [props.id, props.live] : null), ([id]) => {
    dirtyMain = false;
    return rpc("agents.shell", { id }).catch(() => null);
  });
  useEvent("terminals:exit", () => {
    if (isActive()) refetch();
    else dirtyList = true;
  });
  useEvent("terminals:changed", () => {
    if (isActive()) { refetch(); refetchMain(); }
    else { dirtyList = true; dirtyMain = true; }
  });
  createEffect(() => {
    if (!isActive()) return;
    if (!activated()) { setActivated(true); return; }
    if (dirtyList) { dirtyList = false; refetch(); }
    if (dirtyMain) { dirtyMain = false; refetchMain(); }
  });
  const [sel, setSel] = createSignal(null);
  const [showDone, setShowDone] = createSignal(false);

  // Main shell first, then running background terminals, then finished ones.
  const items = createMemo(() => {
    const all = list() ?? [];
    const rank = (t) => (t.id === mainId() ? 0 : t.state === "running" ? 1 : 2);
    return all.slice().sort((a, b) => rank(a) - rank(b) || b.created_at - a.created_at);
  });
  const visible = createMemo(() => items().filter((t) => showDone() || t.state === "running" || t.id === sel()));
  const done = () => items().filter((t) => t.state !== "running").length;
  const current = createMemo(() => items().find((t) => t.id === sel()) ?? visible()[0] ?? items()[0]);
  createEffect(on(() => props.id, () => setSel(null)));
  // Follow the main shell when it appears.
  createEffect(on(mainId, (m) => { if (m && !sel()) setSel(m); }));

  const openShell = async () => {
    const id = await rpc("agents.shell", { id: props.id, open: true }).catch((e) => { toast(String(e), "danger"); return null; });
    if (id) { refetchMain(); refetch(); setSel(id); }
    else if (!props.live) toast("Start the agent to open its shell", "danger");
  };

  return (
    <div class="aside-terms">
      <div class="aside-tabs">
        <For each={visible()}>
          {(t) => (
            <button class={`aside-tab ${current()?.id === t.id ? "on" : ""}`} onClick={() => setSel(t.id)} title={t.title}>
              <span class={`tdot ${t.state === "running" ? "run" : t.exit_code === 0 ? "ok" : "bad"}`} />
              <span class="ellipsis">{termLabel(t, props.name)}</span>
            </button>
          )}
        </For>
        <Show when={done() > 0}>
          <button class="aside-tab ghost" onClick={() => setShowDone(!showDone())}>
            {showDone() ? "Hide finished" : `${done()} finished`}
          </button>
        </Show>
        <div class="grow" />
        <Show when={props.live && !mainId()}>
          <button class="aside-tab ghost" onClick={openShell}><Icon name="plus" size={11} /> Shell</button>
        </Show>
      </div>
      <Show when={current()?.id} keyed fallback={
        <Empty title={props.live ? "No shell yet" : "Agent is stopped"}
          action={props.live ? <Button size="sm" onClick={openShell}>Open its shell</Button> : null}>
          {props.live
            ? "The agent's commands run in one shell you can watch and type into. It opens on its first command."
            : "Resume the agent to see its shell."}
        </Empty>}>
        {(tid) => { const t = () => items().find((x) => x.id === tid) ?? {}; return (
          <div class="aside-term">
            <div class="aside-term-head">
              <span class="mono xs faint ellipsis grow">{t().title}</span>
              <span class={`xs ${t().state === "running" ? "ok-text" : "faint"}`}>
                {t().state === "running" ? (tid === mainId() ? "Live, you can type here" : "Running") : `Exited ${t().exit_code ?? ""}`}
              </span>
              <Show when={t().state === "running"}>
                <button class="icon-btn" title="Stop this terminal" onClick={() => rpc("terminals.kill", { id: tid }).catch((e) => toast(String(e), "danger"))}>
                  <Icon name="stop" size={11} />
                </button>
              </Show>
            </div>
            <TermView id={tid} running={t().state === "running"} onData={(d) => { if (isActive()) props.onOutput?.(d); }} />
          </div>
        ); }}
      </Show>
    </div>
  );
}
