// Philium shell: sidebar, screen router, command palette, global hosts.
// Screens register in SCREENS below; each lives in src/screens/<id>.jsx and
// default-exports a component. Adding a screen = one line here.
import { render } from "solid-js/web";
import { For, Show, createSignal, createMemo, createEffect, lazy, Suspense, onMount, onCleanup } from "solid-js";
import { Dynamic } from "solid-js/web";
import { ToastHost, ConfirmHost, Guard, toast, cleanError } from "./ui/kit.jsx";
import { ImproveHost } from "./ui/improve.jsx";
import { UpdateBanner } from "./ui/update.jsx";
import { Icon } from "./ui/icons.jsx";
import { rpc, useEvent, failures, clearFailures } from "./lib/rpc.js";
import { repoLabels } from "./lib/repo-labels.js";
import { workspaceAgents } from "./lib/agent-workspace.js";
import { agentNeedsUser, pendingAccessByAgent } from "./lib/access-helpers.js";
import { route, go, commands, improve } from "./lib/nav.js";
import { isMac } from "./lib/shortcuts.js";
import { useStore, activeRepoId, setActiveRepoId } from "./lib/store.js";
import { ScreenVisibilityProvider } from "./lib/visibility.js";
import { soundEnabled, setSoundEnabled, playSound } from "./lib/sound.js";
import "./lib/alerts.js";
import "./styles/sheet.css";
import "./styles/surfaces-memory.css";

const SCREENS = [
  { id: "home", label: "Home", icon: "home", group: "work", load: () => import("./screens/home.jsx") },
  { id: "agents", label: "Agents", icon: "agents", group: "work", load: () => import("./screens/agents.jsx") },
  { id: "board", label: "Board", icon: "board", group: "work", load: () => import("./screens/board.jsx") },
  { id: "review", label: "Review", icon: "pr", group: "work", load: () => import("./screens/review.jsx") },
  { id: "code", label: "Code", icon: "graph", group: "workbench", load: () => import("./screens/code.jsx") },
  { id: "terminals", label: "Terminals", icon: "terminal", group: "workbench", load: () => import("./screens/terminals.jsx") },
  { id: "browser", label: "Browser", icon: "globe", group: "workbench", load: () => import("./screens/browser.jsx") },
  { id: "surfaces", label: "Apps", icon: "layers", group: "workbench", load: () => import("./screens/surfaces.jsx") },
  { id: "skills", label: "Skills", icon: "layers", group: "workbench", load: () => import("./screens/skills.jsx") },
  { id: "memory", label: "Memory", icon: "brain", group: "workbench", load: () => import("./screens/memory.jsx") },
  { id: "chat", label: "Chat", icon: "chat", group: "system", load: () => import("./screens/chat.jsx") },
  { id: "environments", label: "Environments", icon: "shield", group: "system", load: () => import("./screens/environments.jsx") },
  { id: "repos", label: "Repos", icon: "repo", group: "system", load: () => import("./screens/repos.jsx") },
  { id: "activity", label: "Activity", icon: "activity", group: "system", load: () => import("./screens/activity.jsx") },
  { id: "stats", label: "Stats", icon: "stats", group: "system", load: () => import("./screens/stats.jsx") },
  { id: "workday", label: "Workday", icon: "grid", group: "system", optional: "show_workday", load: () => import("./screens/workday.jsx") },
  { id: "providers", label: "Providers", icon: "key", group: "system", load: () => import("./screens/providers.jsx") },
  { id: "settings", label: "Settings", icon: "settings", group: "system", load: () => import("./screens/settings.jsx") },
];
const COMPONENTS = Object.fromEntries(SCREENS.map((s) => [s.id, lazy(s.load)]));
const PRIMARY_GROUPS = [["work", "Work"], ["workbench", "Workbench"]];
const SYSTEM_IDS = new Set(SCREENS.filter((s) => s.group === "system").map((s) => s.id));

