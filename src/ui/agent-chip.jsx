import { For, Show } from "solid-js";

// Stable color per agent name, used for chips across the app.
export function agentHue(name = "") {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

export function AgentChip(props) {
  return <span class="agent-chip" style={{ "--h": agentHue(props.name) }}>{props.name}</span>;
}

export function RoleBadge(props) {
  const role = () => (props.role || "worker").toLowerCase();
  return (
    <span class={`role-badge role-${role()}`} title={`Role: ${role()}`}>
      {role() === "orchestrator" ? "ORCH" : role() === "reviewer" ? "REV" : "WORK"}
    </span>
  );
}

export function KvBadge(props) {
  const kv = () => props.kv || {};
  const reqs = () => kv().requests || 0;
  const hit = () => kv().cache_hit_pct ?? 0;
  const inp = () => kv().input_tokens || 0;
  const cached = () => kv().cached_tokens || 0;
  const ttft = () => kv().avg_ttft_ms;
  const tone = () => (hit() >= 60 ? "ok" : hit() >= 25 ? "warn" : inp() >= 8000 ? "bad" : "neutral");
  return (
    <Show when={reqs() > 0}>
      <span
        class={`kv-badge kv-${tone()}`}
        title={`KV Cache: ${hit()}% hit (${(cached() / 1000).toFixed(1)}k cached / ${(inp() / 1000).toFixed(1)}k input across ${reqs()} reqs)${ttft() ? ` · avg TTFT ${ttft()}ms` : ""}`}
      >
        KV {hit()}%
      </span>
    </Show>
  );
}

export function WarningPills(props) {
  const warnings = () => props.warnings ?? [];
  return (
    <Show when={warnings().length}>
      <div class="agent-warnings">
        <For each={warnings().slice(0, props.max ?? 3)}>
          {(w) => (
            <span class={`warn-pill warn-${w.severity || "med"} warn-kind-${w.kind}`} title={w.text}>
              {w.kind === "clash" ? "⚡ Clash" :
               w.kind === "kv_cache" ? "❄ Cold KV" :
               w.kind === "guard" ? "🛡 Guard" :
               w.kind === "rate_limit" ? "⏳ 429" :
               w.kind === "waiting" ? "✋ Approval" :
               "⚠ Alert"}
              <Show when={!props.compact}><span class="warn-pill-detail">{w.text}</span></Show>
            </span>
          )}
        </For>
      </div>
    </Show>
  );
}
