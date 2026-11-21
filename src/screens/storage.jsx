import { createResource, createSignal, For, Show } from "solid-js";
import { Button, Field, Input, confirm } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";

const CLEANABLE_KEYS = new Set(["check_runs", "agents", "check_caches", "agent_caches", "database"]);

// What Philium keeps on disk, what each part is for, and what cleaning up does.
// Philium applies the same rules by itself every time it starts.
export function Storage(props = {}) {
  const [st, { refetch }] = createResource(
    () => props.refreshTick ?? 0,
    () => rpc("storage.status", {}, { local: true }).catch((e) => ({ error: String(e) })),
  );
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [budget, setBudget] = createSignal("");
  const total = () => (st()?.parts || []).reduce((s, p) => s + (p.bytes || 0), 0);
  const anyPartial = () => (st()?.parts || []).some((p) => p.partial === true);
  const low = () => st()?.disk && st().disk.free < st().disk.min_free_for_checks;
  async function clean() {
    const parts = (st()?.parts || []).filter((p) => CLEANABLE_KEYS.has(p.key));
    const lines = parts.length
      ? parts.map((p) => `• ${p.label} (${fmt.bytes(p.bytes)}${p.partial ? "+" : ""}): ${p.rule}`).join("\n")
      : [
          "• Check runs: leftover folders from exited check runs",
          "• Agent folders: removed agents' folders and stopped agents' temp folders",
          `• Check & build caches: Go build caches trimmed to the ${st()?.budget_gb ?? 10} GB budget`,
          "• Records: compact unused database and code-index space",
        ].join("\n");
    const ok = await confirm(
      "Clean up storage now?",
      `Philium will clean the following categories:\n\n${lines}\n\nActive agent worktrees, repositories, and running checks are not touched.`,
      { ok: "Clean up now" },
    );
    if (!ok) return;
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
              <td class="mono">{fmt.bytes(total())}{anyPartial() ? "+" : ""}</td>
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
