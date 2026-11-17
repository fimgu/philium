import { createMemo, createSignal, For, Show } from "solid-js";

function cleanLabel(raw) {
  if (!raw) return "";
  return String(raw)
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/^["'`]|["'`]$/g, "")
    .trim();
}

function parseNodeToken(raw) {
  const s = raw.trim();
  // Match id + shape wrapper
  const m =
    s.match(/^([A-Za-z0-9_.-]+)\s*\(\(\(([\s\S]*)\)\)\)$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "double-circle" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\(\(([\s\S]*)\)\)$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "circle" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\(\[([\s\S]*)\]\)$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "stadium" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\[\(([\s\S]*)\)\]$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "cylinder" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\[\[([\s\S]*)\]\]$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "subroutine" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\{\{([\s\S]*)\}\}$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "hexagon" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\{([\s\S]*)\}$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "diamond" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\(([\s\S]*)\)$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "round" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*>([\s\S]*)\]$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "asymmetric" } :
    s.match(/^([A-Za-z0-9_.-]+)\s*\[([\s\S]*)\]$/) ? { id: RegExp.$1, label: RegExp.$2, shape: "rect" } :
    null;
  if (m) return { id: m.id, label: cleanLabel(m.label) || m.id, shape: m.shape };
  const idOnly = s.match(/^([A-Za-z0-9_.-]+)$/);
  if (idOnly) return { id: idOnly[1], label: idOnly[1], shape: null };
  return null;
}

function splitChainedEdges(line) {
  // Match arrows like -->|label|, -.->|label|, ==>|label|, -- text -->, -. text .->, == text ==>, -->, ---, -.->, ==>
  const arrowRe = /(\s*(?:--\s*[^-|>=]+\s*-->|-\.\s*[^.|>=]+\s*\.->|==\s*[^=|>]+\s*==>|<-->|-->|-\.->|==>|--o|--x|---|-\.-|===)\s*(?:\|[^|]*\|\s*)?)/g;
  const parts = line.split(arrowRe).map((p) => p.trim()).filter(Boolean);
  if (parts.length < 3) return null;
  const segments = [];
  for (let i = 0; i + 2 < parts.length; i += 2) {
    const left = parts[i];
    const op = parts[i + 1];
    const right = parts[i + 2];
    let label = "";
    let style = "solid";
    let directed = true;
    const pipeMatch = op.match(/\|([^|]*)\|/);
    if (pipeMatch) label = cleanLabel(pipeMatch[1]);
    const inlineDash = op.match(/^--\s*(.+?)\s*-->/);
    const inlineDot = op.match(/^-\.\s*(.+?)\s*\.->/);
    const inlineThick = op.match(/^==\s*(.+?)\s*==>/);
    if (inlineDash) label = cleanLabel(inlineDash[1]);
    if (inlineDot) { label = cleanLabel(inlineDot[1]); style = "dashed"; }
    if (inlineThick) { label = cleanLabel(inlineThick[1]); style = "thick"; }
    if (op.includes("-.") || op.includes(".->")) style = "dashed";
    else if (op.includes("==")) style = "thick";
    if (op.startsWith("---") || op.startsWith("-.-") || op.startsWith("===")) directed = false;
    segments.push({ left, right, label, style, directed });
  }
  return segments;
}

export function parseMermaid(code) {
  const lines = String(code || "")
    .replace(/\r\n/g, "\n")
    .split("\n")
    .map((l) => l.replace(/%%.*$/, "").trim())
    .filter(Boolean);
  if (!lines.length) return { kind: "empty" };

  // Strip optional frontmatter
  let idx = 0;
  if (lines[0] === "---") {
    idx = 1;
    while (idx < lines.length && lines[idx] !== "---") idx++;
    idx++;
  }
  const head = lines[idx] || "";
  const body = lines.slice(idx + 1);

  if (/^(sequenceDiagram)\b/i.test(head)) {
    return parseSequence(body);
  }
  if (/^(pie)\b/i.test(head)) {
    return parsePie(head, body);
  }
  if (/^(stateDiagram(?:-v2)?)\b/i.test(head)) {
    return parseState(body);
  }
  if (/^(erDiagram)\b/i.test(head)) {
    return parseEr(body);
  }
  if (/^(classDiagram)\b/i.test(head)) {
    return parseClass(body);
  }
  if (/^(gantt)\b/i.test(head)) {
    return parseGantt(body);
  }
  if (/^(timeline)\b/i.test(head)) {
    return parseTimeline(body);
  }
  if (/^(mindmap)\b/i.test(head)) {
    return parseMindmap( code );
  }
  const flowMatch = head.match(/^(?:flowchart|graph)\s*(TD|TB|BT|LR|RL)?/i);
  if (flowMatch) {
    return parseFlowchart(flowMatch[1]?.toUpperCase() || "TD", body);
  }
  // Default: try flowchart if arrows exist
  if (lines.some((l) => /-->|-\.->|==>/.test(l))) {
    return parseFlowchart("TD", lines);
  }
  return { kind: "raw", head, lines };
}

