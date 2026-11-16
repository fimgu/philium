// Obsidian-style canvas graph. No dependencies.
//
// Props:
//   nodes: [{id, label?, group?, weight?, ring?}]   edges: [{source, target, weight?}]
//   color: (node) => css color            hidden?: Set of groups to hide
//   selected?: id   match?: Set of ids (search hits, drawn with a ring)
//   settings?: {link, repel, center, size, thickness, arrows}  (all ~0..2, 1 = default)
//   onSelect?(node|null)  onOpen?(node)  onHover?(node|null)  api?(handle)
// The handle: {fit(ids?), zoomTo(id), zoom(f), reheat()}.
// Wheel, trackpad scroll and pinch zoom; drag background to pan,
// drag a node to move it (pinned while dragging), click selects, double click opens.
import { onMount, onCleanup, createEffect } from "solid-js";
import { wheelZoomFactor, zoomCamera } from "../lib/graph-navigation.js";

const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const hash = (s) => { let h = 2166136261; for (const c of String(s)) h = Math.imul(h ^ c.charCodeAt(0), 16777619); return (h >>> 0) / 4294967296; };

// Barnes-Hut quadtree, flat arrays rebuilt every tick.
function repulse(ns, strength, alpha) {
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (const n of ns) { if (n.x < x0) x0 = n.x; if (n.x > x1) x1 = n.x; if (n.y < y0) y0 = n.y; if (n.y > y1) y1 = n.y; }
  const size = Math.max(x1 - x0, y1 - y0) + 1;
  const root = { x0, y0, s: size, m: 0, cx: 0, cy: 0, kids: null, n: null };
  const insert = (q, n, depth) => {
    for (;;) {
      q.cx = (q.cx * q.m + n.x * n.mass) / (q.m + n.mass); q.cy = (q.cy * q.m + n.y * n.mass) / (q.m + n.mass); q.m += n.mass;
      if (!q.kids) {
        if (!q.n || depth > 24) { if (!q.n) { q.n = n; return; } q.n.next = n; n.next = null; return; }
        const o = q.n; q.n = null; q.kids = [null, null, null, null];
        const h = q.s / 2;
        const pushO = (qq) => { const i = (o.x >= qq.x0 + h ? 1 : 0) + (o.y >= qq.y0 + h ? 2 : 0); const k = qq.kids[i] = { x0: qq.x0 + (i & 1) * h, y0: qq.y0 + (i >> 1) * h, s: h, m: o.mass, cx: o.x, cy: o.y, kids: null, n: o }; return k; };
        pushO(q);
      }
      const h = q.s / 2, i = (n.x >= q.x0 + h ? 1 : 0) + (n.y >= q.y0 + h ? 2 : 0);
      if (!q.kids[i]) { q.kids[i] = { x0: q.x0 + (i & 1) * h, y0: q.y0 + (i >> 1) * h, s: h, m: n.mass, cx: n.x, cy: n.y, kids: null, n }; n.next = null; return; }
      q = q.kids[i]; depth++;
    }
  };
  for (const n of ns) { n.next = null; insert(root, n, 0); }
  const k = -strength * alpha, theta2 = 0.81;
  const stack = [];
  for (const n of ns) {
    stack.length = 0; stack.push(root);
    while (stack.length) {
      const q = stack.pop();
      let dx = q.cx - n.x, dy = q.cy - n.y, d2 = dx * dx + dy * dy;
      if (q.kids && (q.s * q.s) / (d2 || 1e-9) < theta2) {
        if (d2 < 1) d2 = 1; const f = (k * q.m) / d2; n.vx += dx * f; n.vy += dy * f; continue;
      }
      if (q.kids) { for (const c of q.kids) if (c) stack.push(c); continue; }
      for (let o = q.n; o; o = o.next) {
        if (o === n) continue;
        dx = o.x - n.x; dy = o.y - n.y; d2 = dx * dx + dy * dy;
        if (d2 < 1) { dx = (hash(n.id) - 0.5) || 0.1; dy = (hash(o.id) - 0.5) || 0.1; d2 = 1; }
        const f = (k * o.mass) / d2; n.vx += dx * f; n.vy += dy * f;
      }
    }
  }
}

