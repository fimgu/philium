// Providers: accounts, trust level, guard mode, live model lists.
import { For, Show, createResource, createSignal, createMemo } from "solid-js";
import { Page, Button, Card, Badge, Empty, Field, Input, Select, Toggle, Segmented, Modal, Shield, toast, confirm } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";

const KINDS = [
  { value: "openai_chat", label: "OpenAI-compatible (chat/completions)", url: "https://api.example.com/v1" },
  { value: "openai_responses", label: "OpenAI Responses API", url: "https://api.openai.com/v1" },
  { value: "anthropic", label: "Anthropic Messages API", url: "https://api.anthropic.com/v1" },
  { value: "chatgpt", label: "ChatGPT subscription (Codex login)", sub: true },
  { value: "claude_code", label: "Claude subscription (Claude Code login)", sub: true },
];
const kindLabel = (k) => KINDS.find((x) => x.value === k)?.label ?? k;
const isSub = (k) => KINDS.find((x) => x.value === k)?.sub;

function ProviderForm(props) {
  const editing = !!props.provider && !props.provider.preset;
  const p = props.provider ?? {};
  const [kind, setKind] = createSignal(p.kind ?? "openai_chat");
  const [id, setId] = createSignal(p.id ?? "");
  const [name, setName] = createSignal(p.name ?? "");
  const [url, setUrl] = createSignal(p.base_url ?? "");
  const [key, setKey] = createSignal("");
  const [trusted, setTrusted] = createSignal(p.trusted ?? false);
  const [guard, setGuard] = createSignal(p.guard ?? "redact");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");
  const sub = () => isSub(kind());
  const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);

  const save = async () => {
    setBusy(true); setErr("");
    try {
      await rpc("providers.save", {
        create: !editing, id: id() || slug(name()), name: name(), kind: kind(), base_url: url(),
        api_key: key(), trusted: sub() ? true : trusted(), guard: guard(),
      });
      toast(`${name()} saved. Discovering models…`, "good");
      props.onClose(true);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title={editing ? `Edit ${p.name}` : "Add provider"} onClose={() => props.onClose(false)}
      footer={<><Button onClick={() => props.onClose(false)}>Cancel</Button><Button variant="primary" busy={busy()} onClick={save}>Save</Button></>}>
      <Field label="Type">
        <Select value={kind()} disabled={editing} onValue={(v) => { setKind(v); if (isSub(v)) { setTrusted(true); if (!name()) setName(v === "chatgpt" ? "ChatGPT" : "Claude"); } }} options={KINDS} />
      </Field>
      <div class="grid grid-2">
        <Field label="Name"><Input value={name()} onValue={(v) => { setName(v); if (!editing) setId(slug(v)); }} placeholder="e.g. Zhipu" /></Field>
        <Field label="ID" hint="Used in model refs: id/model"><Input mono value={id()} disabled={editing} onValue={setId} /></Field>
      </div>
      <Show when={!sub()} fallback={
        <Shield>
          <Icon name="shield" /> Uses the CLI's own sign-in on this Mac. Requests go straight to {kind() === "chatgpt" ? "OpenAI" : "Anthropic"}, not through the gateway, so subscriptions are always trusted.
        </Shield>}>
        <Field label="Base URL"><Input mono value={url()} onValue={setUrl} placeholder={KINDS.find((k) => k.value === kind())?.url} /></Field>
        <Field label="API key" hint={editing && p.has_key ? "Stored in the macOS Keychain. Leave empty to keep it." : "Stored only in the macOS Keychain. Agents never see it."}>
          <Input mono type="password" value={key()} onValue={setKey} placeholder={editing && p.has_key ? "••••••••" : "sk-…"} />
        </Field>
        <Field label="Trust">
          <div class="col">
            <Toggle tone="strict" on={!trusted()} onChange={(v) => setTrusted(!v)} label="Untrusted provider" />
            <span class="xs faint">Untrusted providers only run agents in Strict mode: OS sandbox, egress allowlist, no subscriptions, everything observed.</span>
          </div>
        </Field>
        <Field label="Secret guard" hint="Scans every outgoing request for keys, tokens and passwords.">
          <Segmented value={guard()} onChange={setGuard} options={[{ value: "redact", label: "Redact" }, { value: "block", label: "Block" }, { value: "off", label: "Off" }]} />
        </Field>
      </Show>
      <Show when={err()}><Shield tone="danger">{err()}</Shield></Show>
    </Modal>
  );
}

