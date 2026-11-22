// Live "what is this agent doing" view. Data comes from agents.list (`progress`)
// and is refreshed by the `agents:progress` event the backend emits per event batch.
import { createSignal } from "solid-js";
import { summarizeTool } from "./tool-presentation.js";

const [live, setLive] = createSignal({});
let subscribed = false;

export function subscribeAgentProgress(useEvent) {
  if (subscribed) return;
  subscribed = true;
  useEvent("agents:progress", (p) => {
    if (p?.id) setLive((m) => ({ ...m, [p.id]: p.progress }));
  });
}

export function agentProgress(agent) {
  if (!agent) return null;
  return live()[agent.id] ?? agent.progress ?? null;
}

export function fmtDuration(ms) {
  if (ms == null || ms < 0) return "";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

/** One plain-English sentence for the current step. */
export function currentStep(agent, p = agentProgress(agent)) {
  const state = agent?.state;
  if (state === "waiting" || p?.phase === "approval") return "Waiting for your approval";
  if (state === "failed") return "Stopped after an error";
  if (state === "stopped") return "Stopped";
  if (!p) return state === "running" ? "Working" : "";
  const busy = state === "running" || (p.phase && p.phase !== "idle");
  if (busy && p.current_tool) {
    const s = summarizeTool(p.current_tool.tool, p.current_tool.arguments, null);
    const more = p.tools_running > 1 ? ` (+${p.tools_running - 1} more)` : "";
    return `${s.label}${s.summary ? `: ${s.summary}` : ""}${more}`;
  }
  if (busy) {
    return { thinking: "Thinking", writing: "Writing a reply", retrying: "Retrying the model request", working: "Working" }[p.phase] || "Working";
  }
  if (p.last_tool) {
    const s = summarizeTool(p.last_tool.tool, p.last_tool.arguments, null);
    return `Idle. Last step ${s.label}${s.summary ? `: ${s.summary}` : ""}`;
  }
  return "Idle";
}

/** Elapsed time of the current turn, or the duration of the last one. */
export function turnElapsed(p, now = Date.now()) {
  if (!p?.turn_started_at) return "";
  const end = p.phase === "idle" && p.turn_ended_at ? p.turn_ended_at : now;
  return fmtDuration(end - p.turn_started_at);
}
