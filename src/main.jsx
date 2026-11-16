// Philium shell: sidebar, screen router, command palette, global hosts.
// Screens register in SCREENS below; each lives in src/screens/<id>.jsx and
// default-exports a component. Adding a screen = one line here.
import { render } from "solid-js/web";
import { For, Show, createSignal, createMemo, lazy, Suspense, onMount, onCleanup } from "solid-js";
import { Dynamic } from "solid-js/web";
import { ToastHost, ConfirmHost } from "./ui/kit.jsx";
import { ImproveHost } from "./ui/improve.jsx";
import { Icon } from "./ui/icons.jsx";
import { rpc } from "./lib/rpc.js";
import { route, go, commands, improve } from "./lib/nav.js";
import "./styles/tokens.css";
import "./styles/components.css";

const SCREENS = [
  { id: "home", label: "Home", icon: "home", group: "work", load: () => import("./screens/home.jsx") },
  { id: "agents", label: "Agents", icon: "agents", group: "work", load: () => import("./screens/agents.jsx") },
  { id: "board", label: "Board", icon: "board", group: "work", load: () => import("./screens/board.jsx") },
  { id: "chat", label: "Chat", icon: "chat", group: "work", load: () => import("./screens/chat.jsx") },
  { id: "terminals", label: "Terminals", icon: "terminal", group: "tools", load: () => import("./screens/terminals.jsx") },
  { id: "browser", label: "Browser", icon: "globe", group: "tools", load: () => import("./screens/browser.jsx") },
  { id: "repos", label: "Repos", icon: "repo", group: "tools", load: () => import("./screens/repos.jsx") },
  { id: "activity", label: "Activity", icon: "activity", group: "watch", load: () => import("./screens/activity.jsx") },
  { id: "stats", label: "Stats", icon: "stats", group: "watch", load: () => import("./screens/stats.jsx") },
  { id: "providers", label: "Providers", icon: "key", group: "setup", load: () => import("./screens/providers.jsx") },
  { id: "settings", label: "Settings", icon: "settings", group: "setup", load: () => import("./screens/settings.jsx") },
];
const COMPONENTS = Object.fromEntries(SCREENS.map((s) => [s.id, lazy(s.load)]));
const GROUPS = [["work", "Work"], ["tools", "Tools"], ["watch", "Observe"], ["setup", "Setup"]];

function Palette(props) {
  const [q, setQ] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const all = createMemo(() => [
    ...SCREENS.map((s) => ({ label: `Go to ${s.label}`, run: () => go(s.id) })),
    { label: "Improve Philium…", run: () => improve() },
    ...commands(),
  ]);
  const shown = createMemo(() => {
    const t = q().toLowerCase();
    return all().filter((c) => c.label.toLowerCase().includes(t)).slice(0, 12);
  });
  const run = (c) => { props.onClose(); c?.run(); };
  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="palette">
        <input autofocus placeholder="Type a command or screen…" value={q()}
          onInput={(e) => { setQ(e.currentTarget.value); setSel(0); }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") { setSel((s) => Math.min(s + 1, shown().length - 1)); e.preventDefault(); }
            else if (e.key === "ArrowUp") { setSel((s) => Math.max(s - 1, 0)); e.preventDefault(); }
            else if (e.key === "Enter") run(shown()[sel()]);
            else if (e.key === "Escape") props.onClose();
          }} />
        <For each={shown()}>
          {(c, i) => <div class={`opt ${i() === sel() ? "on" : ""}`} onMouseEnter={() => setSel(i())} onClick={() => run(c)}>{c.label}</div>}
        </For>
      </div>
    </div>
  );
}

function App() {
  const [palette, setPalette] = createSignal(false);
  const [status, setStatus] = createSignal(null);
  const refresh = () => rpc("app.status").then(setStatus).catch(() => {});
  onMount(() => {
    refresh();
    const t = setInterval(refresh, 5000);
    const onKey = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setPalette((p) => !p); }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => { clearInterval(t); window.removeEventListener("keydown", onKey); });
  });
  return (
    <div class="app">
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
        <For each={GROUPS}>
          {([g, label]) => (
            <>
              <div class="nav-section">{label}</div>
              <For each={SCREENS.filter((s) => s.group === g)}>
                {(s) => (
                  <button class={`nav-item ${route() === s.id ? "on" : ""}`} onClick={() => go(s.id)}>
                    <Icon name={s.icon} /> {s.label}
                  </button>
                )}
              </For>
            </>
          )}
        </For>
        <div class="sidebar-foot">
          <button class="nav-item" onClick={() => improve()}><Icon name="spark" /> Improve Philium</button>
          <button class="nav-item" onClick={() => setPalette(true)}><Icon name="search" /> Commands <span class="count kbd">⌘K</span></button>
          <Show when={status()}>
            <div class="row" title={status().desk?.error ?? "Harness running"}>
              <span class={`dot ${status().desk?.ok ? "live" : "bad"}`} />
              <span>{status().desk?.ok ? "Gateway protected" : "Harness offline"}</span>
            </div>
          </Show>
        </div>
      </nav>
      <main class="main">
        <Suspense>
          <Dynamic component={COMPONENTS[route()] ?? COMPONENTS.home} />
        </Suspense>
      </main>
      <Show when={palette()}><Palette onClose={() => setPalette(false)} /></Show>
      <ToastHost />
      <ConfirmHost />
      <ImproveHost />
    </div>
  );
}

render(() => <App />, document.getElementById("root"));
