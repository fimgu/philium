// Live xterm view of one Philium terminal. Used by the agent side panel.
// Streams terminals:data from the stored offset so nothing is lost or doubled.
import { Show, createSignal, onMount, onCleanup } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { rpc } from "../lib/rpc.js";

const decode = (s) => {
  const bin = atob(s);
  const len = bin.length;
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i++) out[i] = bin.charCodeAt(i);
  return out;
};
const encode = (s) => {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
};

const THEME = {
  background: "#0e1015", foreground: "#d9dee6", cursor: "#8fb8ff", selectionBackground: "#2a3550",
  black: "#1b1f27", brightBlack: "#5c6370", red: "#ef7b7b", brightRed: "#ff9a9a", green: "#8fd19e", brightGreen: "#a8e6b5",
  yellow: "#e8c77a", brightYellow: "#f5d996", blue: "#7fa8ff", brightBlue: "#9dbcff", magenta: "#c79bf2", brightMagenta: "#dab6ff",
  cyan: "#78d0d8", brightCyan: "#9be3ea", white: "#d9dee6", brightWhite: "#ffffff",
};

export function TermView(props) {
  let el, term, fit, observer, unlisten, dead = false, ready = false, offset = 0;
  let resizeRaf = 0, lastCols = 0, lastRows = 0;
  const queued = [];
  const [error, setError] = createSignal("");
  const append = (p) => {
    const data = decode(p.data);
    const end = p.offset ?? offset + data.length;
    const skip = Math.max(0, offset - (end - data.length));
    if (end > offset) term?.write(data.subarray(skip));
    offset = Math.max(offset, end);
    props.onData?.(data);
  };
  onMount(async () => {
    term = new Terminal({
      cursorBlink: true, fontSize: 12, lineHeight: 1.2, scrollback: 10000, theme: THEME,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    const doResize = () => {
      resizeRaf = 0;
      if (dead || !el || !el.clientWidth || !el.clientHeight) return;
      try { fit.fit(); } catch { return; }
      if (props.running !== false && (term.cols !== lastCols || term.rows !== lastRows)) {
        lastCols = term.cols;
        lastRows = term.rows;
        rpc("terminals.resize", { id: props.id, cols: term.cols, rows: term.rows }).catch(() => {});
      }
    };
    const resize = () => {
      if (dead || resizeRaf) return;
      resizeRaf = requestAnimationFrame(doResize);
    };
    observer = new ResizeObserver(resize);
    observer.observe(el);
    term.onData((d) => rpc("terminals.write", { id: props.id, data: encode(d) }).catch((e) => setError(String(e))));
    try {
      unlisten = await listen("terminals:data", (e) => {
        if (e.payload.id !== props.id) return;
        ready ? append(e.payload) : queued.push(e.payload);
      });
      if (dead) return unlisten();
      const b = await rpc("terminals.buffer", { id: props.id });
      if (dead) return;
      const data = decode(b.data);
      term.write(data);
      props.onData?.(data);
      offset = b.offset ?? data.length;
    } catch (e) {
      setError(String(e));
    }
    ready = true;
    for (const p of queued) append(p);
    queued.length = 0;
    doResize();
  });
  onCleanup(() => {
    dead = true;
    if (resizeRaf) cancelAnimationFrame(resizeRaf);
    observer?.disconnect();
    unlisten?.();
    term?.dispose();
  });
  return (
    <div class="termview">
      <Show when={error()}><div class="termview-err">{error()}</div></Show>
      <div class="termview-host" ref={el} />
    </div>
  );
}
