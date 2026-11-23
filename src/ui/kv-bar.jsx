import { Show } from "solid-js";

export function KvTelemetryBar(props) {
  const kv = () => props.kv || {};
  const reqs = () => Number(kv().requests || 0);
  const inp = () => kv().input_tokens ?? kv().tokens_in;
  const cacheInput = () => kv().cache_input_tokens ?? inp();
  const rawCached = () => kv().cached_tokens ?? kv().tokens_cached;
  const cached = () => rawCached() == null ? null : Math.min(cacheInput() ?? rawCached(), rawCached());
  const fresh = () => cacheInput() != null && cached() != null ? cacheInput() - cached() : null;
  const out = () => kv().output_tokens ?? kv().tokens_out;
  const total = () => Math.max(1, (cached() ?? 0) + (fresh() ?? 0) + (out() ?? 0));
  const hitPct = () => cacheInput() > 0 && cached() != null ? Math.round((cached() / cacheInput()) * 100) : null;
  const pending = () => Number(kv().pending_requests || 0);
  const cacheStatus = () => hitPct() != null ? `KV cache hit rate ${hitPct()}%` : pending() > 0 ? "KV telemetry pending" : "KV cache usage not reported";
  const ttft = () => kv().avg_ttft_ms ?? kv().p50_ttft_ms;
  const tone = () => (hitPct() == null ? "idle" : hitPct() >= 60 ? "hot" : hitPct() >= 25 ? "warm" : "cold");
  const fmtTok = (n) => (n == null ? "—" : n >= 1_000_000 ? `${(n / 1_000_000).toFixed(2)}M` : n >= 1_000 ? `${(n / 1_000).toFixed(1)}k` : String(Math.round(n)));

  return (
    <div class={`kv-telemetry-bar ${props.compact ? "compact" : ""}`} title="Out-of-band Gateway KV Cache & Token Telemetry (0 extra prompt tokens)">
      <div class="kv-telemetry-top">
        <span class={`kv-hit-pill ${tone()}`} title={cacheStatus()}>
          <span class="kv-hit-dot" />
          KV {hitPct() != null ? `${hitPct()}%` : pending() > 0 ? "pending" : "—"}
        </span>
        <span class="kv-legend-item cached" title="Cached prefix tokens in requests with reported cache usage">
          <i /> Cached <b>{fmtTok(cached())}</b>
        </span>
        <span class="kv-legend-item fresh" title="Uncached input tokens in requests with reported cache usage">
          <i /> Fresh <b>{fmtTok(fresh())}</b>
        </span>
        <span class="kv-legend-item out" title="Generated output tokens">
          <i /> Out <b>{fmtTok(out())}</b>
        </span>
        <Show when={props.liveTps}>{(speed) => (
          <span class="mono xs dim" title="Estimated tokens per second from streamed output. Hidden reasoning is not counted.">~{speed().toFixed(1)} TPS</span>
        )}</Show>
        <div class="grow" />
        <Show when={ttft() != null}>
          <span class="mono xs faint" title="Average Time To First Token">TTFT {Math.round(ttft())}ms</span>
        </Show>
        <span class="mono xs faint">{reqs()} req{reqs() === 1 ? "" : "s"}{pending() > 0 ? ` · ${pending()} pending` : ""}</span>
      </div>
      <div class="kv-bar-track" role="img" aria-label={cacheStatus()}>
        <div class="kv-bar-seg cached" style={{ width: `${reqs() > 0 ? (cached() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg fresh" style={{ width: `${reqs() > 0 ? (fresh() / total()) * 100 : 0}%` }} />
        <div class="kv-bar-seg out" style={{ width: `${reqs() > 0 ? (out() / total()) * 100 : 0}%` }} />
      </div>
    </div>
  );
}
