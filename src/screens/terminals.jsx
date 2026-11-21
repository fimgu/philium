import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
  on,
  onMount,
  onCleanup,
} from "solid-js";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { Page, Button, Segmented, Empty, Modal, Field, Input } from "../ui/kit.jsx";
import { rpc, useEvent } from "../lib/rpc.js";
import { params } from "../lib/nav.js";
import { activeRepoId, useStore } from "../lib/store.js";
import { useScreenVisible, useVisibleEvent } from "../lib/visibility.js";
import "../styles/terminals.css";

const decode = (s) => {
  const bin = atob(s || "");
  const len = bin.length;
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i++) out[i] = bin.charCodeAt(i);
  return out;
};
const encode = (s) => {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
};

function Pane(props) {
  const visible = useScreenVisible();
  let el,
    term,
    fit,
    observer,
    resizeRaf = 0,
    lastCols = 0,
    lastRows = 0,
    dead = false,
    ready = false,
    offset = 0;
  const queued = [];
  const hiddenQueue = [];
  const [error, setError] = createSignal("");
  const append = (p) => {
    const data = decode(p.data);
    const end = p.offset ?? offset + data.length;
    const skip = Math.max(0, offset - (end - data.length));
    if (end > offset) term?.write(data.subarray(skip));
    offset = Math.max(offset, end);
  };
  const doResize = () => {
    resizeRaf = 0;
    if (dead || !visible() || !el || !el.clientWidth || !el.clientHeight) return;
    try {
      fit.fit();
    } catch {
      return;
    }
    if (
      props.item.state === "running" &&
      (term.cols !== lastCols || term.rows !== lastRows)
    ) {
      lastCols = term.cols;
      lastRows = term.rows;
      rpc("terminals.resize", {
        id: props.item.id,
        cols: term.cols,
        rows: term.rows,
      }).catch(setError);
    }
  };
  const resize = () => {
    if (dead || !visible() || resizeRaf) return;
    resizeRaf = requestAnimationFrame(doResize);
  };

  useEvent("terminals:data", (p) => {
    if (dead || p?.id !== props.item.id) return;
    if (!ready) {
      queued.push(p);
      return;
    }
    if (!visible()) {
      hiddenQueue.push(p);
      if (hiddenQueue.length > 500) append(hiddenQueue.shift());
      return;
    }
    append(p);
  });

  createEffect(() => {
    if (visible() && ready && !dead) {
      if (hiddenQueue.length) {
        for (const p of hiddenQueue) append(p);
        hiddenQueue.length = 0;
      }
      resize();
    }
  });

  onMount(async () => {
    term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      scrollback: 10000,
      theme: { background: "#111318", foreground: "#dce1e8" },
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    observer = new ResizeObserver(resize);
    observer.observe(el);
    term.onData((data) =>
      rpc("terminals.write", { id: props.item.id, data: encode(data) }).catch(
        setError,
      ),
    );
    try {
      const b = await rpc("terminals.buffer", { id: props.item.id });
      if (dead) return;
      const initialBytes = decode(b?.data);
      if (initialBytes.length) term.write(initialBytes);
      offset = b?.offset ?? initialBytes.length;
      ready = true;
      for (const p of queued) append(p);
      queued.length = 0;
      doResize();
    } catch (e) {
      setError(String(e));
      ready = true;
      for (const p of queued) append(p);
      queued.length = 0;
    }
  });
  onCleanup(() => {
    dead = true;
    if (resizeRaf) cancelAnimationFrame(resizeRaf);
    observer?.disconnect();
    term?.dispose();
  });
  return (
    <section class="terminal-pane">
      <header>
        <strong>{props.item.title}</strong>
        <span>
          {props.item.agent_name ||
            (props.item.agent_id ? "Agent" : "Personal")}
        </span>
        <span
          class={
            props.item.state === "running"
              ? "terminal-running"
              : "terminal-exited"
          }
        >
          {props.item.state === "running"
            ? "Running"
            : `Exited (${props.item.exit_code ?? "interrupted"})`}
        </span>
        <Button size="sm" onClick={props.onRename}>
          Rename
        </Button>
        <Button size="sm" onClick={props.onClose}>
          {props.item.state === "running" ? "Stop" : "Remove"}
        </Button>
      </header>
      <Show when={error()}>
        <p role="alert">{String(error())}</p>
      </Show>
      <div class="terminal-host" ref={el} />
    </section>
  );
}

