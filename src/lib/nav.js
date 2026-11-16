// Global navigation state, palette command registry and "Improve Philium".
import { createSignal } from "solid-js";

const [route, setRoute] = createSignal(localStorage.getItem("route") || "home");
const [params, setParams] = createSignal({});
export { route, params };

/** Navigate to a screen, optionally with params (e.g. {agent: "lion"}). */
export function go(id, p = {}) {
  setParams(p);
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
