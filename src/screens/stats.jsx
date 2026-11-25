// Stats: what can I use now, when does it come back, what am I spending,
// and where is it going. Account limits come live from each provider; the
// activity half is scoped by the range picker and counts Philium traffic only.
import { For, Show, createSignal, createMemo, createEffect, on, onMount, onCleanup } from "solid-js";
import { Page, Button, Segmented, Empty } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { activeRepoId } from "../lib/store.js";
import { useScreenVisible, useVisibleEvent, useClock } from "../lib/visibility.js";
import { accountUsage, refreshAccountUsage, verdict, countdown } from "../lib/account-usage.js";
import { AccountUsageCard, KeyAccountRow } from "../ui/account-usage.jsx";
import "../styles/terminals.css";
import "../styles/account-usage.css";

const RANGE_LABEL = { "24h": "last 24 hours", "7d": "last 7 days", "30d": "last 30 days" };
const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export { costLabel } from "../lib/cost.js";
import { costLabel } from "../lib/cost.js";

function Heatmap(props) {
  // Backend rows are Thursday-first (epoch day 0); rotate to Monday-first.
  const rows = createMemo(() => {
    const h = props.heat ?? [];
    return h.length === 7 ? [...h.slice(4), ...h.slice(0, 4)] : [];
  });
  const max = createMemo(() => Math.max(1, ...rows().flat()));
  return (
    <div class="st-heat" role="img" aria-label="Requests by hour of week">
      <span class="lbl" />
      <For each={Array.from({ length: 24 }, (_, i) => i)}>{(h) => <span class="lbl">{h % 6 === 0 ? `${h}:00` : ""}</span>}</For>
      <For each={rows()}>
        {(row, d) => (
          <>
            <span class="lbl">{DAYS[d()]}</span>
            <For each={row}>
              {(n, h) => <span title={`${DAYS[d()]} ${h()}:00 · ${n} requests`} style={{ opacity: n ? 0.18 + 0.82 * (n / max()) : 0.06 }} />}
            </For>
          </>
        )}
      </For>
    </div>
  );
}

