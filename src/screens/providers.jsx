// Providers: accounts, trust level, guard mode, live model lists.
import { For, Show, createResource, createSignal, createMemo, createEffect, on, onCleanup } from "solid-js";
import { Page, Button, Card, Badge, Empty, Field, Input, Select, Toggle, Modal, Shield, Menu, toast, confirm } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { useStore } from "../lib/store.js";
import { quotaView } from "../lib/quota.js";
import { go } from "../lib/nav.js";
import { AccountUsageLink } from "../ui/account-usage-link.jsx";
import { AccountUsagePill } from "../ui/account-usage.jsx";
import { refreshAccountUsage, usageFor } from "../lib/account-usage.js";
import { useScreenVisible } from "../lib/visibility.js";

function QuotaMeter(props) {
  const v = () => quotaView(props.usage);
  return (
    <Show when={v().rows.length} fallback={<span class="small dim">{props.empty}</span>}>
      <div class="row small dim" style={{ gap: "10px", "flex-wrap": "wrap" }}>
        <For each={v().rows}>
          {(r) => (
            <span title={r.title} class="row" style={{ gap: "4px" }}>
              <span class="mono">{r.label}</span>
              <span style={{ width: "48px", height: "4px", background: "var(--line, #8883)", "border-radius": "2px", overflow: "hidden", display: "inline-block" }}>
                <span style={{ width: `${r.pct}%`, height: "100%", display: "block", background: r.near ? "var(--warn, #c90)" : "var(--accent, #6a8)" }} />
              </span>
              <span>{r.pct}%</span>
              <Show when={r.reset}><span class="faint">{r.reset}</span></Show>
            </span>
          )}
        </For>
      </div>
      <Show when={v().warning}><div class="small" style={{ color: "var(--warn, #c90)" }}>{v().warning}</div></Show>
    </Show>
  );
}

const KINDS = [
  { value: "openai_chat", label: "OpenAI-compatible (chat/completions)", url: "https://api.example.com/v1" },
  { value: "openai_responses", label: "OpenAI Responses API", url: "https://api.openai.com/v1" },
  { value: "anthropic", label: "Anthropic Messages API", url: "https://api.anthropic.com/v1" },
];
const SUB_LABEL = {
  chatgpt_oauth: "ChatGPT subscription (signed in through Philium)",
  claude_oauth: "Claude subscription (signed in through Philium)",
  antigravity_oauth: "Google Antigravity subscription (signed in through Philium)",
};
const SUB_NOTICE = {
  claude_oauth: {
    short: "Third-party OAuth may be flagged by Anthropic; an API key is lower risk.",
    detail: "Using a Claude subscription outside Anthropic's own apps may be billed separately or flagged by Anthropic. An Anthropic API key (add it as a provider) is the risk-free alternative.",
  },
  chatgpt_oauth: {
    short: "Third-party OAuth may be rate-limited by OpenAI; an API key is lower risk.",
    detail: "Using a ChatGPT subscription outside OpenAI's own apps may be rate-limited or flagged by OpenAI. An OpenAI API key (add it as a provider) is the risk-free alternative.",
  },
  antigravity_oauth: {
    short: "Third-party OAuth violates Google's terms; a Gemini API key is lower risk.",
    detail: "Gives agents Gemini, and Claude through Google, on your Antigravity plan. Using Antigravity's sign-in outside Google's own apps breaks Google's terms, and Google has suspended accounts for it. A Gemini API key, added as a provider, carries no such risk.",
  },
};
const SUBSCRIPTIONS = [["claude_oauth", "Claude"], ["chatgpt_oauth", "ChatGPT"], ["antigravity_oauth", "Google Antigravity"]];
const kindLabel = (k) => SUB_LABEL[k] ?? KINDS.find((x) => x.value === k)?.label ?? k;
const isSub = (k) => k in SUB_LABEL;
const needsReauth = (p) =>
  isSub(p?.kind) &&
  (!p.signed_in || /sign-in|invalid_grant|refresh token|expired|unauthorized|401|re-?auth/i.test(String(p.models_error ?? "")));