function parseFlowchart(dir, lines) {
  const nodes = new Map();
  const edges = [];
  const subgraphs = [];
  const sgStack = [];
  const classDefs = new Map();
  const nodeClasses = new Map();

  const ensureNode = (tok) => {
    if (!tok) return null;
    const prev = nodes.get(tok.id);
    const sg = sgStack[sgStack.length - 1] || null;
    if (!prev) {
      nodes.set(tok.id, {
        id: tok.id,
        label: tok.label || tok.id,
        shape: tok.shape || "rect",
        subgraph: sg ? sg.id : null,
      });
    } else {
      if (tok.label && tok.label !== tok.id) prev.label = tok.label;
      if (tok.shape) prev.shape = tok.shape;
      if (!prev.subgraph && sg) prev.subgraph = sg.id;
    }
    return tok.id;
  };

  for (const rawLine of lines) {
    const line = rawLine.replace(/;$/, "").trim();
    if (!line) continue;
    if (/^direction\s+(TD|TB|BT|LR|RL)/i.test(line)) continue;
    const cd = line.match(/^classDef\s+(\S+)\s+(.+)$/i);
    if (cd) {
      classDefs.set(cd[1], cd[2]);
      continue;
    }
    const cl = line.match(/^class\s+([^\s]+)\s+(\S+)$/i);
    if (cl) {
      cl[1].split(",").map((s) => s.trim()).forEach((id) => nodeClasses.set(id, cl[2]));
      continue;
    }
    if (/^(style|linkStyle|click)\s+/i.test(line)) continue;

    const sgMatch = line.match(/^subgraph\s+(.+)$/i);
    if (sgMatch) {
      const rest = sgMatch[1].trim();
      const bracket = rest.match(/^([A-Za-z0-9_.-]+)\s*\[([\s\S]*)\]$/);
      const sg = bracket
        ? { id: bracket[1], title: cleanLabel(bracket[2]) }
        : { id: `sg_${subgraphs.length + 1}`, title: cleanLabel(rest) };
      subgraphs.push(sg);
      sgStack.push(sg);
      continue;
    }
    if (/^end$/i.test(line)) {
      sgStack.pop();
      continue;
    }

    const segs = splitChainedEdges(line);
    if (segs) {
      for (const seg of segs) {
        const leftGroup = seg.left.split(/\s*&\s*/);
        const rightGroup = seg.right.split(/\s*&\s*/);
        for (const lRaw of leftGroup) {
          for (const rRaw of rightGroup) {
            const lt = parseNodeToken(lRaw.replace(/:::(\S+)$/, ""));
            const rt = parseNodeToken(rRaw.replace(/:::(\S+)$/, ""));
            if (lt && rt) {
              const from = ensureNode(lt);
              const to = ensureNode(rt);
              edges.push({ from, to, label: seg.label, style: seg.style, directed: seg.directed });
            }
          }
        }
      }
      continue;
    }

    const single = parseNodeToken(line.replace(/:::(\S+)$/, ""));
    if (single) ensureNode(single);
  }

  return layoutFlowchart(dir === "TB" ? "TD" : dir, Array.from(nodes.values()), edges, subgraphs);
}

