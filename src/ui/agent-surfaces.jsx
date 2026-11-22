// Agent Apps side panel: open KiCad, DBeaver, Chromium etc. directly beside chat.
import { For, Show, createSignal, createResource, createEffect, onCleanup } from "solid-js";
import { Button, Badge, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc } from "../lib/rpc.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/surfaces-memory.css";

const TAGLINE = {
  kicad: "PCB & schematics",
  dbeaver: "SQL & databases",
  chromium: "Sandboxed browser",
  wireshark: "Packet capture",
  freecad: "3D CAD",
};

export function AgentSurfaces(props) {
  const [status, { refetch: refetchStatus }] = createResource(() => rpc("surfaces.status", {}, { local: true }).catch(() => null));
  const [catalog] = createResource(() => rpc("surfaces.catalog", {}, { local: true }).catch(() => []));
  const [running, { refetch: refetchRunning }] = createResource(
    () => [props.agentId || "", props.repoId || ""],
    ([agentId, repoId]) =>
      rpc("surfaces.list", { agent_id: agentId || null, repo_id: agentId ? null : (repoId || null) }, { local: true })
        .then((rows) => (rows ?? []).filter((r) => r.state === "running"))
        .catch(() => []),
  );
  const [activeId, setActiveId] = createSignal(null);
  const [launching, setLaunching] = createSignal("");
  const [reloadTick, setReloadTick] = createSignal(0);
  const [readyId, setReadyId] = createSignal(null);

  useVisibleEvent("surfaces:changed", () => { refetchRunning(); refetchStatus(); }, { debounceMs: 200 });

  const activeSurface = () => {
    const list = running() ?? [];
    return list.find((s) => s.id === activeId()) || list[0] || null;
  };

  createEffect(() => {
    const s = activeSurface();
    if (!s) { setReadyId(null); return; }
    let alive = true;
    const poll = async () => {
      const r = await rpc("surfaces.probe", { id: s.id }, { local: true }).catch(() => ({ ready: false }));
      if (!alive) return;
      if (r?.ready) setReadyId(s.id);
      else setTimeout(() => { if (alive) poll(); }, 700);
    };
    poll();
    onCleanup(() => { alive = false; });
  });

  const launch = async (recipeId) => {
    if (launching()) return;
    setLaunching(recipeId);
    try {
      const s = await rpc("surfaces.launch", {
        recipe_id: recipeId,
        repo_id: props.repoId || null,
        agent_id: props.agentId || null,
      }, { action: true });
      setActiveId(s.id);
      refetchRunning();
    } catch (err) { toast(String(err), "danger"); }
    finally { setLaunching(""); }
  };

  const stop = async (id) => {
    try {
      await rpc("surfaces.stop", { id }, { action: true });
      if (activeId() === id) setActiveId(null);
      refetchRunning();
    } catch (err) { toast(String(err), "danger"); }
  };

  return (
    <div class="surf-shell">
      <div class="surf-bar">
        <Icon name="layers" size={14} />
        <strong class="small grow">Apps</strong>
        <Show when={activeSurface()}>
          {(s) => (
            <>
              <Button size="sm" variant="ghost" onClick={() => setReloadTick((n) => n + 1)}><Icon name="refresh" size={12} /></Button>
              <Button size="sm" variant="danger" onClick={() => stop(s().id)}><Icon name="stop" size={11} /> Stop</Button>
            </>
          )}
        </Show>
      </div>

      <Show when={activeSurface()} fallback={
        <div class="surf-aside-list">
          <For each={catalog() ?? []}>
            {(r) => (
              <div class="surf-card" style={{ padding: "10px 12px", gap: "6px" }}>
                <div class="row">
                  <div class="grow">
                    <div class="small" style={{ "font-weight": "600" }}>{r.name}</div>
                    <div class="xs faint">{TAGLINE[r.id] || r.category}</div>
                  </div>
                  <Button
                    size="sm"
                    variant="primary"
                    busy={launching() === r.id}
                    disabled={!!launching() || status()?.pilot}
                    onClick={() => launch(r.id)}
                  >
                    Open
                  </Button>
                </div>
              </div>
            )}
          </For>
        </div>
      }>
        {(s) => (
          <div class="surf-stage">
            <Show when={readyId() === s().id} fallback={
              <div class="loading" role="status">
                <span class="spinner" />
                <span>Starting {s().name}…</span>
              </div>
            }>
              <iframe
                class="surf-frame"
                title={s().name}
                src={`${s().url}?t=${reloadTick()}`}
                allow="clipboard-read; clipboard-write"
              />
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}
