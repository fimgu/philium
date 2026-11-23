// Only SGR style survives. No HTML, OSC links, clipboard, or terminal commands.
export function safeAnsi(text) {
  return String(text).replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\|$)/g, "")
    .replace(/\x1bP[\s\S]*?(?:\x1b\\|$)/g, "")
    .replace(/\x1b\[([0-?]*)([ -/]*)([@-~])/g, (all, params, mid, cmd) => cmd === "m" && !mid && /^[\d;:]*$/.test(params) ? all : "")
    .replace(/\x1b(?!\[[\d;:]*m)[\s\S]?/g, "")
    .replace(/[\x00-\x08\x0b-\x1a\x1c-\x1f\x7f-\x9f]/g, "");
}