export default function Terminals() {
  const store = useStore();
  const items = () => store.terminals() ?? [];
  const [selected, setSelected] = createSignal(params()?.id ?? null),
    [panes, setPanes] = createSignal("1"),
    [showAll, setShowAll] = createSignal(false),
    [renaming, setRenaming] = createSignal(null),
    [renameTitle, setRenameTitle] = createSignal(""),
    [error, setError] = createSignal("");
  const reload = async () => {
    try {
      await store.refetchTerminals();
    } catch (e) {
      setError(String(e));
    }
  };
  useVisibleEvent(
    "terminals:data",
    (p) => {
      if (p?.id && !items().some((t) => t.id === p.id)) reload();
    },
    { debounceMs: 500 },
  );

  createEffect(
    on(
      activeRepoId,
      () => {
        setShowAll(false);
      },
      { defer: true },
    ),
  );

  createEffect(() => {
    const pid = params()?.id;
    if (!pid) return;
    setSelected(pid);
    const hit = items().find((t) => t.id === pid);
    if (hit && activeRepoId() && hit.workspace_id !== activeRepoId()) {
      setShowAll(true);
    }
  });

  const hiddenCount = createMemo(() => {
    const rid = activeRepoId();
    if (!rid) return 0;
    return items().filter((t) => t.workspace_id !== rid).length;
  });

  const scopedItems = createMemo(() => {
    const rid = activeRepoId();
    if (!rid || showAll()) return items();
    return items().filter((t) => t.workspace_id === rid);
  });

  createEffect(() => {
    const list = scopedItems();
    const pid = params()?.id;
    if (pid && list.some((t) => t.id === pid)) {
      if (selected() !== pid) setSelected(pid);
      return;
    }
    if (!list.some((t) => t.id === selected())) {
      setSelected(list[0]?.id ?? null);
    }
  });

  const visible = createMemo(() => {
    const list = scopedItems(),
      i = Math.max(
        0,
        list.findIndex((t) => t.id === selected()),
      );
    return [...list.slice(i), ...list.slice(0, i)].slice(0, Number(panes()));
  });
  const open = async () => {
    try {
      const rid = activeRepoId();
      const id = await rpc("terminals.open", rid ? { repo_id: rid } : {});
      await reload();
      setSelected(id);
    } catch (e) {
      setError(String(e));
    }
  };
  const close = async (t) => {
    try {
      await rpc(t.state === "running" ? "terminals.kill" : "terminals.remove", {
        id: t.id,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };
  const startRename = (t) => {
    setRenaming(t);
    setRenameTitle(t.title ?? "");
  };
  const submitRename = async () => {
    const t = renaming();
    const title = renameTitle().trim();
    if (!t || !title) return;
    try {
      await rpc("terminals.rename", { id: t.id, title });
      setRenaming(null);
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <Page
      title="Terminals"
      sub="Persistent shells stay running when you leave this screen"
      flush
      actions={
        <>
          <Show when={activeRepoId() && hiddenCount() > 0}>
            <Button
              size="sm"
              variant={showAll() ? "primary" : "ghost"}
              onClick={() => setShowAll(!showAll())}
            >
              {showAll()
                ? `All terminals (${items().length})`
                : `View all terminals (${items().length})`}
            </Button>
          </Show>
          <Segmented
            value={panes()}
            options={["1", "2", "4"]}
            onChange={setPanes}
          />
          <Button onClick={open}>New terminal</Button>
        </>
      }
    >
      <div class="terminals-screen">
        <Show when={error()}>
          <p role="alert">{error()}</p>
        </Show>
        <Show
          when={scopedItems().length}
          fallback={
            <Empty
              title="Your persistent workspace"
              action={
                <div class="row" style={{ gap: "8px", "justify-content": "center" }}>
                  <Button variant="primary" onClick={open}>Open a terminal</Button>
                  <Show when={hiddenCount() > 0 && !showAll()}>
                    <Button onClick={() => setShowAll(true)}>
                      All terminals ({items().length})
                    </Button>
                  </Show>
                </div>
              }
            >
              Run a shell here, or watch an agent's commands. Return at any time
              to see recent output.
              <Show when={hiddenCount() > 0 && !showAll()}>
                {` (${hiddenCount()} terminal${hiddenCount() === 1 ? "" : "s"} in other workspaces or personal shells hidden by workspace filter.)`}
              </Show>
            </Empty>
          }
        >
          <nav class="terminal-tabs" aria-label="Terminal tabs">
            <For each={scopedItems()}>
              {(t) => (
                <button
                  class={selected() === t.id ? "on" : ""}
                  onClick={() => setSelected(t.id)}
                >
                  {t.title}
                  {t.agent_name ? ` · ${t.agent_name}` : ""}
                  {t.state !== "running" ? " · exited" : ""}
                </button>
              )}
            </For>
            <Show when={activeRepoId() && hiddenCount() > 0 && !showAll()}>
              <button
                type="button"
                class="ghost"
                title="Show personal and other workspace terminals"
                onClick={() => setShowAll(true)}
              >
                +{hiddenCount()} in All terminals
              </button>
            </Show>
          </nav>
          <div class={`terminal-grid panes-${panes()}`}>
            <For each={visible()}>
              {(t) => (
                <Pane
                  item={t}
                  onClose={() => close(t)}
                  onRename={() => startRename(t)}
                />
              )}
            </For>
          </div>
        </Show>
      </div>
      <Show when={renaming()}>
        {(t) => (
          <Modal
            title={`Rename ${t().title || "terminal"}`}
            onClose={() => setRenaming(null)}
            footer={
              <>
                <Button onClick={() => setRenaming(null)}>Cancel</Button>
                <Button
                  variant="primary"
                  disabled={!renameTitle().trim()}
                  onClick={submitRename}
                >
                  Save
                </Button>
              </>
            }
          >
            <Field label="Terminal title">
              <Input
                value={renameTitle()}
                onValue={setRenameTitle}
                onKeyDown={(e) => e.key === "Enter" && submitRename()}
                placeholder="Terminal title"
              />
            </Field>
          </Modal>
        )}
      </Show>
    </Page>
  );
}
