// Home: what is running, what needs you, and how much it costs.
import { For, Show, createResource, createMemo, createSignal } from "solid-js";
import { Page, Button, Badge, Dot, Empty, Stat, Card, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { AgentChip } from "../ui/agent-chip.jsx";
import { SpawnModal } from "../ui/spawn-modal.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { useStore, activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import CheckoutHome from "./checkout-home.jsx";
import { setupSteps, setupVisible } from "../lib/setup-steps.js";
import "../styles/home.css";

const n = (x) => {
  const num = Number(x);
  if (!Number.isFinite(num)) return "0";
  return num >= 1e6 ? `${(num / 1e6).toFixed(1)}M` : num >= 1e3 ? `${(num / 1e3).toFixed(1)}k` : String(Math.round(num));
};
const bytes = (b) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : b >= 1e6 ? `${(b / 1e6).toFixed(0)} MB` : `${Math.round((b ?? 0) / 1e3)} KB`);
const guardCount = (ga) => {
  if (typeof ga === "number") return Number.isFinite(ga) ? ga : 0;
  if (ga && typeof ga === "object") {
    return Object.entries(ga).reduce((sum, [k, v]) => (k === "pass" ? sum : sum + (Number(v) || 0)), 0);
  }
  return 0;
};

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
  return <Show when={activeRepoId()} keyed fallback={<GlobalHome />}>{id => <CheckoutHome id={id} />}</Show>;
}