function layoutFlowchart(dir, nodeList, edges, subgraphs) {
  const horiz = dir === "LR" || dir === "RL";
  const rev = dir === "BT" || dir === "RL";
  if (!nodeList.length) return { kind: "flowchart", dir, nodes: [], edges: [], subgraphs: [], width: 320, height: 120 };

  const byId = new Map(nodeList.map((n) => [n.id, n]));
  const indeg = new Map(nodeList.map((n) => [n.id, 0]));
  const adj = new Map(nodeList.map((n) => [n.id, []]));
  for (const e of edges) {
    if (byId.has(e.from) && byId.has(e.to) && e.from !== e.to) {
      adj.get(e.from).push(e.to);
      indeg.set(e.to, (indeg.get(e.to) || 0) + 1);
    }
  }

  // Longest-path layering (cycle-safe)
  const rank = new Map();
  const queue = [];
  for (const n of nodeList) {
    if ((indeg.get(n.id) || 0) === 0) {
      rank.set(n.id, 0);
      queue.push(n.id);
    }
  }
  if (!queue.length) {
    rank.set(nodeList[0].id, 0);
    queue.push(nodeList[0].id);
  }
  const visited = new Set();
  while (queue.length) {
    const u = queue.shift();
    visited.add(u);
    const ru = rank.get(u) || 0;
    for (const v of adj.get(u) || []) {
      const nextRank = Math.min(nodeList.length, ru + 1);
      if ((rank.get(v) ?? -1) < nextRank && !visited.has(v)) {
        rank.set(v, nextRank);
      }
      indeg.set(v, (indeg.get(v) || 1) - 1);
      if ((indeg.get(v) || 0) <= 0 && !visited.has(v) && !queue.includes(v)) {
        queue.push(v);
      }
    }
  }
  for (const n of nodeList) {
    if (!rank.has(n.id)) rank.set(n.id, 0);
  }

  const layers = new Map();
  let maxRank = 0;
  for (const n of nodeList) {
    const r = rank.get(n.id) || 0;
    if (r > maxRank) maxRank = r;
    if (!layers.has(r)) layers.set(r, []);
    layers.get(r).push(n);
  }

  // Size each node based on text lines
  for (const n of nodeList) {
    const lines = String(n.label || n.id).split("\n");
    const longest = Math.max(...lines.map((l) => l.length), 4);
    n.lines = lines;
    n.w = Math.max(108, Math.min(260, longest * 7.6 + 34));
    n.h = Math.max(40, lines.length * 18 + 20);
    if (n.shape === "diamond") {
      n.w = Math.max(n.w + 24, 124);
      n.h = Math.max(n.h + 14, 54);
    } else if (n.shape === "circle" || n.shape === "double-circle") {
      const d = Math.max(56, Math.min(110, longest * 7.2 + 24));
      n.w = d;
      n.h = d;
    }
  }

  const padX = 36;
  const padY = 36;
  const rankGap = horiz ? 195 : 96;
  const siblingGap = horiz ? 68 : 165;

  let maxLayerSpan = 0;
  for (let r = 0; r <= maxRank; r++) {
    const arr = layers.get(r) || [];
    const span = arr.reduce((acc, n) => acc + (horiz ? n.h : n.w), 0) + Math.max(0, arr.length - 1) * 28;
    if (span > maxLayerSpan) maxLayerSpan = span;
  }
  maxLayerSpan = Math.max(maxLayerSpan, 240);

  for (let r = 0; r <= maxRank; r++) {
    const arr = layers.get(r) || [];
    const effRank = rev ? maxRank - r : r;
    const totalSpan = arr.reduce((acc, n) => acc + (horiz ? n.h : n.w), 0) + Math.max(0, arr.length - 1) * 28;
    let cursor = padX + (maxLayerSpan - totalSpan) / 2;
    for (const n of arr) {
      const span = horiz ? n.h : n.w;
      if (horiz) {
        n.x = padX + effRank * rankGap + n.w / 2;
        n.y = cursor + span / 2;
      } else {
        n.x = cursor + span / 2;
        n.y = padY + effRank * rankGap + n.h / 2;
      }
      cursor += span + 28;
    }
  }

  const sgBoxes = [];
  for (const sg of subgraphs) {
    const members = nodeList.filter((n) => n.subgraph === sg.id);
    if (!members.length) continue;
    const minX = Math.min(...members.map((m) => m.x - m.w / 2)) - 16;
    const maxX = Math.max(...members.map((m) => m.x + m.w / 2)) + 16;
    const minY = Math.min(...members.map((m) => m.y - m.h / 2)) - 26;
    const maxY = Math.max(...members.map((m) => m.y + m.h / 2)) + 14;
    sgBoxes.push({ id: sg.id, title: sg.title, x: minX, y: minY, w: maxX - minX, h: maxY - minY });
  }

  const width = Math.max(320, ...nodeList.map((n) => n.x + n.w / 2 + padX), ...sgBoxes.map((b) => b.x + b.w + padX));
  const height = Math.max(120, ...nodeList.map((n) => n.y + n.h / 2 + padY), ...sgBoxes.map((b) => b.y + b.h + padY));

  const routedEdges = edges
    .map((e) => {
      const a = byId.get(e.from);
      const b = byId.get(e.to);
      if (!a || !b) return null;
      let x1 = a.x, y1 = a.y, x2 = b.x, y2 = b.y;
      if (horiz) {
        const dirSign = b.x >= a.x ? 1 : -1;
        x1 = a.x + dirSign * (a.w / 2);
        x2 = b.x - dirSign * (b.w / 2);
      } else {
        const dirSign = b.y >= a.y ? 1 : -1;
        y1 = a.y + dirSign * (a.h / 2);
        y2 = b.y - dirSign * (b.h / 2);
      }
      const mx = (x1 + x2) / 2;
      const my = (y1 + y2) / 2;
      const d = horiz
        ? `M ${x1} ${y1} C ${mx} ${y1}, ${mx} ${y2}, ${x2} ${y2}`
        : `M ${x1} ${y1} C ${x1} ${my}, ${x2} ${my}, ${x2} ${y2}`;
      return { ...e, d, lx: mx, ly: my };
    })
    .filter(Boolean);

  return { kind: "flowchart", dir, nodes: nodeList, edges: routedEdges, subgraphs: sgBoxes, width, height };
}

