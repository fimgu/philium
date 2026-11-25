// Global navigation state, palette command registry and "Improve Philium".
import { createMemo, createRoot, createSignal } from "solid-js";

const [route, setRoute] = createSignal(localStorage.getItem("route") || "home");
const [params, setParams] = createSignal({});
// Screens stay mounted while hidden, so each one reads only the params it was opened with.
// Otherwise an agent id passed to Agents would be read as a task id by Board, a terminal id by Terminals, and so on.
const [byRoute, setByRoute] = createSignal({});
const scoped = new Map();
export { route, params };

/** Params from the last navigation to `id`; unaffected by navigation to other screens. */
export function paramsFor(id) {
  if (!scoped.has(id)) scoped.set(id, createRoot(() => createMemo(() => byRoute()[id] ?? {})));
  return scoped.get(id);
}

/** Navigate to a screen, optionally with params (e.g. {agent: "lion"}). */
export function go(id, p = {}) {
  setParams(p);
  setByRoute((m) => ({ ...m, [id]: p }));
  setRoute(id);
  localStorage.setItem("route", id);
}

const [cmds, setCmds] = createSignal([]);
export const commands = cmds;
/** Register palette commands for the lifetime of a screen. Returns unregister. */
export function registerCommands(list) {
  setCmds((c) => [...c, ...list]);
  return () => setCmds((c) => c.filter((x) => !list.includes(x)));
}

// "Improve Philium" opens a modal owned by the selfdev screen code.
const [improveOpen, setImproveOpen] = createSignal(null);
export { improveOpen };
export function improve(context = {}) {
  setImproveOpen({ screen: route(), ...context });
}
export function closeImprove() {
  setImproveOpen(null);
}
