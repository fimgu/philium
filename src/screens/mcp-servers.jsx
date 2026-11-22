import { createSignal, Show, For } from "solid-js";
import { Button, Field, Input, Select, Toggle, Badge, confirm, toast } from "../ui/kit.jsx";
import { rpc } from "../lib/rpc.js";

// Lines "KEY=value" <-> object. Secret lines are "KEY" only, values entered separately.
const toPairs = (o) => Object.entries(o ?? {}).map(([k, v]) => `${k}=${v}`).join("\n");
const fromPairs = (t) => Object.fromEntries(t.split("\n").map((l) => l.trim()).filter(Boolean).map((l) => {
  const i = l.indexOf("=");
  return i < 0 ? [l, ""] : [l.slice(0, i).trim(), l.slice(i + 1)];
}));

const blank = () => ({ name: "", transport: "stdio", command: "", args: [], url: "", env: {}, secret_env: [], headers: {}, secret_headers: [], enabled: true, all_repos: true, repos: [], approval: "ask", timeout_secs: 300 });

function Editor(props) {
  const s = props.server;
  const [f, setF] = createSignal({ ...s, argsText: (s.args ?? []).join("\n"), envText: toPairs(s.env), headersText: toPairs(s.headers) });
  const [secretEnv, setSecretEnv] = createSignal(Object.fromEntries((s.secret_env ?? []).map((k) => [k, ""])));
  const [secretHeaders, setSecretHeaders] = createSignal(Object.fromEntries((s.secret_headers ?? []).map((k) => [k, ""])));
  const [newSecret, setNewSecret] = createSignal("");
  const [busy, setBusy] = createSignal(false), [err, setErr] = createSignal("");
  const set = (k, v) => setF({ ...f(), [k]: v });
  const http = () => f().transport === "http";
  const secrets = () => (http() ? secretHeaders() : secretEnv());
  const setSecrets = (v) => (http() ? setSecretHeaders(v) : setSecretEnv(v));
  const isSet = (k) => (http() ? s.secret_headers_set : s.secret_env_set)?.[k];

  async function save() {
    setBusy(true); setErr("");
    const v = f();
    const server = {
      name: v.name.trim(), transport: v.transport, command: v.command.trim(), args: v.argsText.split("\n").filter((a) => a.trim()),
      url: v.url.trim(), env: fromPairs(v.envText), headers: fromPairs(v.headersText),
      secret_env: Object.keys(secretEnv()), secret_headers: Object.keys(secretHeaders()),
      enabled: v.enabled, all_repos: v.all_repos, repos: v.repos ?? [], approval: v.approval, timeout_secs: Number(v.timeout_secs) || 300,
    };
    try {
      await rpc("mcp.servers_save", { server, original: s.name || undefined, secrets: { env: secretEnv(), headers: secretHeaders() } }, { local: true });
      toast(`MCP server '${server.name}' saved. New agents pick it up.`, "good");
      props.onDone();
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  }

  return (
    <div class="ws-section" style={{ "margin-top": "8px" }}>
      <Field label="Name"><Input value={f().name} onValue={(v) => set("name", v)} placeholder="github" mono /></Field>
      <Field label="Transport"><Select value={f().transport} onValue={(v) => set("transport", v)} options={[{ value: "stdio", label: "stdio (local command)" }, { value: "http", label: "HTTP (streamable)" }]} /></Field>
      <Show when={!http()} fallback={<>
        <Field label="URL"><Input value={f().url} onValue={(v) => set("url", v)} placeholder="https://example.com/mcp" mono /></Field>
        <Field label="Headers (KEY=value per line, not secret)"><textarea class="textarea" value={f().headersText} onInput={(e) => set("headersText", e.currentTarget.value)} /></Field>
      </>}>
        <Field label="Command"><Input value={f().command} onValue={(v) => set("command", v)} placeholder="npx" mono /></Field>
        <Field label="Arguments (one per line)"><textarea class="textarea" value={f().argsText} onInput={(e) => set("argsText", e.currentTarget.value)} /></Field>
        <Field label="Environment (KEY=value per line, not secret)"><textarea class="textarea" value={f().envText} onInput={(e) => set("envText", e.currentTarget.value)} /></Field>
      </Show>
      <Field label={http() ? "Secret headers (stored in Philium's vault, never shown)" : "Secret environment variables (stored in Philium's vault, never shown)"}>
        <For each={Object.keys(secrets())}>{(k) => (
          <div class="ws-row">
            <code>{k}</code>
            <Input type="password" value={secrets()[k]} placeholder={isSet(k) ? "stored (leave blank to keep)" : "value"} onValue={(v) => setSecrets({ ...secrets(), [k]: v })} />
            <Button size="sm" onClick={() => { const n = { ...secrets() }; delete n[k]; setSecrets(n); }}>Remove</Button>
          </div>
        )}</For>
        <div class="ws-row">
          <Input value={newSecret()} onValue={setNewSecret} placeholder={http() ? "Authorization" : "API_TOKEN"} mono />
          <Button size="sm" onClick={() => { const k = newSecret().trim(); if (k) { setSecrets({ ...secrets(), [k]: "" }); setNewSecret(""); } }}>Add secret</Button>
        </div>
      </Field>
      <Field label="Tool calls">
        <Select value={f().approval} onValue={(v) => set("approval", v)} options={[{ value: "ask", label: "Ask me each time" }, { value: "allow", label: "Allow without asking" }, { value: "deny", label: "Deny" }]} />
      </Field>
      <div class="ws-row"><span>Enabled</span><Toggle on={f().enabled} onChange={(v) => set("enabled", v)} /></div>
      <div class="ws-row"><span>All repositories</span><Toggle on={f().all_repos} onChange={(v) => set("all_repos", v)} /></div>
      <Show when={!f().all_repos}>
        <Field label="Enabled for repositories">
          <For each={props.repos}>{(r) => (
            <div class="ws-row"><span>{r.name || r.path || r.id}</span>
              <Toggle on={(f().repos ?? []).includes(r.id)} onChange={(on) => set("repos", on ? [...(f().repos ?? []), r.id] : (f().repos ?? []).filter((x) => x !== r.id))} />
            </div>
          )}</For>
        </Field>
      </Show>
      <Field label="Timeout (seconds)"><Input value={String(f().timeout_secs ?? 300)} onValue={(v) => set("timeout_secs", v)} /></Field>
      <p class="muted">Custom servers run with your user's access, so Strict agents never receive them.</p>
      <Show when={err()}><p class="ws-alert">{err()}</p></Show>
      <div class="ws-actions">
        <Button variant="primary" busy={busy()} onClick={save}>Save</Button>
        <Button onClick={props.onDone}>Cancel</Button>
      </div>
    </div>
  );
}

export function McpServersSection(props) {
  const [list, setList] = createSignal([]), [editing, setEditing] = createSignal(null), [err, setErr] = createSignal("");
  async function load() {
    try { setList((await rpc("mcp.servers_list", {}, { local: true })) ?? []); setErr(""); } catch (e) { setErr(String(e)); }
  }
  async function remove(s) {
    if (!(await confirm(`Remove MCP server '${s.name}' and its stored secrets?`))) return;
    try { await rpc("mcp.servers_delete", { name: s.name }, { local: true }); await load(); } catch (e) { setErr(String(e)); }
  }
  const scope = (s) => (s.all_repos ? "all repos" : `${(s.repos ?? []).length} repo(s)`);
  return (
    <section class="ws-section">
      <details onToggle={(e) => e.currentTarget.open && load()}>
        <summary class="ws-row" style={{ cursor: "pointer" }}><strong>MCP servers</strong><Badge>{list().length} custom</Badge></summary>
        <p class="muted">External MCP servers added to agents alongside Philium's own tools. Changes apply to agents started afterwards.</p>
        <For each={list()}>{(s) => (
          <div class="ws-row">
            <code>{s.name}</code>
            <span class="muted">{s.transport === "http" ? s.url.replace(/\?.*$/, "") : s.command}</span>
            <Badge tone={s.enabled ? "secure" : "strict"}>{s.enabled ? scope(s) : "disabled"}</Badge>
            <Badge>{s.approval}</Badge>
            <div class="ws-actions">
              <Button size="sm" onClick={() => setEditing(s)}>Edit</Button>
              <Button size="sm" onClick={() => remove(s)}>Remove</Button>
            </div>
          </div>
        )}</For>
        <Show when={editing()} fallback={<Button onClick={() => setEditing(blank())}>Add server</Button>}>
          {(s) => <Editor server={s()} repos={props.repos ?? []} onDone={() => { setEditing(null); load(); }} />}
        </Show>
        <Show when={err()}><p class="ws-alert">{err()}</p></Show>
      </details>
    </section>
  );
}