function ModelList(props) {
  const [models, { refetch }] = createResource(() => props.provider.id + props.version, () => rpc("providers.models", { provider: props.provider.id }));
  const [q, setQ] = createSignal("");
  const shown = createMemo(() => (models() ?? []).filter((m) => m.model_id.toLowerCase().includes(q().toLowerCase())));
  const setDefault = async (ref) => { await rpc("providers.set_default", { ref }); toast(`Default model: ${ref}`, "good"); props.onChange(); };
  const toggle = async (m) => { await rpc("providers.set_model", { provider: m.provider_id, model: m.model_id, enabled: !m.enabled }); refetch(); };
  return (
    <div class="col">
      <Show when={(models() ?? []).length > 8}><Input placeholder="Filter models…" value={q()} onValue={setQ} /></Show>
      <div style={{ "max-height": "320px", overflow: "auto" }}>
        <table class="table">
          <thead><tr><th>Model</th><th class="num">Context</th><th class="num">$ in / out per 1M</th><th /></tr></thead>
          <tbody>
            <For each={shown()}>
              {(m) => (
                <tr style={{ opacity: m.enabled ? 1 : 0.45 }}>
                  <td class="mono ellipsis" title={m.display ?? m.model_id}>{m.model_id} <Show when={props.defaultRef === m.ref}><Badge tone="accent">default</Badge></Show></td>
                  <td class="num">{m.context_window ? fmt.compact(m.context_window) : "–"}</td>
                  <td class="num">{m.price_in != null ? `${m.price_in.toFixed(2)} / ${m.price_out?.toFixed(2)}` : "–"}</td>
                  <td class="num">
                    <Button size="sm" variant="ghost" onClick={() => setDefault(m.ref)}>Make default</Button>
                    <Button size="sm" variant="ghost" onClick={() => toggle(m)}>{m.enabled ? "Hide" : "Show"}</Button>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </div>
  );
}

export default function Providers() {
  const [version, setVersion] = createSignal(0);
  const [list, { refetch }] = createResource(version, () => rpc("providers.list"));
  const [def, { refetch: refetchDef }] = createResource(version, () => rpc("providers.default"));
  const [logins] = createResource(() => rpc("providers.logins"));
  const [form, setForm] = createSignal(null);
  const [open, setOpen] = createSignal(null);
  const [busy, setBusy] = createSignal(null);
  const bump = () => setVersion((v) => v + 1);
  useEvent("providers:changed", bump);

  const refresh = async (p) => {
    setBusy(p.id);
    try { const m = await rpc("providers.refresh_models", { id: p.id }); toast(`${p.name}: ${m.length} models`, "good"); }
    catch (e) { toast(`${p.name}: ${e}`, "bad"); }
    finally { setBusy(null); bump(); }
  };
  const remove = async (p) => {
    if (!(await confirm(`Remove ${p.name}?`, "Its API key is deleted from the Keychain. Agents using it will stop working.", { danger: true, ok: "Remove" }))) return;
    await rpc("providers.remove", { id: p.id });
    bump();
  };

  return (
    <Page title="Providers" sub="Keys stay in the Keychain. Agents only ever get a local gateway token."
      actions={<Button variant="primary" onClick={() => setForm({})}><Icon name="plus" /> Add provider</Button>}>
      <Show when={list() && list().length === 0}>
        <Empty icon={<Icon name="key" size={32} />} title="No providers yet"
          action={<div class="row"><Button variant="primary" onClick={() => setForm({})}>Add an API provider</Button>
            <Show when={logins()?.chatgpt}><Button onClick={() => setForm({ preset: "chatgpt" })}>Use ChatGPT login</Button></Show>
            <Show when={logins()?.claude_code}><Button onClick={() => setForm({ preset: "claude_code" })}>Use Claude login</Button></Show></div>}>
          Add an API key for any provider, or use the ChatGPT or Claude subscription you're already signed into. Models are discovered live.
        </Empty>
      </Show>
      <div class="col" style={{ gap: "12px" }}>
        <For each={list()}>
          {(p) => (
            <Card>
              <div class="card-head">
                <span class={`dot ${!p.enabled ? "" : p.models_error ? "bad" : "live"}`} />
                <h2>{p.name}</h2>
                <span class="mono faint small">{p.id}</span>
                <Show when={p.trusted} fallback={<Badge tone="strict" title="Agents on this provider always run in Strict mode"><Icon name="lock" size={12} /> Untrusted · Strict only</Badge>}>
                  <Badge tone="secure"><Icon name="shield" size={12} /> Trusted</Badge>
                </Show>
                <Show when={p.via_gateway} fallback={<Badge>Subscription login</Badge>}>
                  <Badge tone={p.guard === "off" ? "danger" : "secure"}>Guard: {p.guard}</Badge>
                </Show>
                <Show when={p.via_gateway && !p.has_key}><Badge tone="danger">No API key</Badge></Show>
                <div style={{ "margin-left": "auto" }} class="row">
                  <Button size="sm" busy={busy() === p.id} onClick={() => refresh(p)}><Icon name="refresh" size={13} /> Refresh models</Button>
                  <Button size="sm" onClick={() => setForm({ provider: p })}>Edit</Button>
                  <Button size="sm" variant="danger" onClick={() => remove(p)}>Remove</Button>
                </div>
              </div>
              <div class="row small dim">
                <span>{kindLabel(p.kind)}</span>
                <Show when={p.base_url}><span class="mono faint">· {p.base_url}</span></Show>
                <span>· {p.model_count} models</span>
                <Show when={p.models_cached_at}><span class="faint">· updated {fmt.ago(p.models_cached_at)}</span></Show>
                <Button size="sm" variant="ghost" style={{ "margin-left": "auto" }} onClick={() => setOpen(open() === p.id ? null : p.id)}>{open() === p.id ? "Hide models" : "Show models"}</Button>
              </div>
              <Show when={p.models_error}><div style={{ "margin-top": "10px" }}><Shield tone="danger">Model discovery failed: {p.models_error}</Shield></div></Show>
              <Show when={open() === p.id}><div style={{ "margin-top": "12px" }}><ModelList provider={p} version={version()} defaultRef={def()} onChange={() => { refetchDef(); bump(); }} /></div></Show>
            </Card>
          )}
        </For>
      </div>
      <Show when={form()}>
        {(f) => <ProviderForm provider={f().provider ?? (f().preset ? { preset: true, kind: f().preset, trusted: true, name: f().preset === "chatgpt" ? "ChatGPT" : "Claude", id: f().preset === "chatgpt" ? "gpt" : "claude" } : null)}
          onClose={(saved) => { setForm(null); if (saved) { refetch(); bump(); } }} />}
      </Show>
    </Page>
  );
}
