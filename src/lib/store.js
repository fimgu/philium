// Shared reactive store: keeps agents, repos, models, providers, status, terminals,
// and per-agent event caches warm across screen/agent switches so navigation is instant.
import { createSignal, createRoot, onCleanup } from "solid-js";
import { rpc, useEvent } from "./rpc.js";
import { subscribeAgentProgress } from "./agent-progress.js";
import { playSound } from "./sound.js";

const storage =
  typeof window !== "undefined" && typeof localStorage !== "undefined" && localStorage
    ? localStorage
    : { getItem: () => "", setItem: () => {} };

const [activeRepoId, setActiveRepoIdRaw] = createSignal(storage.getItem("philium.activeRepo") || "");
export { activeRepoId };

// The profile remembers the selected checkout too, so a reopen restores it even where the
// web store is not kept. A choice made before that answer arrives wins.
let chosen = false;
if (typeof window !== "undefined") {
  rpc("app.settings_get")
    .then((s) => {
      if (!chosen && typeof s?.active_repo === "string") setActiveRepoIdRaw(s.active_repo);
    })
    .catch(() => {});
}

export function setActiveRepoId(id) {
  const next = id || "";
  chosen = true;
  setActiveRepoIdRaw(next);
  storage.setItem("philium.activeRepo", next);
  rpc("app.settings_set", { key: "active_repo", value: next }, { local: true }).catch(() => {});
}

export function createLruMap(max = 10) {
  const map = new Map();
  return {
    get(key) {
      if (!map.has(key)) return undefined;
      const value = map.get(key);
      map.delete(key);
      map.set(key, value);
      return value;
    },
    set(key, value) {
      if (map.has(key)) map.delete(key);
      map.set(key, value);
      while (map.size > max) {
        const oldest = map.keys().next().value;
        map.delete(oldest);
      }
      return this;
    },
    has(key) {
      return map.has(key);
    },
    delete(key) {
      return map.delete(key);
    },
    clear() {
      map.clear();
    },
    keys() {
      return map.keys();
    },
    values() {
      return map.values();
    },
    entries() {
      return map.entries();
    },
    forEach(fn, thisArg) {
      map.forEach(fn, thisArg);
    },
    get size() {
      return map.size;
    },
    [Symbol.iterator]() {
      return map[Symbol.iterator]();
    },
  };
}

// Bounded in-memory LRU event cache per agent id (max 10 agents).
export const agentEventsCache = createLruMap(10);

function createStoreResource(fetcher, { lazy = false } = {}) {
  const [value, setValue] = createSignal(undefined);
  const [state, setState] = createSignal("unresolved");
  const [error, setError] = createSignal(undefined);
  let current = undefined;
  let started = false;
  let seq = 0;

  const load = () => {
    started = true;
    const mySeq = ++seq;
    setState((s) => (s === "ready" || s === "refreshing" ? "refreshing" : "pending"));
    return Promise.resolve()
      .then(() => fetcher(current))
      .then((v) => {
        if (mySeq !== seq) return current;
        current = v;
        setValue(() => v);
        setError(undefined);
        setState("ready");
        return v;
      })
      .catch((e) => {
        if (mySeq !== seq) return current;
        setError(() => e);
        setState("errored");
        return current;
      });
  };

  if (!lazy) load();

  const read = () => {
    if (!started) load();
    return value();
  };

  Object.defineProperties(read, {
    loading: { get: () => state() === "pending" || state() === "refreshing" },
    state: { get: () => state() },
    error: { get: () => error() },
    latest: {
      get: () => {
        if (!started) load();
        return value();
      },
    },
  });

  const mutate = (v) => {
    started = true;
    const next = typeof v === "function" ? v(current) : v;
    current = next;
    setValue(() => next);
    setState("ready");
    return next;
  };

  return [
    read,
    {
      refetch: load,
      refetchIfStarted: () => (started ? load() : undefined),
      isStarted: () => started,
      mutate,
    },
  ];
}

