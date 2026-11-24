// Keeps one native child webview glued to a DOM box. Native webviews float above
// all DOM, so we hide it whenever an overlay intersects the box, the box is
// detached or invisible, or `want()` returns null. Driven by observers and
// reactive signals instead of a continuous 60-120fps requestAnimationFrame loop.
import { createEffect, createRoot, getOwner, onCleanup } from "solid-js";
import { rpc } from "./rpc.js";

export const OVERLAYS =
  ".overlay, .palette, .board-drawer-backdrop, [role=dialog], [role=menu], [role=listbox], .menu, .popover, .toast";

/**
 * @param {() => HTMLElement|undefined} box  element the webview should cover
 * @param {() => string|null} want  tab id that should be visible right now
 * @param {(covered: boolean) => void} [onCovered]
 * @param {(err: string, tab: string) => void} [onError]
 * @param {() => boolean} [active]  reactive accessor; hides & disconnects observers when false
 * @returns {() => void} stop (hides the shown tab and cleans up observers)
 */
export function nativeView(box, want, onCovered, onError, active = () => true) {
  let shown = null;
  let last = "";
  let raf = 0;
  let poll = 0;
  let alive = true;
  let covered = false;
  let ro = null;
  let mo = null;
  let listening = false;
  let observedBox = null;
  let observedParent = null;
  let disposeRoot = null;

  // Tauri runs async commands concurrently, so independent set_bounds/show/hide calls
  // can land out of order and leave the webview at stale coordinates (drawn over the
  // page). Send them strictly one at a time and always send the latest desired state.
  let queue = Promise.resolve();
  const send = (method, args) => {
    const p = queue.then(() => rpc(method, args));
    queue = p.catch(() => {});
    return p;
  };
  let boundsInFlight = false;
  let pendingBounds = null;
  const pushBounds = (args) => {
    pendingBounds = args;
    if (boundsInFlight) return;
    boundsInFlight = true;
    const next = () => {
      const a = pendingBounds;
      pendingBounds = null;
      if (!a) { boundsInFlight = false; return; }
      send("browser.set_bounds", a)
        .catch((e) => { last = ""; onError?.(String(e), a.tab); })
        .finally(next);
    };
    next();
  };
  const hide = (id) => {
    if (pendingBounds?.tab === id) pendingBounds = null;
    return send("browser.hide", { tab: id }).catch(() => {});
  };

  const isDocHidden = () => typeof document !== "undefined" && Boolean(document.hidden);
  const isActive = () => {
    if (!alive || isDocHidden()) return false;
    return Boolean(typeof active === "function" ? active() : active);
  };

  const cancelRaf = () => {
    if (raf && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(raf);
    }
    raf = 0;
  };

  const scheduleCheck = () => {
    if (!alive || raf) return;
    if (typeof requestAnimationFrame === "function") {
      raf = requestAnimationFrame(check);
    } else {
      check();
    }
  };

  const syncObservedBox = (el) => {
    if (!ro) return;
    const nextBox = el && el.isConnected ? el : null;
    const nextParent = nextBox ? nextBox.offsetParent || nextBox.parentElement || null : null;
    if (nextBox === observedBox && nextParent === observedParent) return;
    ro.disconnect();
    observedBox = nextBox;
    observedParent = nextParent;
    if (typeof document !== "undefined" && document.documentElement) {
      try {
        ro.observe(document.documentElement);
      } catch {}
    }
    if (observedBox) {
      try {
        ro.observe(observedBox);
      } catch {}
    }
    if (observedParent && observedParent !== observedBox) {
      try {
        ro.observe(observedParent);
      } catch {}
    }
  };

  const connectObservers = () => {
    if (listening) return;
    listening = true;
    if (typeof ResizeObserver === "function") {
      ro = new ResizeObserver(scheduleCheck);
      syncObservedBox(box?.());
    }
    if (typeof MutationObserver === "function" && typeof document !== "undefined" && document.body) {
      mo = new MutationObserver(scheduleCheck);
      try {
        mo.observe(document.body, {
          childList: true,
          subtree: true,
          characterData: true,
          attributes: true,
          attributeFilter: ["class", "style", "hidden", "open", "role"],
        });
      } catch {}
    }
    if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
      window.addEventListener("resize", scheduleCheck, { passive: true });
      window.addEventListener("scroll", scheduleCheck, { capture: true, passive: true });
      window.addEventListener("focus", scheduleCheck);
      window.addEventListener("transitionend", scheduleCheck, { capture: true, passive: true });
      window.addEventListener("animationend", scheduleCheck, { capture: true, passive: true });
    }
    // Safety net: a box can move without resizing (e.g. a transition above it), which
    // no observer reports. A 1 Hz recheck is cheap and only sends IPC on change.
    if (typeof setInterval === "function") poll = setInterval(scheduleCheck, 1000);
  };

  const disconnectObservers = () => {
    cancelRaf();
    if (!listening) return;
    listening = false;
    if (ro) {
      ro.disconnect();
      ro = null;
    }
    observedBox = null;
    observedParent = null;
    if (mo) {
      mo.disconnect();
      mo = null;
    }
    if (typeof window !== "undefined" && typeof window.removeEventListener === "function") {
      window.removeEventListener("resize", scheduleCheck);
      window.removeEventListener("scroll", scheduleCheck, { capture: true });
      window.removeEventListener("focus", scheduleCheck);
      window.removeEventListener("transitionend", scheduleCheck, { capture: true });
      window.removeEventListener("animationend", scheduleCheck, { capture: true });
    }
    if (poll) {
      clearInterval(poll);
      poll = 0;
    }
  };

  function check() {
    raf = 0;
    if (!alive) return;
    if (!isActive()) {
      disconnectObservers();
      if (shown) {
        hide(shown);
        shown = null;
        last = "";
      }
      return;
    }
    connectObservers();
    const el = box?.();
    syncObservedBox(el);
    let id = null;
    let r = null;
    if (el && el.isConnected && !isDocHidden()) {
      r = el.getBoundingClientRect();
      let hit = false;
      if (typeof document !== "undefined" && typeof document.querySelectorAll === "function") {
        for (const o of document.querySelectorAll(OVERLAYS)) {
          if (el.contains?.(o)) continue;
          const b = o.getBoundingClientRect();
          if (
            b.width &&
            b.height &&
            b.left < r.right &&
            b.right > r.left &&
            b.top < r.bottom &&
            b.bottom > r.top
          ) {
            hit = true;
            break;
          }
        }
      }
      if (hit !== covered) {
        covered = hit;
        onCovered?.(hit);
      }
      if (!hit && r.width > 2 && r.height > 2) id = want?.() ?? null;
    }
    if (shown && shown !== id) {
      hide(shown);
      shown = null;
      last = "";
    }
    if (id && r) {
      const b = `${id}:${r.left}:${r.top}:${r.width}:${r.height}`;
      if (b !== last) {
        last = b;
        pushBounds({
          tab: id,
          x: r.left + 1,
          y: r.top + 1,
          w: r.width - 2,
          h: r.height - 2,
        });
      }
      if (shown !== id) {
        shown = id;
        // Queued after the bounds above, so the page never flashes at old coordinates.
        send("browser.show", { tab: id }).catch((e) => onError?.(String(e), id));
      }
    }
  }

  const onVisibilityChange = () => {
    if (!alive) return;
    check();
  };

  if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
    document.addEventListener("visibilitychange", onVisibilityChange);
  }

  const setupReactive = () => {
    createEffect(() => {
      const act = typeof active === "function" ? active() : active;
      const el = box?.();
      const targetId = act ? want?.() : null;
      void el;
      void targetId;
      check();
    });
  };

  if (getOwner()) {
    setupReactive();
  } else {
    try {
      createRoot((dispose) => {
        disposeRoot = dispose;
        setupReactive();
      });
    } catch {
      check();
    }
  }

  const stop = () => {
    if (!alive) return;
    alive = false;
    disconnectObservers();
    if (typeof document !== "undefined" && typeof document.removeEventListener === "function") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
    if (disposeRoot) {
      const d = disposeRoot;
      disposeRoot = null;
      d();
    }
    if (shown) hide(shown);
    shown = null;
    last = "";
  };

  if (getOwner()) {
    onCleanup(stop);
  }

  return stop;
}

/** Local dev URLs printed by a server (http://localhost:PORT, 127.0.0.1, 0.0.0.0). */
export function findLocalUrls(text) {
  const out = [];
  const re = /\bhttps?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):\d{2,5}(?:\/[^\s"'<>`)\]]*)?/g;
  // Strip ANSI color codes first; dev servers love them.
  const clean = String(text).replace(/\x1b\[[0-9;]*[A-Za-z]/g, "");
  for (const m of clean.matchAll(re)) {
    const u = m[0].replace(/[.,;:]+$/, "").replace("0.0.0.0", "localhost");
    if (!out.includes(u)) out.push(u);
  }
  return out;
}
