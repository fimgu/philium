import { createMemo, createSignal, Show } from "solid-js";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { decodeHTML } from "entities";

export function safeLink(raw) {
  if (typeof raw !== "string" || !raw.trim()) return undefined;
  try {
    const parsed = new URL(decodeHTML(raw.trim()));
    return ["http:", "https:", "mailto:"].includes(parsed.protocol) && !parsed.username && !parsed.password ? parsed.href : undefined;
  } catch {
    return undefined;
  }
}

export function MarkdownLink(props) {
  const [failed, setFailed] = createSignal(false);
  async function open(event) {
    if (event.defaultPrevented || !isTauri()) return;
    event.preventDefault();
    setFailed(false);
    try {
      await invoke("open_markdown_link", { url: props.href });
    } catch {
      setFailed(true);
    }
  }
  return <>
    <a href={props.href} target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer" onClick={open} onAuxClick={(event) => { if (event.button === 1) void open(event); }}>{props.children}</a>
    <Show when={failed()}><span class="markdown-image-status" role="status"> (Couldn’t open link)</span></Show>
  </>;
}

function imageSource(raw) {
  // Limit inline images to raster formats; never accept arbitrary blob/file URLs.
  if (raw.length <= 2_800_000 && /^data:image\/(?:png|jpeg|gif|webp|avif);base64,[a-z0-9+/]+={0,2}$/i.test(raw)) return { url: raw };
  const url = safeLink(raw);
  if (url?.startsWith("https://")) return { url, host: new URL(url).host };
}

export function MarkdownImage(props) {
  const source = createMemo(() => imageSource(props.href || ""));
  const alt = () => decodeHTML(props.alt || "");
  return <Show when={source()} keyed fallback={<span class="markdown-image-status">[Image unavailable{alt() ? `: ${alt()}` : ""}]</span>}>
    {(image) => <ImagePreview image={image} alt={alt()} />}
  </Show>;
}

function ImagePreview(props) {
  const [allowed, setAllowed] = createSignal(!props.image.host);
  const [failed, setFailed] = createSignal(false);
  // The consent action must not follow a surrounding Markdown image link.
  const load = (event) => {
    event.preventDefault();
    event.stopPropagation();
    setAllowed(true);
  };
  return <Show when={allowed()} fallback={
    <span class="markdown-image-load" role="button" tabindex="0" onClick={load} onKeyDown={(event) => {
      if (event.key === "Enter" || event.key === " ") load(event);
    }}>Load image{props.alt ? `: ${props.alt}` : ""} ({props.image.host})</span>
  }>
    <Show when={!failed()} fallback={<span class="markdown-image-status" role="status">[Image failed to load{props.alt ? `: ${props.alt}` : ""}]</span>}>
      <img class="markdown-image" src={props.image.url} alt={props.alt} decoding="async" referrerpolicy="no-referrer" onError={() => setFailed(true)} />
    </Show>
  </Show>;
}
