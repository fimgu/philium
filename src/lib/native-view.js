// Keeps one native child webview glued to a DOM box. Native webviews float above
// all DOM, so we hide it whenever an overlay intersects the box, the box is
// detached or invisible, or `want()` returns null. A rAF loop catches
// position-only moves (sidebar collapse, split resize) that ResizeObserver misses.
import { rpc } from "./rpc.js";

export const OVERLAYS = ".overlay, .palette, .board-drawer-backdrop, [role=dialog], [role=menu], [role=listbox], .menu, .popover, .toast";

/**
 * @param {() => HTMLElement|undefined} box  element the webview should cover
 * @param {() => string|null} want  tab id that should be visible right now
 * @param {(covered: boolean) => void} [onCovered]
 * @param {(err: string, tab: string) => void} [onError]
 * @returns {() => void} stop (hides the shown tab)
 */
export function nativeView(box, want, onCovered, onError) {
  let shown = null, last = "", raf = 0, alive = true, covered = false;
  const hide = (id) => rpc("browser.hide", { tab: id }).catch(() => {});
  const tick = () => {
    if (!alive) return;
    const el = box();
    let id = null, r = null;
    if (el && el.isConnected && !document.hidden) {
      r = el.getBoundingClientRect();
      let hit = false;
      for (const o of document.querySelectorAll(OVERLAYS)) {
        if (el.contains(o)) continue;
        const b = o.getBoundingClientRect();
        if (b.width && b.height && b.left < r.right && b.right > r.left && b.top < r.bottom && b.bottom > r.top) { hit = true; break; }
      }
      if (hit !== covered) { covered = hit; onCovered?.(hit); }
      if (!hit && r.width > 2 && r.height > 2) id = want();
    }
    if (shown && shown !== id) { hide(shown); shown = null; last = ""; }
    if (id) {
      const b = `${id}:${r.left}:${r.top}:${r.width}:${r.height}`;
      if (b !== last) {
        last = b;
        rpc("browser.set_bounds", { tab: id, x: r.left + 1, y: r.top + 1, w: r.width - 2, h: r.height - 2 }).catch((e) => onError?.(String(e), id));
      }
      if (shown !== id) { shown = id; rpc("browser.show", { tab: id }).catch((e) => onError?.(String(e), id)); }
    }
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick);
  return () => { alive = false; cancelAnimationFrame(raf); if (shown) hide(shown); shown = null; };
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
