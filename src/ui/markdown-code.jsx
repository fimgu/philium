import { createMemo } from "solid-js";
import { common, createLowlight } from "lowlight";

const highlighter = createLowlight(common);

function render(nodes) {
  return nodes.map((node) => node.type === "text" ? node.value
    : <span class={node.properties?.className?.join(" ")}>{render(node.children || [])}</span>);
}

export function HighlightedCode(props) {
  const content = createMemo(() => {
    const code = props.code;
    if (props.live || !props.lang || code.length > 30_000 || !highlighter.registered(props.lang)) return code;
    try {
      return render(highlighter.highlight(props.lang, code).children);
    } catch {
      return code;
    }
  });
  return <code>{content()}</code>;
}
