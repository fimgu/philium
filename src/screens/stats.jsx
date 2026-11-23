import {
  For,
  Show,
  createSignal,
  createResource,
  createMemo,
  createEffect,
  on,
  onMount,
  onCleanup,
} from "solid-js";
import { Page, Button, Stat, Segmented, Bars, Empty } from "../ui/kit.jsx";
import { KvTelemetryBar } from "../ui/kv-bar.jsx";
import { AccountUsageLink } from "../ui/account-usage-link.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { activeRepoId, useStore } from "../lib/store.js";
import { useScreenVisible, useVisibleEvent, useClock } from "../lib/visibility.js";
import { accountUsage, refreshAccountUsage, verdict } from "../lib/account-usage.js";
import { AccountUsageCard } from "../ui/account-usage.jsx";
import { providerUsageRows } from "../lib/provider-usage.js";
import { quotaView } from "../lib/quota.js";
import "../styles/terminals.css";

const usageColumns = ["Requests", "Input", "Output", "TTFT avg / p50", "TPS", "Known cost", "Errors", "Guard actions"];

function usageValues(r) {
  return [
    fmt.int(r.requests),
    fmt.compact(r.tokens_in),
    fmt.compact(r.tokens_out),
    `${fmt.ms(r.avg_ttft_ms)} / ${fmt.ms(r.p50_ttft_ms)}`,
    r.avg_tps?.toFixed(1) ?? "N/A",
    `${fmt.money(r.cost)}${r.unpriced_requests ? " + unknown" : ""}`,
    fmt.pct(r.error_rate),
    Object.entries(r.guard_actions || {})
      .filter(([k]) => k !== "pass")
      .map(([k, v]) => `${k}: ${v}`)
      .join(", ") || "None",
  ];
}

function ProviderQuota(props) {
  // Refreshing the snapshot also recomputes the reset countdown.
  const windows = createMemo(() => quotaView(props.usage).rows);
  return (
    <Show when={windows().length} fallback={<span class="dim">{props.empty}</span>}>
      <For each={windows()}>
        {(w) => (
          <div class="row" style={{ gap: "6px" }}>
            <meter min="0" max="100" value={w.pct} aria-label={`${w.label} quota used`} style={{ width: "50px" }} />
            <span>{w.label}: {w.pct}% used{w.reset ? ` · ${w.reset}` : ""}</span>
          </div>
        )}
      </For>
      <Show when={props.usage?.observed_at}>
        <span class="faint xs">
          Last reported {new Date(props.usage.observed_at * 1000).toLocaleString()}
        </span>
      </Show>
    </Show>
  );
}