let store;
export function useStore() {
  if (store) return store;
  store = createRoot(() => {
    const prevStates = new Map();
    let initialized = false;

    const [agents, { refetch: refetchAgents }] = createStoreResource(async () => {
      let ok = true;
      const list =
        (await rpc("agents.list").catch(() => {
          ok = false;
          return [];
        })) ?? [];
      if (ok) {
        const activeIds = new Set(list.map((a) => a.id));
        for (const cachedId of [...agentEventsCache.keys()]) {
          if (!activeIds.has(cachedId)) agentEventsCache.delete(cachedId);
        }
      }
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

    const [repos, { refetch: refetchRepos }] = createStoreResource(() =>
      rpc("workspace.repos_list").catch(() => []),
    );
    const [providers, { refetch: refetchProviders }] = createStoreResource(() =>
      rpc("providers.list").catch(() => []),
    );
    const [pendingAccess, { refetch: refetchPendingAccess }] = createStoreResource(() =>
      rpc("access.pending", {}, { local: true }),
    );
    const [agentApprovals, { refetch: refetchAgentApprovals }] = createStoreResource(() =>
      rpc("agents.pending_permissions", {}, { local: true }),
    );

    // Lazy slices: fetched on demand when first read so Home cold launch avoids unused RPCs.
    const [models, { refetch: refetchModels, refetchIfStarted: refetchModelsIfStarted }] =
      createStoreResource(() => rpc("providers.models", {}).catch(() => []), { lazy: true });
    const [defaultModel, { refetch: refetchDefault, refetchIfStarted: refetchDefaultIfStarted }] =
      createStoreResource(() => rpc("providers.default").catch(() => null), { lazy: true });
    const [terminals, { refetch: refetchTerminals, refetchIfStarted: refetchTerminalsIfStarted }] =
      createStoreResource(() => rpc("terminals.list").catch(() => []), { lazy: true });

    const [status, { refetch: refetchStatus, mutate: mutateStatus }] = createStoreResource(
      async (prev) => {
        try {
          return await rpc("app.status");
        } catch {
          return { ...(prev ?? {}), desk: { ok: false, error: "Backend not responding" } };
        }
      },
    );

    subscribeAgentProgress(useEvent);

    let agTimer;
    useEvent("agents:changed", () => {
      clearTimeout(agTimer);
      agTimer = setTimeout(refetchAgents, 120);
    });
    let approvalsTimer;
    const scheduleApprovals = () => {
      clearTimeout(approvalsTimer);
      approvalsTimer = setTimeout(refetchAgentApprovals, 120);
    };
    useEvent("agents:events", ({ events }) => {
      if (events?.some((event) => /^(permission\.|turn\.(cancelled|failed|finished))/.test(event.type))) scheduleApprovals();
    });
    useEvent("access:requested", refetchPendingAccess);
    useEvent("access:changed", refetchPendingAccess);

    useEvent("providers:changed", () => {
      refetchModelsIfStarted();
      refetchProviders();
      refetchDefaultIfStarted();
    });

    useEvent("workspace:changed", () => refetchRepos());

    useEvent("app:desk", (payload) => {
      if (payload?.error) {
        mutateStatus((prev) => ({ ...(prev ?? {}), desk: { ok: false, error: payload.error } }));
      }
      refetchStatus();
    });

    let termTimer;
    const scheduleTerminalsRefetch = () => {
      clearTimeout(termTimer);
      termTimer = setTimeout(() => refetchTerminalsIfStarted(), 120);
    };
    useEvent("terminals:changed", scheduleTerminalsRefetch);
    useEvent("terminals:exit", scheduleTerminalsRefetch);

    if (typeof window !== "undefined") {
      const statusTimer = setInterval(() => {
        if (typeof document === "undefined" || !document.hidden) {
          refetchStatus();
          refetchAgentApprovals();
        }
      }, 30000);
      statusTimer?.unref?.();
      onCleanup(() => clearInterval(statusTimer));
    }

    return {
      agents,
      refetchAgents,
      pendingAccess,
      agentApprovals,
      refetchAgentApprovals,
      repos,
      refetchRepos,
      models,
      refetchModels,
      providers,
      refetchProviders,
      defaultModel,
      refetchDefault,
      status,
      refetchStatus,
      mutateStatus,
      terminals,
      refetchTerminals,
    };
  });
  return store;
}
