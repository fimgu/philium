import { createSignal, createEffect, on, Show, For } from "solid-js";
import { Field, Input, Select, Toggle, Button, toast } from "../ui/kit.jsx";
import { rpc } from "../lib/rpc.js";
import { JOBS, OFF_LINE, PRIVACY_LINE, normaliseConfig, validateConfig, pickableProviders, scopeLabel, jobModelOptions, previewLine } from "../lib/helper-routing.js";

// Settings > Helper models (docs/helper-routing.md, "Settings UI").
export function HelperModelsSection(props) {
  const [repoId, setRepoId] = createSignal("");
  const [data, setData] = createSignal(null);
  const [cfg, setCfg] = createSignal(normaliseConfig(null));
  const [previews, setPreviews] = createSignal({});
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");
  const rid = () => repoId() || null;

  async function load() {
    try {
      const r = await rpc("routing.get", rid() ? { repo_id: rid() } : {}, { local: true });
      setData(r ?? null);
      setCfg(normaliseConfig(r?.config));
      setErr("");
    } catch (e) { setErr(String(e)); }
    loadPreviews();
  }
  async function loadPreviews() {
    const parent = props.parentModel;
    if (!cfg().enabled || !parent) { setPreviews({}); return; }
    const out = {};
    await Promise.all(JOBS.map(async ({ id }) => {
      out[id] = await rpc("routing.preview", { repo_id: rid(), parent_model: parent, job: id }, { local: true }).catch((e) => ({ error: String(e) }));
    }));
    setPreviews(out);
  }
  createEffect(on([repoId, () => props.parentModel], load));

  const providers = () => data()?.providers ?? [];
  const pickable = () => pickableProviders(providers());

  async function save(next) {
    const problem = validateConfig(next, providers());
    if (problem) { setErr(problem); return; }
    setBusy(true);
    try {
      await rpc("routing.set", { repo_id: rid(), config: next }, { local: true });
      toast("Helper models saved", "good");
      await load();
    } catch (e) { setErr(String(e)); toast(String(e), "danger"); }
    finally { setBusy(false); }
  }
  async function reset() {
    setBusy(true);
    try {
      await rpc("routing.set", { repo_id: rid(), config: null }, { local: true });
      toast("Workspace now uses the global setting", "good");
      await load();
    } catch (e) { setErr(String(e)); }
    finally { setBusy(false); }
  }
  const patch = (p) => save({ ...cfg(), ...p });
  const toggleAllowed = (id, on) => patch({ allowed: on ? [...cfg().allowed, id] : cfg().allowed.filter((x) => x !== id) });
  const off = () => busy() || props.busy;

  return <section class="ws-section" data-section="helper-models">
    <h2>Helper models</h2>
    <p class="ws-muted small">Pick which models helpers use, based on the job and how busy each provider is. Strict agents always keep their own model.</p>
    <Show when={err()}><div class="ws-alert" role="alert">{err()}</div></Show>
    <Field label="Applies to" hint={scopeLabel(data()?.source, rid())}>
      <Select value={repoId()} options={[{ value: "", label: "All workspaces" }, ...(props.repos ?? []).map((r) => ({ value: r.id, label: r.name || r.path || r.id }))]} onValue={(v) => setRepoId(v ?? "")} />
      <Show when={rid() && data()?.source === "workspace"}><Button disabled={off()} onClick={reset}>Reset to global</Button></Show>
    </Field>
    <Toggle on={cfg().enabled} disabled={off()} label="Route helpers to other models" onChange={(v) => patch({ enabled: v })} />
    <Show when={cfg().enabled} fallback={<p class="ws-muted">{OFF_LINE}</p>}>
      <Field label="Allowed providers" hint={PRIVACY_LINE}>
        <div class="col gap-4">
          <For each={pickable()} fallback={<span class="ws-muted small">No trusted providers are set up.</span>}>{(p) =>
            <label class="row gap-4 small"><input type="checkbox" checked={cfg().allowed.includes(p.id)} disabled={off()} onChange={(e) => toggleAllowed(p.id, e.currentTarget.checked)} />{p.name}<span class="xs faint">{p.usage}</span></label>
          }</For>
        </div>
      </Field>
      <div class="ws-row">
        <For each={JOBS}>{(j) =>
          <Field label={j.label} hint={j.hint}>
            <Select value={cfg().jobs[j.id] ?? ""} options={jobModelOptions(props.models ?? [], providers(), cfg().allowed, cfg().jobs[j.id])} disabled={off()} onValue={(v) => patch({ jobs: { ...cfg().jobs, [j.id]: v || null } })} />
          </Field>
        }</For>
      </div>
      <Field label="Avoid a provider above this usage (%)" hint="Skip a provider once its busiest usage window reaches this.">
        <Input type="number" min="1" max="100" value={cfg().avoid_above_pct} disabled={off()} onChange={(e) => patch({ avoid_above_pct: Number(e.currentTarget.value) })} />
      </Field>
      <Toggle on={cfg().verify_elsewhere} disabled={off()} label="Verify on a different provider" onChange={(v) => patch({ verify_elsewhere: v })} />
      <div class="ws-muted small" data-testid="routing-preview">
        <div>Right now, a helper of {props.parentModel || "the default model"} would use:</div>
        <For each={JOBS}>{(j) => <div class="mono xs">{previewLine(j.id, previews()[j.id])}</div>}</For>
      </div>
    </Show>
  </section>;
}
