// Presentation only. Infer activity from public HAL lifecycle events, never model reasoning.
export function conversationActivity(events = [], busy = false, readOnly = false) {
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
      case "model.delta": if (e.delta) phase = "responding"; break;
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
  const labels = { waiting: "Waiting for model response", responding: "Writing response", working: "Working" };
  return { key: `${turn}:${phase}`, kind: phase, label: labels[phase], detail: phase === "waiting" ? "No response text yet. This does not indicate whether the model is thinking." : "Activity reflects received session events." };
}
