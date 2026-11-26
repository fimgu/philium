// Philium shell: sidebar, screen router, command palette, global hosts.
// Screens register in SCREENS below; each lives in src/screens/<id>.jsx and
// default-exports a component. Adding a screen = one line here.
import { render } from "solid-js/web";
import { For, Show, createSignal, createMemo, createEffect, lazy, Suspense, onMount, onCleanup } from "solid-js";
import { Dynamic } from "solid-js/web";
import { ToastHost, ConfirmHost, Guard, Modal, toast, cleanError } from "./ui/kit.jsx";
import { ImproveHost } from "./ui/improve.jsx";
import { UpdateBanner } from "./ui/update.jsx";
import { Icon } from "./ui/icons.jsx";
import { rpc, useEvent, failures, clearFailures } from "./lib/rpc.js";
import { repoLabels } from "./lib/repo-labels.js";
import { workspaceAgents } from "./lib/agent-workspace.js";
import { agentNeedsUser, pendingAccessByAgent } from "./lib/access-helpers.js";
import { route, go, commands, improve } from "./lib/nav.js";
import { isMac, SHORTCUTS, shortcutLabel } from "./lib/shortcuts.js";
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
  { id: "memory", label: "Memory", icon: "brain", group: "work", load: () => import("./screens/memory.jsx") },
  { id: "settings", label: "Settings", icon: "settings", group: "work", load: () => import("./screens/settings.jsx") },
  { id: "code", label: "Code", icon: "graph", group: "workbench", load: () => import("./screens/code.jsx") },
  { id: "terminals", label: "Terminals", icon: "terminal", group: "workbench", load: () => import("./screens/terminals.jsx") },
  { id: "browser", label: "Browser", icon: "globe", group: "workbench", load: () => import("./screens/browser.jsx") },
  { id: "surfaces", label: "Apps", icon: "layers", group: "workbench", load: () => import("./screens/surfaces.jsx") },
  { id: "skills", label: "Skills", icon: "spark", group: "workbench", load: () => import("./screens/skills.jsx") },
  { id: "providers", label: "Providers", icon: "key", group: "system", load: () => import("./screens/providers.jsx") },
  { id: "repos", label: "Repos", icon: "repo", group: "system", load: () => import("./screens/repos.jsx") },
  { id: "stats", label: "Stats", icon: "stats", group: "system", load: () => import("./screens/stats.jsx") },
  { id: "activity", label: "Activity", icon: "activity", group: "system", load: () => import("./screens/activity.jsx") },
  { id: "environments", label: "Environments", icon: "shield", group: "system", load: () => import("./screens/environments.jsx") },
  { id: "chat", label: "Chat", icon: "chat", group: "system", load: () => import("./screens/chat.jsx") },
  { id: "workday", label: "Workday", icon: "grid", group: "system", optional: "show_workday", load: () => import("./screens/workday.jsx") },
];
const COMPONENTS = Object.fromEntries(SCREENS.map((s) => [s.id, lazy(s.load)]));
const WORK_SCREENS = SCREENS.filter((s) => s.group === "work");
const WORKBENCH_IDS = new Set(SCREENS.filter((s) => s.group === "workbench").map((s) => s.id));
const SYSTEM_IDS = new Set(SCREENS.filter((s) => s.group === "system").map((s) => s.id));

function fuzzyScore(text, query) {
  if (!query) return 1;
  const hay = text.toLowerCase();
  const q = query.toLowerCase();
  const idx = hay.indexOf(q);
  if (idx >= 0) return 100 - idx + (idx === 0 ? 40 : hay[idx - 1] === " " || hay[idx - 1] === ":" ? 20 : 0);
  let qi = 0, score = 0, prev = -1;
  for (let i = 0; i < hay.length && qi < q.length; i++) {
    if (hay[i] === q[qi]) {
      score += i === prev + 1 ? 12 : i === 0 || hay[i - 1] === " " ? 10 : 4;
      prev = i;
      qi++;
    }
  }
  return qi === q.length ? score : 0;
}

