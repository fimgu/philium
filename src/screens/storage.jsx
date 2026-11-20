import { createResource, createSignal, For, Show } from "solid-js";
import { Button, Field, Input } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";

// What Philium keeps on disk, what each part is for, and what cleaning up does.
// Philium applies the same rules by itself every time it starts.
export function Storage() {
  const [st, { refetch }] = createResource(() => rpc("storage.status").catch(e => ({ error: String(e) })));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [budget, setBudget] = createSignal("");
  const total = () => (st()?.parts || []).reduce((s, p) => s + (p.bytes || 0), 0);
  const low = () => st()?.disk && st().disk.free < st().disk.min_free_for_checks;
  async function clean() {
    setBusy(true);
    setError("");
    try {
      await rpc("storage.clean", {}, { action: true });
      await refetch();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  async function saveBudget() {
    setError("");
    const gb = Number(budget());
    if (!Number.isInteger(gb) || gb < 1 || gb > 1000) {
      setError("The build cache budget is a whole number of GB, from 1 to 1000.");
      return;
    }
    try {
      await rpc("storage.set_budget", { gb }, { action: true });
      setBudget("");
      await refetch();
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <section class="ws-section" aria-label="Storage">
      <div class="ws-row">
        <h2>Storage</h2>
        <Button busy={busy()} onClick={clean}>Clean up now</Button>
      </div>
      <Show when={st()} fallback={<p class="ws-muted">Measuring what Philium keeps on disk…</p>}>
        <Show when={st().error}>
          <p class="ws-alert" role="alert">{st().error}</p>
        </Show>
        <Show when={st().disk}>
          <p class={low() ? "ws-alert" : ""} role={low() ? "alert" : undefined}>
            {fmt.bytes(st().disk.free)} free of {fmt.bytes(st().disk.total)} on this Mac.
            {low() ? ` Checks do not start below ${fmt.bytes(st().disk.min_free_for_checks)} free, so a full disk is never read as a failing check. Clean up here, or free space elsewhere.` : ""}
          </p>
        </Show>
        <table class="ready plan" aria-label="What Philium keeps">
          <tbody>
            <For each={st().parts || []}>
              {p => (
                <tr data-part={p.key}>
                  <th scope="row">{p.label}</th>
                  <td class="mono">{fmt.bytes(p.bytes)}{p.partial ? "+" : ""}</td>
                  <td class="ws-muted">{p.rule}</td>
                </tr>
              )}
            </For>
            <tr>
              <th scope="row">In all</th>
              <td class="mono">{fmt.bytes(total())}</td>
              <td />
            </tr>
          </tbody>
        </table>
        <Field label={`Build cache budget (now ${st().budget_gb} GB)`} hint="Go build caches shared by agents and checks are trimmed to this, oldest files first, once a day and only while nothing builds.">
          <div class="ws-row">
            <Input type="number" min="1" max="1000" placeholder={String(st().budget_gb ?? "")} value={budget()} onValue={setBudget} aria-label="Build cache budget in GB" />
            <Button disabled={!budget().trim()} onClick={saveBudget}>Save</Button>
          </div>
        </Field>
        <Show when={st().last?.text}>
          <p aria-label="Last clean up">
            Last clean up {fmt.ago(st().last.at)}: {st().last.text}.
          </p>
          <Show when={(st().last.steps || []).length}>
            <ul>
              <For each={st().last.steps}>{s => <li>{s}</li>}</For>
            </ul>
          </Show>
        </Show>
      </Show>
      <Show when={error()}>
        <p class="ws-alert" role="alert">{error()}</p>
      </Show>
    </section>
  );
}