function ProviderForm(props) {
  const editing = !!props.provider && !props.provider.preset;
  const p = props.provider ?? {};
  const initialKind = p.kind ?? "openai_chat";
  const initialTrusted = isSub(initialKind) ? true : Boolean(p.trusted ?? false);
  const [kind, setKind] = createSignal(initialKind);
  const [id, setId] = createSignal(p.id ?? "");
  const [name, setName] = createSignal(p.name ?? "");
  const [url, setUrl] = createSignal(p.base_url ?? "");
  const [key, setKey] = createSignal("");
  const [trusted, setTrusted] = createSignal(initialTrusted);
  const [guard, setGuard] = createSignal(!initialTrusted && p.guard === "off" ? "redact" : (p.guard ?? "redact"));
  const [enabled, setEnabled] = createSignal(p.enabled !== false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");
  const sub = () => isSub(kind());
  const untrusted = () => !sub() && !trusted();
  const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);

  createEffect(() => {
    if (untrusted() && guard() === "off") {
      setGuard("redact");
    }
  });

  const save = async () => {
    setBusy(true); setErr("");
    const effTrusted = sub() ? true : trusted();
    const effGuard = !effTrusted && guard() === "off" ? "redact" : guard();
    try {
      await rpc("providers.save", {
        create: !editing,
        id: id() || slug(name()),
        name: name(),
        kind: kind(),
        base_url: url(),
        api_key: key(),
        trusted: effTrusted,
        guard: effGuard,
        enabled: enabled(),
        default_model: p.default_model,
      });
      toast(`${name()} saved. Discovering models…`, "good");
      props.onClose(true);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  };

  return (
    <Modal title={editing ? `Edit ${p.name}` : "Add provider"} onClose={() => props.onClose(false)}
      footer={<><Button onClick={() => props.onClose(false)}>Cancel</Button><Button variant="primary" busy={busy()} onClick={save}>Save</Button></>}>
      <Field label="Type">
        <Select
          value={kind()}
          disabled={editing || sub()}
          onValue={(v) => setKind(v)}
          options={sub() ? [{ value: kind(), label: kindLabel(kind()) }] : KINDS}
        />
      </Field>
      <div class="grid grid-2">
        <Field label="Name"><Input value={name()} onValue={(v) => { setName(v); if (!editing) setId(slug(v)); }} placeholder="e.g. Zhipu" /></Field>
        <Field label="ID" hint="Used in model refs: id/model"><Input mono value={id()} disabled={editing} onValue={setId} /></Field>
      </div>
      <Show when={!sub()}>
        <Field label="Base URL"><Input mono value={url()} onValue={setUrl} placeholder={KINDS.find((k) => k.value === kind())?.url} /></Field>
        <Field label="API key" hint={editing && p.has_key ? "Stored in local vault. Leave empty to keep." : "Stored in local vault (unreadable by agents)."}>
          <Input mono type="password" value={key()} onValue={setKey} placeholder={editing && p.has_key ? "••••••••" : "sk-…"} />
        </Field>
        <Field label="Trust">
          <div class="col">
            <Toggle
              tone="strict"
              on={!trusted()}
              onChange={(v) => {
                setTrusted(!v);
                if (v && guard() === "off") setGuard("redact");
              }}
              label="Untrusted provider"
            />
            <span class="xs faint" title="Untrusted providers always run in Strict mode with OS sandbox, host allowlist, and full request logging.">Runs agents in Strict mode only.</span>
          </div>
        </Field>
      </Show>
      <Field
        label="Secret guard"
        hint={
          untrusted()
            ? "Scans outgoing requests for secrets. Required for untrusted providers."
            : "Scans outgoing requests for keys, tokens, and passwords."
        }
      >
        <div class="segmented" role="group" aria-label="Secret guard">
          <button type="button" class={guard() === "redact" ? "on" : ""} onClick={() => setGuard("redact")}>Redact</button>
          <button type="button" class={guard() === "block" ? "on" : ""} onClick={() => setGuard("block")}>Block</button>
          <button
            type="button"
            class={guard() === "off" ? "on" : ""}
            disabled={untrusted()}
            title={untrusted() ? "Untrusted providers require Redact or Block" : undefined}
            onClick={() => { if (!untrusted()) setGuard("off"); }}
          >
            Off
          </button>
        </div>
      </Field>
      <Field label="Status">
        <Toggle on={enabled()} onChange={setEnabled} label="Provider enabled" />
      </Field>
      <Show when={err()}><Shield tone="danger">{err()}</Shield></Show>
    </Modal>
  );
}

function ModelList(props) {
  const [models, { refetch }] = createResource(
    () => `${props.provider.id}:${props.provider.models_cached_at ?? 0}:${props.provider.model_count ?? 0}`,
    () => rpc("providers.models", { provider: props.provider.id }),
  );
  const [q, setQ] = createSignal("");
  const [editingId, setEditingId] = createSignal(null);
  const [editCtx, setEditCtx] = createSignal("");
  const [editIn, setEditIn] = createSignal("");
  const [editOut, setEditOut] = createSignal("");
  const shown = createMemo(() => (models() ?? []).filter((m) => m.model_id.toLowerCase().includes(q().toLowerCase())));

  const setDefault = async (ref) => {
    try {
      await rpc("providers.set_default", { ref });
      toast(`Default model: ${ref}`, "good");
      props.onChange?.();
    } catch (e) {
      toast(String(e), "bad");
    }
  };
  const toggle = async (m) => {
    try {
      await rpc("providers.set_model", { provider: m.provider_id, model: m.model_id, enabled: !m.enabled });
      await refetch();
      props.onChange?.();
    } catch (e) {
      toast(String(e), "bad");
    }
  };
  const startEdit = (m) => {
    setEditingId(m.model_id);
    setEditCtx(m.context_window != null ? String(m.context_window) : "");
    setEditIn(m.price_in != null ? String(m.price_in) : "");
    setEditOut(m.price_out != null ? String(m.price_out) : "");
  };
  const saveEdit = async (m) => {
    const parseNum = (s) => (s.trim() === "" ? null : Number(s));
    const ctxVal = parseNum(editCtx());
    const inVal = parseNum(editIn());
    const outVal = parseNum(editOut());
    if ((ctxVal != null && (!Number.isFinite(ctxVal) || ctxVal < 1)) || (inVal != null && (!Number.isFinite(inVal) || inVal < 0)) || (outVal != null && (!Number.isFinite(outVal) || outVal < 0))) {
      toast("Enter valid numbers for context window and prices", "bad");
      return;
    }
    try {
      await rpc("providers.set_model", {
        provider: m.provider_id,
        model: m.model_id,
        ...(ctxVal != null ? { context_window: Math.round(ctxVal) } : {}),
        ...(inVal != null ? { price_in: inVal } : {}),
        ...(outVal != null ? { price_out: outVal } : {}),
      });
      setEditingId(null);
      await refetch();
      props.onChange?.();
      toast(`Updated ${m.model_id}`, "good");
    } catch (e) {
      toast(String(e), "bad");
    }
  };

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
                  <Show
                    when={editingId() === m.model_id}
                    fallback={
                      <>
                        <td class="num">{m.context_window ? fmt.compact(m.context_window) : "–"}</td>
                        <td class="num">{m.price_in != null ? `${m.price_in.toFixed(2)} / ${m.price_out?.toFixed(2)}` : "–"}</td>
                        <td class="num">
                          <Button size="sm" variant="ghost" onClick={() => setDefault(m.ref)}>Make default</Button>
                          <Button size="sm" variant="ghost" onClick={() => startEdit(m)}>Pricing</Button>
                          <Button size="sm" variant="ghost" onClick={() => toggle(m)}>{m.enabled ? "Hide" : "Show"}</Button>
                        </td>
                      </>
                    }
                  >
                    <td class="num">
                      <Input
                        mono
                        value={editCtx()}
                        onValue={setEditCtx}
                        placeholder="Context tokens"
                        aria-label={`Context window for ${m.model_id}`}
                        style={{ width: "110px", display: "inline-block" }}
                      />
                    </td>
                    <td class="num">
                      <div class="row" style={{ gap: "4px", "justify-content": "flex-end" }}>
                        <Input
                          mono
                          value={editIn()}
                          onValue={setEditIn}
                          placeholder="$ in"
                          aria-label={`Input price per 1M for ${m.model_id}`}
                          style={{ width: "70px" }}
                        />
                        <Input
                          mono
                          value={editOut()}
                          onValue={setEditOut}
                          placeholder="$ out"
                          aria-label={`Output price per 1M for ${m.model_id}`}
                          style={{ width: "70px" }}
                        />
                      </div>
                    </td>
                    <td class="num">
                      <Button size="sm" variant="primary" onClick={() => saveEdit(m)}>Save</Button>
                      <Button size="sm" variant="ghost" onClick={() => setEditingId(null)}>Cancel</Button>
                    </td>
                  </Show>
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
  const google = props.kind === "antigravity_oauth";
  const [st, setSt] = createSignal(null);
  const [paste, setPaste] = createSignal("");
  const [err, setErr] = createSignal("");
  useEvent("oauth:done", (e) => {
    const d = e?.payload ?? e;
    if (!st() || (d.id && st().id !== d.id)) return;
    setSt(null);
    if (d.ok) { toast("Signed in", "good"); props.onDone?.(); } else setErr(String(d.error ?? "Sign-in failed"));
  });
  const start = async () => {
    setErr("");
    try { setSt(await rpc("providers.oauth_start", { kind: props.kind })); } catch (e) { setErr(String(e)); }
  };
  const submit = async () => {
    try { await rpc("providers.oauth_paste", { id: st().id, input: paste() }); setSt(null); setPaste(""); props.onDone?.(); }
    catch (e) { setErr(String(e)); }
  };
  const cancel = async () => { await rpc("providers.oauth_cancel", { kind: props.kind }).catch(() => {}); setSt(null); };
  const copyCode = async () => {
    const code = st()?.user_code;
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code);
      toast("Code copied", "good");
    } catch {
      toast("Could not copy the code. Select it and press ⌘C to copy.", "bad");
    }
  };
  const btnLabel = () => {
    const who = claude ? "Claude" : google ? "Google" : "ChatGPT";
    return props.again ? `Sign in again with ${who}` : `Sign in with ${who}`;
  };
  return (
    <div class="col" style={{ gap: "8px" }}>
      <Show when={!st()} fallback={
        <div class="col" style={{ gap: "8px" }}>
          <Show when={claude || google}>
            <Show when={google && st().listening === false}><Shield tone="strict">Another app is using the sign-in port Google returns to. When the browser says it cannot connect, copy that page's address and paste it here.</Shield></Show>
            <div class="small">{google
              ? "Finish in your browser. It completes here by itself. If it does not, copy the address the browser ends on (it starts with http://127.0.0.1:51121) and paste it here:"
              : "Finish in your browser. It completes here by itself. If it does not, paste the code or the full callback URL:"}</div>
            <div class="row"><Input mono value={paste()} onValue={setPaste} placeholder={google ? "http://127.0.0.1:51121/oauth-callback?..." : "code#state or callback URL"} />
              <Button variant="primary" onClick={submit} disabled={!paste().trim()}>Submit</Button></div>
            <div class="xs faint">Different browser? <a href="#" onClick={(e) => { e.preventDefault(); navigator.clipboard?.writeText(st().manual_url); toast("Link copied", "good"); }}>Copy the sign-in link</a>, finish there, then paste the code above.</div>
          </Show>
          <Show when={!claude && !google}>
            <div class="small">Open <span class="mono">{st().verify_url}</span> and enter this code:</div>
            <div class="row" style={{ gap: "12px", "flex-wrap": "wrap" }}>
              <span class="mono" style={{ "font-size": "28px", "letter-spacing": "4px", "user-select": "text" }}>{st().user_code}</span>
              <Button type="button" size="sm" onClick={copyCode}>Copy code</Button>
            </div>
            <div class="xs faint">Waiting for you to approve. This page updates automatically.</div>
          </Show>
          <div><Button size="sm" onClick={cancel}>Cancel</Button></div>
        </div>}>
        <div class="row">
          <Button variant="primary" onClick={start}>{btnLabel()}</Button>
        </div>
      </Show>
      <Show when={err()}><Shield tone="danger">{err()}</Shield></Show>
    </div>
  );
}