function ShortcutsModal(props) {
  const mod = isMac() ? "⌘" : "Ctrl+";
  const rows = [
    ...SHORTCUTS.map((s) => ({ label: s.label, scope: s.scope || "Global", kbd: shortcutLabel(s.id) })),
    { label: "Switch primary screens", scope: "Global", kbd: `${mod}1–6` },
    { label: "Keyboard shortcuts", scope: "Global", kbd: `${mod}/` },
    { label: "Slash commands in composer", scope: "Agents", kbd: "/" },
    { label: "Mention file or symbol", scope: "Agents", kbd: "@" },
  ];
  return (
    <Modal title="Keyboard shortcuts" onClose={props.onClose}>
      <div class="shortcuts-grid">
        <For each={rows}>
          {(r) => (
            <div class="shortcut-row">
              <span class="grow">{r.label}</span>
              <span class="xs faint">{r.scope}</span>
              <span class="kbd">{r.kbd}</span>
            </div>
          )}
        </For>
      </div>
    </Modal>
  );
}

function Palette(props) {
  const store = useStore();
  const labels = createMemo(() => repoLabels(store.repos() ?? []));
  const [q, setQ] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const mod = isMac() ? "⌘" : "Ctrl+";
  const all = createMemo(() => {
    const coreCmds = SCREENS.filter((s) => s.group === "work").map((s, idx) => ({
      group: "Navigate",
      icon: s.icon,
      label: s.label,
      kbd: `${mod}${idx + 1}`,
      run: () => go(s.id),
    }));
    const extraScreens = SCREENS.filter((s) => s.group !== "work").map((s) => ({
      group: s.group === "workbench" ? "Tools" : "System",
      icon: s.icon,
      label: s.label,
      run: () => go(s.id),
    }));
    const repoCmds = [
      { group: "Workspaces", icon: "repo", label: "All Workspaces", run: () => setActiveRepoId("") },
      ...(store.repos() ?? []).map((r) => ({
        group: "Workspaces",
        icon: "repo",
        label: labels().get(r.id) || r.name,
        run: () => setActiveRepoId(r.id),
      })),
    ];
    const agentCmds = (store.agents() ?? []).map((a) => ({
      group: "Agents",
      icon: "agents",
      label: `${a.name}${a.repo_name ? ` (${a.repo_name})` : ""}`,
      run: () => go("agents", { id: a.id }),
    }));
    const registered = commands().map((c) => ({ group: "Actions", icon: "command", ...c }));
    const hasNewAgent = registered.some((c) => c.id === "agent.new");
    const actionCmds = [
      ...(!hasNewAgent ? [{ group: "Actions", icon: "plus", label: "New agent", kbd: shortcutLabel("agent.new"), run: () => go("agents", { spawn: true }) }] : []),
      { group: "Actions", icon: "keyboard", label: "Keyboard shortcuts", kbd: `${mod}/`, run: () => props.onShortcuts?.() },
      { group: "Actions", icon: "spark", label: "Improve Philium…", run: () => improve() },
      ...registered,
    ];
    return [...actionCmds, ...coreCmds, ...agentCmds, ...repoCmds, ...extraScreens];
  });
  const shown = createMemo(() => {
    const t = q().trim();
    if (!t) return all().slice(0, 32);
    return all()
      .map((c) => ({ c, score: fuzzyScore(`${c.group} ${c.label}`, t) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 30)
      .map((x) => x.c);
  });
  const run = (c) => { props.onClose(); c?.run(); };
  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette" role="dialog" aria-modal="true" aria-label="Command palette">
        <div class="palette-head">
          <Icon name="search" size={16} />
          <input autofocus placeholder="Jump to screen, agent, workspace, or command…" value={q()}
            onInput={(e) => { setQ(e.currentTarget.value); setSel(0); }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") { setSel((s) => Math.min(s + 1, Math.max(0, shown().length - 1))); e.preventDefault(); }
              else if (e.key === "ArrowUp") { setSel((s) => Math.max(s - 1, 0)); e.preventDefault(); }
              else if (e.key === "Enter") run(shown()[sel()]);
              else if (e.key === "Escape") props.onClose();
            }} />
          <span class="kbd">ESC</span>
        </div>
        <div class="palette-list">
          <Show when={shown().length} fallback={<div class="palette-empty">No commands matching “{q()}”</div>}>
            <For each={shown()}>
              {(c, i) => {
                const showGroup = () => !q().trim() && (i() === 0 || shown()[i() - 1]?.group !== c.group);
                return (
                  <>
                    <Show when={showGroup()}><div class="palette-group">{c.group}</div></Show>
                    <div class={`opt ${i() === sel() ? "on" : ""}`} onMouseEnter={() => setSel(i())} onClick={() => run(c)}>
                      <Icon name={c.icon || "command"} size={14} />
                      <span class="grow ellipsis">{c.label}</span>
                      <Show when={q().trim()}><span class="xs faint">{c.group}</span></Show>
                      <Show when={c.kbd}><span class="kbd">{c.kbd}</span></Show>
                    </div>
                  </>
                );
              }}
            </For>
          </Show>
        </div>
        <div class="palette-foot">
          <span><span class="kbd">↑↓</span> Navigate</span>
          <span><span class="kbd">↵</span> Run</span>
          <span><span class="kbd">Esc</span> Close</span>
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
  const [shortcutsOpen, setShortcutsOpen] = createSignal(false);
  const [sidebarCollapsed, setSidebarCollapsedRaw] = createSignal(localStorage.getItem("philium.sidebarCollapsed") === "1");
  const setSidebarCollapsed = (v) => { setSidebarCollapsedRaw(v); localStorage.setItem("philium.sidebarCollapsed", v ? "1" : "0"); };
  const [theme, setThemeRaw] = createSignal(localStorage.getItem("philium.theme") || "dark");
  const setTheme = (t) => {
    setThemeRaw(t);
    localStorage.setItem("philium.theme", t);
    document.documentElement.dataset.theme = t;
  };
  createEffect(() => { document.documentElement.dataset.theme = theme(); });
  const [showWorkday, setShowWorkday] = createSignal(false);
  const [toolsOpen, setToolsOpenRaw] = createSignal(
    localStorage.getItem("philium.sidebarTools") === "1" || WORKBENCH_IDS.has(route()),
  );
  const setToolsOpen = (next) => {
    setToolsOpenRaw(next);
    localStorage.setItem("philium.sidebarTools", next ? "1" : "0");
  };
  const [systemOpen, setSystemOpenRaw] = createSignal(
    (localStorage.getItem("philium.sidebarSystem") ?? localStorage.getItem("philium.sidebarMore")) === "1" || SYSTEM_IDS.has(route()),
  );
  const setSystemOpen = (next) => {
    setSystemOpenRaw(next);
    localStorage.setItem("philium.sidebarSystem", next ? "1" : "0");
  };
  createEffect(() => {
    if (WORKBENCH_IDS.has(route()) && !toolsOpen()) setToolsOpenRaw(true);
    if (SYSTEM_IDS.has(route()) && !systemOpen()) setSystemOpenRaw(true);
  });
  const loadShellSettings = () => {
    rpc("app.settings_get", {}, { local: true })
      .then((s) => setShowWorkday(s?.show_workday === "true"))
      .catch(() => {});
  };
  useEvent("settings", loadShellSettings);
  const workbenchScreens = createMemo(() => SCREENS.filter((s) => s.group === "workbench"));
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
  const liveSidebarAgents = createMemo(() => {
    const list = workspaceAgents(store.agents() ?? [], activeRepoId(), store.repos() ?? []);
    const pending = pendingAccessByAgent(store.pendingAccess());
    return list
      .filter((a) => a.live || a.state === "waiting" || a.state === "failed")
      .sort((x, y) => {
        const xn = agentNeedsUser(x, pending, store.agentApprovals()) ? 2 : x.state === "running" ? 1 : 0;
        const yn = agentNeedsUser(y, pending, store.agentApprovals()) ? 2 : y.state === "running" ? 1 : 0;
        return yn - xn;
      })
      .slice(0, 5)
      .map((a) => ({
        ...a,
        needs: agentNeedsUser(a, pending, store.agentApprovals()),
      }));
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
      if ((e.metaKey || e.ctrlKey) && e.key === "/") {
        e.preventDefault();
        setShortcutsOpen((s) => !s);
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "\\") {
        e.preventDefault();
        setSidebarCollapsed(!sidebarCollapsed());
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
    <div class={`app ${sidebarCollapsed() ? "sidebar-collapsed" : ""}`}>
      <style>{`.nav-item .count[data-count]::after { content: attr(data-count); }`}</style>
      <Show when={sidebarCollapsed()}>
        <button type="button" class="sidebar-expand-btn" onClick={() => setSidebarCollapsed(false)} title={`Show sidebar (${isMac() ? "⌘\\" : "Ctrl+\\"})`} aria-label="Show sidebar">
          <Icon name="sidebar" size={14} />
        </button>
      </Show>
      <nav class="sidebar">
        <div class="brand">
          <svg class="brand-mark" viewBox="0 0 64 64" aria-hidden="true">
            <defs>
              <linearGradient id="bm-ring" x1="0.15" y1="0.1" x2="0.85" y2="0.95"><stop offset="0" stop-color="#b4c0ff" /><stop offset="0.55" stop-color="#8b9cff" /><stop offset="1" stop-color="#4fd1a5" /></linearGradient>
            </defs>
            <circle cx="32" cy="32" r="19" fill="none" stroke="url(#bm-ring)" stroke-width="7" />
            <path d="M32 5V59" stroke="#e9ecff" stroke-width="6" stroke-linecap="round" />
          </svg>
          <span class="grow">Philium</span>
          <button type="button" class="icon-btn" onClick={() => setSidebarCollapsed(true)} title={`Collapse sidebar (${isMac() ? "⌘\\" : "Ctrl+\\"})`} aria-label="Collapse sidebar">
            <Icon name="sidebar" size={13} />
          </button>
        </div>
        <button type="button" class="sidebar-cmd" onClick={() => setPalette(true)} title="Command palette">
          <Icon name="search" size={13} />
          <span>Command…</span>
          <span class="kbd">{isMac() ? "⌘K" : "Ctrl+K"}</span>
        </button>
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
        <div class="nav-section">Workspace</div>
        <For each={WORK_SCREENS}>
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
        <Show when={liveSidebarAgents().length > 0}>
          <div class="nav-section">Live agents</div>
          <div class="sidebar-live">
            <For each={liveSidebarAgents()}>
              {(a) => (
                <button type="button" class="sidebar-live-item" onClick={() => go("agents", { id: a.id })} title={`${a.name} · ${a.repo_name || a.state}`}>
                  <span class={`dot ${a.needs ? "warn" : a.state === "running" ? "busy" : a.state === "failed" ? "bad" : "live"}`} />
                  <span class="grow ellipsis">{a.name}</span>
                  <Show when={a.needs}><span class="xs" style={{ color: "var(--strict)", "font-weight": "600" }}>!</span></Show>
                </button>
              )}
            </For>
          </div>
        </Show>
        <button type="button" class="nav-group-toggle" aria-expanded={toolsOpen()} onClick={() => setToolsOpen(!toolsOpen())}>
          <Icon name={toolsOpen() ? "chevronDown" : "chevronRight"} size={12} />
          <span class="grow">Tools</span>
        </button>
        <Show when={toolsOpen()}>
          <For each={workbenchScreens()}>
            {(s) => (
              <button class={`nav-item ${route() === s.id ? "on" : ""}`} onClick={() => go(s.id)}>
                <Icon name={s.icon} /> {s.label}
              </button>
            )}
          </For>
        </Show>
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
            <button class="icon-btn" onClick={() => setTheme(theme() === "dark" ? "light" : "dark")} title={theme() === "dark" ? "Switch to light theme" : "Switch to dark theme"} aria-label="Toggle theme"><Icon name={theme() === "dark" ? "sun" : "moon"} size={14} /></button>
            <button class="icon-btn" onClick={() => setShortcutsOpen(true)} title={`Keyboard shortcuts (${isMac() ? "⌘/" : "Ctrl+/"})`} aria-label="Keyboard shortcuts"><Icon name="keyboard" size={14} /></button>
            <button class="icon-btn" onClick={() => improve()} title="Improve Philium" aria-label="Improve Philium"><Icon name="spark" size={14} /></button>
            <button class="icon-btn" onClick={() => { const next = !soundEnabled(); setSoundEnabled(next); if (next) playSound("done"); }} title={soundEnabled() ? "Mute sounds" : "Enable sounds"} aria-label={soundEnabled() ? "Mute sounds" : "Enable sounds"}>
              <Icon name={soundEnabled() ? "volume" : "volumeOff"} size={14} />
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
      <Show when={palette()}><Palette onClose={() => setPalette(false)} onShortcuts={() => setShortcutsOpen(true)} /></Show>
      <Show when={shortcutsOpen()}><ShortcutsModal onClose={() => setShortcutsOpen(false)} /></Show>
      <ToastHost />
      <ConfirmHost />
      <ImproveHost />
    </div>
  );
}

window.addEventListener("unhandledrejection", (e) => { e.preventDefault(); console.error("unhandled", e.reason); toast(cleanError(e.reason), "bad"); });
window.__philiumGo = go; // used by scripts/render for stale-id checks
render(() => <App />, document.getElementById("root"));
