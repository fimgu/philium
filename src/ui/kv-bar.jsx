import { Show } from "solid-js";

export function KvTelemetryBar(props) {
  const kv = () => props.kv || {};
  const reqs = () => Number(kv().requests || 0);
  const inp = () => Number(kv().input_tokens || kv().tokens_in || 0);
  const cached = () => Math.min(inp(), Number(kv().cached_tokens || kv().tokens_cached || 0));
  const fresh = () => Math.max(0, inp() - cached());
  const out = () => Number(kv().output_tokens || kv().tokens_out || 0);
  const total = () => Math.max(1, cached() + fresh() + out());
  const hitPct = () => (inp() > 0 ? Math.round((cached() / inp()) * 100) : 0);
  const ttft = () => kv().avg_ttft_ms ?? kv().p50_ttft_ms;
  const tone = () => (hitPct() >= 60 ? "hot" : hitPct() >= 25 ? "warm" : reqs() > 0 ? "cold" : "idle");
  const fmtTok = (n) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(2)}M` : n >= 1_000 ? `${(n / 1_000).toFixed(1)}k` : String(Math.round(n)));

  return (
    <div class={`kv-telemetry-bar ${props.compact ? "compact" : ""}`} title="Out-of-band Gateway KV Cache & Token Telemetry (0 extra prompt tokens)">
      <div class="kv-telemetry-top">
        <span class={`kv-hit-pill ${tone()}`}>
          <span class="kv-hit-dot" />
          KV {reqs() > 0 ? `${hitPct()}%` : "—"}
        </span>
        <span class="kv-legend-item cached" title="Cached prefix tokens (shared repo map + conversation prefix)">
          <i /> Cached <b>{fmtTok(cached())}</b>
        </span>
        <span class="kv-legend-item fresh" title="Uncached fresh input tokens">
          <i /> Fresh <b>{fmtTok(fresh())}</b>
        </span>
        <span class="kv-legend-item out" title="Generated output tokens">
          <i /> Out <b>{fmtTok(out())}</b>
        </span>
        <div class="grow" />
        <Show when={ttft() != null}>
          <span class="mono xs faint" title="Average Time To First Token">TTFT {Math.round(ttft())}ms</span>
        </Show>
        <span class="mono xs faint">{reqs()} req{reqs() === 1 ? "" : "s"}</span>
      </div>
      <div class="kv-bar-track" role="img" aria-label={`KV cache hit rate ${hitPct()}%`}>
        <div class="kv-bar-seg cached" style={{ width: `${reqs() > 0 ? (cached() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg fresh" style={{ width: `${reqs() > 0 ? (fresh() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg out" style={{ width: `${reqs() > 0 ? (out() / total()) * 100 : 0}%` }} />
      </div>
    </div>
  );
}
