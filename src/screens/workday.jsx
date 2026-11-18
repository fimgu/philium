// Workday: what you did in Philium, and each time you had to leave it.
import { For, Show, createResource, createSignal } from "solid-js";
import { Page, Button } from "../ui/kit.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";

export default function Workday() {
  const [data, { refetch }] = createResource(() => rpc("workday.summary", {}, { local: true }).catch((e) => ({ error: String(e) })));
  const [tool, setTool] = createSignal("Jcode"), [note, setNote] = createSignal(""), [busy, setBusy] = createSignal(false), [message, setMessage] = createSignal("");
  let timer;
  useEvent("event", () => { clearTimeout(timer); timer = setTimeout(refetch, 500); });
  const act = async (fn) => { setBusy(true); setMessage(""); try { await fn(); await refetch(); } catch (e) { setMessage(String(e)); } finally { setBusy(false); } };
  const record = (e) => { e.preventDefault(); act(async () => { await rpc("workday.fallback", { tool: tool(), note: note() }, { local: true }); setNote(""); setMessage("Fallback recorded."); }); };
  return <Page title="Workday" sub="What you did here, and when you had to leave" actions={<><Button onClick={() => refetch()} disabled={busy()}>Refresh</Button><Button disabled={busy()} onClick={() => act(async () => { const r = await rpc("workday.export", {}, { local: true }); setMessage(`Summary written to ${r.path}`); })}>Export summary</Button></>}>
    <Show when={data()?.error}><p role="alert">{data().error}</p></Show>
    <Show when={data() && !data().error}>{() => <div aria-label="Workday ledger">
      <p>Philium records the name and outcome of each action you take on a screen. It does not record what you typed, any path or any output, and nothing leaves this Mac. It cannot see when you switch to another tool, so record that below when it happens.</p>
      <p aria-label="Observation window">{data().started ? `Observation window started ${new Date(data().started).toLocaleString()}.` : "No observation window started. Showing everything recorded so far."} <Button size="sm" disabled={busy()} onClick={() => act(() => rpc("workday.start", {}, { local: true }))}>Start a new window now</Button></p>
      <section aria-label="Fallbacks"><h2>Times you needed something else: {data().fallbacks.length}</h2>
        <form onSubmit={record}><label>What did you use instead? <select aria-label="Fallback tool" value={tool()} onChange={(e) => setTool(e.currentTarget.value)}><For each={data().tools}>{(t) => <option value={t}>{t}</option>}</For></select></label> <label>What could you not do in Philium? <input aria-label="Fallback note" value={note()} onInput={(e) => setNote(e.currentTarget.value)} placeholder="One line is enough" /></label> <Button type="submit" disabled={busy()}>Record fallback</Button></form>
        <For each={data().fallbacks} fallback={<p class="small faint">None recorded in this window.</p>}>{(f) => <p data-fallback={f.tool}><strong>{f.tool}</strong> · {new Date(f.ts).toLocaleString()} · {f.note || "No note"}</p>}</For>
      </section>
      <section aria-label="Completed actions"><h2>Completed in Philium: {data().completed}</h2>
        <table><thead><tr><th>Action</th><th>Checkout</th><th>Completed</th><th>Failed</th><th>Last</th></tr></thead><tbody>
          <For each={data().actions} fallback={<tr><td colspan="5">Nothing recorded in this window yet.</td></tr>}>{(a) => <tr data-action={a.method}><td>{a.action}</td><td>{a.checkout || "All workspaces"}</td><td>{a.completed}</td><td>{a.failed}</td><td>{fmt.ago(a.last)}</td></tr>}</For>
        </tbody></table>
      </section>
    </div>}</Show>
    <p role="status">{message()}</p>
  </Page>;
}