function Palette(props) {
  const store = useStore();
  const labels = createMemo(() => repoLabels(store.repos() ?? []));
  const [q, setQ] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const all = createMemo(() => {
    const screenCmds = SCREENS.map((s, idx) => ({
      label: `Go to ${s.label}`,
      kbd: idx < 9 ? `${isMac() ? "⌘" : "Ctrl+"}${idx + 1}` : undefined,
      run: () => go(s.id),
    }));
    const repoCmds = [
      { label: "Switch workspace: All Workspaces", run: () => setActiveRepoId("") },
      ...(store.repos() ?? []).map((r) => ({
        label: `Switch workspace: ${labels().get(r.id) || r.name}`,
        run: () => setActiveRepoId(r.id),
      })),
    ];
    const agentCmds = (store.agents() ?? []).map((a) => ({
      label: `Open agent: ${a.name}${a.repo_name ? ` (${a.repo_name})` : ""}`,
      run: () => go("agents", { id: a.id }),
    }));
    const registered = commands();
    const hasNewAgent = registered.some((c) => c.id === "agent.new");
    return [
      ...screenCmds,
      ...(!hasNewAgent ? [{ label: "New agent", run: () => go("agents", { spawn: true }) }] : []),
      { label: "Improve Philium…", run: () => improve() },
      ...registered,
      ...repoCmds,
      ...agentCmds,
    ];
  });
  const shown = createMemo(() => {
    const t = q().trim().toLowerCase();
    return all().filter((c) => c.label.toLowerCase().includes(t)).slice(0, 30);
  });
  const run = (c) => { props.onClose(); c?.run(); };
  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette">
        <input autofocus placeholder="Type a command, screen, workspace, or agent…" value={q()}
          onInput={(e) => { setQ(e.currentTarget.value); setSel(0); }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") { setSel((s) => Math.min(s + 1, Math.max(0, shown().length - 1))); e.preventDefault(); }
            else if (e.key === "ArrowUp") { setSel((s) => Math.max(s - 1, 0)); e.preventDefault(); }
            else if (e.key === "Enter") run(shown()[sel()]);
            else if (e.key === "Escape") props.onClose();
          }} />
        <div style={{ "max-height": "56vh", "overflow-y": "auto" }}>
          <For each={shown()}>
            {(c, i) => (
              <div class={`opt ${i() === sel() ? "on" : ""}`} onMouseEnter={() => setSel(i())} onClick={() => run(c)}>
                <span class="grow ellipsis">{c.label}</span>
                <Show when={c.kbd}><span class="kbd">{c.kbd}</span></Show>
              </div>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}

function App() {
  const store = useStore();
  const status = store.status;
  const labels = createMemo(() => repoLabels(store.repos() ?? []));
  const [palette, setPalette] = createSignal(false);
  const [showWorkday, setShowWorkday] = createSignal(false);
  const [systemOpen, setSystemOpenRaw] = createSignal(
    (localStorage.getItem("philium.sidebarSystem") ?? localStorage.getItem("philium.sidebarMore")) === "1" || SYSTEM_IDS.has(route()),
  );
  const setSystemOpen = (next) => {
    setSystemOpenRaw(next);
    localStorage.setItem("philium.sidebarSystem", next ? "1" : "0");
  };
  createEffect(() => {
    if (SYSTEM_IDS.has(route()) && !systemOpen()) setSystemOpenRaw(true);
  });
  const loadShellSettings = () => {
    rpc("app.settings_get", {}, { local: true })
      .then((s) => setShowWorkday(s?.show_workday === "true"))
      .catch(() => {});
  };
  useEvent("settings", loadShellSettings);
  const systemScreens = createMemo(() =>
    SCREENS.filter((s) => s.group === "system" && (!s.optional || (s.optional === "show_workday" && showWorkday()))),
  );
  const [attempts, setAttempts] = createSignal({});
  const [visited, setVisited] = createSignal([route()]);
  createEffect(() => {
    const r = route();
    setVisited((prev) => (prev.includes(r) ? prev : [...prev, r]));
  });
  // A checkout that was unregistered must not stay selected and silently empty every screen.
  createEffect(() => {
    const list = store.repos();
    if (store.repos.state === "ready" && activeRepoId() && !(list ?? []).some((r) => r.id === activeRepoId())) setActiveRepoId("");
  });
  const agentBadge = createMemo(() => {
    const list = workspaceAgents(store.agents() ?? [], activeRepoId(), store.repos() ?? []);
    const pending = pendingAccessByAgent(store.pendingAccess());
    const waiting = list.filter((a) => agentNeedsUser(a, pending, store.agentApprovals())).length;
    if (waiting > 0) return { count: String(waiting), warn: true, title: `${waiting} agent${waiting === 1 ? "" : "s"} waiting for approval` };
    const running = list.filter((a) => a.live && a.state === "running").length;
    if (running > 0) return { count: String(running), warn: false, title: `${running} agent${running === 1 ? "" : "s"} running` };
    return null;
  });
  const failing = () => Object.entries(failures());
  const retry = () => {
    clearFailures();
    store.refetchStatus();
    store.refetchAgents();
    store.refetchRepos();
    store.refetchProviders();
    const cur = route();
    setAttempts((prev) => ({ ...prev, [cur]: (prev[cur] ?? 0) + 1 }));
  };
  onMount(() => {
    loadShellSettings();
    const onKey = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
        return;
      }
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && /^[1-9]$/.test(e.key)) {
        if (e.target?.closest?.("input, textarea, select, [contenteditable=true]")) return;
        const target = SCREENS[Number(e.key) - 1];
        if (target) {
          e.preventDefault();
          go(target.id);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });
  return (
    <div class="app">
      <style>{`.nav-item .count[data-count]::after { content: attr(data-count); }`}</style>
      <nav class="sidebar">
        <div class="brand">
          <svg class="brand-mark" viewBox="0 0 64 64" aria-hidden="true">
            <defs>
              <linearGradient id="bm-ring" x1="0.15" y1="0.1" x2="0.85" y2="0.95"><stop offset="0" stop-color="#b4c0ff" /><stop offset="0.55" stop-color="#8b9cff" /><stop offset="1" stop-color="#4fd1a5" /></linearGradient>
            </defs>
            <circle cx="32" cy="32" r="19" fill="none" stroke="url(#bm-ring)" stroke-width="7" />
            <path d="M32 5V59" stroke="#e9ecff" stroke-width="6" stroke-linecap="round" />
          </svg>
          Philium
        </div>
        <div class="workspace-switch">
          <select
            aria-label="Active workspace repository"
            title="Scope Agents, Board, and Terminals to a repository/folder"
            onChange={(e) => setActiveRepoId(e.currentTarget.value)}
          >
            <option value="" selected={!activeRepoId()}>All Workspaces</option>
            <For each={store.repos() ?? []}>
              {(r) => <option value={r.id} title={r.path} selected={activeRepoId() === r.id}>{labels().get(r.id)}</option>}
            </For>
          </select>
        </div>
        <For each={PRIMARY_GROUPS}>
          {([g, label]) => (
            <>
              <div class="nav-section">{label}</div>
              <For each={SCREENS.filter((s) => s.group === g)}>
                {(s) => (
                  <button class={`nav-item ${route() === s.id ? "on" : ""}`} onClick={() => go(s.id)}>
                    <Icon name={s.icon} /> {s.label}
                    <Show when={s.id === "agents" && agentBadge()}>
                      {(b) => (
                        <span
                          class="count"
                          data-count={b().count}
                          title={b().title}
                          style={b().warn ? { color: "var(--strict)", "font-weight": "600" } : undefined}
                        />
                      )}
                    </Show>
                  </button>
                )}
              </For>
            </>
          )}
        </For>
        <button type="button" class="nav-group-toggle" aria-expanded={systemOpen()} onClick={() => setSystemOpen(!systemOpen())}>
          <Icon name={systemOpen() ? "chevronDown" : "chevronRight"} size={12} />
          <span class="grow">System</span>
        </button>
        <Show when={systemOpen()}>
          <For each={systemScreens()}>
            {(s) => (
              <button class={`nav-item ${route() === s.id ? "on" : ""}`} onClick={() => go(s.id)}>
                <Icon name={s.icon} /> {s.label}
              </button>
            )}
          </For>
        </Show>
        <div class="sidebar-foot">
          <div class="sidebar-foot-bar">
            <Show when={status()}>
              <span class="row grow" style={{ gap: "6px", "padding-left": "4px" }} title={status().desk?.error ?? "Gateway protected"}>
                <span class={`dot ${status().desk?.ok ? "live" : "bad"}`} />
                <span class="xs">{status().desk?.ok ? "Protected" : "Offline"}</span>
              </span>
            </Show>
            <button class="icon-btn" onClick={() => setPalette(true)} title="Command palette (⌘K)"><Icon name="search" size={14} /></button>
            <button class="icon-btn" onClick={() => improve()} title="Improve Philium"><Icon name="spark" size={14} /></button>
            <button class="icon-btn" onClick={() => { const next = !soundEnabled(); setSoundEnabled(next); if (next) playSound("done"); }} title={soundEnabled() ? "Mute sounds" : "Enable sounds"}>
              <Icon name={soundEnabled() ? "activity" : "stop"} size={13} />
            </button>
          </div>
        </div>
      </nav>
      <main class="main">
        <Show when={import.meta.env.MODE === "fixture"}>
          <div class="fixture-notice" role="status">Sample data · UI preview</div>
        </Show>
        <Show when={failing().length}>
          <div class="fail-banner" role="alert">
            <span class="ellipsis grow">Some data could not load: {cleanError(failing()[0][1])}{failing().length > 1 ? ` (+${failing().length - 1} more)` : ""}</span>
            <button class="btn sm" onClick={retry}>Retry</button>
            <button class="btn ghost sm" onClick={clearFailures} aria-label="Dismiss">Dismiss</button>
          </div>
        </Show>
        <UpdateBanner />
        <For each={visited()}>
          {(id) => (
            <div class={`screen-pane ${route() === id ? "" : "hidden"}`}>
              <ScreenVisibilityProvider visible={route() === id}>
                <Suspense fallback={<div class="loading" role="status"><span class="spinner" />Loading</div>}>
                  <For each={[attempts()[id] ?? 0]}>{() => <Guard><Dynamic component={COMPONENTS[id] ?? COMPONENTS.home} /></Guard>}</For>
                </Suspense>
              </ScreenVisibilityProvider>
            </div>
          )}
        </For>
      </main>
      <Show when={palette()}><Palette onClose={() => setPalette(false)} /></Show>
      <ToastHost />
      <ConfirmHost />
      <ImproveHost />
    </div>
  );
}

window.addEventListener("unhandledrejection", (e) => { e.preventDefault(); console.error("unhandled", e.reason); toast(cleanError(e.reason), "bad"); });
window.__philiumGo = go; // used by scripts/render for stale-id checks
render(() => <App />, document.getElementById("root"));