function ProviderUsage(props) {
  return (
    <section class="lane-section">
      <h2>Provider usage</h2>
      <p>
        Requests, tokens, and costs come from model calls made in Philium.
        The date range and workspace filter apply to those totals.
        Quota meters show the latest values reported to Philium by each provider.
        Open a provider’s plan usage page for account-wide usage.
      </p>
      <Show when={props.rows.length} fallback={
        <p>{props.pending ? "Loading providers…" : "No configured providers or recorded usage to show."}</p>
      }>
        <div class="lane-table-wrap">
          <table class="lane-table" aria-label="Provider usage">
            <thead><tr>
              <th scope="col">Provider</th>
              <th scope="col">Reported quota</th>
              <For each={usageColumns}>{(label) => <th scope="col">{label}</th>}</For>
            </tr></thead>
            <tbody>
              <For each={props.rows}>
                {(row) => (
                  <tr>
                    <th scope="row">
                      {row.provider?.name || row.id}
                      <Show when={row.provider?.name && row.provider.name !== row.id}>
                        {" "}<span class="faint mono">{row.id}</span>
                      </Show>
                      <Show when={row.provider?.enabled === false}> · Disabled</Show>
                    </th>
                    <td>
                      <ProviderQuota
                        usage={row.provider ? props.quotas?.[row.id] : undefined}
                        empty={row.provider
                          ? props.quotaStatus
                          : props.pending ? "Loading…" : "Not configured"}
                      />
                      <div class="xs"><AccountUsageLink kind={row.provider?.kind} /></div>
                    </td>
                    <Show when={row.stats} fallback={<td colspan={usageColumns.length} class="dim">{props.statsStatus}</td>}>
                      <For each={usageValues(row.stats)}>{(value) => <td>{value}</td>}</For>
                    </Show>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
    </section>
  );
}

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function Heatmap(props) {
  // Backend rows are Thu-first (epoch day 0); rotate to Monday-first.
  const rows = createMemo(() => {
    const h = props.heat ?? [];
    return h.length === 7 ? [...h.slice(4), ...h.slice(0, 4)] : [];
  });
  const max = createMemo(() => Math.max(1, ...rows().flat()));
  return (
    <div class="st-heat" role="img" aria-label="Requests by hour of week">
      <span class="lbl" />
      <For each={Array.from({ length: 24 }, (_, i) => i)}>{(h) => <span class="lbl">{h % 6 === 0 ? h : ""}</span>}</For>
      <For each={rows()}>
        {(row, d) => (
          <>
            <span class="lbl">{DAYS[d()]}</span>
            <For each={row}>
              {(n, h) => <span title={`${DAYS[d()]} ${h()}:00 · ${n} requests`} style={{ opacity: n ? 0.15 + 0.85 * (n / max()) : 0.05 }} />}
            </For>
          </>
        )}
      </For>
    </div>
  );
}

function HBars(props) {
  const max = createMemo(() => Math.max(1, ...(props.rows ?? []).map((r) => r.tokens || 0)));
  return (
    <Show when={props.rows?.length} fallback={<p class="dim small">Nothing recorded in this range.</p>}>
      <div class="st-hbars">
        <For each={props.rows.slice(0, 8)}>
          {(r) => (
            <div class="st-hbar">
              <span class="truncate" title={r.id}>{r.name || r.id || "Unattributed"}</span>
              <div class="track"><div style={{ width: `${(100 * (r.tokens || 0)) / max()}%` }} /></div>
              <span class="mono xs">{fmt.compact(r.tokens)} · {fmt.money(r.cost)}</span>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

function ModelTable(props) {
  return (
    <div class="lane-table-wrap">
      <table class="lane-table" aria-label="Model performance">
        <thead><tr>
          <th scope="col">Model</th><th scope="col">Requests</th><th scope="col">TTFT p50 / p95</th>
          <th scope="col">Tokens / s</th><th scope="col">Cache hit</th><th scope="col">Errors</th><th scope="col">Cost</th>
        </tr></thead>
        <tbody>
          <For each={props.rows}>
            {(m) => (
              <tr>
                <th scope="row">{m.model} <span class="faint mono xs">{m.provider_id}</span></th>
                <td>{fmt.int(m.requests)}</td>
                <td>{fmt.ms(m.p50_ttft_ms)} / {fmt.ms(m.p95_ttft_ms)}</td>
                <td>{m.avg_tps == null ? "–" : m.avg_tps.toFixed(1)}</td>
                <td>{fmt.pct(m.cache_hit)}</td>
                <td class={m.error_rate >= 0.05 ? "au-err" : ""}>{fmt.pct(m.error_rate)}</td>
                <td>{fmt.money(m.cost)}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

const spendSub = (s) =>
  !s ? "" : [`${fmt.int(s.requests)} requests`, s.included ? `${fmt.int(s.included)} on plan` : "", s.unpriced ? `${fmt.int(s.unpriced)} unpriced` : ""].filter(Boolean).join(" · ");

export default function Stats() {
  const store = useStore();
  const visible = useScreenVisible();
  const now = useClock(1000);
  const [quotaResult, { refetch: refreshQuotas }] = createResource(async () => {
    try {
      return { data: await rpc("activity.quotas") };
    } catch {
      return { error: true };
    }
  });
  const [range, setRange] = createSignal("24h"),
    [summary, setSummary] = createSignal(null),
    [insights, setInsights] = createSignal(null),
    [series, setSeries] = createSignal([]),
    [loading, setLoading] = createSignal(false),
    [error, setError] = createSignal("");
  const providerRows = createMemo(() =>
    providerUsageRows(store.providers() ?? [], summary()?.by_provider ?? []),
  );
  const accounts = createMemo(() => (accountUsage.data()?.accounts ?? []).filter((a) => a.enabled));
  const headline = createMemo(() => verdict(accounts(), now()));
  let generation = 0;
  const load = async () => {
    const g = ++generation,
      r = range(),
      rid = activeRepoId() || undefined;
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    setLoading(true);
    try {
      const [s, t, i] = await Promise.all([
        rpc("stats.summary", { range: r, repo_id: rid }),
        rpc("stats.timeseries", { range: r, bucket: r === "24h" ? "hour" : "day", repo_id: rid }),
        rpc("stats.insights", { range: r, repo_id: rid, today_start: midnight.getTime(), tz_offset_min: -midnight.getTimezoneOffset() }).catch(() => null),
      ]);
      if (g !== generation) return;
      setSummary(s);
      setSeries(t);
      setInsights(i);
      setError("");
    } catch (e) {
      if (g === generation) setError(String(e));
    } finally {
      if (g === generation) setLoading(false);
    }
  };
  onMount(() => {
    load();
    refreshAccountUsage();
  });
  // Live account usage: on open, on becoming visible, and every 60s while visible.
  createEffect(on(visible, (v) => v && refreshAccountUsage(), { defer: true }));
  const timer = setInterval(() => {
    if (!visible()) return;
    refreshAccountUsage();
    refreshQuotas();
  }, 60000);
  createEffect(on(activeRepoId, () => load(), { defer: true }));
  onCleanup(() => {
    generation++;
    clearInterval(timer);
  });
  useVisibleEvent(
    "activity",
    (a) => {
      if (a?.state !== "pending") {
        load();
        refreshQuotas();
      }
    },
    { debounceMs: 1500 },
  );
  const totals = () => summary()?.totals || {};
  const spend = () => insights()?.spend ?? {};
  return (
    <Page
      title="Stats"
      sub="Live account limits, Philium spend and model performance"
      actions={
        <>
          <Show when={loading() || accountUsage.loading()}>
            <span class="xs faint" role="status">Updating…</span>
          </Show>
          <Segmented value={range()} options={["24h", "7d", "30d"]} onChange={(r) => { setRange(r); load(); }} />
          <Button
            busy={loading() || accountUsage.loading()}
            onClick={() => {
              load();
              refreshQuotas();
              refreshAccountUsage(true);
              store.refetchProviders();
            }}
          >
            Refresh
          </Button>
        </>
      }
    >
      <div class="stats-screen" style={{ opacity: loading() && summary() ? 0.78 : 1, transition: "opacity 120ms ease" }}>
        <Show when={error()}><p role="alert">{error()}</p></Show>
        <Show when={headline()}>
          <div class={`au-verdict ${headline().tone}`} role="status">{headline().text}</div>
        </Show>

        <section class="lane-section" style={{ "margin-top": "0" }}>
          <h2>Account usage</h2>
          <Show
            when={accounts().length}
            fallback={
              <p>
                {accountUsage.error()
                  ? `Account usage unavailable: ${accountUsage.error()}`
                  : accountUsage.loading() || !accountUsage.data()
                    ? "Checking your accounts…"
                    : "No connected accounts. Add one on the Providers screen."}
              </p>
            }
          >
            <div class="au-grid">
              <For each={accounts()}>{(a) => <AccountUsageCard account={a} />}</For>
            </div>
          </Show>
        </section>

        <section class="lane-section">
          <h2>Spend through Philium</h2>
          <div class="st-spend">
            <Stat label="Today" value={fmt.money(spend().today?.cost)} sub={spendSub(spend().today)} />
            <Stat label="Last 7 days" value={fmt.money(spend().week?.cost)} sub={spendSub(spend().week)} />
            <Stat label="Last 30 days" value={fmt.money(spend().month?.cost)} sub={spendSub(spend().month)} />
            <Stat
              label={`Errors · ${range()}`}
              value={fmt.pct(totals().requests ? (insights()?.totals?.errors ?? 0) / totals().requests : null)}
              sub={`${fmt.int(insights()?.totals?.errors ?? 0)} of ${fmt.int(totals().requests)} requests`}
            />
          </div>
          <div style={{ "margin-top": "14px" }}><KvTelemetryBar kv={totals()} /></div>
        </section>

        <ProviderUsage
          rows={providerRows()}
          pending={store.providers.loading || (!summary() && !error())}
          quotas={quotaResult()?.data}
          quotaStatus={quotaResult()?.error ? "Quota unavailable" : quotaResult.loading ? "Loading quota…" : "No quota reported"}
          statsStatus={error() ? "Usage unavailable" : loading() || !summary() ? "Loading usage…" : "No requests in this range"}
        />

        <Show
          when={summary()?.totals.requests}
          fallback={
            <Empty title="Usage appears as you work" action={<Button onClick={() => go("chat")}>Open desk chat</Button>}>
              Send a message to an agent or desk chat. Completed gateway requests appear here across every provider.
            </Empty>
          }
        >
          <section class="lane-section">
            <h2>Model performance</h2>
            <p>Latency, speed and cache reuse per model for the last {range()}.</p>
            <ModelTable rows={insights()?.models ?? []} />
          </section>
          <section class="lane-section">
            <h2>Tokens over time</h2>
            <Bars
              values={series().map((p) => ({ label: new Date(p.ts).toLocaleString(), value: p.tokens_in + p.tokens_out }))}
              format={fmt.int}
            />
          </section>
          <section class="lane-section">
            <h2>When you work</h2>
            <p>Requests by hour of the week, local time.</p>
            <Heatmap heat={insights()?.heatmap} />
          </section>
          <div class="st-two">
            <section class="lane-section">
              <h2>Repos</h2>
              <HBars rows={insights()?.repos} />
            </section>
            <section class="lane-section">
              <h2>Agents</h2>
              <HBars rows={insights()?.agents} />
            </section>
          </div>
        </Show>
      </div>
    </Page>
  );
}
