// Home: what is running, what needs you, and how much it costs.
import { For, Show, createResource, createMemo, createSignal, onCleanup } from "solid-js";
import { Page, Button, Badge, Dot, Empty, Stat, Card, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc, useEvent, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { AgentChip, SpawnModal } from "./agents.jsx";
import "../styles/home.css";

const n = (x) => (x == null ? "0" : x >= 1e6 ? `${(x / 1e6).toFixed(1)}M` : x >= 1e3 ? `${(x / 1e3).toFixed(1)}k` : String(Math.round(x)));
const bytes = (b) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : b >= 1e6 ? `${(b / 1e6).toFixed(0)} MB` : `${Math.round((b ?? 0) / 1e3)} KB`);

function Section(props) {
  return (
    <section class="home-sec">
      <div class="row" style={{ "margin-bottom": "8px" }}>
        <h2 class="grow">{props.title}</h2>
        <Show when={props.action}>{props.action}</Show>
      </div>
      {props.children}
    </section>
  );
}

export default function Home() {
  const [agents, ag] = createResource(() => rpc("agents.list").catch(() => []));
  const [tasks, tk] = createResource(() => rpc("board.list", {}).catch(() => []));
  const [proposals, pr] = createResource(() => rpc("reaper.proposals").catch(() => []));
  const [leaked] = createResource(() => rpc("reaper.ledger", { state: "leaked" }).catch(() => []));
  const [stats, st] = createResource(() => rpc("stats.summary", { range: "24h" }).catch(() => null));
  const [events, ev] = createResource(() => rpc("app.events", { limit: 30 }).catch(() => []));
  const [disk] = createResource(() => rpc("app.disk").catch(() => null));
  const [providers] = createResource(() => rpc("providers.list").catch(() => []));
  const [clashes, cl] = createResource(() => rpc("codeindex.clashes", {}).then((r) => r?.clashes ?? []).catch(() => []));
  const [spawn, setSpawn] = createSignal(false);

  let t;
  const soon = (f) => { clearTimeout(t); t = setTimeout(f, 300); };
  onCleanup(() => clearTimeout(t));
  useEvent("agents:changed", () => soon(() => { ag.refetch(); ev.refetch(); }));
  useEvent("board:changed", () => tk.refetch());
  useEvent("event", () => soon(() => { ev.refetch(); st.refetch(); }));
  useEvent("codeindex:clashes", () => cl.refetch());
  useEvent("reaper:proposals", () => pr.refetch());
  useEvent("reaper:changed", () => pr.refetch());

  const live = createMemo(() => (agents() ?? []).filter((a) => a.live));
  const waiting = createMemo(() => (agents() ?? []).filter((a) => a.state === "waiting"));
  const failed = createMemo(() => (agents() ?? []).filter((a) => a.state === "failed"));
  const byStatus = createMemo(() => {
    const m = {};
    for (const x of tasks() ?? []) m[x.status] = (m[x.status] ?? 0) + 1;
    return m;
  });
  const guardHits = createMemo(() => (events() ?? []).filter((e) => /guard|blocked|denied/.test(e.kind)));
  const totals = () => stats()?.totals ?? {};
  const diskTotal = () => Object.values(disk() ?? {}).reduce((s, v) => s + (v.bytes ?? 0), 0);
  const attention = () => waiting().length + (proposals() ?? []).length + (leaked() ?? []).length + failed().length + (clashes() ?? []).length;

  return (
    <Page title="Home" actions={<Button variant="primary" onClick={() => setSpawn(true)}><Icon name="plus" size={12} /> New agent</Button>}>
      <Show when={(providers() ?? []).length === 0 && providers.state === "ready"}>
        <Card class="home-hero">
          <h2>Welcome to Philium</h2>
          <p class="dim">Add a provider to start. Subscriptions (ChatGPT, Claude) are used directly. API keys stay in your keychain and every request passes through the local gateway, which removes secrets before they leave your machine.</p>
          <Button variant="primary" onClick={() => go("providers")}><Icon name="key" size={12} /> Add a provider</Button>
        </Card>
      </Show>

      <div class="home-stats">
        <Stat label="Agents running" value={live().length} sub={`${(agents() ?? []).length} total`} />
        <Stat label="Needs you" value={attention()} sub={attention() ? "see below" : "all clear"} />
        <Stat label="Requests (24h)" value={n(totals().requests)} sub={totals().error_rate ? `${(totals().error_rate * 100).toFixed(1)}% errors` : "no errors"} />
        <Stat label="Tokens (24h)" value={n((totals().tokens_in ?? 0) + (totals().tokens_out ?? 0))} sub={totals().avg_tps ? `${totals().avg_tps.toFixed(0)} tok/s avg` : ""} />
        <Stat label="Cost (24h)" value={totals().cost != null ? `$${totals().cost.toFixed(2)}` : "n/a"} sub={totals().unpriced_requests ? `${totals().unpriced_requests} unpriced` : ""} />
        <Stat label="Secrets removed" value={n(totals().guard_actions)} sub="by the gateway, 24h" />
      </div>

      <Show when={attention()}>
        <Section title="Needs you">
          <div class="list card" style={{ padding: 0 }}>
            <For each={waiting()}>
              {(a) => (
                <div class="list-item" onClick={() => go("agents", { id: a.id })}>
                  <Dot state="warn" /><AgentChip name={a.name} />
                  <span class="grow small">is waiting for your approval</span>
                  <Button size="sm">Review</Button>
                </div>
              )}
            </For>
            <For each={failed()}>
              {(a) => (
                <div class="list-item" onClick={() => go("agents", { id: a.id })}>
                  <Dot state="bad" /><AgentChip name={a.name} />
                  <span class="grow small ellipsis">failed{a.error ? `: ${a.error}` : ""}</span>
                  <Button size="sm">Open</Button>
                </div>
              )}
            </For>
            <For each={clashes()}>
              {(c) => (
                <div class="list-item" onClick={() => go("code", c.repo_id ? { repo_id: c.repo_id } : {})}>
                  <Dot state="warn" /><AgentChip name={c.a} /><span class="small">and</span><AgentChip name={c.b} />
                  <span class="grow small ellipsis">both change {(c.detail ?? [])[0] ?? "the same code"} ({String(c.severity).replace(/_/g, " ")})</span>
                  <Button size="sm">See code</Button>
                </div>
              )}
            </For>
            <For each={proposals()}>
              {(p) => (
                <div class="list-item">
                  <Dot state="warn" /><Icon name="trash" size={12} />
                  <span class="grow small ellipsis">Clean up {p.kind} <span class="mono xs">{p.handle}</span>{p.agent_name ? ` from ${p.agent_name}` : ""}: {p.reason}</span>
                  <Button size="sm" onClick={() => rpc("reaper.dismiss", { ids: [p.id] }).then(pr.refetch)}>Keep</Button>
                  <Button size="sm" variant="primary" onClick={() => rpc("reaper.approve", { ids: [p.id] }).then(pr.refetch).catch((e) => toast(String(e), "danger"))}>Clean up</Button>
                </div>
              )}
            </For>
            <For each={leaked()}>
              {(r) => (
                <div class="list-item" onClick={() => go("settings")}>
                  <Dot state="bad" />
                  <span class="grow small ellipsis">Leaked {r.kind}: <span class="mono xs">{r.handle}</span></span>
                  <Badge tone="danger">leaked</Badge>
                </div>
              )}
            </For>
          </div>
        </Section>
      </Show>

      <div class="home-grid">
        <Section title="Running agents" action={<Button size="sm" variant="ghost" onClick={() => go("agents")}>All agents</Button>}>
          <Show when={live().length} fallback={
            <Empty title="Nothing running" action={<Button onClick={() => setSpawn(true)}>Start an agent</Button>}>Agents work in their own worktree and report back on the board.</Empty>}>
            <div class="list card" style={{ padding: 0 }}>
              <For each={live().slice(0, 12)}>
                {(a) => (
                  <div class="list-item" onClick={() => go("agents", { id: a.id })}>
                    <Dot state={a.state === "running" ? "busy" : a.state === "waiting" ? "warn" : "live"} />
                    <AgentChip name={a.name} />
                    <Show when={a.strict}><Badge tone="strict">Strict</Badge></Show>
                    <span class="grow small dim ellipsis">{a.task_title || a.repo_name || ""}</span>
                    <span class="xs faint mono">{(a.model_ref ?? "").split("/").pop()}</span>
                  </div>
                )}
              </For>
              <Show when={live().length > 12}><div class="list-item small faint" onClick={() => go("agents")}>and {live().length - 12} more</div></Show>
            </div>
          </Show>
        </Section>

        <Section title="Board" action={<Button size="sm" variant="ghost" onClick={() => go("board")}>Open board</Button>}>
          <Card>
            <div class="home-board">
              <For each={[["open", "Open"], ["claimed", "In progress"], ["in_review", "In review"], ["done", "Done"]]}>
                {([k, l]) => <div><div class="xs faint">{l}</div><div class="home-num">{byStatus()[k] ?? 0}</div></div>}
              </For>
            </div>
            <For each={(tasks() ?? []).filter((x) => x.blocked || ["claimed", "in_review"].includes(x.status)).slice(0, 5)}>
              {(x) => (
                <div class="row small" style={{ padding: "6px 0", "border-top": "1px solid var(--line)", cursor: "pointer" }} onClick={() => go("board", { id: x.id })}>
                  <Badge tone={x.blocked ? "danger" : x.status === "in_review" ? "accent" : ""}>{x.blocked ? "blocked" : x.status.replace("_", " ")}</Badge>
                  <span class="grow ellipsis">{x.title}</span>
                  <Show when={x.owner}><AgentChip name={x.owner} /></Show>
                </div>
              )}
            </For>
          </Card>
        </Section>

        <Section title="Recent activity" action={<Button size="sm" variant="ghost" onClick={() => go("activity")}>Model requests</Button>}>
          <Card style={{ padding: 0 }}>
            <Show when={(events() ?? []).length} fallback={<div class="small faint" style={{ padding: "14px" }}>Nothing yet.</div>}>
              <div class="list home-feed">
                <For each={events()}>
                  {(e) => (
                    <div class="list-item" style={{ cursor: e.agent_id ? "pointer" : "default" }} onClick={() => e.agent_id && go("agents", { id: e.agent_id })}>
                      <span class={`dot ${/fail|error|leak|denied|blocked/.test(e.kind) ? "bad" : /guard/.test(e.kind) ? "warn" : ""}`} />
                      <span class="small grow ellipsis">{e.kind.replace(/[_:]/g, " ")}</span>
                      <span class="xs faint">{fmt.ago(e.ts)}</span>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </Card>
        </Section>

        <Section title="Disk" action={<Button size="sm" variant="ghost" onClick={() => go("settings")}>Clean up</Button>}>
          <Card>
            <div class="home-num">{bytes(diskTotal())}</div>
            <div class="xs faint" style={{ "margin-bottom": "8px" }}>used by Philium</div>
            <For each={Object.entries(disk() ?? {})}>
              {([k, v]) => <div class="row small"><span class="grow dim">{k}</span><span class="mono">{bytes(v.bytes)}{v.partial ? "+" : ""}</span></div>}
            </For>
            <Show when={guardHits().length}>
              <div class="small" style={{ "margin-top": "10px", color: "var(--strict)" }}><Icon name="shield" size={11} /> {guardHits().length} guard events recently</div>
            </Show>
          </Card>
        </Section>
      </div>
      <Show when={spawn()}><SpawnModal onClose={(a) => { setSpawn(false); if (a) go("agents", { id: a.id }); }} /></Show>
    </Page>
  );
}
