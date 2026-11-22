// Apps: isolated desktop apps running right inside Philium.
import { For, Show, createSignal, createResource, createMemo, createEffect, onCleanup } from "solid-js";
import { Page, Button, Badge, Empty, Field, Input, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc } from "../lib/rpc.js";
import { activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/surfaces-memory.css";

const TAGLINE = {
  kicad: "PCB & schematics",
  dbeaver: "SQL & databases",
  chromium: "Sandboxed browser",
  wireshark: "Packet capture",
  freecad: "3D CAD",
};

export default function Surfaces() {
  const [status, { refetch: refetchStatus }] = createResource(() => rpc("surfaces.status", {}, { local: true }).catch(() => null));
  const [catalog, { refetch: refetchCatalog }] = createResource(() => rpc("surfaces.catalog", {}, { local: true }).catch(() => []));
  const [surfaces, { refetch: refetchSurfaces }] = createResource(() => rpc("surfaces.list", {}, { local: true }).catch(() => []));
  const [activeId, setActiveId] = createSignal(null);
  const [launching, setLaunching] = createSignal("");
  const [reloadTick, setReloadTick] = createSignal(0);
  const [readyId, setReadyId] = createSignal(null);
  const [showCustom, setShowCustom] = createSignal(false);

  const [cid, setCid] = createSignal("");
  const [cname, setCname] = createSignal("");
  const [cimage, setCimage] = createSignal("");
  const [cport, setCport] = createSignal("3000");
  const [adding, setAdding] = createSignal(false);

  useVisibleEvent("surfaces:changed", () => { refetchSurfaces(); refetchStatus(); }, { debounceMs: 200 });

  const running = createMemo(() => (surfaces() ?? []).filter((s) => s.state === "running"));
  const active = createMemo(() => running().find((s) => s.id === activeId()) || null);

  createEffect(() => {
    const s = active();
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
      const s = await rpc("surfaces.launch", { recipe_id: recipeId, repo_id: activeRepoId() || null }, { action: true });
      setActiveId(s.id);
      refetchSurfaces();
      refetchStatus();
    } catch (err) { toast(String(err), "danger"); }
    finally { setLaunching(""); }
  };

  const stop = async (id) => {
    try {
      await rpc("surfaces.stop", { id }, { action: true });
      if (activeId() === id) setActiveId(null);
      refetchSurfaces();
      refetchStatus();
    } catch (err) { toast(String(err), "danger"); }
  };

  const addCustom = async (e) => {
    e?.preventDefault();
    if (!cid().trim() || !cname().trim() || !cimage().trim() || adding()) return;
    setAdding(true);
    try {
      await rpc("surfaces.add_recipe", {
        id: cid().trim(),
        name: cname().trim(),
        image: cimage().trim(),
        internal_port: Number(cport()) || 3000,
        category: "custom",
      });
      setCid(""); setCname(""); setCimage(""); setShowCustom(false);
      toast("App added", "secure");
      refetchCatalog();
    } catch (err) { toast(String(err), "danger"); }
    finally { setAdding(false); }
  };

  const runtimeLabel = () => {
    const rt = status()?.runtime;
    return rt && rt !== "none" ? `Isolated · ${rt}` : "No container runtime";
  };

  return (
    <Page
      title="Apps"
      flush
      actions={
        <>
          <Badge tone={status()?.runtime && status().runtime !== "none" ? "secure" : "strict"} title="Runs inside Philium, never opens on your host desktop">
            {runtimeLabel()}
          </Badge>
          <Show when={active()}>
            <Button size="sm" variant="ghost" onClick={() => setActiveId(null)}>All apps</Button>
          </Show>
          <Button size="sm" onClick={() => { setActiveId(null); setShowCustom(!showCustom()); }}>
            <Icon name="plus" size={12} /> Add
          </Button>
        </>
      }
    >
      <div class="surf-shell">
        <Show when={running().length > 0}>
          <div class="surf-bar">
            <button type="button" class={`aside-tab ${!active() ? "on" : ""}`} onClick={() => setActiveId(null)}>
              <Icon name="grid" size={12} /> All
            </button>
            <For each={running()}>
              {(s) => (
                <button type="button" class={`aside-tab ${active()?.id === s.id ? "on" : ""}`} onClick={() => setActiveId(s.id)}>
                  <span class="tdot run" />
                  <span>{s.name}</span>
                </button>
              )}
            </For>
            <div class="grow" />
            <Show when={active()}>
              {(s) => (
                <>
                  <Button size="sm" variant="ghost" onClick={() => setReloadTick((n) => n + 1)}><Icon name="refresh" size={12} /></Button>
                  <Button size="sm" variant="danger" onClick={() => stop(s().id)}><Icon name="stop" size={11} /> Stop</Button>
                </>
              )}
            </Show>
          </div>
        </Show>

        <Show when={active()} fallback={
          <div class="page-body">
            <div class="col" style={{ gap: "16px" }}>
              <Show when={showCustom()}>
                <form class="card col" style={{ gap: "10px" }} onSubmit={addCustom}>
                  <div class="row">
                    <strong class="grow">Add app</strong>
                    <Button size="sm" variant="ghost" type="button" onClick={() => setShowCustom(false)}>Close</Button>
                  </div>
                  <div class="grid grid-4">
                    <Field label="ID"><Input mono value={cid()} onValue={setCid} placeholder="pgadmin" /></Field>
                    <Field label="Name"><Input value={cname()} onValue={setCname} placeholder="pgAdmin" /></Field>
                    <Field label="Image"><Input mono value={cimage()} onValue={setCimage} placeholder="lscr.io/linuxserver/..." /></Field>
                    <Field label="Port"><Input mono value={cport()} onValue={setCport} placeholder="3000" /></Field>
                  </div>
                  <div class="row" style={{ "justify-content": "flex-end" }}>
                    <Button variant="primary" type="submit" busy={adding()}>Save</Button>
                  </div>
                </form>
              </Show>

              <Show when={(catalog() ?? []).length} fallback={<Empty title="No apps" />}>
                <div class="surf-catalog">
                  <For each={catalog()}>
                    {(r) => {
                      const liveSurf = () => running().find((s) => s.recipe_id === r.id);
                      return (
                        <div class="surf-card">
                          <div class="row">
                            <strong class="grow">{r.name}</strong>
                            <Badge tone={liveSurf() ? "secure" : ""}>{liveSurf() ? "Running" : r.category}</Badge>
                          </div>
                          <div class="xs dim">{TAGLINE[r.id] || r.description}</div>
                          <div class="row" style={{ "margin-top": "4px" }}>
                            <Show when={liveSurf()} fallback={
                              <Button
                                size="sm"
                                variant="primary"
                                busy={launching() === r.id}
                                disabled={!!launching() || status()?.pilot}
                                onClick={() => launch(r.id)}
                              >
                                <Icon name="play" size={11} /> Open
                              </Button>
                            }>
                              {(s) => (
                                <>
                                  <Button size="sm" variant="primary" onClick={() => setActiveId(s().id)}>Show</Button>
                                  <Button size="sm" variant="ghost" onClick={() => stop(s().id)}>Stop</Button>
                                </>
                              )}
                            </Show>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </Show>
            </div>
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
    </Page>
  );
}
