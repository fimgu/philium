// One keyboard map for the shell and the Agents screen, modelled on terminal
// agent CLI habits: palette, new agent, cycle agents, split/grid panes, search.
// "Mod" is Cmd on macOS and Ctrl elsewhere.

export const SHORTCUTS = [
  { id: "palette", keys: "Mod+K", label: "Command palette", global: true },
  { id: "agent.new", keys: "Mod+N", label: "New agent", scope: "agents" },
  { id: "agent.next", keys: "Mod+]", label: "Next agent", scope: "agents" },
  { id: "agent.prev", keys: "Mod+[", label: "Previous agent", scope: "agents" },
  { id: "agent.split", keys: "Mod+D", label: "Focus view (one agent)", scope: "agents" },
  { id: "agent.grid", keys: "Mod+Shift+D", label: "Grid view (side by side)", scope: "agents" },
  { id: "pane.next", keys: "Mod+Alt+ArrowRight", label: "Focus next pane", scope: "agents" },
  { id: "pane.prev", keys: "Mod+Alt+ArrowLeft", label: "Focus previous pane", scope: "agents" },
  { id: "agent.search", keys: "Mod+F", label: "Search agents", scope: "agents" },
];

export const isMac = () => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || "");

function parse(keys) {
  const parts = keys.split("+");
  const key = parts.pop();
  return { key, mod: parts.includes("Mod"), shift: parts.includes("Shift"), alt: parts.includes("Alt") };
}

/** Return the shortcut id for a keydown-like event, or null. */
export function matchShortcut(e, mac = isMac()) {
  const mod = mac ? e.metaKey : e.ctrlKey;
  for (const s of SHORTCUTS) {
    const p = parse(s.keys);
    if (p.mod !== !!mod || p.shift !== !!e.shiftKey || p.alt !== !!e.altKey) continue;
    const k = (e.key || "").toUpperCase();
    // Letters can arrive as other glyphs on some layouts; compare by code too.
    if (k === p.key.toUpperCase() || (p.key.length === 1 && e.code === `Key${p.key}`)) return s.id;
  }
  return null;
}

/** Human label such as "⌘⇧D" or "Ctrl+Shift+D". */
export function shortcutLabel(id, mac = isMac()) {
  const s = SHORTCUTS.find((x) => x.id === id);
  if (!s) return undefined;
  const p = parse(s.keys);
  const arrows = { ArrowRight: "→", ArrowLeft: "←" };
  const key = arrows[p.key] || p.key;
  if (mac) return `${p.mod ? "⌘" : ""}${p.alt ? "⌥" : ""}${p.shift ? "⇧" : ""}${key}`;
  return [p.mod && "Ctrl", p.alt && "Alt", p.shift && "Shift", key].filter(Boolean).join("+");
}

/** Next index when cycling through n items. */
export function cycle(index, n, dir) {
  if (n <= 0) return -1;
  if (index < 0) return dir > 0 ? 0 : n - 1;
  return (index + dir + n) % n;
}
