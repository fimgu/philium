import { For, Show, createSignal, onCleanup } from "solid-js";
import { agentProgress, currentStep, turnElapsed } from "../lib/agent-progress.js";
import { fmt } from "../lib/rpc.js";

const [tick, setTick] = createSignal(Date.now());
let users = 0;
let timer;
function useTick() {
  if (users++ === 0) timer = setInterval(() => setTick(Date.now()), 1000);
  onCleanup(() => { if (--users === 0) clearInterval(timer); });
  return tick;
}

/** Compact progress for list rows and tiles: step sentence, done/total, elapsed. */
export function AgentProgressLine(props) {
  const now = useTick();
  const p = () => agentProgress(props.agent);
  const step = () => currentStep(props.agent, p());
  const busy = () => props.agent?.state === "running" || props.agent?.state === "waiting";
  return (
    <Show when={step()}>
      <div class={`agent-progress-line xs ${busy() ? "busy" : "faint"}`} title={step()}>
        <span class="ellipsis grow">{step()}</span>
        <Show when={p()?.checklist_total > 0}>
          <span class="agent-progress-count">{p().checklist_done}/{p().checklist_total}</span>
        </Show>
        <Show when={busy() && turnElapsed(p(), now())}>
          <span class="mono">{turnElapsed(p(), now())}</span>
        </Show>
      </div>
    </Show>
  );
}

/** Full card panel: step, checklist bar + items, elapsed, last activity, tools run. */
export function AgentProgressPanel(props) {
  const now = useTick();
  const p = () => agentProgress(props.agent);
  const [open, setOpen] = createSignal(false);
  const total = () => p()?.checklist_total ?? 0;
  const done = () => p()?.checklist_done ?? 0;
  const busy = () => props.agent?.state === "running" || props.agent?.state === "waiting";
  const last = () => Math.max(p()?.last_event_at ?? 0, props.agent?.last_active_at ?? 0);
  return (
    <Show when={p() || busy()}>
      <div class="agent-progress" onClick={(e) => e.stopPropagation()}>
        <div class="agent-progress-head xs">
          <Show when={busy()}><span class="agent-progress-pulse" /></Show>
          <span class={`ellipsis grow ${busy() ? "" : "faint"}`} title={currentStep(props.agent, p())}>{currentStep(props.agent, p())}</span>
          <Show when={turnElapsed(p(), now())}>
            <span class="faint" title={busy() ? "Time in this turn" : "Last turn took"}>{busy() ? "⏱" : "took"} <span class="mono">{turnElapsed(p(), now())}</span></span>
          </Show>
          <span class="faint" title="Last activity">{(now(), fmt.ago(last()))}</span>
          <Show when={p()?.tools_run}><span class="faint" title="Tool calls this turn">{p().tools_run} steps</span></Show>
        </div>
        <Show when={total() > 0}>
          <button class="agent-progress-plan xs" onClick={() => setOpen(!open())} aria-expanded={open()}>
            <span class="agent-progress-bar"><span style={{ width: `${Math.round((done() / total()) * 100)}%` }} /></span>
            <span>Plan {done()}/{total()}</span>
            <span class="faint">{open() ? "▾" : "▸"}</span>
          </button>
          <Show when={open()}>
            <ul class="agent-progress-list xs">
              <For each={p().checklist}>
                {(it) => <li class={it.done ? "done" : ""}><span>{it.done ? "☑" : "☐"}</span> {it.text}</li>}
              </For>
            </ul>
          </Show>
        </Show>
      </div>
    </Show>
  );
}
