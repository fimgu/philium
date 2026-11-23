import { For, Show } from "solid-js";
import { useClock } from "../lib/visibility.js";
import { fmt } from "../lib/rpc.js";
import { countdown, resetClock, tone, tightest, planLabel } from "../lib/account-usage.js";
import { AccountUsageLink } from "./account-usage-link.jsx";
import "../styles/account-usage.css";

function WindowRow(props) {
  const now = useClock(1000);
  const w = () => props.w;
  const pct = () => Math.min(100, Math.max(0, w().used_percent));
  const runsOut = () =>
    w().projected_full_at && (!w().resets_at || w().projected_full_at < w().resets_at);
  return (
    <div class="au-window">
      <div class="au-window-head">
        <span class="au-window-name" title={w().models?.length ? w().models.join(", ") : undefined}>{w().name}</span>
        <span class={`au-pct ${tone(w().used_percent)}`}>{Math.round(w().used_percent)}% used</span>
      </div>
      <div class={`au-bar ${tone(w().used_percent)}`} role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct()} aria-label={`${w().name} used`}>
        <div style={{ width: `${pct()}%` }} />
      </div>
      <div class="au-window-foot">
        <Show when={w().resets_at} fallback={<span class="faint">No reset reported</span>}>
          <span title={new Date(w().resets_at * 1000).toLocaleString()}>
            Resets in <b class="mono">{countdown(w().resets_at, now())}</b>
            <span class="faint"> · {resetClock(w().resets_at)}</span>
          </span>
        </Show>
        <Show when={runsOut()}>
          <span class="au-warn">Runs out in ~{countdown(w().projected_full_at, now())} at this pace</span>
        </Show>
      </div>
    </div>
  );
}

/** Full live usage card for one account, like a `jcode usage` block. */
export function AccountUsageCard(props) {
  const a = () => props.account;
  const now = useClock(1000);
  return (
    <article class={`au-card ${a().limit_reached ? "limited" : ""}`} aria-label={`${a().provider_name} usage`}>
      <header class="au-card-head">
        <div>
          <h3>{a().provider_name}</h3>
          <div class="xs dim">
            <Show when={a().account}><span class="mono">{a().account}</span></Show>
            <Show when={a().plan}><span class="au-plan">{planLabel(a().plan)}</span></Show>
          </div>
        </div>
        <div class="xs faint au-fresh">
          <Show when={a().stale}><span class="au-warn">Stale</span> · </Show>
          <Show when={a().fetched_at}>checked {fmt.ago(a().fetched_at)}</Show>
        </div>
      </header>
      <Show when={a().error && !a().windows?.length}>
        <p class="small au-err" role="alert">{a().error}</p>
      </Show>
      <Show when={a().error && a().windows?.length}>
        <p class="xs au-warn">Last refresh failed: {a().error}. Showing the last good reading.</p>
      </Show>
      <Show when={a().limit_reached}><p class="small au-err">Limit reached</p></Show>
      <For each={a().windows}>{(w) => <WindowRow w={w} />}</For>
      <Show when={a().key_status}>
        <p class="small">API key: <b>{a().key_status}</b> <span class="faint">(pay as you go, no usage windows)</span></p>
      </Show>
      <Show when={a().banked_resets || a().details?.length}>
        <dl class="au-details">
          <Show when={a().banked_resets}>
            <dt>Banked resets</dt>
            <dd>
              {a().banked_resets.available} available
              <Show when={a().banked_resets.next_available_at}>
                <span class="faint"> · next in {countdown(a().banked_resets.next_available_at, now())}</span>
              </Show>
            </dd>
          </Show>
          <For each={a().details}>{([k, v]) => <><dt>{k}</dt><dd>{v}</dd></>}</For>
        </dl>
      </Show>
      <footer class="xs faint au-card-foot">
        <Show when={a().last_used_ms} fallback={<span>Not used through Philium yet</span>}>
          <span>Last used in Philium {fmt.ago(a().last_used_ms)}</span>
        </Show>
        <AccountUsageLink kind={a().kind} />
      </footer>
    </article>
  );
}

/** Compact pill for Providers cards: tightest window plus live countdown. */
export function AccountUsagePill(props) {
  const now = useClock(1000);
  const w = () => tightest(props.account);
  return (
    <Show when={props.account}>
      <Show
        when={w()}
        fallback={
          <Show when={props.account.error || props.account.key_status}>
            <span class={`au-pill ${props.account.error ? "hot" : "ok"}`} title={props.account.error || ""}>
              {props.account.error ? "Usage unavailable" : `Key ${props.account.key_status}`}
            </span>
          </Show>
        }
      >
        <span
          class={`au-pill ${tone(w().used_percent)}`}
          title={props.account.windows.map((x) => `${x.name}: ${Math.round(x.used_percent)}% used`).join("\n")}
        >
          <span class="au-pill-bar"><span style={{ width: `${Math.min(100, w().used_percent)}%` }} /></span>
          {w().name} {Math.round(w().used_percent)}%
          <Show when={w().resets_at}><span class="faint">· {countdown(w().resets_at, now())}</span></Show>
          <Show when={props.account.stale}><span class="faint">· stale</span></Show>
        </span>
      </Show>
    </Show>
  );
}
