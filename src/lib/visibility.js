import {
  createContext,
  useContext,
  createSignal,
  createMemo,
  createEffect,
  createComponent,
  getOwner,
  onCleanup,
} from "solid-js";
import { useEvent } from "./rpc.js";

const isDocHidden = () => typeof document !== "undefined" && Boolean(document.hidden);
const [docHidden, setDocHidden] = createSignal(isDocHidden());

const clocks = new Map();

function syncAllClocks() {
  const hidden = isDocHidden();
  setDocHidden(hidden);
  for (const entry of clocks.values()) {
    if (!hidden && entry.subscribers > 0) entry.setNow(Date.now());
    entry.sync();
  }
}

if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
  document.addEventListener("visibilitychange", syncAllClocks);
}

const defaultVisible = () => !docHidden() && !isDocHidden();

export const ScreenVisibilityContext = createContext(defaultVisible);

export function ScreenVisibilityProvider(props) {
  const parent = useContext(ScreenVisibilityContext) || defaultVisible;
  const visible = createMemo(() => {
    if (docHidden() || isDocHidden()) return false;
    if (!parent()) return false;
    const raw = typeof props.visible === "function" ? props.visible() : props.visible;
    return raw === undefined ? true : Boolean(raw);
  });
  return createComponent(ScreenVisibilityContext.Provider, {
    value: visible,
    get children() {
      return props.children;
    },
  });
}

export function useScreenVisible() {
  const ctx = useContext(ScreenVisibilityContext) || defaultVisible;
  return () => !docHidden() && !isDocHidden() && Boolean(ctx());
}

/**
 * Subscribe to a backend event while respecting screen visibility.
 * When visible, invokes `fn` (optionally debounced by `debounceMs`).
 * When hidden, coalesces events into a single `dirty` flag and invokes `fn` once when visible again.
 */
export function useVisibleEvent(topic, fn, { debounceMs = 0 } = {}) {
  const visible = useScreenVisible();
  let dirty = false;
  let lastPayload = {};
  let timer = null;

  const clearTimer = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  useEvent(topic, (payload) => {
    lastPayload = payload ?? {};
    if (!visible()) {
      clearTimer();
      dirty = true;
      return;
    }
    if (debounceMs > 0) {
      clearTimer();
      timer = setTimeout(() => {
        timer = null;
        if (!visible()) {
          dirty = true;
          return;
        }
        dirty = false;
        fn(lastPayload);
      }, debounceMs);
    } else {
      dirty = false;
      fn(lastPayload);
    }
  });

  createEffect(() => {
    if (visible() && dirty) {
      clearTimer();
      dirty = false;
      fn(lastPayload);
    }
  });

  if (getOwner()) {
    onCleanup(() => {
      clearTimer();
      dirty = false;
    });
  }
}

/**
 * Shared global clock signal ticking every `intervalMs`.
 * Active ONLY while there is >= 1 subscriber and `!document.hidden`.
 */
export function useClock(intervalMs = 1000) {
  const ms = Math.max(50, Number(intervalMs) || 1000);
  let entry = clocks.get(ms);
  if (!entry) {
    const [now, setNow] = createSignal(Date.now());
    entry = {
      now,
      setNow,
      subscribers: 0,
      timer: null,
      sync() {
        const shouldRun = entry.subscribers > 0 && !isDocHidden();
        if (shouldRun && entry.timer === null) {
          entry.timer = setInterval(() => {
            if (isDocHidden()) {
              entry.sync();
              return;
            }
            entry.setNow(Date.now());
          }, ms);
        } else if (!shouldRun && entry.timer !== null) {
          clearInterval(entry.timer);
          entry.timer = null;
        }
      },
    };
    clocks.set(ms, entry);
  }

  entry.subscribers += 1;
  entry.sync();

  if (getOwner()) {
    onCleanup(() => {
      entry.subscribers = Math.max(0, entry.subscribers - 1);
      entry.sync();
    });
  }

  return entry.now;
}
