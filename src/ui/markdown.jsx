import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";
import { marked } from "marked";
import { decodeHTML } from "entities";
import "./markdown.css";

// Never pass parser HTML to innerHTML. Every leaf is a Solid text node.
export function safeLink(value) {
  try {
    const url = new URL(decodeHTML(value));
    return ["https:", "http:", "mailto:"].includes(url.protocol) ? url.href : undefined;
  } catch { return undefined; }
}

function CopyButton(props) {
  const [state, setState] = createSignal("idle");
  let timer;
  onCleanup(() => clearTimeout(timer));
  async function copy() {
    clearTimeout(timer);
    try {
      await navigator.clipboard.writeText(props.text);
      setState("success");
    } catch { setState("failure"); }
    timer = setTimeout(() => setState("idle"), 2500);
  }
  return <button type="button" class="markdown-copy" onClick={copy} aria-label={props.label}>
    <span aria-live="polite">{state() === "success" ? "Copied!" : state() === "failure" ? "Copy failed, try again" : props.label}</span>
  </button>;
}

// Marked's code text omits the final newline. Recover fenced contents from raw
// source instead, including CRLFs, so clipboard contents match what was sent.
export function codeContents(token, source) {
  if (token.codeBlockStyle === "indented") return token.text;
  const normalized = source.replace(/\r\n|\r/g, "\n");
  const start = token.sourceStart ?? normalized.indexOf(token.raw);
  const raw = start < 0 ? token.raw : (() => {
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
  if (!width) return token.text;
  while (lines.at(-1) === "") lines.pop();
  const last = lines.at(-1)?.trim();
  if (last && [...last].every(c => c === fence) && last.length >= width) lines.pop();
  return lines.join("");
}

function Tokens(props) {
  return <For each={props.tokens}>{token => {
    const children = () => <Tokens tokens={token.tokens || []} source={props.source} />;
    switch (token.type) {
      case "space": return null;
      case "heading": {
        // Explicit tags keep the renderer independent of HTML interpretation.
        return token.depth === 1 ? <h1>{children()}</h1> : token.depth === 2 ? <h2>{children()}</h2> : token.depth === 3 ? <h3>{children()}</h3> : token.depth === 4 ? <h4>{children()}</h4> : token.depth === 5 ? <h5>{children()}</h5> : <h6>{children()}</h6>;
      }
      case "paragraph": return <p>{children()}</p>;
      case "text": return token.tokens ? children() : decodeHTML(token.text);
      case "escape": return token.text;
      case "strong": return <strong>{children()}</strong>;
      case "em": return <em>{children()}</em>;
      case "del": return <del>{children()}</del>;
      case "codespan": return <code>{token.text}</code>;
      case "br": return <br />;
      case "hr": return <hr />;
      case "blockquote": return <blockquote>{children()}</blockquote>;
      case "link": return <Show when={safeLink(token.href)} fallback={children()}>{href => <a href={href()} target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer">{children()}</a>}</Show>;
      case "image": return <span class="markdown-image">[Image: {decodeHTML(token.text || "image omitted")}]</span>;
      case "html": return token.raw;
      case "code": return <div class="markdown-code"><CopyButton label="Copy code" text={codeContents(token, props.source)} /><pre><code>{token.text}</code></pre></div>;
      case "list": {
        const items = () => <For each={token.items}>{item => <li><Show when={item.task}><input type="checkbox" disabled checked={item.checked} /></Show><Tokens tokens={item.tokens} source={props.source} /></li>}</For>;
        return token.ordered ? <ol start={token.start}>{items()}</ol> : <ul>{items()}</ul>;
      }
      case "table": return <div class="markdown-table"><table><thead><tr><For each={token.header}>{cell => <th><Tokens tokens={cell.tokens} source={props.source} /></th>}</For></tr></thead><tbody><For each={token.rows}>{row => <tr><For each={row}>{cell => <td><Tokens tokens={cell.tokens} source={props.source} /></td>}</For></tr>}</For></tbody></table></div>;
      default: return token.raw || token.text || "";
    }
  }}</For>;
}

export function Markdown(props) {
  const tokens = createMemo(() => {
    const source = props.text || "";
    const normalized = source.replace(/\r\n|\r/g, "\n");
    const parsed = marked.lexer(source, { gfm: true });
    let cursor = 0;
    marked.walkTokens(parsed, token => {
      if (token.type !== "code") return;
      const start = normalized.indexOf(token.raw, cursor);
      if (start >= 0) { token.sourceStart = start; cursor = start + token.raw.length; }
    });
    return parsed;
  });
  return <div class="assistant-markdown"><Tokens tokens={tokens()} source={props.text || ""} /></div>;
}

export function AssistantReply(props) {
  return <><Markdown text={props.text} /><Show when={props.live}><span class="caret" /></Show><div class="markdown-actions"><CopyButton label="Copy reply" text={props.text} /></div></>;
}