function parseSequence(lines) {
  const participants = [];
  const pMap = new Map();
  const events = [];

  const ensureP = (id, label = null, actor = false) => {
    const cleanId = id.trim();
    if (!pMap.has(cleanId)) {
      const p = { id: cleanId, label: cleanLabel(label || cleanId), actor, idx: participants.length };
      pMap.set(cleanId, p);
      participants.push(p);
    } else if (label) {
      pMap.get(cleanId).label = cleanLabel(label);
    }
    return pMap.get(cleanId);
  };

  for (const raw of lines) {
    const line = raw.trim();
    if (!line || /^autonumber\b/i.test(line)) continue;
    const pm = line.match(/^(participant|actor)\s+([^\s]+?)(?:\s+as\s+(.+))?$/i);
    if (pm) {
      ensureP(pm[2], pm[3] || pm[2], pm[1].toLowerCase() === "actor");
      continue;
    }
    const note = line.match(/^Note\s+(left of|right of|over)\s+([^:]+):\s*(.+)$/i);
    if (note) {
      const targets = note[2].split(",").map((s) => ensureP(s.trim()).id);
      events.push({ type: "note", pos: note[1].toLowerCase(), targets, text: cleanLabel(note[3]) });
      continue;
    }
    const blockStart = line.match(/^(loop|alt|opt|par|critical|rect)\s*(.*)$/i);
    if (blockStart) {
      events.push({ type: "block", op: blockStart[1].toUpperCase(), text: cleanLabel(blockStart[2]) });
      continue;
    }
    const elseLine = line.match(/^(else|and)\s*(.*)$/i);
    if (elseLine) {
      events.push({ type: "divider", op: elseLine[1].toUpperCase(), text: cleanLabel(elseLine[2]) });
      continue;
    }
    if (/^end$/i.test(line)) {
      events.push({ type: "end" });
      continue;
    }
    const msg = line.match(/^([A-Za-z0-9_.-]+)\s*(-->>|->>|-->|->|--x|-x|--\)|-\))\s*([+-]?[A-Za-z0-9_.-]+)\s*:\s*(.*)$/);
    if (msg) {
      const from = ensureP(msg[1]).id;
      const toRaw = msg[3].replace(/^[+-]/, "");
      const to = ensureP(toRaw).id;
      const arrow = msg[2];
      events.push({
        type: "msg",
        from,
        to,
        dashed: arrow.startsWith("--"),
        text: cleanLabel(msg[4]),
      });
    }
  }

  const colW = 165;
  const padX = 76;
  const width = Math.max(360, participants.length * colW + padX * 2);
  const topY = 46;
  const rowH = 42;
  const height = topY + events.length * rowH + 64;

  participants.forEach((p, i) => {
    p.x = padX + i * colW + colW / 2 - 20;
  });

  return { kind: "sequence", participants, events, width, height, topY, rowH };
}

function parseState(lines) {
  const flowLines = [];
  for (const raw of lines) {
    const line = raw.trim();
    if (!line) continue;
    const m = line.match(/^(\[\*\]|[A-Za-z0-9_.-]+)\s*-->\s*(\[\*\]|[A-Za-z0-9_.-]+)(?:\s*:\s*(.+))?$/);
    if (m) {
      const from = m[1] === "[*]" ? "start((●))" : `${m[1]}(${m[1]})`;
      const to = m[2] === "[*]" ? "end_state(((◉)))" : `${m[2]}(${m[2]})`;
      const lbl = m[3] ? `|${cleanLabel(m[3])}|` : "";
      flowLines.push(`${from} -->${lbl} ${to}`);
      continue;
    }
    const desc = line.match(/^state\s+"([^"]+)"\s+as\s+([A-Za-z0-9_.-]+)$/i) || line.match(/^([A-Za-z0-9_.-]+)\s*:\s*(.+)$/);
    if (desc) {
      const id = desc[2] && line.toLowerCase().startsWith("state") ? desc[2] : desc[1];
      const lbl = desc[2] && line.toLowerCase().startsWith("state") ? desc[1] : `${desc[1]}\\n${desc[2]}`;
      flowLines.push(`${id}(${lbl})`);
    }
  }
  const res = parseFlowchart("TD", flowLines);
  return { ...res, subkind: "state" };
}

