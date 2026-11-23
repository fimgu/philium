import { createEffect } from "solid-js";
import katex from "katex";

export function MathPreview(props) {
  let host;
  createEffect(() => {
    const source = props.code;
    try {
      const markup = katex.renderToString(source, {
        output: "mathml", displayMode: props.display, trust: false,
        maxExpand: 200, maxSize: 20,
      });
      const doc = new DOMParser().parseFromString(markup, "application/xml");
      const math = doc.getElementsByTagNameNS("http://www.w3.org/1998/Math/MathML", "math")[0];
      if (!math) throw new Error("Invalid math output");
      host.replaceChildren(math);
    } catch {
      host.textContent = source;
    }
  });
  return <span class="markdown-equation" ref={host} />;
}
