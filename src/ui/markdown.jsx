import { createEffect, createMemo, createSignal, For, Index, lazy, Match, onCleanup, Show, Suspense, Switch } from "solid-js";
import { marked } from "marked";
import { decodeHTML } from "entities";
import "./markdown.css";

const MermaidBlock = lazy(() => import("./mermaid.jsx").then((m) => ({ default: m.MermaidBlock })));

// Do not use innerHTML here; the security regression test checks this file.
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
  const [state, setState] = createSignal("idle");
  let timer;
  onCleanup(() => clearTimeout(timer));
  const copy = async (e) => {
    e?.stopPropagation?.();
    clearTimeout(timer);
    try {
      const value = typeof props.getText === "function" ? props.getText() : (props.text ?? "");
      await navigator.clipboard.writeText(value ?? "");
      setState("success");
    } catch {
      setState("failure");
    }
    timer = setTimeout(() => setState("idle"), 2500);
  };
  return (
    <button type="button" class={props.class || "markdown-copy"} onClick={copy} aria-label={props.label}>
      <span aria-live="polite">
        {state() === "success" ? "Copied!" : state() === "failure" ? "Copy failed, try again" : (props.textLabel || props.label)}
      </span>
    </button>
  );
}

export function codeContents(token, source = "") {
  if (token.codeBlockStyle === "indented") return token.text || "";
  const hasCr = source.includes("\r");
  const normalized = hasCr ? source.replace(/\r\n|\r/g, "\n") : source;
  const start = token.sourceStart ?? (token.raw ? normalized.indexOf(token.raw) : -1);
  const raw = start < 0 ? (token.raw || "") : !hasCr ? source.slice(start, start + token.raw.length) : (() => {
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
  createEffect(() => {
    const src = String(props.code || "");
    if (!hostRef) return;
    try {
      const doc = new DOMParser().parseFromString(src, "image/svg+xml");
      const root = doc.querySelector("svg");
      if (root) {
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
  const lang = createMemo(() => (token().lang || "").trim().split(/\s+/)[0].toLowerCase());
  const code = createMemo(() => token().text || "");
  const lineCount = createMemo(() => code().split("\n").length);
  const [collapsed, setCollapsed] = createSignal(false);
  const kind = createMemo(() => {
    const l = lang();
    if (l === "mermaid" || (!l && /^(?:flowchart|graph|sequenceDiagram|stateDiagram|erDiagram|classDiagram|pie|gantt|timeline|mindmap)\b/m.test(code().trim()))) return "mermaid";
    if (l === "diff" || l === "patch") return "diff";
    if (l === "svg" || (l === "xml" && code().trim().startsWith("<svg"))) return "svg";
    if (l === "math" || l === "latex" || l === "tex") return "math";
    if (l === "csv" || l === "tsv" || l === "json-table") return "table";
    return "code";
  });

  return (
    <Switch
      fallback={
        <div class="markdown-code">
          <div class="markdown-code-bar">
            <span class="markdown-code-lang">{lang() || "code"} · {lineCount()}L</span>
            <div class="markdown-rich-actions">
              <Show when={lineCount() > 24}>
                <button type="button" class="markdown-rich-btn" onClick={() => setCollapsed((c) => !c)}>
                  {collapsed() ? `Expand (${lineCount()} lines)` : "Collapse"}
                </button>
              </Show>
              <CopyButton class="markdown-rich-btn" label="Copy code" getText={() => codeContents(token(), props.source)} />
            </div>
          </div>
          <Show when={!collapsed()}>
            <pre><code>{code()}</code></pre>
          </Show>
        </div>
      }
    >
      <Match when={kind() === "mermaid"}>
        <Suspense fallback={<pre class="markdown-rich-source"><code>{code()}</code></pre>}>
          <MermaidBlock
            code={code()}
            copyButton={<CopyButton class="markdown-rich-btn" label="Copy Mermaid" getText={code} />}
          />
        </Suspense>
      </Match>
      <Match when={kind() === "diff"}><DiffBlock code={code()} /></Match>
      <Match when={kind() === "svg"}><SvgBlock code={code()} /></Match>
      <Match when={kind() === "math"}><MathBlock code={code()} /></Match>
      <Match when={kind() === "table"}><DataTableBlock lang={lang()} code={code()} /></Match>
    </Switch>
  );
}

function Tokens(props) {
  return <Index each={props.tokens || []}>{(token) => {
    const shape = createMemo(() => {
      const t = token();
      if (t.type === "heading") return `h${t.depth}`;
      if (t.type === "list") return t.ordered ? "ol" : "ul";
      if (t.type === "paragraph") {
        const s = (t.text || "").trim();
        return s.startsWith("$$") && s.endsWith("$$") && s.length > 4 ? "p-math" : "p";
      }
      if (t.type === "text") return t.tokens ? "text-nested" : "text-leaf";
      return t.type;
    });
    const children = () => <Tokens tokens={token().tokens || []} source={props.source} />;
    return createMemo(() => {
      switch (shape()) {
        case "space": return null;
        case "h1": return <h1>{children()}</h1>;
        case "h2": return <h2>{children()}</h2>;
        case "h3": return <h3>{children()}</h3>;
        case "h4": return <h4>{children()}</h4>;
        case "h5": return <h5>{children()}</h5>;
        case "h6": return <h6>{children()}</h6>;
        case "p-math": return <MathBlock code={(token().text || "").trim().slice(2, -2)} />;
        case "p": return <p>{children()}</p>;
        case "text-nested": return children();
        case "text-leaf": return decodeHTML(token().text);
        case "escape": return token().text;
        case "strong": return <strong>{children()}</strong>;
        case "em": return <em>{children()}</em>;
        case "del": return <del>{children()}</del>;
        case "codespan": return <code>{token().text}</code>;
        case "br": return <br />;
        case "hr": return <hr />;
        case "blockquote": return <blockquote>{children()}</blockquote>;
        case "link": return <Show when={safeLink(token().href)} fallback={children()}>{(href) => <a href={href()} target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer">{children()}</a>}</Show>;
        case "image": return <span class="markdown-image">[Image: {decodeHTML(token().text || "image omitted")}]</span>;
        case "html": return token().raw;
        case "code": return <CodeBlock token={token()} source={props.source} />;
        case "checkbox": return <input type="checkbox" disabled checked={token().checked} />;
        case "ol": return <ol start={token().start}><Index each={token().items}>{(item) => <li><Tokens tokens={item().tokens} source={props.source} /></li>}</Index></ol>;
        case "ul": return <ul><Index each={token().items}>{(item) => <li><Tokens tokens={item().tokens} source={props.source} /></li>}</Index></ul>;
        case "table": return <div class="markdown-table-wrap"><table>
          <thead><tr><Index each={token().header}>{(cell) => <th><Tokens tokens={cell().tokens} source={props.source} /></th>}</Index></tr></thead>
          <tbody><Index each={token().rows}>{(row) => <tr><Index each={row()}>{(cell) => <td><Tokens tokens={cell().tokens} source={props.source} /></td>}</Index></tr>}</Index></tbody>
        </table></div>;
        default: return token().raw || token().text || "";
      }
    });
  }}</Index>;
}

export function Markdown(props) {
  const tokens = createMemo((prev = []) => {
    const source = props.text || "";
    const normalized = source.includes("\r") ? source.replace(/\r\n|\r/g, "\n") : source;
    const parsed = marked.lexer(source, { gfm: true });
    let cursor = 0;
    marked.walkTokens(parsed, (token) => {
      if (token.type !== "code") return;
      const start = normalized.indexOf(token.raw, cursor);
      if (start >= 0) { token.sourceStart = start; cursor = start + token.raw.length; }
    });
    if (Object.keys(parsed.links || {}).length === Object.keys(prev.links || {}).length) {
      const max = Math.min(prev.length, parsed.length);
      for (let i = 0; i < max; i++) {
        const a = prev[i], b = parsed[i];
        if (a && a.type === b.type && a.raw === b.raw && a.sourceStart === b.sourceStart) {
          parsed[i] = a;
        } else {
          break;
        }
      }
    }
    return parsed;
  });
  return <div class="assistant-markdown"><Tokens tokens={tokens()} source={props.text || ""} /></div>;
}

export function AssistantReply(props) {
  return <>
    <Markdown text={props.text} />
    <Show when={props.live}><span class="caret" /></Show>
    <div class="markdown-actions"><CopyButton label="Copy reply" text={props.text} /></div>
  </>;
}
