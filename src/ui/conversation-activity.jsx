import { Show, createMemo, createSignal, createEffect, on, onCleanup } from "solid-js";
import { conversationActivity } from "../lib/conversation-activity.js";
import "./conversation-activity.css";

export function ConversationActivity(props) {
  const activity = createMemo(() => conversationActivity(props.events, props.busy, props.readOnly));
  const [seconds, setSeconds] = createSignal(0);
  const key = createMemo(() => activity()?.key ?? null);
  createEffect(on(key, value => {
    setSeconds(0);
    if (!value) return;
    const start = Date.now();
    const timer = setInterval(() => setSeconds(Math.floor((Date.now() - start) / 1000)), 1000);
    onCleanup(() => clearInterval(timer));
  }));
  return <Show when={activity()}>{state => <div class="conversation-activity" data-activity={state().kind}>
    <span class="activity-dot" aria-hidden="true" />
    <span role="status" title={state().detail}>{state().label}</span>
    <span class="activity-elapsed" title="Time observed in this activity state, not total execution time">{seconds()}s</span>
    <Show when={state().kind === "waiting" && seconds() >= 15}><span class="activity-hint">Still waiting. You can stop this turn.</span></Show>
  </div>}</Show>;
}
