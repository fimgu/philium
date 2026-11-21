import {
  For,
  Show,
  createSignal,
  createMemo,
  createEffect,
  on,
  onMount,
  onCleanup,
} from "solid-js";
import {
  Page,
  Button,
  Stat,
  Segmented,
  Bars,
  Empty,
  VirtualList,
} from "../ui/kit.jsx";
import { KvTelemetryBar } from "../ui/kv-bar.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { go } from "../lib/nav.js";
import { activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/terminals.css";
function UsageTable(props) {
  return (
    <section class="lane-section">
      <h2>{props.title}</h2>
      <div class="lane-table-wrap">
        <div
          class="lane-virtual-table"
          role="table"
          aria-label={props.title}
          aria-rowcount={props.rows.length + 1}
        >
          <div class="lane-usage-row lane-usage-head" role="row">
            <For
              each={[
                props.label,
                "Requests",
                "Input",
                "Output",
                "TTFT avg / p50",
                "TPS",
                "Known cost",
                "Errors",
                "Guard actions",
              ]}
            >
              {(s) => <span role="columnheader">{s}</span>}
            </For>
          </div>
          <div
            style={{
              height: `${Math.min(400, Math.max(44, props.rows.length * 44))}px`,
            }}
          >
            <VirtualList items={props.rows} rowHeight={44}>
              {(r) => (
                <div class="lane-usage-row" role="row">
                  <span role="cell">
                    {r[props.field]}
                    {props.field === "model" ? ` (${r.provider_id})` : ""}
                  </span>
                  <span role="cell">{fmt.int(r.requests)}</span>
                  <span role="cell">{fmt.compact(r.tokens_in)}</span>
                  <span role="cell">{fmt.compact(r.tokens_out)}</span>
                  <span role="cell">
                    {fmt.ms(r.avg_ttft_ms)} / {fmt.ms(r.p50_ttft_ms)}
                  </span>
                  <span role="cell">{r.avg_tps?.toFixed(1) ?? "N/A"}</span>
                  <span role="cell">
                    {fmt.money(r.cost)}
                    {r.unpriced_requests ? " + unknown" : ""}
                  </span>
                  <span role="cell">{fmt.pct(r.error_rate)}</span>
                  <span role="cell">
                    {Object.entries(r.guard_actions || {})
                      .filter(([k]) => k !== "pass")
                      .map(([k, v]) => `${k}: ${v}`)
                      .join(", ") || "None"}
                  </span>
                </div>
              )}
            </VirtualList>
          </div>
        </div>
      </div>
    </section>
  );
}
export default function Stats() {
  const [range, setRange] = createSignal("24h"),
    [summary, setSummary] = createSignal(null),
    [series, setSeries] = createSignal([]),
    [efficiency, setEfficiency] = createSignal([]),
    [loading, setLoading] = createSignal(false),
    [error, setError] = createSignal("");
  let generation = 0;
  const clampPct = (ratio) =>
    Math.min(100, Math.max(0, Math.round((Number(ratio) || 0) * 100)));
  const load = async () => {
    const g = ++generation,
      r = range(),
      rid = activeRepoId() || undefined;
    setLoading(true);
    try {
      const [s, t, e] = await Promise.all([
        rpc("stats.summary", { range: r, repo_id: rid }),
        rpc("stats.timeseries", {
          range: r,
          bucket: r === "24h" ? "hour" : "day",
          repo_id: rid,
        }),
        rpc("stats.efficiency", { range: r, repo_id: rid }),
      ]);
      if (g !== generation) return;
      setSummary(s);
      setSeries(t);
      setEfficiency(e);
      setError("");
    } catch (e) {
      if (g === generation) setError(String(e));
    } finally {
      if (g === generation) setLoading(false);
    }
  };
  onMount(load);
  createEffect(on(activeRepoId, () => load(), { defer: true }));
  onCleanup(() => {
    generation++;
  });
  useVisibleEvent(
    "activity",
    (a) => {
      if (a?.state !== "pending") load();
    },
    { debounceMs: 1500 },
  );
  const totals = () => summary()?.totals || {};
  const leaderboard = createMemo(() =>
    [...(summary()?.by_agent || [])].sort(
      (a, b) => b.tokens_out - a.tokens_out,
    ),
  );
  return (
    <Page
      title="Stats"
      sub="All providers, token efficiency and generation speed"
      actions={
        <>
          <Show when={loading()}>
            <span class="xs faint" role="status">
              Updating {range()}…
            </span>
          </Show>
          <Segmented
            value={range()}
            options={["24h", "7d", "30d"]}
            onChange={(r) => {
              setRange(r);
              load();
            }}
          />
          <Button busy={loading()} onClick={load}>
            Refresh
          </Button>
        </>
      }
    >
      <div
        class="stats-screen"
        style={{
          opacity: loading() && summary() ? 0.78 : 1,
          transition: "opacity 120ms ease",
        }}
      >
        <Show when={error()}>
          <p role="alert">{error()}</p>
        </Show>
        <div style={{ "margin-bottom": "14px" }}>
          <KvTelemetryBar kv={totals()} />
        </div>
        <div class="lane-stats">
          <Stat label="Requests" value={fmt.int(totals().requests)} />
          <Stat label="Input tokens" value={fmt.compact(totals().tokens_in)} />
          <Stat
            label="Cached prefix"
            value={fmt.compact(totals().tokens_cached)}
            sub={
              totals().tokens_in > 0
                ? (totals().tokens_cached || 0) > 0
                  ? `${clampPct((totals().tokens_cached || 0) / totals().tokens_in)}% KV cache hit`
                  : "0% KV cache hit (0 cached)"
                : "Not reported yet"
            }
          />
          <Stat
            label="Output tokens"
            value={fmt.compact(totals().tokens_out)}
          />
          <Stat
            label="Tokens / second"
            value={totals().avg_tps?.toFixed(1) ?? "N/A"}
          />
          <Stat
            label="Known cost"
            value={fmt.money(totals().cost)}
            sub={
              totals().unpriced_requests
                ? `${totals().unpriced_requests} requests without prices`
                : "Based on model prices"
            }
          />
          <Stat label="TTFT p50" value={fmt.ms(totals().p50_ttft_ms)} />
        </div>
        <Show
          when={summary()?.totals.requests}
          fallback={
            <Empty
              title="Usage appears as you work"
              action={
                <Button onClick={() => go("chat")}>Open desk chat</Button>
              }
            >
              Send a message to an agent or desk chat. Completed gateway
              requests appear here across every provider.
            </Empty>
          }
        >
          <UsageTable
            title="Providers"
            label="Provider"
            field="provider_id"
            rows={summary()?.by_provider || []}
          />
          <UsageTable
            title="Models"
            label="Model"
            field="model"
            rows={summary()?.by_model || []}
          />
          <section class="lane-section">
            <h2>Tokens over time</h2>
            <p>
              Input + output tokens per {range() === "24h" ? "hour" : "day"}.
              Hover a bar for the bucket total.
            </p>
            <Bars
              values={series().map((p) => ({
                label: new Date(p.ts).toLocaleString(),
                value: p.tokens_in + p.tokens_out,
              }))}
              format={fmt.int}
            />
          </section>
          <section class="lane-section">
            <h2>Token efficiency</h2>
            <div class="lane-table-wrap">
              <table class="lane-table">
                <thead>
                  <tr>
                    <th>Model</th>
                    <th>Output / input</th>
                    <th>Cached share</th>
                    <th>Cost / 1k output</th>
                  </tr>
                </thead>
                <tbody>
                  <For each={efficiency()}>
                    {(e) => (
                      <tr>
                        <td>
                          {e.provider_id}/{e.model}
                        </td>
                        <td>{e.output_input_ratio?.toFixed(3) ?? "N/A"}</td>
                        <td>
                          {e.cache_available || typeof e.cached_share === "number"
                            ? `${clampPct(e.cached_share)}%`
                            : e.tokens_in > 0 && Number(e.tokens_cached) === 0
                              ? "0% (0 cached)"
                              : "Not reported by gateway"}
                        </td>
                        <td>
                          {e.cost_per_1k_output == null
                            ? "Unknown"
                            : fmt.money(e.cost_per_1k_output)}
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </section>
          <UsageTable
            title="Agent leaderboard · output tokens"
            label="Agent"
            field="agent_id"
            rows={leaderboard()}
          />
        </Show>
      </div>
    </Page>
  );
}