export default function Providers() {
  const store = useStore();
  const list = () => store.providers();
  const def = () => store.defaultModel();
  const [form, setForm] = createSignal(null);
  const [open, setOpen] = createSignal(null);
  const [reauthId, setReauthId] = createSignal(null);
  const [busy, setBusy] = createSignal(null);
  const [quotas, { refetch: refetchQuotas }] = createResource(() =>
    rpc("activity.quotas").then((data) => ({ data })).catch(() => ({ error: true })),
  );
  // Live account usage for the card pills: on open and every 60s while visible.
  const visible = useScreenVisible();
  refreshAccountUsage();
  createEffect(on(visible, (v) => v && refreshAccountUsage(), { defer: true }));
  const quotaTimer = setInterval(refetchQuotas, 30000);
  const usageTimer = setInterval(() => visible() && refreshAccountUsage(), 60000);
  onCleanup(() => {
    clearInterval(quotaTimer);
    clearInterval(usageTimer);
  });

  const syncStore = () =>
    Promise.all([
      store.refetchProviders(),
      store.refetchDefault(),
      store.refetchModels(),
    ]);

  const refresh = async (p) => {
    setBusy(p.id);
    try {
      const m = await rpc("providers.refresh_models", { id: p.id });
      toast(`${p.name}: ${m.length} models`, "good");
    } catch (e) {
      toast(`${p.name}: ${e}`, "bad");
    } finally {
      setBusy(null);
      await syncStore();
    }
  };
  const toggleEnabled = async (p) => {
    setBusy(p.id);
    try {
      const next = p.enabled === false;
      await rpc("providers.save", {
        create: false,
        id: p.id,
        name: p.name,
        kind: p.kind,
        base_url: p.base_url ?? "",
        trusted: isSub(p.kind) ? true : Boolean(p.trusted),
        guard: !p.trusted && !isSub(p.kind) && p.guard === "off" ? "redact" : p.guard,
        enabled: next,
        default_model: p.default_model,
      });
      toast(`${p.name} ${next ? "enabled" : "disabled"}`, "good");
      await syncStore();
    } catch (e) {
      toast(String(e), "bad");
    } finally {
      setBusy(null);
    }
  };
  const signOut = async (p) => {
    if (
      !(await confirm(
        `Sign out of ${p.name}?`,
        "Agents using this subscription will pause until you sign in again.",
        { danger: true, ok: "Sign out" },
      ))
    )
      return;
    setBusy(p.id);
    try {
      await rpc("providers.oauth_signout", { id: p.id });
      toast(`Signed out of ${p.name}`, "good");
      await syncStore();
    } catch (e) {
      toast(String(e), "bad");
    } finally {
      setBusy(null);
    }
  };
  const remove = async (p) => {
    if (!(await confirm(`Remove ${p.name}?`, "Its API key is deleted from Philium's vault. Agents using it will stop working.", { danger: true, ok: "Remove" }))) return;
    setBusy(p.id);
    try {
      await rpc("providers.remove", { id: p.id });
      toast(`Removed ${p.name}`, "good");
      await syncStore();
    } catch (e) {
      toast(String(e), "bad");
    } finally {
      setBusy(null);
    }
  };

  return (
    <Page title="Providers" sub="Keys stay in the local vault; agents receive gateway tokens only."
      actions={<>
        <Button variant="ghost" onClick={() => go("stats")}>Usage in Philium</Button>
        <Button variant="primary" onClick={() => setForm({})}><Icon name="plus" /> Add provider</Button>
      </>}>
      <Show when={list() && list().length === 0}>
        <Empty icon={<Icon name="key" size={32} />} title="No providers yet"
          action={<Button variant="primary" onClick={() => setForm({})}>Add provider</Button>}>
          Add an API key or sign in with a subscription below.
        </Empty>
      </Show>
      <div class="col" style={{ gap: "12px" }}>
        <For each={SUBSCRIPTIONS.filter(([k]) => !(list() ?? []).some((p) => p.kind === k))}>
          {([k, label]) => (
            <Card>
              <div class="card-head"><h2>{label} subscription</h2></div>
              <Show when={SUB_NOTICE[k]}>
                <div class="small dim" style={{ "margin-bottom": "8px" }} title={SUB_NOTICE[k].detail}>{SUB_NOTICE[k].short}</div>
              </Show>
              <SignIn kind={k} onDone={syncStore} />
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
                <Show when={p.enabled === false}><Badge>Disabled</Badge></Show>
                <Show when={p.trusted} fallback={<Badge tone="strict" title="Agents on this provider always run in Strict mode"><Icon name="lock" size={12} /> Untrusted · Strict only</Badge>}>
                  <Badge tone="secure"><Icon name="shield" size={12} /> Trusted</Badge>
                </Show>
                <Badge tone={p.guard === "off" ? "danger" : "secure"}>Guard: {p.guard}</Badge>
                <Show when={isSub(p.kind)}><Badge tone={p.signed_in ? "secure" : "danger"}>{p.signed_in ? [p.account?.email, p.account?.plan].filter(Boolean).join(" · ") || "Signed in" : "Sign in again"}</Badge></Show>
                <Show when={!isSub(p.kind) && !p.has_key}><Badge tone="danger">No API key</Badge></Show>
                <AccountUsagePill account={usageFor(p.id)} />
                <div style={{ "margin-left": "auto" }} class="row">
                  <Button size="sm" busy={busy() === p.id} onClick={() => refresh(p)}><Icon name="refresh" size={13} /> Refresh models</Button>
                  <Button size="sm" onClick={() => setForm({ provider: p })}>Edit</Button>
                  <Menu
                    title="Provider actions"
                    items={[
                      ...(isSub(p.kind) ? [{ label: "Sign in again", disabled: busy() === p.id, onClick: () => setReauthId(p.id) }] : []),
                      ...(isSub(p.kind) && p.signed_in ? [{ label: "Sign out", disabled: busy() === p.id, onClick: () => signOut(p) }] : []),
                      { label: p.enabled === false ? "Enable provider" : "Disable provider", disabled: busy() === p.id, onClick: () => toggleEnabled(p) },
                      { label: "Remove provider", danger: true, disabled: busy() === p.id, onClick: () => remove(p) },
                    ]}
                  />
                </div>
              </div>
              <div class="row" style={{ gap: "10px", "flex-wrap": "wrap" }}>
                <QuotaMeter
                  usage={quotas()?.data?.[p.id]}
                  empty={quotas.loading ? "Loading quota…" : quotas()?.error ? "Quota unavailable" : "No quota reported yet"}
                />
                <span class="small"><AccountUsageLink kind={p.kind} /></span>
              </div>
              <div class="row small dim">
                <span>{kindLabel(p.kind)}</span>
                <Show when={p.base_url && !isSub(p.kind)}><span class="mono faint">· {p.base_url}</span></Show>
                <span>· {p.model_count} models</span>
                <Show when={p.models_cached_at}><span class="faint">· updated {fmt.ago(p.models_cached_at)}</span></Show>
                <Button size="sm" variant="ghost" style={{ "margin-left": "auto" }} onClick={() => setOpen(open() === p.id ? null : p.id)}>{open() === p.id ? "Hide models" : "Show models"}</Button>
              </div>
              <Show when={p.models_error}><div style={{ "margin-top": "10px" }}><Shield tone="danger">Model discovery failed: {p.models_error}</Shield></div></Show>
              <Show when={needsReauth(p) || reauthId() === p.id}>
                <div style={{ "margin-top": "10px" }}>
                  <Show when={SUB_NOTICE[p.kind]}>
                    <div class="small dim" style={{ "margin-bottom": "8px" }} title={SUB_NOTICE[p.kind].detail}>{SUB_NOTICE[p.kind].short}</div>
                  </Show>
                  <SignIn kind={p.kind} again onDone={() => { setReauthId(null); syncStore(); }} />
                </div>
              </Show>
              <Show when={open() === p.id}><div style={{ "margin-top": "12px" }}><ModelList provider={p} defaultRef={def()} onChange={syncStore} /></div></Show>
            </Card>
          )}
        </For>
      </div>
      <Show when={form()}>
        {(f) => <ProviderForm provider={f().provider ?? null}
          onClose={(saved) => { setForm(null); if (saved) syncStore(); }} />}
      </Show>
    </Page>
  );
}
