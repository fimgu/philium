import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Marked } from "marked";
import { decodeHTML } from "entities";
import { MermaidBlock } from "./mermaid.jsx";
import "./markdown.css";

// Do not use innerHTML here; the security regression test checks this file.
const markdown = new Marked({ gfm: true, breaks: true });

export function safeLink(raw) {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  try {
    const parsed = new URL(decodeHTML(trimmed));
    return ["http:", "https:", "mailto:"].includes(parsed.protocol) ? parsed.href : undefined;
  } catch {
    return undefined;
  }
}

function CopyButton(props) {
  const [copied, setCopied] = createSignal(false);
  let timer;
  onCleanup(() => clearTimeout(timer));
  const copy = async (e) => {
    e.stopPropagation();
    try {
      await navigator.clipboard?.writeText(props.text || "");
      setCopied(true);
      clearTimeout(timer);
      timer = setTimeout(() => setCopied(false), 1200);
    } catch {}
  };
  return (
    <button type="button" class={props.class || "markdown-copy"} onClick={copy} aria-label={props.label}>
      {copied() ? "Copied" : props.textLabel || "Copy"}
    </button>
  );
}

export function codeContents(token, source = "") {
  if (token.codeBlockStyle === "indented") return token.text || "";
  const normalized = source.replace(/\r\n|\r/g, "\n");
  const start = token.sourceStart ?? (token.raw ? normalized.indexOf(token.raw) : -1);
  const raw = start < 0 ? (token.raw || "") : (() => {
    const offsets = [];
    for (let i = 0; i < source.length; i++) {
      offsets.push(i);
      if (source[i] === "\r" && source[i + 1] === "\n") i++;
    }
    offsets.push(source.length);
    return source.slice(offsets[start], offsets[start + token.raw.length]);
  })();
  const lines = raw.match(/[^\r\n]*(?:\r\n|\r|\n|$)/g) || [];
  const opening = lines.shift() || "";
  const fence = opening.trimStart()[0];
  const width = opening.trimStart().match(/^(`+|~+)/)?.[0].length;
  if (!width) return token.text || "";
  while (lines.at(-1) === "") lines.pop();
  const last = lines.at(-1)?.trim();
  if (last && [...last].every((c) => c === fence) && last.length >= width) lines.pop();
  return lines.join("");
}

const LATEX_MAP = {
  "\\alpha": "α", "\\beta": "β", "\\gamma": "γ", "\\delta": "δ", "\\epsilon": "ε", "\\zeta": "ζ",
  "\\eta": "η", "\\theta": "θ", "\\iota": "ι", "\\kappa": "κ", "\\lambda": "λ", "\\mu": "μ",
  "\\nu": "ν", "\\xi": "ξ", "\\pi": "π", "\\rho": "ρ", "\\sigma": "σ", "\\tau": "τ",
  "\\phi": "φ", "\\chi": "χ", "\\psi": "ψ", "\\omega": "ω",
  "\\Gamma": "Γ", "\\Delta": "Δ", "\\Theta": "Θ", "\\Lambda": "Λ", "\\Xi": "Ξ", "\\Pi": "Π",
  "\\Sigma": "Σ", "\\Phi": "Φ", "\\Psi": "Ψ", "\\Omega": "Ω",
  "\\rightarrow": "→", "\\to": "→", "\\leftarrow": "←", "\\leftrightarrow": "↔",
  "\\Rightarrow": "⇒", "\\Leftarrow": "⇐", "\\Leftrightarrow": "⇔", "\\mapsto": "↦",
  "\\times": "×", "\\cdot": "·", "\\div": "÷", "\\pm": "±", "\\mp": "∓",
  "\\leq": "≤", "\\le": "≤", "\\geq": "≥", "\\ge": "≥", "\\neq": "≠", "\\ne": "≠",
  "\\approx": "≈", "\\equiv": "≡", "\\sim": "∼", "\\propto": "∝",
  "\\infty": "∞", "\\partial": "∂", "\\nabla": "∇", "\\forall": "∀", "\\exists": "∃",
  "\\in": "∈", "\\notin": "∉", "\\subset": "⊂", "\\subseteq": "⊆", "\\cup": "∪", "\\cap": "∩",
  "\\emptyset": "∅", "\\sum": "∑", "\\prod": "∏", "\\int": "∫", "\\sqrt": "√",
  "\\dots": "…", "\\ldots": "…", "\\cdots": "⋯",
};

function formatMathString(src) {
  let s = String(src || "").trim().replace(/^\$\$|\$\$$/g, "").replace(/^\$|\$$/g, "");
  s = s.replace(/\\frac\{([^{}]+)\}\{([^{}]+)\}/g, "($1) / ($2)");
  s = s.replace(/\\text\{([^{}]+)\}/g, "$1");
  s = s.replace(/\\(?:mathbf|mathrm|mathit|mathcal|mathbb)\{([^{}]+)\}/g, "$1");
  s = s.replace(/\^\{([^{}]+)\}/g, "^($1)");
  s = s.replace(/_\{([^{}]+)\}/g, "_($1)");
  for (const [k, v] of Object.entries(LATEX_MAP)) {
    s = s.split(k).join(v);
  }
  return s;
}

function DiffBlock(props) {
  const [view, setView] = createSignal("diff");
  const parsed = createMemo(() => {
    const lines = String(props.code || "").replace(/\r\n/g, "\n").split("\n");
    let adds = 0, dels = 0;
    const rows = lines.map((line) => {
      if (line.startsWith("+++") || line.startsWith("---")) return { kind: "file", text: line };
      if (line.startsWith("@@")) return { kind: "hunk", text: line };
      if (line.startsWith("+")) { adds++; return { kind: "add", text: line }; }
      if (line.startsWith("-")) { dels++; return { kind: "del", text: line }; }
      return { kind: "ctx", text: line };
    });
    return { rows, adds, dels };
  });

  return (
    <div class="markdown-rich markdown-diff">
      <div class="markdown-rich-bar">
        <span class="markdown-rich-badge">
          DIFF <span class="diff-stat-add">+{parsed().adds}</span> <span class="diff-stat-del">−{parsed().dels}</span>
        </span>
        <div class="markdown-rich-actions">
          <button type="button" class="markdown-rich-btn" onClick={() => setView((v) => (v === "diff" ? "raw" : "diff"))}>
            {view() === "diff" ? "Raw" : "Diff"}
          </button>
          <CopyButton class="markdown-rich-btn" label="Copy diff" text={props.code} />
        </div>
      </div>
      <Show when={view() === "diff"} fallback={<pre class="markdown-rich-source"><code>{props.code}</code></pre>}>
        <div class="diff-lines">
          <For each={parsed().rows}>
            {(r, i) => (
              <div class={`diff-line diff-line-${r.kind}`}>
                <span class="diff-ln">{i() + 1}</span>
                <span class="diff-code">{r.text}</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

const ALLOWED_SVG_TAGS = new Set([
  "svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon",
  "text", "tspan", "defs", "lineargradient", "radialgradient", "stop", "marker",
  "clippath", "mask", "pattern", "title", "desc",
]);

function sanitizeSvgNode(node) {
  if (node.nodeType === 3) return document.createTextNode(node.nodeValue || "");
  if (node.nodeType !== 1) return null;
  const tag = node.tagName.toLowerCase();
  if (!ALLOWED_SVG_TAGS.has(tag)) return null;
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const attr of Array.from(node.attributes || [])) {
    const name = attr.name.toLowerCase();
    const val = String(attr.value || "");
    if (name.startsWith("on") || name === "href" || name === "xlink:href") continue;
    if (/javascript:|data:text\/html/i.test(val)) continue;
    el.setAttribute(attr.name, val);
  }
  for (const child of Array.from(node.childNodes || [])) {
    const c = sanitizeSvgNode(child);
    if (c) el.appendChild(c);
  }
  return el;
}

function SvgBlock(props) {
  const [view, setView] = createSignal("preview");
  let hostRef;
  onMount(() => {
    try {
      const doc = new DOMParser().parseFromString(String(props.code || ""), "image/svg+xml");
      const root = doc.querySelector("svg");
      if (root && hostRef) {
        const clean = sanitizeSvgNode(root);
        if (clean) {
          clean.classList.add("mermaid-svg");
          hostRef.replaceChildren(clean);
        }
      }
    } catch {}
  });
  return (
    <div class="markdown-rich markdown-svg">
      <div class="markdown-rich-bar">
        <span class="markdown-rich-badge">SVG PREVIEW</span>
        <div class="markdown-rich-actions">
          <button type="button" class="markdown-rich-btn" onClick={() => setView((v) => (v === "preview" ? "source" : "preview"))}>
            {view() === "preview" ? "Source" : "Preview"}
          </button>
          <CopyButton class="markdown-rich-btn" label="Copy SVG" text={props.code} />
        </div>
      </div>
      <div class="mermaid-canvas" style={{ display: view() === "preview" ? "flex" : "none" }} ref={hostRef} />
      <Show when={view() === "source"}>
        <pre class="markdown-rich-source"><code>{props.code}</code></pre>
      </Show>
    </div>
  );
}

function MathBlock(props) {
  const [view, setView] = createSignal("math");
  const formatted = createMemo(() => formatMathString(props.code));
  return (
    <div class="markdown-rich markdown-math">
      <div class="markdown-rich-bar">
        <span class="markdown-rich-badge">MATH · LATEX</span>
        <div class="markdown-rich-actions">
          <button type="button" class="markdown-rich-btn" onClick={() => setView((v) => (v === "math" ? "source" : "math"))}>
            {view() === "math" ? "LaTeX" : "Math"}
          </button>
          <CopyButton class="markdown-rich-btn" label="Copy LaTeX" text={props.code} />
        </div>
      </div>
      <Show when={view() === "math"} fallback={<pre class="markdown-rich-source"><code>{props.code}</code></pre>}>
        <div class="markdown-math-body">{formatted()}</div>
      </Show>
    </div>
  );
}

function DataTableBlock(props) {
  const [view, setView] = createSignal("table");
  const [sortCol, setSortCol] = createSignal(-1);
  const [asc, setAsc] = createSignal(true);
  const parsed = createMemo(() => {
    const lang = (props.lang || "").toLowerCase();
    const raw = String(props.code || "").trim();
    if (lang === "json-table") {
      try {
        const arr = JSON.parse(raw);
        if (Array.isArray(arr) && arr.length && typeof arr[0] === "object") {
          const headers = Object.keys(arr[0]);
          const rows = arr.map((obj) => headers.map((h) => String(obj?.[h] ?? "")));
          return { headers, rows };
        }
      } catch {}
    }
    const sep = lang === "tsv" ? "\t" : ",";
    const lines = raw.split(/\r?\n/).filter(Boolean);
    if (!lines.length) return { headers: [], rows: [] };
    const splitRow = (l) => l.split(sep).map((c) => c.trim().replace(/^"|"$/g, ""));
    return { headers: splitRow(lines[0]), rows: lines.slice(1).map(splitRow) };
  });
  const sortedRows = createMemo(() => {
    const { rows } = parsed();
    const c = sortCol();
    if (c < 0) return rows;
    const dir = asc() ? 1 : -1;
    return [...rows].sort((a, b) => String(a[c] || "").localeCompare(String(b[c] || ""), undefined, { numeric: true }) * dir);
  });
  return (
    <div class="markdown-rich markdown-datatable">
      <div class="markdown-rich-bar">
        <span class="markdown-rich-badge">TABLE · {parsed().rows.length} ROWS</span>
        <div class="markdown-rich-actions">
          <button type="button" class="markdown-rich-btn" onClick={() => setView((v) => (v === "table" ? "raw" : "table"))}>
            {view() === "table" ? "Raw" : "Table"}
          </button>
          <CopyButton class="markdown-rich-btn" label="Copy table" text={props.code} />
        </div>
      </div>
      <Show when={view() === "table" && parsed().headers.length} fallback={<pre class="markdown-rich-source"><code>{props.code}</code></pre>}>
        <div class="markdown-table-wrap">
          <table>
            <thead>
              <tr>
                <For each={parsed().headers}>
                  {(h, i) => (
                    <th style={{ cursor: "pointer" }} onClick={() => { if (sortCol() === i()) setAsc((a) => !a); else { setSortCol(i()); setAsc(true); } }}>
                      {h} {sortCol() === i() ? (asc() ? "↑" : "↓") : ""}
                    </th>
                  )}
                </For>
              </tr>
            </thead>
            <tbody>
              <For each={sortedRows()}>
                {(r) => (
                  <tr>
                    <For each={r}>{(c) => <td>{c}</td>}</For>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
    </div>
  );
}

function CodeBlock(props) {
  const token = () => props.token;
  const lang = () => (token().lang || "").trim().split(/\s+/)[0].toLowerCase();
  const code = () => token().text || "";
  const rawText = () => codeContents(token(), props.source);
  const lineCount = () => code().split("\n").length;
  const [collapsed, setCollapsed] = createSignal(false);

  if (lang() === "mermaid" || (!lang() && /^(?:flowchart|graph|sequenceDiagram|stateDiagram|erDiagram|classDiagram|pie|gantt|timeline|mindmap)\b/m.test(code().trim()))) {
    return (
      <MermaidBlock
        code={code()}
        copyButton={<CopyButton class="markdown-rich-btn" label="Copy Mermaid" text={code()} />}
      />
    );
  }
  if (lang() === "diff" || lang() === "patch") {
    return <DiffBlock code={code()} />;
  }
  if (lang() === "svg" || (lang() === "xml" && code().trim().startsWith("<svg"))) {
    return <SvgBlock code={code()} />;
  }
  if (lang() === "math" || lang() === "latex" || lang() === "tex") {
    return <MathBlock code={code()} />;
  }
  if (lang() === "csv" || lang() === "tsv" || lang() === "json-table") {
    return <DataTableBlock lang={lang()} code={code()} />;
  }

  return (
    <div class="markdown-code">
      <div class="markdown-code-bar">
        <span class="markdown-code-lang">{lang() || "code"} · {lineCount()}L</span>
        <div class="markdown-rich-actions">
          <Show when={lineCount() > 24}>
            <button type="button" class="markdown-rich-btn" onClick={() => setCollapsed((c) => !c)}>
              {collapsed() ? `Expand (${lineCount()} lines)` : "Collapse"}
            </button>
          </Show>
          <CopyButton class="markdown-rich-btn" label="Copy code" text={rawText()} />
        </div>
      </div>
      <Show when={!collapsed()}>
        <pre><code>{code()}</code></pre>
      </Show>
    </div>
  );
}

function Inline(props) {
  const raw = (token) => decodeHTML(token.raw ?? token.text ?? "");
  return <For each={props.tokens || []}>{(token) => {
    switch (token.type) {
      case "escape":
      case "text": return token.tokens ? <Inline tokens={token.tokens} /> : decodeHTML(token.text || "");
      case "strong": return <strong><Inline tokens={token.tokens} /></strong>;
      case "em": return <em><Inline tokens={token.tokens} /></em>;
      case "del": return <del><Inline tokens={token.tokens} /></del>;
      case "codespan": return <code>{decodeHTML(token.text || "")}</code>;
      case "br": return <br />;
      case "image": return <span class="markdown-image">[Image: {decodeHTML(token.text || "image omitted")}]</span>;
      case "link": {
        const href = safeLink(token.href);
        const label = token.tokens?.length ? <Inline tokens={token.tokens} /> : decodeHTML(token.text || token.href || "");
        return href ? <a href={href} target="_blank" rel="noopener noreferrer">{label}</a> : <span>{raw(token)}</span>;
      }
      default: return raw(token);
    }
  }}</For>;
}

function Tokens(props) {
  const children = (token) => token.tokens ? <Inline tokens={token.tokens} /> : decodeHTML(token.text || "");
  return <For each={props.tokens || []}>{(token) => {
    switch (token.type) {
      case "space": return null;
      case "paragraph": {
        const t = (token.text || "").trim();
        if (t.startsWith("$$") && t.endsWith("$$") && t.length > 4) {
          return <MathBlock code={t.slice(2, -2)} />;
        }
        return <p>{children(token)}</p>;
      }
      case "text": return <p>{children(token)}</p>;
      case "heading": {
        const depth = Math.min(Math.max(token.depth || 1, 1), 6);
        if (depth === 1) return <h1>{children(token)}</h1>;
        if (depth === 2) return <h2>{children(token)}</h2>;
        if (depth === 3) return <h3>{children(token)}</h3>;
        if (depth === 4) return <h4>{children(token)}</h4>;
        if (depth === 5) return <h5>{children(token)}</h5>;
        return <h6>{children(token)}</h6>;
      }
      case "blockquote": return <blockquote><Tokens tokens={token.tokens} source={props.source} /></blockquote>;
      case "hr": return <hr />;
      case "code": return <CodeBlock token={token} source={props.source} />;
      case "list": {
        const items = <For each={token.items || []}>{(item) => <li>
          {item.task && <input type="checkbox" checked={Boolean(item.checked)} disabled />}
          <Tokens tokens={item.tokens} source={props.source} />
        </li>}</For>;
        return token.ordered ? <ol start={token.start || 1}>{items}</ol> : <ul>{items}</ul>;
      }
      case "table": return <div class="markdown-table-wrap"><table>
        <thead><tr><For each={token.header || []}>{(cell, idx) => <th style={{ "text-align": token.align?.[idx()] || undefined }}><Inline tokens={cell.tokens} /></th>}</For></tr></thead>
        <tbody><For each={token.rows || []}>{(row) => <tr><For each={row}>{(cell, idx) => <td style={{ "text-align": token.align?.[idx()] || undefined }}><Inline tokens={cell.tokens} /></td>}</For></tr>}</For></tbody>
      </table></div>;
      default: return <pre class="markdown-raw"><code>{token.raw || token.text || ""}</code></pre>;
    }
  }}</For>;
}

export function Markdown(props) {
  const source = () => typeof props.text === "string" ? props.text : String(props.text ?? "");
  const tokens = createMemo(() => {
    try { return markdown.lexer(source()); } catch { return null; }
  });
  return <div class="assistant-markdown">
    {tokens() ? <Tokens tokens={tokens()} source={source()} /> : <div style={{ "white-space": "pre-wrap" }}>{source()}</div>}
  </div>;
}

export function AssistantReply(props) {
  const source = () => typeof props.text === "string" ? props.text : String(props.text ?? "");
  return <>
    <Markdown text={source()} />
    <Show when={props.live}><span class="caret" /></Show>
    <div class="markdown-actions"><CopyButton label="Copy reply" textLabel="Copy reply" text={source()} /></div>
  </>;
}