function Breakdown(props) {
  const max = createMemo(() => Math.max(1, ...(props.rows ?? []).map((r) => r.tokens || 0)));
  return (
    <div class="st-panel">
      <h3>{props.title}</h3>
      <Show when={props.rows?.length} fallback={<p class="dim small">Nothing in this range.</p>}>
        <div class="st-hbars">
          <For each={props.rows.slice(0, 6)}>
            {(r) => (
              <div class="st-hbar">
                <span class="truncate" title={r.id || ""}>{r.name || r.id || "Unattributed"}</span>
                <div class="track"><div style={{ width: `${(100 * (r.tokens || 0)) / max()}%` }} /></div>
                <span class="mono xs">{fmt.compact(r.tokens)} tok</span>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function ModelTable(props) {
  return (
    <div class="lane-table-wrap">
      <table class="lane-table st-models" aria-label="Models">
        <thead><tr>
          <th scope="col">Model</th><th scope="col">Requests</th><th scope="col">Tokens</th>
          <th scope="col">First token p50 / p95</th><th scope="col">Speed</th><th scope="col">Cache reuse</th>
          <th scope="col">Errors</th><th scope="col">Cost</th>
        </tr></thead>
        <tbody>
          <For each={props.rows}>
            {(m) => (
              <tr>
                <th scope="row">{m.model} <span class="faint xs">{m.provider_id}</span></th>
                <td>{fmt.int(m.requests)}</td>
                <td>{fmt.compact((m.tokens_in || 0) + (m.tokens_out || 0))}</td>
                <td>{fmt.ms(m.p50_ttft_ms)} / {fmt.ms(m.p95_ttft_ms)}</td>
                <td>{m.avg_tps == null ? "–" : `${m.avg_tps.toFixed(0)} tok/s`}</td>
                <td>{fmt.pct(m.cache_hit)}</td>
                <td class={m.error_rate >= 0.05 ? "au-err" : ""}>{fmt.pct(m.error_rate)}</td>
                <td class={costLabel(m) === "On plan" ? "dim" : ""}>{costLabel(m)}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

function Kpi(props) {
  return (
    <div class="st-kpi">
      <span class="label">{props.label}</span>
      <span class="value">{props.value}</span>
      <Show when={props.sub}><span class="sub">{props.sub}</span></Show>
    </div>
  );
}

export default function Stats() {
  const visible = useScreenVisible();
  const now = useClock(1000);
  const [range, setRange] = createSignal("24h"),
    [insights, setInsights] = createSignal(null),
    [loading, setLoading] = createSignal(false),
    [error, setError] = createSignal("");
  const accounts = createMemo(() => (accountUsage.data()?.accounts ?? []).filter((a) => a.enabled));
  // Plans with windows get full cards; pay-as-you-go keys and failures without
  // any reading collapse into one compact list so card heights stay even.
  const planAccounts = createMemo(() => accounts().filter((a) => a.windows?.length || a.kind.endsWith("_oauth")));
  const keyAccounts = createMemo(() => accounts().filter((a) => !planAccounts().includes(a)));
  const headline = createMemo(() => verdict(accounts(), now()));
  const nextReset = createMemo(() =>
    accounts().flatMap((a) => a.windows ?? []).map((w) => w.resets_at).filter(Boolean).sort((a, b) => a - b)[0],
  );

  let generation = 0;
  const load = async () => {
    const g = ++generation;
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    setLoading(true);
    try {
      const i = await rpc("stats.insights", {
        range: range(),
        repo_id: activeRepoId() || undefined,
        today_start: midnight.getTime(),
        tz_offset_min: -midnight.getTimezoneOffset(),
      });
      if (g !== generation) return;
      setInsights(i);
      setError("");
    } catch (e) {
      if (g === generation) setError(String(e?.message ?? e));
    } finally {
      if (g === generation) setLoading(false);
    }
  };
  onMount(() => {
    load();
    refreshAccountUsage();
  });
  createEffect(on(visible, (v) => v && refreshAccountUsage(), { defer: true }));
  const timer = setInterval(() => visible() && refreshAccountUsage(), 60000);
  createEffect(on([activeRepoId, range], () => load(), { defer: true }));
  onCleanup(() => {
    generation++;
    clearInterval(timer);
  });
  useVisibleEvent("activity", (a) => a?.state !== "pending" && load(), { debounceMs: 1500 });

  const t = () => insights()?.totals ?? {};
  const models = () => insights()?.models ?? [];
  const sum = (k) => models().reduce((n, m) => n + (m[k] || 0), 0);
  const spendValue = () => {
    const priced = sum("priced");
    if (priced > 0) return fmt.money(t().cost);
    return sum("included") > 0 ? "On plan" : "–";
  };
  const spendSub = () => {
    const parts = [];
    if (sum("included")) parts.push(`${fmt.int(sum("included"))} on plan`);
    if (sum("priced")) parts.push(`${fmt.int(sum("priced"))} priced`);
    const unknown = (t().requests || 0) - sum("included") - sum("priced");
    if (unknown > 0) parts.push(`${fmt.int(unknown)} without a price`);
    return parts.join(" · ");
  };
  const cacheShare = () => (t().tokens_in > 0 ? (t().tokens_cached || 0) / t().tokens_in : null);

  return (
    <Page
      title="Stats"
      sub="What you can use now, and where it goes"
      actions={
        <Button
          busy={loading() || accountUsage.loading()}
          onClick={() => {
            load();
            refreshAccountUsage(true);
          }}
        >
          Refresh
        </Button>
      }
    >
      <div class="st-page">
        {/* ---- Now: live account limits ---- */}
        <section class="st-block" aria-labelledby="st-now">
          <header class="st-block-head">
            <div>
              <h2 id="st-now">Available now</h2>
              <p>Live limits from each account. Countdowns tick to the next reset.</p>
            </div>
            <Show when={nextReset()}>
              <span class="st-next xs">Next reset in <b class="mono">{countdown(nextReset(), now())}</b></span>
            </Show>
          </header>
          <Show when={headline()}>
            <div class={`au-verdict ${headline().tone}`} role="status">{headline().text}</div>
          </Show>
          <Show
            when={accounts().length}
            fallback={
              <p class="st-empty">
                {accountUsage.error()
                  ? `Could not check accounts: ${accountUsage.error()}`
                  : accountUsage.loading() || !accountUsage.data()
                    ? "Checking your accounts…"
                    : "No connected accounts yet. Add one on the Providers screen."}
              </p>
            }
          >
            <div class="au-grid" style={{ "grid-template-columns": `repeat(${Math.min(3, Math.max(1, planAccounts().length))}, minmax(0, 1fr))` }}>
              <For each={planAccounts()}>{(a) => <AccountUsageCard account={a} />}</For>
            </div>
            <Show when={keyAccounts().length}>
              <div class="au-keys" aria-label="Pay-as-you-go providers">
                <span class="label">Pay as you go</span>
                <For each={keyAccounts()}>{(a) => <KeyAccountRow account={a} />}</For>
              </div>
            </Show>
          </Show>
        </section>

        {/* ---- Activity: everything below follows the range ---- */}
        <section class="st-block st-activity" aria-labelledby="st-act">
          <header class="st-block-head st-sticky">
            <div>
              <h2 id="st-act">Your activity in Philium</h2>
              <p>Requests routed through Philium in the {RANGE_LABEL[range()]}{activeRepoId() ? ", this workspace only" : ""}.</p>
            </div>
            <div class="row" style={{ gap: "10px" }}>
              <Show when={loading()}><span class="xs faint" role="status">Updating…</span></Show>
              <Segmented value={range()} options={["24h", "7d", "30d"]} onChange={setRange} />
            </div>
          </header>
          <Show when={error()}><p role="alert" class="au-err small">Activity unavailable: {error()}</p></Show>
          <Show
            when={t().requests}
            fallback={
              <Show when={insights() && !error()}>
                <Empty title={`Nothing in the ${RANGE_LABEL[range()]}`} action={<Button onClick={() => go("chat")}>Open desk chat</Button>}>
                  Requests from agents and desk chat appear here as you work.
                </Empty>
              </Show>
            }
          >
            <div class="st-kpis" style={{ opacity: loading() ? 0.7 : 1 }}>
              <Kpi label="Spend" value={spendValue()} sub={spendSub()} />
              <Kpi label="Requests" value={fmt.int(t().requests)} sub={`${fmt.pct((t().errors || 0) / t().requests)} errors`} />
              <Kpi label="Tokens" value={fmt.compact((t().tokens_in || 0) + (t().tokens_out || 0))} sub={`${fmt.compact(t().tokens_in)} in · ${fmt.compact(t().tokens_out)} out`} />
              <Kpi label="Cache reuse" value={fmt.pct(cacheShare())} sub={cacheShare() == null ? "" : `${fmt.compact(t().tokens_cached)} tokens served from cache`} />
            </div>

            <div class="st-sub">
              <h3>Models</h3>
              <ModelTable rows={models()} />
            </div>

            <div class="st-sub">
              <h3>Where it went</h3>
              <div class="st-two">
                <Breakdown title="Workspaces" rows={insights()?.repos} />
                <Breakdown title="Agents" rows={insights()?.agents} />
              </div>
            </div>

            <div class="st-sub">
              <h3>When you work</h3>
              <Heatmap heat={insights()?.heatmap} />
            </div>
          </Show>
        </section>

        <footer class="st-end xs faint">
          Account limits come live from each provider and cover all apps on that account. Activity counts only requests made through Philium.
        </footer>
      </div>
    </Page>
  );
}
