// Lightweight canvas force-layout graph. No dependencies.
//
// Props:
//   nodes: [{id, label?, group?, kind?}]
//   edges: [{source, target}]
//   height?: number (default 420)       selected?: node id
//   onSelect?: (node|null) => void      onHover?: (node|null) => void
// Drag a node to pin it, wheel to zoom, drag background to pan, click to select.
import { onMount, onCleanup, createEffect } from "solid-js";

const PALETTE = ["#6e9bff", "#4cc38a", "#f5a524", "#e5484d", "#b07cff", "#2cc4d8", "#e879c6", "#9ca3af"];
const hash = (s) => { let h = 0; for (const c of String(s ?? "")) h = (h * 31 + c.charCodeAt(0)) | 0; return Math.abs(h); };

export default function Graph(props) {
  let canvas, raf, sim = null, cam = { x: 0, y: 0, k: 1 }, alpha = 1, drag = null, hover = null;

  const build = () => {
    const idx = new Map();
    const ns = (props.nodes || []).map((n, i) => {
      const a = (i / Math.max(1, props.nodes.length)) * Math.PI * 2, r = 40 + Math.sqrt(i) * 14;
      const o = { ...n, x: Math.cos(a) * r, y: Math.sin(a) * r, vx: 0, vy: 0, deg: 0, color: PALETTE[hash(n.group) % PALETTE.length] };
      idx.set(n.id, o);
      return o;
    });
    const es = [];
    for (const e of props.edges || []) {
      const s = idx.get(e.source), t = idx.get(e.target);
      if (s && t) { es.push({ s, t }); s.deg++; t.deg++; }
    }
    sim = { ns, es };
    alpha = 1;
  };

  const step = () => {
    const { ns, es } = sim;
    const n = ns.length;
    // Repulsion on a coarse grid so large graphs stay cheap.
    const cell = 120, g = new Map();
    for (const a of ns) { const k = `${Math.floor(a.x / cell)},${Math.floor(a.y / cell)}`; (g.get(k) || g.set(k, []).get(k)).push(a); }
    for (const a of ns) {
      const cx = Math.floor(a.x / cell), cy = Math.floor(a.y / cell);
      for (let i = -1; i <= 1; i++) for (let j = -1; j <= 1; j++) for (const b of g.get(`${cx + i},${cy + j}`) || []) {
        if (a === b) continue;
        let dx = a.x - b.x, dy = a.y - b.y, d2 = dx * dx + dy * dy || 0.01;
        if (d2 > cell * cell) continue;
        const f = 900 / d2; a.vx += dx * f * 0.05; a.vy += dy * f * 0.05;
      }
    }
    for (const { s, t } of es) {
      const dx = t.x - s.x, dy = t.y - s.y, d = Math.sqrt(dx * dx + dy * dy) || 0.01, f = (d - 40) * 0.02;
      s.vx += (dx / d) * f; s.vy += (dy / d) * f; t.vx -= (dx / d) * f; t.vy -= (dy / d) * f;
    }
    for (const a of ns) {
      a.vx -= a.x * 0.004; a.vy -= a.y * 0.004;
      if (a.pin) { a.vx = a.vy = 0; continue; }
      a.vx *= 0.82; a.vy *= 0.82;
      a.x += a.vx * alpha; a.y += a.vy * alpha;
    }
    alpha = Math.max(0.02, alpha * 0.985);
    return n;
  };

  const draw = () => {
    if (!canvas || !sim) return;
    const dpr = window.devicePixelRatio || 1, w = canvas.clientWidth, h = canvas.clientHeight;
    if (canvas.width !== w * dpr || canvas.height !== h * dpr) { canvas.width = w * dpr; canvas.height = h * dpr; }
    const c = canvas.getContext("2d");
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, w, h);
    c.translate(w / 2 + cam.x, h / 2 + cam.y); c.scale(cam.k, cam.k);
    c.lineWidth = 1 / cam.k; c.strokeStyle = "rgba(150,150,170,.25)";
    c.beginPath();
    for (const { s, t } of sim.es) { c.moveTo(s.x, s.y); c.lineTo(t.x, t.y); }
    c.stroke();
    for (const a of sim.ns) {
      const r = 2.5 + Math.min(7, Math.sqrt(a.deg));
      c.fillStyle = a.color; c.beginPath(); c.arc(a.x, a.y, r, 0, 7); c.fill();
      if (a.id === props.selected || a === hover) { c.strokeStyle = "#fff"; c.lineWidth = 1.5 / cam.k; c.stroke(); }
    }
    if (cam.k > 1.2 || sim.ns.length < 60) {
      c.fillStyle = "rgba(230,230,240,.8)"; c.font = `${10 / Math.max(cam.k, 0.8)}px sans-serif`;
      for (const a of sim.ns) if (a.deg > 2 || cam.k > 1.6 || sim.ns.length < 60) c.fillText(a.label ?? a.id, a.x + 6, a.y + 3);
    }
  };

  const loop = () => { if (sim) { if (alpha > 0.03 || drag) step(); draw(); } raf = requestAnimationFrame(loop); };

  const world = (e) => {
    const r = canvas.getBoundingClientRect();
    return { x: (e.clientX - r.left - r.width / 2 - cam.x) / cam.k, y: (e.clientY - r.top - r.height / 2 - cam.y) / cam.k };
  };
  const pick = (p) => sim?.ns.find((a) => (a.x - p.x) ** 2 + (a.y - p.y) ** 2 < (8 / cam.k) ** 2);

  onMount(() => {
    build();
    const down = (e) => {
      const p = world(e), n = pick(p);
      drag = n ? { n, moved: false } : { pan: true, sx: e.clientX, sy: e.clientY, cx: cam.x, cy: cam.y, moved: false };
      canvas.setPointerCapture(e.pointerId);
    };
    const move = (e) => {
      if (!drag) { const n = pick(world(e)); if (n !== hover) { hover = n; props.onHover?.(n || null); } return; }
      drag.moved = true;
      if (drag.n) { const p = world(e); drag.n.x = p.x; drag.n.y = p.y; drag.n.pin = true; alpha = Math.max(alpha, 0.3); }
      else { cam.x = drag.cx + e.clientX - drag.sx; cam.y = drag.cy + e.clientY - drag.sy; }
    };
    const up = () => {
      if (drag && !drag.moved) props.onSelect?.(drag.n || null);
      if (drag?.n) drag.n.pin = false;
      drag = null;
    };
    const wheel = (e) => { e.preventDefault(); cam.k = Math.min(6, Math.max(0.15, cam.k * (e.deltaY < 0 ? 1.1 : 0.9))); };
    canvas.addEventListener("pointerdown", down); canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", up); canvas.addEventListener("wheel", wheel, { passive: false });
    raf = requestAnimationFrame(loop);
    onCleanup(() => { cancelAnimationFrame(raf); canvas.removeEventListener("pointerdown", down); canvas.removeEventListener("pointermove", move); canvas.removeEventListener("pointerup", up); canvas.removeEventListener("wheel", wheel); });
  });
  createEffect(() => { props.nodes; props.edges; if (canvas) build(); });

  return <canvas ref={canvas} style={{ width: "100%", height: `${props.height ?? 420}px`, display: "block", cursor: "grab", "touch-action": "none" }} />;
}
