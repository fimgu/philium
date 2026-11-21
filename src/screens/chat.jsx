// Chat: quick conversations with any model, no tools. Runs on the desk HAL
// behind the gateway. For real work, start an agent instead.
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup } from "solid-js";
import { NotFound, Page, Button, ModelPicker, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { Conversation, Composer, KvTelemetryBar, speedEffort, effortSpeed } from "../ui/conversation.jsx";
import { rpc, hal, fmt, sendError } from "../lib/rpc.js";
import { go, params } from "../lib/nav.js";
import { useStore } from "../lib/store.js";
import { useScreenVisible } from "../lib/visibility.js";
import { createSessionPoller } from "../lib/session-poller.js";
import "../styles/agents.css";

const TERMINAL = new Set(["assistant.completed", "turn.failed", "turn.cancelled"]);

export default function Chat() {
  const store = useStore();
  const screenVisible = useScreenVisible();
  const models = store.models;
  const providers = store.providers;
  const def = store.defaultModel;
  const [sessions, { refetch }] = createResource(() =>
    hal("GET", "/sessions").then((l) => (l ?? []).filter((s) => !s.parent_id)).catch(() => []));
  const [model, setModel] = createSignal("");
  const [events, setEvents] = createSignal([]);
  const [busy, setBusy] = createSignal(false);
  const selected = () => params().session ?? sessions()?.[0]?.id;
  const activeId = () => selected() || "new";
  const current = () => (sessions() ?? []).find((s) => s.id === selected());

  const provById = createMemo(() => Object.fromEntries((providers() ?? []).map((p) => [p.id, p])));
  const chosen = () => model() || current()?.model || def() || "";
  const activeModel = createMemo(() => (models() ?? []).find((m) => m.ref === chosen()));
  const untrusted = () => provById()[(chosen() || "").split("/")[0]]?.trusted === false;

  const sessionKv = createMemo(() => {
    let reqs = 0, inp = 0, cached = 0, out = 0;
    for (const ev of events()) {
      const u = ev?.data?.usage;
      if (u && (u.input_tokens || u.output_tokens || u.prompt_tokens)) {
        reqs += 1;
        inp += Number(u.input_tokens || u.prompt_tokens || 0);
        cached += Number(u.cached_tokens || u.cache_read_input_tokens || 0);
        out += Number(u.output_tokens || u.completion_tokens || 0);
      }
    }
    return { requests: reqs, input_tokens: inp, cached_tokens: cached, output_tokens: out };
  });

  const changeChatModel = async (newRef) => {
    setModel(newRef);
    const id = selected();
    if (id && newRef && newRef !== current()?.model) {
      try {
        await hal("PATCH", `/sessions/${id}`, { model: newRef });
        await refetch();
        toast(`Switched chat model to ${newRef} (0 tokens used)`, "secure");
      } catch (e) {
        toast(String(e), "danger");
        return false;
      }
    }
    return true;
  };

  const [pickModel, setPickModel] = createSignal(0);
  const handleSlashModel = async (query) => {
    if (!query) {
      // Bare /model opens the picker; /model <name> still switches straight away.
      setPickModel((n) => n + 1);
      return true;
    }
    const q = query.toLowerCase();
    const match =
      (models() ?? []).find((m) => m.enabled !== 0 && (m.ref.toLowerCase() === q || m.model_id.toLowerCase() === q)) ||
      (models() ?? []).find((m) => m.enabled !== 0 && `${m.provider_name || ""} ${m.display || ""} ${m.ref}`.toLowerCase().includes(q));
    if (!match) {
      toast(`No model matching "${query}"`, "danger");
      return false;
    }
    return await changeChatModel(match.ref);
  };

  const poller = createSessionPoller({
    fetchEvents: (id, after) => hal("GET", `/sessions/${id}/events?after=${after}`),
    isBusy: () => busy() && (typeof screenVisible === "function" ? screenVisible() : true),
    onEvents(evs) {
      setEvents((x) => x.concat(evs));
      for (const event of evs) {
        if (event.type === "turn.started") setBusy(true);
        else if (TERMINAL.has(event.type)) setBusy(false);
      }
    },
  });
  const poll = (id) => poller.poll(id);
  createEffect(on(selected, (id) => {
    setEvents([]); setBusy(false);
    poller.select(id);
  }));
  createEffect(() => {
    if (typeof screenVisible === "function" && screenVisible() && busy()) {
      const id = selected();
      if (id) poller.poll(id);
    }
  });
  onCleanup(() => poller.dispose());

  const newChat = async () => {
    try {
      const s = await rpc("app.chat_create", { model: chosen() || null, title: "Chat" });
      await refetch();
      go("chat", { session: s.id });
    } catch (e) { toast(String(e), "danger"); }
  };

  const send = async (text, images = []) => {
    let id = selected();
    if (!id) {
      try {
        const s = await rpc("app.chat_create", { model: chosen() || null, title: text.slice(0, 60) }, { action: true });
        id = s.id; await refetch(); go("chat", { session: id });
      } catch (e) { toast(String(e), "danger"); return false; }
    } else if (model() && model() !== current()?.model) {
      try { await hal("PATCH", `/sessions/${id}`, { model: model() }); }
      catch (e) { toast(String(e), "danger"); return false; }
    }
    setBusy(true);
    try {
      await hal("POST", `/sessions/${id}/messages`, { text, images });
    } catch (e) { setBusy(false); toast(sendError(e), "danger"); return false; }
    poll(id);
    return true;
  };

  const setSpeed = async (v) => {
    const id = selected();
    if (!id) return toast("Send a message first, then pick a speed", "info");
    try { await hal("PATCH", `/sessions/${id}`, { effort: speedEffort(v) }); await refetch(); } catch (e) { toast(String(e), "danger"); }
  };

  const remove = async (id) => {
    await hal("DELETE", `/sessions/${id}`).catch((e) => toast(String(e), "danger"));
    await refetch();
    if (selected() === id) go("chat", {});
  };

  return (
    <Page title="Chat" flush sub="Conversation only. No tools, no files. Start an agent for real work."
      actions={<>
        <ModelPicker
          compact
          value={chosen()}
          onValue={changeChatModel}
          models={models() ?? []}
          providers={providers() ?? []}
          allowUntrusted={true}
          currentProvider={(chosen() || "").split("/")[0]}
          openRequest={pickModel()}
          onDismiss={() => {
            const focus = () => document.querySelector(".screen-pane:not(.hidden) textarea.composer-input:not(:disabled)")?.focus();
            focus();
            for (const ms of [150, 500, 1200]) setTimeout(() => { const el = document.activeElement; if (!el || el === document.body) focus(); }, ms);
          }}
        />
        <Button variant="primary" onClick={newChat}><Icon name="plus" size={12} /> New chat</Button>
      </>}>
      <div class="split">
        <div class="agent-list">
          <Show when={(sessions() ?? []).length} fallback={<div class="small faint" style={{ padding: "16px", "line-height": "1.5" }}>No chats yet. Start one with New chat.</div>}>
            <For each={sessions()}>
              {(s) => (
                <div class={`list-item ${s.id === selected() ? "on" : ""}`} onClick={() => go("chat", { session: s.id })}>
                  <div class="grow" style={{ "min-width": 0 }}>
                    <div class="small ellipsis">{s.title || "Chat"}</div>
                    <div class="xs faint ellipsis">{s.model}</div>
                  </div>
                  <span class="xs faint">{fmt.ago(Date.parse(s.updated_at ?? s.created_at))}</span>
                  <button class="btn ghost sm" title="Delete chat" onClick={(e) => { e.stopPropagation(); remove(s.id); }}><Icon name="trash" size={11} /></button>
                </div>
              )}
            </For>
          </Show>
        </div>
        <div class="agent-view">
          <div style={{ padding: "8px 16px 0" }}>
            <KvTelemetryBar kv={sessionKv()} compact />
          </div>
          <Show when={untrusted()}>
            <div class="shield strict small" style={{ margin: "8px 16px 0" }}>
              <Icon name="shield" size={12} /> Untrusted provider. Everything you type here goes through the gateway, which removes secrets it recognizes, but assume the provider can read the rest.
            </div>
          </Show>
          <Show when={params().session && sessions.state === "ready" && !current()}>
            <NotFound title="Chat not found" action={<Button onClick={() => go("chat", {})}>Open latest chat</Button>}>This chat was deleted.</NotFound>
          </Show>
          <Conversation hidden={!!(params().session && sessions.state === "ready" && !current())} events={events()} busy={busy()} emptyText="Ask anything. This chat has no access to files, commands or the web." />
          <Composer draftKey={`chat:${activeId()}`} onSend={send} busy={busy()} onCancel={() => hal("POST", `/sessions/${selected()}/cancel`).catch((e) => toast(String(e), "danger"))}
            effort={current()?.effort || "default"} speed={effortSpeed(current()?.effort)} effortOptions={activeModel()?.effort_details ?? []} onEffort={setSpeed} onSpeed={setSpeed} onSlashModel={handleSlashModel}
            placeholder={`Message ${chosen() || "a model"} (or /effort, /model)`} />
        </div>
      </div>
    </Page>
  );
}
