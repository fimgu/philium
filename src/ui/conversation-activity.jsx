import { Show, createMemo, createSignal, createEffect, on, onCleanup } from "solid-js";
import { conversationActivity } from "../lib/conversation-activity.js";
import { useEvent } from "../lib/rpc.js";
import "./conversation-activity.css";

// props.agent: agent name whose gateway stream feeds this view; null/undefined = desk chat.
export function ConversationActivity(props) {
  const [stream, setStream] = createSignal(null);
  const [lastByteAt, setLastByteAt] = createSignal(Date.now());
  const [now, setNow] = createSignal(Date.now());
  useEvent("gateway:progress", p => {
    if ((p?.agent ?? null) !== (props.agent ?? null)) return;
    setStream(p);
    if (!p.stalled) setLastByteAt(Date.now());
  });
  // Any session event also counts as liveness.
  createEffect(on(() => props.events?.length, () => setLastByteAt(Date.now())));
  createEffect(on(() => props.busy, b => { if (!b) setStream(null); setLastByteAt(Date.now()); }));
  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));
  const activity = createMemo(() => conversationActivity(props.events, props.busy, props.readOnly, { stream: stream(), now: now(), lastByteAt: lastByteAt() }));
  const [startedAt, setStartedAt] = createSignal(Date.now());
  const turnKey = createMemo(() => activity()?.key?.split(":")[0] ?? null);
  createEffect(on(turnKey, () => setStartedAt(Date.now())));
  const seconds = () => Math.max(0, Math.floor((now() - startedAt()) / 1000));
  return <Show when={activity()}>{state => <div class="conversation-activity" data-activity={state().kind}>
    <span class="activity-dot" aria-hidden="true" />
    <span role="status" title={state().detail}>{state().label}</span>
    <Show when={state().meter}><span class="activity-meter" title="Output received so far in this model request">{state().meter}</span></Show>
    <span class="activity-elapsed" title="Time since this turn started">{seconds()}s</span>
    <Show when={state().hint}><span class="activity-hint">{state().hint}</span></Show>
  </div>}</Show>;
}
