// Read-only ANSI data using the same xterm renderer as live terminals.
import { createEffect, on, onMount, onCleanup } from "solid-js";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { safeAnsi } from "../lib/ansi-diff.js";

export function AnsiDiff(props) {
  let el, term, observer;
  const write = () => {
    if (!term) return;
    term.reset();
    term.write(safeAnsi(props.text).replace(/\r?\n/g, "\r\n"), () => term?.scrollToTop());
  };
  onMount(() => {
    term = new Terminal({ disableStdin: true, cursorBlink: false, cursorInactiveStyle: "none", fontSize: 12,
      scrollback: 100000, cols: 120, rows: 24, theme: { background: "#0e1015", foreground: "#d9dee6" } });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    observer = new ResizeObserver(() => { try { fit.fit(); } catch {} });
    observer.observe(el);
    write();
  });
  createEffect(on(() => props.text, write, { defer: true }));
  onCleanup(() => { observer?.disconnect(); term?.dispose(); term = null; });
  return <div ref={el} aria-label="Read-only Delta diff" style={{ flex: 1, "min-height": "240px", overflow: "hidden" }} />;
}
