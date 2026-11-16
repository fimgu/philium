// Chat: quick conversations with any model, no tools. Runs on the desk HAL
// behind the gateway. For real work, start an agent instead.
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup } from "solid-js";
import { Page, Button, Select, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { Conversation, Composer } from "../ui/conversation.jsx";
import { rpc, hal, fmt } from "../lib/rpc.js";
import { go, params } from "../lib/nav.js";
import "../styles/agents.css";

const TERMINAL = new Set(["assistant.completed", "turn.failed", "turn.cancelled"]);

export default function Chat() {
  const [sessions, { refetch }] = createResource(() =>
    hal("GET", "/sessions").then((l) => (l ?? []).filter((s) => !s.parent_id)).catch(() => []));
  const [models] = createResource(() => rpc("providers.models", {}).catch(() => []));
  const [providers] = createResource(() => rpc("providers.list").catch(() => []));
  const [def] = createResource(() => rpc("providers.default").catch(() => null));
  const [model, setModel] = createSignal("");
  const [events, setEvents] = createSignal([]);
  const [busy, setBusy] = createSignal(false);
  const selected = () => params().session ?? sessions()?.[0]?.id;
  const current = () => (sessions() ?? []).find((s) => s.id === selected());

  const provById = createMemo(() => Object.fromEntries((providers() ?? []).map((p) => [p.id, p])));
  const modelOptions = createMemo(() => (models() ?? []).filter((m) => m.enabled !== 0).map((m) => ({
    value: m.ref,
    label: `${m.provider_name} / ${m.display || m.model_id}${provById()[m.provider_id]?.trusted === false ? "  (untrusted)" : ""}`,
  })));
  const chosen = () => model() || current()?.model || def() || "";
  const untrusted = () => provById()[(chosen() || "").split("/")[0]]?.trusted === false;

  let after = 0;
  let timer;
  const poll = async (id) => {
    clearTimeout(timer);
    try {
      const evs = await hal("GET", `/sessions/${id}/events?after=${after}`);
      if (id !== selected()) return;
      if (evs?.length) {
        after = evs.at(-1).seq;
        setEvents((x) => x.concat(evs));
        if (evs.some((e) => TERMINAL.has(e.type))) setBusy(false);
        if (evs.some((e) => e.type === "turn.started")) setBusy(true);
      }
    } catch { /* desk restarting */ }
    if (busy()) timer = setTimeout(() => poll(id), 400);
  };
  createEffect(on(selected, (id) => {
    clearTimeout(timer); setEvents([]); after = 0; setBusy(false);
    if (id) poll(id);
  }));
  onCleanup(() => clearTimeout(timer));

  const newChat = async () => {
    try {
      const s = await rpc("app.chat_create", { model: chosen() || null, title: "Chat" });
      await refetch();
      go("chat", { session: s.id });
    } catch (e) { toast(String(e), "danger"); }
  };

  const send = async (text) => {
    let id = selected();
    if (!id) {
      try {
        const s = await rpc("app.chat_create", { model: chosen() || null, title: text.slice(0, 60) });
        id = s.id; await refetch(); go("chat", { session: id });
      } catch (e) { return toast(String(e), "danger"); }
    } else if (model() && model() !== current()?.model) {
      await hal("PATCH", `/sessions/${id}`, { model: model() }).catch((e) => toast(String(e), "danger"));
    }
    setBusy(true);
    try {
      await hal("POST", `/sessions/${id}/messages`, { text });
    } catch (e) { setBusy(false); return toast(String(e), "danger"); }
    poll(id);
  };

  const remove = async (id) => {
    await hal("DELETE", `/sessions/${id}`).catch((e) => toast(String(e), "danger"));
    await refetch();
    if (selected() === id) go("chat", {});
  };

  return (
    <Page title="Chat" flush sub="Conversation only. No tools, no files. Start an agent for real work."
      actions={<>
        <div style={{ width: "320px" }}><Select value={chosen()} onValue={setModel} options={modelOptions()} placeholder="Model" /></div>
        <Button variant="primary" onClick={newChat}><Icon name="plus" size={12} /> New chat</Button>
      </>}>
      <div class="split">
        <div class="agent-list">
          <Show when={(sessions() ?? []).length} fallback={<div class="small faint" style={{ padding: "16px" }}>No chats yet.</div>}>
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
          <Show when={untrusted()}>
            <div class="shield strict small" style={{ margin: "8px 16px 0" }}>
              <Icon name="shield" size={12} /> Untrusted provider. Everything you type here goes through the gateway, which removes secrets it recognizes, but assume the provider can read the rest.
            </div>
          </Show>
          <Conversation events={events()} emptyText="Ask anything. This chat has no access to files, commands or the web." />
          <Composer onSend={send} busy={busy()} onCancel={() => hal("POST", `/sessions/${selected()}/cancel`)}
            placeholder={`Message ${chosen() || "a model"}`} />
        </div>
      </div>
    </Page>
  );
}
