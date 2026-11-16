// Providers: accounts, trust level, guard mode, live model lists.
import { For, Show, createResource, createSignal, createMemo } from "solid-js";
import { Page, Button, Card, Badge, Empty, Field, Input, Select, Toggle, Segmented, Modal, Shield, toast, confirm } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";

const KINDS = [
  { value: "openai_chat", label: "OpenAI-compatible (chat/completions)", url: "https://api.example.com/v1" },
  { value: "openai_responses", label: "OpenAI Responses API", url: "https://api.openai.com/v1" },
  { value: "anthropic", label: "Anthropic Messages API", url: "https://api.anthropic.com/v1" },
];
const SUB_LABEL = { chatgpt_oauth: "ChatGPT subscription (signed in through Philium)", claude_oauth: "Claude subscription (signed in through Philium)" };
const kindLabel = (k) => SUB_LABEL[k] ?? KINDS.find((x) => x.value === k)?.label ?? k;
const isSub = (k) => k in SUB_LABEL;

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
        <Select value={kind()} disabled={editing} onValue={(v) => { setKind(v); }} options={KINDS} />
      </Field>
      <div class="grid grid-2">
        <Field label="Name"><Input value={name()} onValue={(v) => { setName(v); if (!editing) setId(slug(v)); }} placeholder="e.g. Zhipu" /></Field>
        <Field label="ID" hint="Used in model refs: id/model"><Input mono value={id()} disabled={editing} onValue={setId} /></Field>
      </div>
      <Show when={!sub()}>
        <Field label="Base URL"><Input mono value={url()} onValue={setUrl} placeholder={KINDS.find((k) => k.value === kind())?.url} /></Field>
        <Field label="API key" hint={editing && p.has_key ? "Saved in Philium's private vault. Leave empty to keep it." : "Saved in Philium's private vault, which agents cannot read."}>
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

function SignIn(props) {
  const claude = props.kind === "claude_oauth";
  const [st, setSt] = createSignal(null);
  const [paste, setPaste] = createSignal("");
  const [err, setErr] = createSignal("");
  const [importable, setImportable] = createSignal(false);
  if (claude) rpc("providers.oauth_import_check").then((r) => setImportable(!!r.available)).catch(() => {});
  useEvent("oauth:done", (e) => {
    const d = e?.payload ?? e;
    if (!st() || (d.id && st().id !== d.id)) return;
    setSt(null);
    if (d.ok) { toast("Signed in", "good"); props.onDone(); } else setErr(String(d.error ?? "Sign-in failed"));
  });
  const start = async () => {
    setErr("");
    try { setSt(await rpc("providers.oauth_start", { kind: props.kind })); } catch (e) { setErr(String(e)); }
  };
  const submit = async () => {
    try { await rpc("providers.oauth_paste", { id: st().id, input: paste() }); setSt(null); setPaste(""); props.onDone(); }
    catch (e) { setErr(String(e)); }
  };
  const cancel = async () => { await rpc("providers.oauth_cancel", { kind: props.kind }).catch(() => {}); setSt(null); };
  const doImport = async () => {
    if (!(await confirm("Import your Claude Code login?", "Philium will read the Claude Code credentials stored on this Mac (macOS may ask for Keychain access) and keep its own copy. Nothing is changed in Claude Code.", { ok: "Import" }))) return;
    try { await rpc("providers.oauth_import"); toast("Imported", "good"); props.onDone(); } catch (e) { setErr(String(e)); }
  };
  return (
    <div class="col" style={{ gap: "8px" }}>
      <Show when={!st()} fallback={
        <div class="col" style={{ gap: "8px" }}>
          <Show when={claude}>
            <div class="small">Finish in your browser. It completes here by itself. If it does not, paste the code or the full callback URL:</div>
            <div class="row"><Input mono value={paste()} onValue={setPaste} placeholder="code#state or callback URL" />
              <Button variant="primary" onClick={submit} disabled={!paste().trim()}>Submit</Button></div>
            <div class="xs faint">Different browser? <a href="#" onClick={(e) => { e.preventDefault(); navigator.clipboard?.writeText(st().manual_url); toast("Link copied", "good"); }}>Copy the sign-in link</a>, finish there, then paste the code above.</div>
          </Show>
          <Show when={!claude}>
            <div class="small">Open <span class="mono">{st().verify_url}</span> and enter this code:</div>
            <div class="mono" style={{ "font-size": "28px", "letter-spacing": "4px" }}>{st().user_code}</div>
            <div class="xs faint">Waiting for you to approve. This page updates automatically.</div>
          </Show>
          <div><Button size="sm" onClick={cancel}>Cancel</Button></div>
        </div>}>
        <div class="row">
          <Button variant="primary" onClick={start}>{claude ? "Sign in with Claude" : "Sign in with ChatGPT"}</Button>
          <Show when={claude && importable()}><Button onClick={doImport}>Import existing Claude Code login</Button></Show>
        </div>
      </Show>
      <Show when={err()}><Shield tone="danger">{err()}</Shield></Show>
    </div>
  );
}

