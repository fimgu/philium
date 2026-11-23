// Presentation only. Infer activity from public HAL lifecycle events plus gateway stream
// progress (phase, tool name, byte counts). Never shows model reasoning content.
export const STALL_HINT_MS = 30_000;
export const STALL_MS = 90_000;

const toolName = name => String(name || "tool").replace(/^mcp_philium_/, "").replace(/^mcp_/, "");

export function fmtBytes(n) {
  if (!n) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Label for the in-flight model stream. `stream` is the latest gateway:progress payload. */
export function streamLabel(stream) {
  if (!stream) return null;
  if (stream.phase === "thinking") return "Thinking…";
  if (stream.phase === "tool") {
    const t = stream.tool ? toolName(stream.tool) : null;
    if (stream.file) return /write|edit|patch|replace|create/i.test(t || "") || !t ? `Writing edit to ${stream.file}…` : `Preparing ${t} for ${stream.file}…`;
    return t ? `Preparing ${t}…` : "Preparing a tool call…";
  }
  if (stream.phase === "text") return "Writing response";
  return null;
}

/**
 * opts.stream: latest gateway progress for this conversation ({phase, tool, file, bytes, tokens, stalled}).
 * opts.now / opts.lastByteAt: ms timestamps; lastByteAt is the last moment any stream byte or event arrived.
 */
export function conversationActivity(events = [], busy = false, readOnly = false, opts = {}) {
  if (!busy || readOnly) return null;
  const tools = new Map();
  const permissions = new Set();
  let phase = "waiting";
  let turn = "pending";
  let ended = false;
  let retry = null;
  for (const e of events) {
    switch (e.type) {
      case "turn.started":
        tools.clear(); permissions.clear(); ended = false;
        turn = e.turn_id ?? e.seq ?? "pending"; phase = "waiting"; retry = null;
        break;
      case "model.started": phase = "waiting"; retry = null; break;
      case "model.retrying": phase = "retrying"; retry = e; break;
      case "model.delta": phase = "responding"; break;
      case "model.finished": phase = "working"; break;
      case "tool.started": tools.set(e.call_id, e.tool); phase = "working"; break;
      case "tool.finished": tools.delete(e.call_id); break;
      case "permission.requested": permissions.add(e.request_id); break;
      case "permission.resolved": permissions.delete(e.request_id); break;
      case "assistant.completed":
      case "turn.failed":
      case "turn.cancelled":
        ended = true; tools.clear(); permissions.clear(); break;
    }
  }
  if (ended) return null;
  if (permissions.size) return { key: `${turn}:permission`, kind: "permission", label: "Waiting for your approval", detail: "Review the permission request before work can continue." };
  if (tools.size) {
    const names = [...tools.values()].map(name => String(name || "tool").replace(/^mcp_philium_/, "").replace(/^mcp_/, ""));
    return { key: `${turn}:tools`, kind: "tools", label: tools.size === 1 ? `Running ${names[0]}` : `Running ${tools.size} tools`, detail: "Expand a tool entry for its arguments, output and completion status." };
  }
  if (phase === "retrying") return { key: `${turn}:retry:${retry?.attempt}`, kind: "retrying", label: Number.isInteger(retry?.attempt) && Number.isInteger(retry?.max_attempts) ? `Retrying model request (${retry.attempt}/${retry.max_attempts})` : "Retrying model request", detail: "The harness reported a retry. No retry countdown is available." };
  const { stream, now, lastByteAt } = opts;
  const live = stream && !stream.done ? stream : null;
  const idle = Number.isFinite(now) && Number.isFinite(lastByteAt) ? Math.max(0, now - lastByteAt) : 0;
  const meter = live && live.bytes ? (live.tokens ? `~${live.tokens.toLocaleString("en-US")} tokens` : fmtBytes(live.bytes)) : null;
  if (phase !== "working" && (stream?.stalled || idle > STALL_MS)) {
    return { key: `${turn}:stalled`, kind: "stalled", label: "Model stream stalled", detail: `No data from the model for ${Math.round(idle / 1000)}s. The gateway retries once, then ends the request.`, idleMs: idle, hint: "No response for a while. You can stop this turn." };
  }
  const label = phase !== "working" ? streamLabel(live) : null;
  const hint = phase !== "working" && idle > STALL_HINT_MS ? "Still waiting. You can stop this turn." : null;
  if (label) return { key: `${turn}:stream`, kind: live.phase === "text" ? "responding" : "streaming", label, meter, detail: "Live from the model stream: bytes are arriving.", idleMs: idle, hint };
  const labels = { waiting: "Waiting for model response", responding: "Writing response", working: "Working" };
  return { key: `${turn}:${phase}`, kind: phase, label: labels[phase], meter: phase === "working" ? null : meter, idleMs: idle, hint, detail: phase === "waiting" ? "No stream data yet." : "Activity reflects received session events." };
}
