// Shared reactive store: keeps agents, repos, models, providers, and per-agent
// event caches warm across screen/agent switches so navigation is instant.
import { createSignal, createResource, createRoot } from "solid-js";
import { rpc, useEvent } from "./rpc.js";
import { playSound } from "./sound.js";

const [activeRepoId, setActiveRepoIdRaw] = createSignal(localStorage.getItem("philium.activeRepo") || "");
export { activeRepoId };

export function setActiveRepoId(id) {
  const next = id || "";
  setActiveRepoIdRaw(next);
  localStorage.setItem("philium.activeRepo", next);
}

// In-memory event cache per agent id so switching agents never flashes empty/loading.
export const agentEventsCache = new Map();

let store;
export function useStore() {
  if (store) return store;
  store = createRoot(() => {
    const prevStates = new Map();
    let initialized = false;

    const [agents, { refetch: refetchAgents }] = createResource(async () => {
      const list = await rpc("agents.list").catch(() => []);
      if (initialized) {
        for (const a of list) {
          const prev = prevStates.get(a.id);
          if (prev && prev !== a.state) {
            if (a.state === "waiting") playSound("waiting");
            else if (a.state === "done" || (prev === "running" && a.state === "idle")) playSound("done");
            else if (a.state === "failed") playSound("failed");
          }
        }
      }
      prevStates.clear();
      for (const a of list) prevStates.set(a.id, a.state);
      initialized = true;
      return list;
    });

    const [repos, { refetch: refetchRepos }] = createResource(() => rpc("workspace.repos_list").catch(() => []));
    const [models, { refetch: refetchModels }] = createResource(() => rpc("providers.models", {}).catch(() => []));
    const [providers, { refetch: refetchProviders }] = createResource(() => rpc("providers.list").catch(() => []));
    const [defaultModel, { refetch: refetchDefault }] = createResource(() => rpc("providers.default").catch(() => null));

    let agTimer;
    useEvent("agents:changed", () => {
      clearTimeout(agTimer);
      agTimer = setTimeout(refetchAgents, 120);
    });
    useEvent("providers:changed", () => {
      refetchModels();
      refetchProviders();
      refetchDefault();
    });
    useEvent("workspace:changed", () => refetchRepos());

    return {
      agents,
      refetchAgents,
      repos,
      refetchRepos,
      models,
      refetchModels,
      providers,
      refetchProviders,
      defaultModel,
    };
  });
  return store;
}
