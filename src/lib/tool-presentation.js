// Presentation only. Never interpret terminal escape sequences as HTML.
export function toolOutput(result, streamed = "") {
  let value = result;
  if (typeof value === "string") {
    try { value = JSON.parse(value); } catch { return value || streamed; }
  }
  if (value && typeof value === "object") {
    if (Array.isArray(value.content)) return value.content.map(x => x.text ? toolOutput(x.text) : "").filter(Boolean).join("\n") || streamed;
    for (const key of ["output", "stdout", "text"]) {
      if (typeof value[key] === "string") return [value[key], value.stderr].filter(Boolean).join("\n") || streamed;
    }
    return JSON.stringify(value, null, 2);
  }
  return streamed || "";
}
export function cleanTerminalText(text) {
  return text.replace(/\x1b\][^\x07]*(?:\x07|\x1b\\)/g, "").replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

export function shellStatus(result) {
  let value = result;
  if (typeof value === "string") { try { value = JSON.parse(value); } catch { return null; } }
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value.content)) return value.content.map(x => shellStatus(x.text)).find(Boolean) ?? null;
  if (value.running === true || value.state === "running") return "Still running";
  if (value.state === "interrupted") return "Interrupted";
  if (typeof value.exit_code === "number") return `Exit ${value.exit_code}`;
  return null;
}