function GlobalHome() {
  const store = useStore();
  const agents = store.agents;
  const providers = store.providers;
  const repos = store.repos;
  const [tasks, tk] = createResource(() => rpc("board.list", {}).catch(() => []));
  const [proposals, pr] = createResource(() => rpc("reaper.proposals").catch(() => []));
  const [leaked] = createResource(() => rpc("reaper.ledger", { state: "leaked" }).catch(() => []));
  const [stats, st] = createResource(() => rpc("stats.summary", { range: "24h" }).catch(() => null));
  const [events, ev] = createResource(() => rpc("app.events", { limit: 30 }).catch(() => []));
  const [disk] = createResource(() => rpc("app.disk").catch(() => null));
  const [clashes, cl] = createResource(() => rpc("codeindex.clashes", {}).then((r) => r?.clashes ?? []).catch(() => []));
  const [libStatus, libSt] = createResource(() => rpc("library.status", {}, { local: true }).catch(() => null));
  const [spawn, setSpawn] = createSignal(false);
  const [hideSetup, setHideSetup] = createSignal(false);

  useVisibleEvent("agents:changed", () => ev.refetch(), { debounceMs: 300 });
  useVisibleEvent("board:changed", () => tk.refetch(), { debounceMs: 200 });
  useVisibleEvent("event", () => { ev.refetch(); st.refetch(); }, { debounceMs: 300 });
  useVisibleEvent("codeindex:clashes", () => cl.refetch(), { debounceMs: 300 });
  useVisibleEvent("reaper:proposals", () => pr.refetch(), { debounceMs: 200 });
  useVisibleEvent("reaper:changed", () => pr.refetch(), { debounceMs: 200 });
  useVisibleEvent("library", () => libSt.refetch(), { debounceMs: 300 });

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
  const proposedMemories = () => Number(libStatus()?.proposed ?? 0);
  const attention = () => waiting().length + (proposals() ?? []).length + (leaked() ?? []).length + failed().length + (clashes() ?? []).length + (proposedMemories() > 0 ? 1 : 0);

  const [setupRaw, su] = createResource(async () => {
    const [models, github, runtimes, settings, skills, mcp] = await Promise.all([
      rpc("library.models_status", {}, { local: true }).catch(() => null),
      rpc("setup.github", {}, { local: true }).catch(() => ({ installed: false, signed_in: false })),
      rpc("environments.runtimes", {}, { local: true }).catch(() => []),
      rpc("app.settings_get", {}, { local: true }).catch(() => ({})),
      rpc("skills.list", {}, { local: true }).then((r) => r?.skills?.length ?? 0).catch(() => 0),
      rpc("mcp.servers_list", {}, { local: true }).then((r) => r?.length ?? 0).catch(() => 0),
    ]);
    return { models: models ?? { embed: { state: "missing" } }, github, runtimes: runtimes ?? [], notifications: settings?.notifications === "true", skills, mcp };
  });
  useVisibleEvent("library", () => su.refetch(), { debounceMs: 500 });
  const setupRows = createMemo(() => setupSteps({
    ...(setupRaw() ?? {}),
    providers: providers.state === "ready" ? providers() ?? [] : undefined,
    repos: repos.state === "ready" ? repos() ?? [] : undefined,
  }));
  const showSetup = () => !hideSetup() && setupRaw.state === "ready" && setupVisible(setupRows());

  return (
    <Page title="Home" actions={<Button variant="primary" onClick={() => setSpawn(true)}><Icon name="plus" size={12} /> New agent</Button>}>
      <Show when={showSetup()}>
        <Card class="home-hero home-setup">
          <div class="row" style={{ "margin-bottom": "6px" }}>
            <h2 class="grow" style={{ margin: 0 }}>Get set up</h2>
            <Button size="sm" variant="ghost" onClick={() => su.refetch()}>Check again</Button>
            <Button size="sm" variant="ghost" onClick={() => setHideSetup(true)}>Later</Button>
          </div>
          <p class="muted">A few things make Philium useful. Each one updates as you go.</p>
          <ul class="setup-list">
            <For each={setupRows()}>{(r) => (
              <li class={r.done ? "done" : ""}>
                <button type="button" onClick={() => go(r.screen)}>
                  <span class="setup-mark" aria-hidden="true">{r.done ? "✓" : "○"}</span>
                  <span class="grow">
                    <b>{r.label}</b>{r.optional ? <span class="muted"> · optional</span> : null}
                    <span class="setup-detail muted">{r.detail}</span>
                  </span>
                  <span class="sr-only">{r.done ? "Done" : "Not done"}</span>
                </button>
              </li>
            )}</For>
          </ul>
        </Card>
      </Show>

      <div class="home-stats">
        <Stat label="Agents running" value={live().length} sub={`${(agents() ?? []).length} total`} />
        <Stat label="Needs you" value={attention()} sub={attention() ? "see below" : "all clear"} />
        <Stat label="Requests (24h)" value={n(totals().requests)} sub={totals().error_rate ? `${(totals().error_rate * 100).toFixed(1)}% errors` : "no errors"} />
        <Stat label="Tokens (24h)" value={n((totals().tokens_in ?? 0) + (totals().tokens_out ?? 0))} sub={totals().avg_tps ? `${totals().avg_tps.toFixed(0)} tok/s avg` : ""} />
        <Stat label="Cost (24h)" value={totals().cost != null ? `$${totals().cost.toFixed(2)}` : "n/a"} sub={totals().unpriced_requests ? `${totals().unpriced_requests} unpriced` : ""} />
        <Stat label="Secrets removed" value={n(guardCount(totals().guard_actions))} sub="24h · gateway" />
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
                  <span class="grow small ellipsis">both change {(Array.isArray(c.detail) ? c.detail[0] : c.detail) ?? "the same code"}{c.severity ? ` (${String(c.severity).replace(/_/g, " ")})` : ""}</span>
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
            <Show when={proposedMemories() > 0}>
              <div class="list-item" onClick={() => go("memory")}>
                <Dot state="warn" /><Icon name="brain" size={12} />
                <span class="grow small">{proposedMemories()} proposed memory update{proposedMemories() === 1 ? "" : "s"} waiting for review</span>
                <Button size="sm">Review in Memory</Button>
              </div>
            </Show>
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
            <Empty title="No running agents" action={<Button onClick={() => setSpawn(true)}>New agent</Button>}>Each agent works in an isolated worktree.</Empty>}>
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