export default function Providers() {
  const [version, setVersion] = createSignal(0);
  const [list, { refetch }] = createResource(version, () => rpc("providers.list"));
  const [def, { refetch: refetchDef }] = createResource(version, () => rpc("providers.default"));
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
    if (!(await confirm(`Remove ${p.name}?`, "Its API key is deleted from Philium's vault. Agents using it will stop working.", { danger: true, ok: "Remove" }))) return;
    await rpc("providers.remove", { id: p.id });
    bump();
  };

  return (
    <Page title="Providers" sub="Keys stay in Philium's private vault. Agents only ever get a local gateway token."
      actions={<Button variant="primary" onClick={() => setForm({})}><Icon name="plus" /> Add provider</Button>}>
      <Show when={list() && list().length === 0}>
        <Empty icon={<Icon name="key" size={32} />} title="No providers yet"
          action={<Button variant="primary" onClick={() => setForm({})}>Add an API provider</Button>}>
          Add an API key for any provider, or sign in with a Claude or ChatGPT subscription below. Models are discovered live.
        </Empty>
      </Show>
      <div class="col" style={{ gap: "12px" }}>
        <For each={[["claude_oauth", "Claude"], ["chatgpt_oauth", "ChatGPT"]].filter(([k]) => !(list() ?? []).some((p) => p.kind === k && p.signed_in))}>
          {([k, label]) => (
            <Card>
              <div class="card-head"><h2>{label} subscription</h2>
                <Show when={(list() ?? []).some((p) => p.kind === k)}><Badge tone="danger">Sign in again</Badge></Show></div>
              <Show when={k === "claude_oauth"}>
                <div class="small dim" style={{ "margin-bottom": "8px" }}>Using a Claude subscription outside Anthropic's own apps may be billed separately or flagged by Anthropic. An Anthropic API key (add it as a provider) is the risk-free alternative.</div>
              </Show>
              <SignIn kind={k} onDone={() => { refetch(); bump(); }} />
            </Card>
          )}
        </For>
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
                <Badge tone={p.guard === "off" ? "danger" : "secure"}>Guard: {p.guard}</Badge>
                <Show when={isSub(p.kind)}><Badge tone={p.signed_in ? "secure" : "danger"}>{p.signed_in ? [p.account?.email, p.account?.plan].filter(Boolean).join(" · ") || "Signed in" : "Sign in again"}</Badge></Show>
                <Show when={!isSub(p.kind) && !p.has_key}><Badge tone="danger">No API key</Badge></Show>
                <div style={{ "margin-left": "auto" }} class="row">
                  <Button size="sm" busy={busy() === p.id} onClick={() => refresh(p)}><Icon name="refresh" size={13} /> Refresh models</Button>
                  <Show when={isSub(p.kind) && p.signed_in}><Button size="sm" onClick={async () => { await rpc("providers.oauth_signout", { id: p.id }); bump(); }}>Sign out</Button></Show>
                  <Button size="sm" onClick={() => setForm({ provider: p })}>Edit</Button>
                  <Button size="sm" variant="danger" onClick={() => remove(p)}>Remove</Button>
                </div>
              </div>
              <div class="row small dim">
                <span>{kindLabel(p.kind)}</span>
                <Show when={p.base_url && !isSub(p.kind)}><span class="mono faint">· {p.base_url}</span></Show>
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
        {(f) => <ProviderForm provider={f().provider ?? null}
          onClose={(saved) => { setForm(null); if (saved) { refetch(); bump(); } }} />}
      </Show>
    </Page>
  );
}