function parseEr(lines) {
  const flowLines = [];
  let curEntity = null;
  let attrs = [];
  const flush = () => {
    if (curEntity) {
      const body = attrs.length ? `${curEntity}<br/>${attrs.slice(0, 5).join("<br/>")}` : curEntity;
      flowLines.push(`${curEntity}[${body}]`);
      curEntity = null;
      attrs = [];
    }
  };
  for (const raw of lines) {
    const line = raw.trim();
    if (curEntity) {
      if (line === "}") { flush(); continue; }
      attrs.push(line);
      continue;
    }
    const entOpen = line.match(/^([A-Za-z0-9_.-]+)\s*\{$/);
    if (entOpen) { curEntity = entOpen[1]; attrs = []; continue; }
    const rel = line.match(/^([A-Za-z0-9_.-]+)\s+(\S+)\s+([A-Za-z0-9_.-]+)\s*:\s*(.+)$/);
    if (rel) {
      flowLines.push(`${rel[1]}[${rel[1]}] -->|${cleanLabel(rel[4])}| ${rel[3]}[${rel[3]}]`);
    }
  }
  flush();
  const res = parseFlowchart("LR", flowLines);
  return { ...res, subkind: "er" };
}

function parseClass(lines) {
  const classes = new Map();
  const rels = [];
  let cur = null;
  const ensure = (name) => {
    if (!classes.has(name)) classes.set(name, { name, members: [] });
    return classes.get(name);
  };
  for (const raw of lines) {
    const line = raw.trim();
    if (cur) {
      if (line === "}") { cur = null; continue; }
      ensure(cur).members.push(line);
      continue;
    }
    const open = line.match(/^class\s+([A-Za-z0-9_.-]+)\s*\{$/);
    if (open) { cur = open[1]; ensure(cur); continue; }
    const member = line.match(/^([A-Za-z0-9_.-]+)\s*:\s*(.+)$/);
    if (member) { ensure(member[1]).members.push(member[2]); continue; }
    const rel = line.match(/^([A-Za-z0-9_.-]+)\s*(<\|--|--\|>|\*--|o--|-->|--|\.\.>)\s*([A-Za-z0-9_.-]+)(?:\s*:\s*(.+))?$/);
    if (rel) {
      ensure(rel[1]);
      ensure(rel[3]);
      rels.push({ from: rel[1], to: rel[3], label: cleanLabel(rel[4] || "") });
    }
  }
  const flowLines = [];
  for (const c of classes.values()) {
    const label = c.members.length ? `${c.name}<br/>${c.members.slice(0, 5).join("<br/>")}` : c.name;
    flowLines.push(`${c.name}[${label}]`);
  }
  for (const r of rels) {
    flowLines.push(`${r.from} -->${r.label ? `|${r.label}|` : ""} ${r.to}`);
  }
  const res = parseFlowchart("TD", flowLines);
  return { ...res, subkind: "class" };
}

function parsePie(head, lines) {
  let title = "";
  const inlineTitle = head.match(/^pie\s+(?:showData\s+)?(?:title\s+(.+))?$/i);
  if (inlineTitle?.[1]) title = cleanLabel(inlineTitle[1]);
  const slices = [];
  for (const raw of lines) {
    const tm = raw.match(/^title\s+(.+)$/i);
    if (tm) { title = cleanLabel(tm[1]); continue; }
    const sm = raw.match(/^"([^"]+)"\s*:\s*([0-9.]+)$/);
    if (sm) slices.push({ label: sm[1], value: parseFloat(sm[2]) || 0 });
  }
  const total = slices.reduce((a, s) => a + s.value, 0) || 1;
  const palette = ["#d97757", "#4f8cff", "#38b2ac", "#eab308", "#a855f7", "#ec4899", "#22c55e"];
  let acc = 0;
  const arcs = slices.map((s, i) => {
    const frac = s.value / total;
    const a0 = acc * Math.PI * 2 - Math.PI / 2;
    acc += frac;
    const a1 = acc * Math.PI * 2 - Math.PI / 2;
    const r = 68, cx = 100, cy = 96;
    const x0 = cx + r * Math.cos(a0), y0 = cy + r * Math.sin(a0);
    const x1 = cx + r * Math.cos(a1), y1 = cy + r * Math.sin(a1);
    const large = frac > 0.5 ? 1 : 0;
    const d = slices.length === 1
      ? `M ${cx} ${cy - r} A ${r} ${r} 0 1 1 ${cx - 0.01} ${cy - r} Z`
      : `M ${cx} ${cy} L ${x0} ${y0} A ${r} ${r} 0 ${large} 1 ${x1} ${y1} Z`;
    return { ...s, pct: Math.round(frac * 100), d, color: palette[i % palette.length] };
  });
  return { kind: "pie", title, arcs, width: 420, height: 192 };
}

function parseGantt(lines) {
  let title = "";
  let section = "Tasks";
  const items = [];
  for (const raw of lines) {
    if (/^(dateFormat|axisFormat|tickInterval|excludes)\b/i.test(raw)) continue;
    const tm = raw.match(/^title\s+(.+)$/i);
    if (tm) { title = cleanLabel(tm[1]); continue; }
    const sm = raw.match(/^section\s+(.+)$/i);
    if (sm) { section = cleanLabel(sm[1]); continue; }
    const im = raw.match(/^([^:]+):\s*(.+)$/);
    if (im) {
      const meta = im[2].split(",").map((s) => s.trim());
      const state = meta.find((m) => /^(done|active|crit|milestone)$/i.test(m))?.toLowerCase() || "normal";
      items.push({ section, label: cleanLabel(im[1]), meta: meta.join(" · "), state });
    }
  }
  return { kind: "gantt", title, items, width: 520, height: Math.max(120, items.length * 32 + 54) };
}

function parseTimeline(lines) {
  let title = "";
  const steps = [];
  for (const raw of lines) {
    const tm = raw.match(/^title\s+(.+)$/i);
    if (tm) { title = cleanLabel(tm[1]); continue; }
    const parts = raw.split(":").map((s) => cleanLabel(s)).filter(Boolean);
    if (parts.length >= 2) {
      steps.push({ period: parts[0], detail: parts.slice(1).join(" · ") });
    } else if (steps.length && raw.startsWith(":")) {
      steps[steps.length - 1].detail += ` · ${cleanLabel(raw.slice(1))}`;
    }
  }
  return { kind: "timeline", title, steps };
}

function parseMindmap(code) {
  const rawLines = String(code || "").split("\n").filter((l) => l.trim() && !/^\s*mindmap\b/i.test(l));
  const flow = [];
  const stack = [];
  rawLines.forEach((line, i) => {
    const indent = line.match(/^\s*/)[0].length;
    const text = cleanLabel(line.trim().replace(/^root\(\((.*)\)\)$/, "$1").replace(/^[\[(]+|[\])]+$/g, ""));
    const id = `mm_${i}`;
    flow.push(`${id}(${text})`);
    while (stack.length && stack[stack.length - 1].indent >= indent) stack.pop();
    if (stack.length) flow.push(`${stack[stack.length - 1].id} --> ${id}`);
    stack.push({ id, indent });
  });
  const res = parseFlowchart("LR", flow);
  return { ...res, subkind: "mindmap" };
}

function FlowShape(props) {
  const n = () => props.node;
  const x = () => n().x - n().w / 2;
  const y = () => n().y - n().h / 2;
  return (
    <g class="mermaid-node">
      {n().shape === "diamond" ? (
        <polygon
          points={`${n().x},${y()} ${x() + n().w},${n().y} ${n().x},${y() + n().h} ${x()},${n().y}`}
          class="mermaid-shape mermaid-shape-diamond"
        />
      ) : n().shape === "circle" || n().shape === "double-circle" ? (
        <>
          <circle cx={n().x} cy={n().y} r={n().w / 2} class="mermaid-shape" />
          {n().shape === "double-circle" && <circle cx={n().x} cy={n().y} r={n().w / 2 - 4} class="mermaid-shape" />}
        </>
      ) : n().shape === "stadium" ? (
        <rect x={x()} y={y()} width={n().w} height={n().h} rx={n().h / 2} class="mermaid-shape" />
      ) : n().shape === "round" ? (
        <rect x={x()} y={y()} width={n().w} height={n().h} rx="8" class="mermaid-shape" />
      ) : (
        <rect x={x()} y={y()} width={n().w} height={n().h} rx="4" class="mermaid-shape" />
      )}
      <text x={n().x} y={n().y - ((n().lines.length - 1) * 16) / 2 + 4} text-anchor="middle" class="mermaid-label">
        <For each={n().lines}>
          {(ln, idx) => (
            <tspan x={n().x} dy={idx() === 0 ? 0 : 16}>
              {ln}
            </tspan>
          )}
        </For>
      </text>
    </g>
  );
}

export function MermaidBlock(props) {
  const [view, setView] = createSignal("diagram");
  const [zoom, setZoom] = createSignal(1);
  const parsed = createMemo(() => parseMermaid(props.code));
  const badge = createMemo(() => {
    const p = parsed();
    const k = p.subkind || p.kind || "diagram";
    return `MERMAID · ${k.toUpperCase()}`;
  });

  return (
    <div class="markdown-rich markdown-mermaid">
      <div class="markdown-rich-bar">
        <span class="markdown-rich-badge">{badge()}</span>
        <div class="markdown-rich-actions">
          <Show when={view() === "diagram"}>
            <button type="button" class="markdown-rich-btn" onClick={() => setZoom((z) => Math.max(0.5, +(z - 0.15).toFixed(2)))} title="Zoom out">−</button>
            <button type="button" class="markdown-rich-btn" onClick={() => setZoom(1)} title="Reset zoom">{Math.round(zoom() * 100)}%</button>
            <button type="button" class="markdown-rich-btn" onClick={() => setZoom((z) => Math.min(2.2, +(z + 0.15).toFixed(2)))} title="Zoom in">+</button>
          </Show>
          <button
            type="button"
            class="markdown-rich-btn"
            onClick={() => setView((v) => (v === "diagram" ? "source" : "diagram"))}
          >
            {view() === "diagram" ? "Source" : "Diagram"}
          </button>
          {props.copyButton}
        </div>
      </div>

      <Show
        when={view() === "diagram" && parsed().kind !== "raw" && parsed().kind !== "empty"}
        fallback={<pre class="markdown-rich-source"><code>{props.code}</code></pre>}
      >
        <div class="mermaid-canvas">
          {parsed().kind === "flowchart" && (
            <svg
              viewBox={`0 0 ${parsed().width} ${parsed().height}`}
              style={{ width: `${Math.round(parsed().width * zoom())}px`, "max-width": zoom() === 1 ? "100%" : "none", height: "auto" }}
              class="mermaid-svg"
            >
              <defs>
                <marker id="mermaid-arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                  <path d="M 0 1 L 10 5 L 0 9 z" fill="currentColor" />
                </marker>
              </defs>
              <For each={parsed().subgraphs}>
                {(sg) => (
                  <g class="mermaid-subgraph">
                    <rect x={sg.x} y={sg.y} width={sg.w} height={sg.h} rx="6" class="mermaid-subgraph-box" />
                    <text x={sg.x + 10} y={sg.y + 16} class="mermaid-subgraph-title">{sg.title}</text>
                  </g>
                )}
              </For>
              <For each={parsed().edges}>
                {(e) => (
                  <g class="mermaid-edge">
                    <path
                      d={e.d}
                      fill="none"
                      stroke-dasharray={e.style === "dashed" ? "5,4" : undefined}
                      stroke-width={e.style === "thick" ? "2.4" : "1.5"}
                      marker-end={e.directed ? "url(#mermaid-arrow)" : undefined}
                      class="mermaid-edge-path"
                    />
                    <Show when={e.label}>
                      <rect
                        x={e.lx - (e.label.length * 3.4 + 8)}
                        y={e.ly - 10}
                        width={e.label.length * 6.8 + 16}
                        height="19"
                        rx="4"
                        class="mermaid-edge-bg"
                      />
                      <text x={e.lx} y={e.ly + 3.5} text-anchor="middle" class="mermaid-edge-label">{e.label}</text>
                    </Show>
                  </g>
                )}
              </For>
              <For each={parsed().nodes}>{(n) => <FlowShape node={n} />}</For>
            </svg>
          )}

          {parsed().kind === "sequence" && (
            <svg
              viewBox={`0 0 ${parsed().width} ${parsed().height}`}
              style={{ width: `${Math.round(parsed().width * zoom())}px`, "max-width": zoom() === 1 ? "100%" : "none", height: "auto" }}
              class="mermaid-svg"
            >
              <defs>
                <marker id="seq-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto">
                  <path d="M 0 1 L 10 5 L 0 9 z" fill="currentColor" />
                </marker>
              </defs>
              <For each={parsed().participants}>
                {(p) => (
                  <g>
                    <line x1={p.x} y1={parsed().topY + 16} x2={p.x} y2={parsed().height - 24} stroke-dasharray="4,4" class="mermaid-lifeline" />
                    <rect x={p.x - 62} y={12} width="124" height="32" rx="5" class="mermaid-shape" />
                    <text x={p.x} y={32} text-anchor="middle" class="mermaid-label">{p.label}</text>
                  </g>
                )}
              </For>
              <For each={parsed().events}>
                {(ev, i) => {
                  const y = () => parsed().topY + 34 + i() * parsed().rowH;
                  const pById = (id) => parsed().participants.find((p) => p.id === id);
                  return (
                    <g>
                      {ev.type === "msg" && (() => {
                        const a = pById(ev.from), b = pById(ev.to);
                        if (!a || !b) return null;
                        if (a.id === b.id) {
                          return (
                            <g class="mermaid-edge">
                              <path d={`M ${a.x} ${y() - 6} L ${a.x + 38} ${y() - 6} L ${a.x + 38} ${y() + 10} L ${a.x} ${y() + 10}`} fill="none" marker-end="url(#seq-arrow)" class="mermaid-edge-path" />
                              <text x={a.x + 44} y={y() + 4} class="mermaid-edge-label">{ev.text}</text>
                            </g>
                          );
                        }
                        const mx = (a.x + b.x) / 2;
                        return (
                          <g class="mermaid-edge">
                            <line
                              x1={a.x}
                              y1={y()}
                              x2={b.x}
                              y2={y()}
                              stroke-dasharray={ev.dashed ? "5,4" : undefined}
                              marker-end="url(#seq-arrow)"
                              class="mermaid-edge-path"
                            />
                            <text x={mx} y={y() - 6} text-anchor="middle" class="mermaid-edge-label">{ev.text}</text>
                          </g>
                        );
                      })()}
                      {ev.type === "note" && (() => {
                        const t = pById(ev.targets[0]) || parsed().participants[0];
                        const nx = t ? t.x - 80 : 40;
                        return (
                          <g>
                            <rect x={nx} y={y() - 14} width="160" height="24" rx="4" class="mermaid-note-box" />
                            <text x={nx + 80} y={y() + 2} text-anchor="middle" class="mermaid-edge-label">{ev.text}</text>
                          </g>
                        );
                      })()}
                      {(ev.type === "block" || ev.type === "divider") && (
                        <g>
                          <rect x="24" y={y() - 12} width={parsed().width - 48} height="22" rx="3" class="mermaid-subgraph-box" />
                          <text x="34" y={y() + 3} class="mermaid-subgraph-title">{ev.op}: {ev.text}</text>
                        </g>
                      )}
                    </g>
                  );
                }}
              </For>
            </svg>
          )}

          {parsed().kind === "pie" && (
            <svg viewBox={`0 0 ${parsed().width} ${parsed().height}`} class="mermaid-svg" style={{ "max-width": "460px" }}>
              <Show when={parsed().title}>
                <text x="16" y="20" class="mermaid-subgraph-title">{parsed().title}</text>
              </Show>
              <For each={parsed().arcs}>
                {(a, i) => (
                  <g>
                    <path d={a.d} fill={a.color} stroke="var(--bg)" stroke-width="1.5" />
                    <rect x="200" y={34 + i() * 24} width="12" height="12" rx="3" fill={a.color} />
                    <text x="218" y={44 + i() * 24} class="mermaid-label">{a.label} ({a.pct}% · {a.value})</text>
                  </g>
                )}
              </For>
            </svg>
          )}

          {parsed().kind === "gantt" && (
            <div class="mermaid-gantt">
              <Show when={parsed().title}><div class="mermaid-gantt-title">{parsed().title}</div></Show>
              <For each={parsed().items}>
                {(item, i) => (
                  <div class="mermaid-gantt-row">
                    <span class="mermaid-gantt-sec">{item.section}</span>
                    <span class="mermaid-gantt-lbl">{item.label}</span>
                    <div class="mermaid-gantt-track">
                      <div
                        class={`mermaid-gantt-bar state-${item.state}`}
                        style={{ "margin-left": `${(i() * 14) % 45}%`, width: `${Math.max(25, 55 - (i() * 7) % 30)}%` }}
                      >
                        {item.meta}
                      </div>
                    </div>
                  </div>
                )}
              </For>
            </div>
          )}

          {parsed().kind === "timeline" && (
            <div class="mermaid-timeline">
              <Show when={parsed().title}><div class="mermaid-gantt-title">{parsed().title}</div></Show>
              <div class="mermaid-timeline-row">
                <For each={parsed().steps}>
                  {(s) => (
                    <div class="mermaid-timeline-step">
                      <div class="mermaid-timeline-period">{s.period}</div>
                      <div class="mermaid-timeline-detail">{s.detail}</div>
                    </div>
                  )}
                </For>
              </div>
            </div>
          )}
        </div>
      </Show>
    </div>
  );
}