export default function Graph(props) {
  let canvas, wrap, raf = 0, ro;
  let sim = { ns: [], es: [], byId: new Map(), adj: new Map(), maxW: 1, maxE: 1 };
  const cache = new Map(); // id -> {x, y} so rebuilds and local mode do not jump
  let cam = { x: 0, y: 0, k: 1 }, goal = null, alpha = 1, drag = null, hover = null, focus = 0, focusNode = null, lastClick = 0, lastClickId = null;
  const S = () => ({ link: 1, repel: 1, center: 1, size: 1, thickness: 1, arrows: false, ...(props.settings || {}) });

  const kick = () => { if (!raf) raf = requestAnimationFrame(loop); };
  const reheat = (a = 0.5) => { alpha = Math.max(alpha, a); kick(); };

  const build = () => {
    const hidden = props.hidden || new Set();
    const byId = new Map(), adj = new Map();
    const ns = [];
    for (const n of props.nodes || []) {
      if (hidden.has(n.group)) continue;
      const c = cache.get(n.id);
      const o = { ...n, w: n.weight ?? 0, x: c?.x ?? (hash(n.id) - 0.5) * 600, y: c?.y ?? (hash(n.id + "y") - 0.5) * 600, vx: 0, vy: 0, deg: 0 };
      byId.set(n.id, o); adj.set(n.id, new Set()); ns.push(o);
    }
    const es = [];
    let maxE = 1;
    for (const e of props.edges || []) {
      const s = byId.get(e.source), t = byId.get(e.target);
      if (!s || !t || s === t) continue;
      const w = e.weight ?? 1;
      es.push({ s, t, w }); s.deg++; t.deg++; adj.get(s.id).add(t.id); adj.get(t.id).add(s.id);
      if (w > maxE) maxE = w;
    }
    for (const n of ns) { if (!n.w) n.w = n.deg; n.mass = 1 + Math.sqrt(n.deg) * 0.5; }
    // Scale against the 97th percentile so one giant hub does not shrink everything else.
    const ws = ns.map((n) => n.w).sort((a, b) => a - b);
    const maxW = Math.max(1, ws[Math.floor(ws.length * 0.97)] ?? 1);
    const labelW = ws[Math.floor(ws.length * 0.9)] ?? Infinity;
    // Seed new nodes near a placed neighbour so they bloom out instead of flying in.
    for (const n of ns) if (!cache.has(n.id)) { const nb = [...adj.get(n.id)].map((i) => byId.get(i)).find((m) => cache.has(m.id)); if (nb) { n.x = nb.x + (hash(n.id) - 0.5) * 40; n.y = nb.y + (hash(n.id + "q") - 0.5) * 40; } }
    const fresh = ns.filter((n) => !cache.has(n.id)).length;
    sim = { ns, es, byId, adj, maxW, maxE, labelW };
    for (const n of ns) cache.set(n.id, n);
    alpha = fresh > ns.length * 0.5 ? 1 : Math.max(alpha, 0.35);
    if (fresh > ns.length * 0.5) {
      // Settle most of the layout before the first frame, within a small time budget.
      const t0 = performance.now();
      for (let i = 0; i < 300 && ns.length && performance.now() - t0 < 160; i++) step();
      queueMicrotask(() => fit(null, true));
    }
    kick();
  };

  const radius = (n) => (3.5 + 15 * Math.sqrt(Math.min(1.3, n.w / sim.maxW))) * S().size;

  const step = () => {
    const { ns, es } = sim, s = S();
    if (!ns.length) return;
    repulse(ns, 90 * s.repel, alpha);
    for (const { s: a, t: b, w } of es) {
      const dx = b.x + b.vx - a.x - a.vx, dy = b.y + b.vy - a.y - a.vy, d = Math.sqrt(dx * dx + dy * dy) || 1;
      const same = a.group === b.group;
      const rest = (same ? 26 : 60) + (radius(a) + radius(b)) * 0.5;
      const str = (0.5 * s.link * Math.min(1, 0.4 + Math.log2(1 + w) * 0.2)) / Math.min(a.deg, b.deg);
      const f = ((d - rest) / d) * alpha * str;
      const ba = a.deg / (a.deg + b.deg);
      b.vx -= dx * f * (1 - ba); b.vy -= dy * f * (1 - ba); a.vx += dx * f * ba; a.vy += dy * f * ba;
    }
    const c = 0.02 * s.center * alpha;
    for (const n of ns) {
      n.vx -= n.x * c; n.vy -= n.y * c;
      if (n.pin) { n.vx = n.vy = 0; continue; }
      n.vx *= 0.6; n.vy *= 0.6;
      n.x += n.vx; n.y += n.vy;
    }
    alpha += (0 - alpha) * 0.0228;
  };

  const size = () => ({ w: canvas.clientWidth, h: canvas.clientHeight });
  const toScreen = (x, y) => { const { w, h } = size(); return [w / 2 + cam.x + x * cam.k, h / 2 + cam.y + y * cam.k]; };
  const world = (e) => { const r = canvas.getBoundingClientRect(); return { x: (e.clientX - r.left - r.width / 2 - cam.x) / cam.k, y: (e.clientY - r.top - r.height / 2 - cam.y) / cam.k }; };
  const pick = (p) => {
    let best = null, bd = Infinity;
    for (const n of sim.ns) { const d = (n.x - p.x) ** 2 + (n.y - p.y) ** 2, r = Math.max(radius(n), 6 / cam.k); if (d < r * r && d < bd) { bd = d; best = n; } }
    return best;
  };

  const fit = (ids, instant) => {
    if (!canvas) return;
    const list = ids ? ids.map((i) => sim.byId.get(i)).filter(Boolean) : sim.ns;
    if (!list.length) return;
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (const n of list) { const r = radius(n); x0 = Math.min(x0, n.x - r); x1 = Math.max(x1, n.x + r); y0 = Math.min(y0, n.y - r); y1 = Math.max(y1, n.y + r); }
    const { w, h } = size();
    const k = clamp(0.88 * Math.min(w / Math.max(80, x1 - x0), h / Math.max(80, y1 - y0)), 0.05, ids ? 2.2 : 1.6);
    const g = { k, x: -((x0 + x1) / 2) * k, y: -((y0 + y1) / 2) * k };
    if (instant) cam = g; else goal = g;
    kick();
  };
  const zoomTo = (id) => { const n = sim.byId.get(id); if (!n) return; const k = Math.max(cam.k, 1.6); goal = { k, x: -n.x * k, y: -n.y * k }; kick(); };
  const zoomAt = (f, sx, sy) => {
    const { w, h } = size(); sx ??= w / 2; sy ??= h / 2;
    cam = zoomCamera(cam, f, sx - w / 2, sy - h / 2); goal = null; kick();
  };

  const draw = () => {
    const dpr = window.devicePixelRatio || 1, { w, h } = size();
    if (!w || !h) return;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr); }
    const c = canvas.getContext("2d"), s = S(), k = cam.k;
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, w, h);
    c.translate(w / 2 + cam.x, h / 2 + cam.y); c.scale(k, k);
    const color = props.color || (() => "#8b93a7");
    const sel = props.selected ? sim.byId.get(props.selected) : null;
    const fn = hover || (drag?.n) || null;
    if (fn) focusNode = fn;
    const lit = focusNode ? sim.adj.get(focusNode.id) : null;
    const isLit = (n) => !focusNode || n === focusNode || lit?.has(n.id);
    const match = props.match && props.match.size ? props.match : null;
    // Visible world rect for culling.
    const vx0 = (-w / 2 - cam.x) / k - 40, vx1 = (w / 2 - cam.x) / k + 40, vy0 = (-h / 2 - cam.y) / k - 40, vy1 = (h / 2 - cam.y) / k + 40;
    const vis = (n) => n.x > vx0 && n.x < vx1 && n.y > vy0 && n.y < vy1;

    // Edges: thin, low alpha, width and opacity follow weight.
    const thick = s.thickness / Math.sqrt(Math.max(k, 0.3));
    c.lineCap = "round";
    for (const e of sim.es) {
      if (!vis(e.s) && !vis(e.t)) continue;
      const wn = Math.log2(1 + e.w) / Math.log2(1 + sim.maxE);
      const on = focusNode && (e.s === focusNode || e.t === focusNode);
      let a = 0.07 + 0.3 * wn;
      if (focusNode) a = on ? 0.35 + 0.5 * wn * focus + 0.15 : a * (1 - 0.85 * focus);
      if (match && !(match.has(e.s.id) || match.has(e.t.id))) a *= 0.4;
      c.globalAlpha = a;
      c.strokeStyle = on ? color(focusNode) : "#9aa3b8";
      c.lineWidth = (0.6 + 2.6 * wn) * thick * (on ? 1.3 : 1);
      const mx = (e.s.x + e.t.x) / 2, my = (e.s.y + e.t.y) / 2, dx = e.t.x - e.s.x, dy = e.t.y - e.s.y;
      const bend = 0.08;
      c.beginPath(); c.moveTo(e.s.x, e.s.y); c.quadraticCurveTo(mx - dy * bend, my + dx * bend, e.t.x, e.t.y); c.stroke();
      if (s.arrows && (on || k > 1.2)) {
        const d = Math.hypot(dx, dy) || 1, r = radius(e.t) + 2, ux = dx / d, uy = dy / d, ax = e.t.x - ux * r, ay = e.t.y - uy * r, al = 5 * thick;
        c.fillStyle = c.strokeStyle; c.beginPath(); c.moveTo(ax, ay); c.lineTo(ax - ux * al - uy * al * 0.5, ay - uy * al + ux * al * 0.5); c.lineTo(ax - ux * al + uy * al * 0.5, ay - uy * al - ux * al * 0.5); c.fill();
      }
    }

    // Nodes: soft circles with a faint halo.
    for (const n of sim.ns) {
      if (!vis(n)) continue;
      const r = radius(n), on = isLit(n);
      let a = on ? 1 : 1 - 0.82 * focus;
      if (match && !match.has(n.id)) a *= 0.35;
      const col = color(n);
      c.globalAlpha = a * 0.16; c.fillStyle = col;
      c.beginPath(); c.arc(n.x, n.y, r * 1.9, 0, 6.2832); c.fill();
      c.globalAlpha = a; c.beginPath(); c.arc(n.x, n.y, r, 0, 6.2832); c.fill();
      if (n.ring) { c.strokeStyle = n.ring; c.lineWidth = 2 / k; c.beginPath(); c.arc(n.x, n.y, r + 3 / k, 0, 6.2832); c.stroke(); }
      if (n === sel || (match && match.has(n.id)) || n === fn) {
        c.globalAlpha = 1; c.strokeStyle = n === sel ? "#ffffff" : match?.has(n.id) ? "#f5d76e" : "rgba(255,255,255,.7)";
        c.lineWidth = (n === sel ? 2.2 : 1.6) / k; c.beginPath(); c.arc(n.x, n.y, r + 2.5 / k, 0, 6.2832); c.stroke();
      }
    }

    // Labels in screen space, greedy placement by importance, no overlaps.
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.font = "500 11px Inter, system-ui, sans-serif"; c.textAlign = "center"; c.textBaseline = "top";
    const order = sim.ns.filter(vis).map((n) => {
      const sr = radius(n) * k;
      let pri = sr, fade = clamp((sr - 4.5) / 3.5, 0, 1);
      if (n === sel || n === fn || match?.has(n.id)) { pri = 1e9; fade = 1; }
      else if (focusNode && lit?.has(n.id)) { pri = 1e6 + sr; fade = Math.max(fade, focus); }
      else if (n.w >= sim.labelW) fade = Math.max(fade, 1);
      return { n, pri, fade };
    }).filter((o) => o.fade > 0.02).sort((a, b) => b.pri - a.pri).slice(0, 400);
    const boxes = [];
    for (const { n, fade } of order) {
      const label = String(n.label ?? n.id);
      const tw = Math.min(180, c.measureText(label).width), [sx, sy] = toScreen(n.x, n.y), ty = sy + radius(n) * k + 4;
      const b = [sx - tw / 2 - 3, ty - 1, sx + tw / 2 + 3, ty + 14];
      if (boxes.some((o) => b[0] < o[2] && b[2] > o[0] && b[1] < o[3] && b[3] > o[1])) continue;
      boxes.push(b);
      let a = fade * (isLit(n) ? 1 : 1 - 0.85 * focus);
      if (match && !match.has(n.id)) a *= 0.4;
      c.globalAlpha = a;
      c.lineWidth = 3; c.strokeStyle = "rgba(14,15,19,.85)"; c.strokeText(label, sx, ty, 180);
      c.fillStyle = n === sel || n === fn ? "#ffffff" : "rgba(222,226,236,.92)"; c.fillText(label, sx, ty, 180);
    }
    c.globalAlpha = 1;
  };

  const loop = () => {
    raf = 0;
    if (!canvas) return;
    let busy = false;
    if (alpha > 0.004 || drag?.n) { step(); busy = true; }
    const target = hover || drag?.n ? 1 : 0;
    if (Math.abs(focus - target) > 0.01) { focus += (target - focus) * 0.2; busy = true; } else { focus = target; if (!target) focusNode = null; }
    if (goal) {
      cam = { k: cam.k + (goal.k - cam.k) * 0.18, x: cam.x + (goal.x - cam.x) * 0.18, y: cam.y + (goal.y - cam.y) * 0.18 };
      if (Math.abs(goal.k - cam.k) < 0.001 && Math.abs(goal.x - cam.x) < 0.5 && Math.abs(goal.y - cam.y) < 0.5) { cam = goal; goal = null; }
      busy = true;
    }
    draw();
    if (busy) kick();
  };

  onMount(() => {
    const down = (e) => {
      const p = world(e), n = pick(p);
      goal = null;
      drag = n ? { n, sx: e.clientX, sy: e.clientY, moved: false } : { pan: true, sx: e.clientX, sy: e.clientY, cx: cam.x, cy: cam.y, moved: false };
      canvas.setPointerCapture(e.pointerId);
      canvas.style.cursor = "grabbing";
    };
    const move = (e) => {
      if (!drag) {
        const n = pick(world(e));
        if (n !== hover) { hover = n; canvas.style.cursor = n ? "pointer" : "grab"; props.onHover?.(n || null); kick(); }
        return;
      }
      if (!drag.moved && Math.hypot(e.clientX - drag.sx, e.clientY - drag.sy) < 4) return;
      drag.moved = true;
      if (drag.n) { const p = world(e); drag.n.x = p.x; drag.n.y = p.y; drag.n.pin = true; reheat(0.25); }
      else { cam = { ...cam, x: drag.cx + e.clientX - drag.sx, y: drag.cy + e.clientY - drag.sy }; kick(); }
    };
    const up = () => {
      if (drag && !drag.moved) {
        const now = performance.now();
        if (drag.n && drag.n.id === lastClickId && now - lastClick < 320) { props.onOpen?.(drag.n); zoomTo(drag.n.id); }
        else props.onSelect?.(drag.n || null);
        lastClick = drag.n ? now : 0;
        lastClickId = drag.n?.id ?? null;
      }
      if (drag?.n) drag.n.pin = false;
      drag = null; canvas.style.cursor = hover ? "pointer" : "grab"; kick();
    };
    const leave = () => { if (!drag && hover) { hover = null; props.onHover?.(null); kick(); } };
    const wheel = (e) => {
      e.preventDefault();
      const r = canvas.getBoundingClientRect(), sx = e.clientX - r.left, sy = e.clientY - r.top;
      // No device guessing: scrolling the graph always zooms at the pointer.
      zoomAt(wheelZoomFactor(e, r.height), sx, sy);
    };
    canvas.addEventListener("pointerdown", down); canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", up); canvas.addEventListener("pointerleave", leave);
    canvas.addEventListener("wheel", wheel, { passive: false });
    ro = new ResizeObserver(() => kick()); ro.observe(canvas);
    props.api?.({ fit: (ids) => fit(ids), zoomTo, zoom: (f) => zoomAt(f), reheat: () => reheat(0.6) });
    onCleanup(() => {
      cancelAnimationFrame(raf); raf = 0; ro.disconnect();
      canvas.removeEventListener("pointerdown", down); canvas.removeEventListener("pointermove", move);
      canvas.removeEventListener("pointerup", up); canvas.removeEventListener("pointerleave", leave); canvas.removeEventListener("wheel", wheel);
      canvas = null;
    });
  });
  createEffect(() => { props.nodes; props.edges; props.hidden; build(); });
  createEffect(() => { const s = props.settings; if (s) { s.link; s.repel; s.center; s.size; s.thickness; s.arrows; } reheat(0.4); });
  createEffect(() => { props.selected; props.match; props.color; kick(); });

  return <div ref={wrap} class="pgraph"><canvas ref={canvas} style={{ width: "100%", height: "100%", display: "block", cursor: "grab", "touch-action": "none" }} /></div>;
}
