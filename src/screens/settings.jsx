import { createSignal, createMemo, onMount, onCleanup, Show, For } from "solid-js";
import { Page, Button, Field, Input, TextArea, Select, Toggle, Segmented, Badge, VirtualList, confirm, toast } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { useStore } from "../lib/store.js";
import { go } from "../lib/nav.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/workspace.css";
import { Storage } from "./storage.jsx";
import { McpServersSection } from "./mcp-servers.jsx";
import { HelperModelsSection } from "./helper-models.jsx";
import { useUpdate, UpdateControls } from "../ui/update.jsx";
import { modelsSummary, retrievalInUse, customServerFields } from "../lib/memory-format.js";

function LocalModelsRow() {
  const [st, setSt] = createSignal(null);
  const [busy, setBusy] = createSignal(false);
  let timer;
  const info = () => modelsSummary(st());
  async function load() {
    clearTimeout(timer);
    const r = await rpc("library.models_status", {}, { local: true }).catch(() => null);
    setSt(r);
    if (modelsSummary(r).downloading) timer = setTimeout(load, 1500);
  }
  async function download() {
    setBusy(true);
    try { await rpc("library.models_download", { enabled: true }, { local: true }); }
    catch (e) { toast(String(e), "danger"); }
    finally { setBusy(false); load(); }
  }
  onMount(load);
  onCleanup(() => clearTimeout(timer));
  return (
    <div class="ws-row">
      <div class="col" style={{ gap: "2px" }}>
        <strong style={{ "font-size": "12px" }}>Local search models</strong>
        <span class="xs faint">On-device embedding and reranking, about 90 MB, downloaded once.</span>
      </div>
      <div class="ws-actions">
        <Badge title={st()?.embed?.error || st()?.rerank?.error || ""} tone={info().state === "ready" ? "secure" : info().state === "failed" ? "strict" : ""}>{info().text}</Badge>
        <Show when={info().action}>
          <Button size="sm" busy={busy()} onClick={download}>{info().action}</Button>
        </Show>
      </div>
    </div>
  );
}

function DreamingRow() {
  const [cfg, setCfg] = createSignal(null);
  const load = () => rpc("library.dream_config_get", {}, { local: true }).then(setCfg).catch(() => null);
  async function save(patch) {
    try { setCfg(await rpc("library.dream_config_set", patch, { local: true })); }
    catch (e) { toast(String(e), "danger"); load(); }
  }
  onMount(load);
  return (
    <div class="ws-row">
      <div class="col" style={{ gap: "2px" }}>
        <strong style={{ "font-size": "12px" }}>Dreaming</strong>
        <span class="xs faint">When a workspace is idle, re-read recent sessions to fix, merge and re-rank its memories.</span>
      </div>
      <div class="ws-actions">
        <Show when={cfg()}>
          <Input type="number" min="0" step="10000" style={{ width: "96px" }} title="Daily token budget per workspace" value={String(cfg().daily_tokens)} onChange={(e) => save({ daily_tokens: Math.max(0, parseInt(e.currentTarget.value, 10) || 0) })} />
          <span class="xs faint">tokens/day</span>
          <Toggle on={!!cfg().enabled} label="" onChange={(v) => save({ enabled: v })} />
        </Show>
      </div>
    </div>
  );
}

function StrictSandboxSection(props) {
  const [sbStatus, setSbStatus] = createSignal(null);
  const [allowInput, setAllowInput] = createSignal("");
  const [isCustom, setIsCustom] = createSignal(false);
  const [egress, setEgress] = createSignal([]);
  const [showEgress, setShowEgress] = createSignal(false);
  const [sbBusy, setSbBusy] = createSignal(false);
  const [sbErr, setSbErr] = createSignal("");

  async function loadSandbox() {
    setSbErr("");
    try {
      const [st, al] = await Promise.all([
        rpc("sandbox.status", {}, { local: true }),
        rpc("sandbox.allowlist_get", {}, { local: true }),
      ]);
      setSbStatus(st);
      setAllowInput((al?.hosts ?? st?.allow_hosts ?? []).join(", "));
      setIsCustom(Boolean(al?.custom));
    } catch (e) {
      setSbErr(String(e));
    }
  }

  async function loadEgress() {
    setSbBusy(true);
    setSbErr("");
    try {
      const rows = await rpc("sandbox.egress", { limit: 100 }, { local: true });
      setEgress(rows ?? []);
      setShowEgress(true);
    } catch (e) {
      setSbErr(String(e));
    } finally {
      setSbBusy(false);
    }
  }
  // Keep an open egress log current as agents connect out.
  useVisibleEvent("sandbox:egress", () => showEgress() && !sbBusy() && rpc("sandbox.egress", { limit: 100 }, { local: true }).then((r) => setEgress(r ?? [])).catch(() => {}), { debounceMs: 1000 });

  async function saveAllowlist() {
    setSbBusy(true);
    setSbErr("");
    try {
      const r = await rpc("sandbox.allowlist_set", { hosts: allowInput() }, { local: true });
      setAllowInput((r?.hosts ?? []).join(", "));
      await loadSandbox();
      toast("Strict egress allowlist saved", "good");
    } catch (e) {
      setSbErr(String(e));
      toast(String(e), "danger");
    } finally {
      setSbBusy(false);
    }
  }

  onMount(loadSandbox);

  return (
    <section class="ws-section">
      <details onToggle={(e) => e.currentTarget.open && loadSandbox()}>
        <summary class="ws-row" style={{ cursor: "pointer" }}>
          <strong>Strict Sandbox &amp; Egress Allowlist</strong>
          <Show when={sbStatus()}>
            {(st) => (
              <div class="ws-actions">
                <Badge tone={st().available ? "secure" : "danger"}>
                  {st().available ? `OS sandbox active (${st().platform})` : "Sandbox unavailable"}
                </Badge>
                <Badge tone="secure">{st().allowed_total ?? 0} allowed</Badge>
                <Badge tone={(st().denied_total ?? 0) > 0 ? "strict" : "secure"}>
                  {st().denied_total ?? 0} blocked
                </Badge>
              </div>
            )}
          </Show>
        </summary>
        <div class="col" style={{ gap: "14px", "margin-top": "14px" }}>
          <p class="ws-muted" title="Agents on untrusted providers run in an OS sandbox and can only reach listed hosts via Philium's loopback proxy.">
            Outbound hosts allowed for Strict agents through the loopback egress proxy.
          </p>
          <Show when={sbErr()}>
            <div class="ws-alert" role="alert">{sbErr()}</div>
          </Show>
          <Show when={sbStatus()?.error}>
            <div class="ws-alert" role="alert">{sbStatus().error}</div>
          </Show>
          <Field
            label={`Allowed outbound hosts${isCustom() ? " (custom)" : " (defaults)"}`}
            hint="Comma- or newline-separated hostnames (e.g. registry.npmjs.org, pypi.org, crates.io, github.com)."
          >
            <div class="ws-row">
              <Input
                mono
                value={allowInput()}
                onValue={setAllowInput}
                placeholder="registry.npmjs.org, pypi.org, crates.io, github.com"
                aria-label="Allowed outbound hosts"
              />
              <Button busy={sbBusy() || props.busy} onClick={saveAllowlist}>Save allowlist</Button>
            </div>
          </Field>
          <div class="ws-row">
            <span class="xs faint">
              <Show when={sbStatus()?.egress_url}>Loopback proxy: <code>{sbStatus().egress_url}</code></Show>
            </span>
            <div class="ws-actions">
              <Button size="sm" busy={sbBusy()} onClick={loadEgress}>
                {showEgress() ? "Refresh egress log" : "Inspect egress log"}
              </Button>
              <Show when={showEgress()}>
                <Button size="sm" variant="ghost" onClick={() => setShowEgress(false)}>Hide log</Button>
              </Show>
            </div>
          </div>
          <Show when={showEgress()}>
            <Show
              when={egress().length}
              fallback={<p class="ws-muted">No strict egress connections recorded yet.</p>}
            >
              <div class="ws-ledger-list">
                <VirtualList items={egress()} rowHeight={64}>
                  {(r) => (
                    <article class="ws-ledger">
                      <div class="ws-row">
                        <strong>
                          {r.host}:{r.port} · {r.agent || "strict-agent"}
                        </strong>
                        <Badge tone={r.allowed ? "secure" : "danger"}>
                          {r.allowed ? "Allowed" : "Denied"}
                        </Badge>
                      </div>
                      <small>
                        {fmt.ago(r.ts)} · {fmt.bytes(r.bytes ?? 0)}
                      </small>
                    </article>
                  )}
                </VirtualList>
              </div>
            </Show>
          </Show>
        </div>
      </details>
    </section>
  );
}

function PlaybooksSection(props) {
  const [items, setItems] = createSignal([]);
  const [selectedJob, setSelectedJob] = createSignal("commit");
  const [currentPb, setCurrentPb] = createSignal(null);
  const [draftBody, setDraftBody] = createSignal("");
  const [pbBusy, setPbBusy] = createSignal(false);
  const [pbErr, setPbErr] = createSignal("");

  async function loadPlaybooks() {
    setPbErr("");
    try {
      const list = (await rpc("board.playbooks", {}, { local: true })) ?? [];
      setItems(list);
      const hit = list.find((p) => p.job === selectedJob()) || list[0];
      if (hit) {
        setSelectedJob(hit.job);
        setCurrentPb(hit);
        setDraftBody(hit.body ?? "");
      }
    } catch (e) {
      setPbErr(String(e));
    }
  }

  async function pickJob(job) {
    setSelectedJob(job);
    setPbErr("");
    try {
      const p = await rpc("board.playbook_get", { job }, { local: true });
      setCurrentPb(p);
      setDraftBody(p?.body ?? "");
    } catch (e) {
      setPbErr(String(e));
    }
  }

  async function savePlaybook() {
    const job = selectedJob();
    if (!job || job === "review_pr") return;
    setPbBusy(true);
    setPbErr("");
    try {
      const updated = await rpc("board.playbook_set", { job, body: draftBody() }, { local: true });
      setCurrentPb(updated);
      setDraftBody(updated?.body ?? draftBody());
      await loadPlaybooks();
      toast(`Playbook '${job}' saved`, "good");
    } catch (e) {
      setPbErr(String(e));
      toast(String(e), "danger");
    } finally {
      setPbBusy(false);
    }
  }

  async function resetPlaybook() {
    const job = selectedJob();
    if (!job || job === "review_pr") return;
    if (!(await confirm(`Reset playbook '${job}'?`, "Restores the built-in default steps and checks for this job.", { ok: "Reset to default" }))) return;
    setPbBusy(true);
    setPbErr("");
    try {
      const reset = await rpc("board.playbook_reset", { job }, { local: true });
      setCurrentPb(reset);
      setDraftBody(reset?.body ?? "");
      await loadPlaybooks();
      toast(`Playbook '${job}' reset to default`, "good");
    } catch (e) {
      setPbErr(String(e));
      toast(String(e), "danger");
    } finally {
      setPbBusy(false);
    }
  }

  return (
    <section class="ws-section">
      <details onToggle={(e) => e.currentTarget.open && !items().length && loadPlaybooks()}>
        <summary class="ws-row" style={{ cursor: "pointer" }}>
          <strong>Job Playbooks</strong>
          <span class="xs faint">Standard steps and checks for commit, PR, review, release &amp; teardown</span>
        </summary>
        <div class="col" style={{ gap: "14px", "margin-top": "14px" }}>
          <Show when={pbErr()}>
            <div class="ws-alert" role="alert">{pbErr()}</div>
          </Show>
          <div class="ws-row">
            <Field label="Playbook job">
              <Select
                value={selectedJob()}
                options={
                  items().length
                    ? items().map((p) => ({
                        value: p.job,
                        label: `${p.job} (${p.source || "builtin"})`,
                      }))
                    : ["commit", "open_pr", "test_pr", "review_pr", "docs", "release", "teardown"].map((j) => ({
                        value: j,
                        label: j,
                      }))
                }
                onValue={pickJob}
              />
            </Field>
            <Show when={currentPb()}>
              {(pb) => (
                <div class="ws-actions">
                  <Badge tone={pb().source === "user" ? "strict" : "secure"}>
                    Source: {pb().source || "builtin"}
                  </Badge>
                  <Show when={(pb().checks ?? []).length}>
                    <Badge>Checks: {pb().checks.join(", ")}</Badge>
                  </Show>
                </div>
              )}
            </Show>
          </div>
          <Show when={currentPb()}>
            {(pb) => (
              <>
                <Show when={pb().job === "review_pr"}>
                  <p class="ws-muted">
                    The bundled review policy is locked (SHA-256: <code>{(pb().policy_sha256 ?? "").slice(0, 16)}…</code>) and cannot be overridden.
                  </p>
                </Show>
                <Field
                  label="Playbook markdown (YAML front matter + numbered steps)"
                  hint="Agents load this via playbook_get and verify each step and check before completing a task."
                >
                  <TextArea
                    rows={8}
                    value={draftBody()}
                    onValue={setDraftBody}
                    disabled={pb().job === "review_pr" || pbBusy() || props.busy}
                    class="mono"
                  />
                </Field>
                <Show when={pb().job !== "review_pr"}>
                  <div class="ws-actions">
                    <Button
                      variant="primary"
                      busy={pbBusy() || props.busy}
                      onClick={savePlaybook}
                    >
                      Save playbook
                    </Button>
                    <Show when={pb().source === "user"}>
                      <Button
                        busy={pbBusy() || props.busy}
                        onClick={resetPlaybook}
                      >
                        Reset to built-in
                      </Button>
                    </Show>
                  </div>
                </Show>
              </>
            )}
          </Show>
        </div>
      </details>
    </section>
  );
}

function MemoryEmbeddingsSection(props) {
  const [cfg, setCfg] = createSignal(null);
  const [embProvider, setEmbProvider] = createSignal("");
  const [embEndpoint, setEmbEndpoint] = createSignal("");
  const [embModel, setEmbModel] = createSignal("");
  const [rrProvider, setRrProvider] = createSignal("");
  const [rrEndpoint, setRrEndpoint] = createSignal("");
  const [rrModel, setRrModel] = createSignal("");
  const [memBusy, setMemBusy] = createSignal(false);
  const [reindexing, setReindexing] = createSignal(false);
  const [memErr, setMemErr] = createSignal("");

  const trustedProviders = createMemo(() =>
    (props.providers ?? []).filter((p) => p && p.enabled !== 0 && p.enabled !== false && p.trusted !== false),
  );

  const providerOptions = (current) => {
    const list = trustedProviders();
    const opts = [
      { value: "", label: "Auto / Direct loopback" },
      ...list.map((p) => ({ value: p.id, label: p.name || p.id })),
    ];
    if (current && !opts.some((o) => o.value === current)) {
      opts.push({ value: current, label: current });
    }
    return opts;
  };

  async function loadConfig() {
    setMemErr("");
    try {
      const c = await rpc("library.config_get", {}, { local: true });
      if (!c) return;
      setCfg(c);
      const f = customServerFields(c);
      setEmbProvider(f.embProvider);
      setEmbEndpoint(f.embEndpoint);
      setEmbModel(f.embModel);
      setRrProvider(f.rrProvider);
      setRrEndpoint(f.rrEndpoint);
      setRrModel(f.rrModel);
    } catch {
      // Degrade gracefully if backend config_get is unavailable.
    }
  }

  async function saveConfig() {
    setMemBusy(true);
    setMemErr("");
    try {
      await rpc("library.config_set", {
        embedding_provider: embProvider().trim(),
        embedding_endpoint: embEndpoint().trim(),
        embedding_model: embModel().trim(),
        rerank_provider: rrProvider().trim(),
        rerank_endpoint: rrEndpoint().trim(),
        rerank_model: rrModel().trim(),
      });
      await loadConfig();
      toast("Memory retrieval config saved", "good");
    } catch (e) {
      setMemErr(String(e));
      toast(String(e), "danger");
    } finally {
      setMemBusy(false);
    }
  }

  async function runReindex() {
    setReindexing(true);
    setMemErr("");
    try {
      const res = await rpc("library.reindex", {});
      await loadConfig();
      const label = res?.embed_model || res?.mode || "Lexical (FTS5)";
      toast(`Reindexed ${res?.indexed ?? 0} memories (${label})`, "good");
    } catch (e) {
      setMemErr(String(e));
      toast(String(e), "danger");
    } finally {
      setReindexing(false);
    }
  }

  onMount(loadConfig);

  const activeEmbed = () => cfg()?.active_embed_model || "";
  const rerankerOn = () => Boolean(cfg()?.reranker_active || cfg()?.active_rerank_model);

  return (
    <section class="ws-section">
      <details onToggle={(e) => e.currentTarget.open && loadConfig()}>
        <summary class="ws-row" style={{ cursor: "pointer" }}>
          <strong>Memory Embeddings &amp; Reranker</strong>
          <div class="ws-actions">
            <Badge tone={activeEmbed() ? "secure" : ""}>
              {activeEmbed() || "Lexical (FTS5)"}
            </Badge>
            <Badge
              tone={rerankerOn() ? "secure" : ""}
              title={cfg()?.active_rerank_model ? `Active reranker: ${cfg().active_rerank_model}` : "Cross-encoder reranker off"}
            >
              {rerankerOn() ? "Reranker: Active" : "Reranker: Off"}
            </Badge>
          </div>
        </summary>
        <div class="col" style={{ gap: "14px", "margin-top": "14px" }}>
          <p class="ws-row" data-testid="retrieval-in-use">
            <span>
              In use: embedder <code>{retrievalInUse(cfg()).embed}</code>, reranker <code>{retrievalInUse(cfg()).rerank}</code>
            </span>
            <Button busy={reindexing() || props.busy} onClick={runReindex}>
              Reindex
            </Button>
          </p>
          <LocalModelsRow />
          <DreamingRow />
          <Show when={memErr()}>
            <div class="ws-alert" role="alert">{memErr()}</div>
          </Show>
          <details open={customServerFields(cfg()).open}>
            <summary class="ws-muted" style={{ cursor: "pointer" }}>Use a different model server</summary>
            <div class="col" style={{ gap: "14px", "margin-top": "10px" }}>
          <p class="ws-muted">
            Only needed to replace the local models. Remote providers route via Philium&apos;s gateway; direct endpoints must be localhost/loopback.
          </p>
          <div class="ws-row">
            <Field label="Embedding provider" hint="Trusted gateway provider, or leave on Auto for loopback.">
              <Select
                value={embProvider()}
                options={providerOptions(embProvider())}
                onValue={setEmbProvider}
                disabled={memBusy() || props.busy}
              />
            </Field>
            <Field label="Loopback endpoint" hint="Localhost only (e.g. http://127.0.0.1:11434/v1/embeddings).">
              <Input
                mono
                value={embEndpoint()}
                onValue={setEmbEndpoint}
                placeholder="http://127.0.0.1:11434/v1/embeddings"
                disabled={memBusy() || props.busy}
              />
            </Field>
            <Field label="Embedding model" hint="e.g. bge-m3, Qwen3-Embedding-0.6B">
              <Input
                mono
                value={embModel()}
                onValue={setEmbModel}
                placeholder="bge-m3"
                disabled={memBusy() || props.busy}
              />
            </Field>
          </div>
          <div class="ws-row">
            <Field label="Reranker provider" hint="Optional cross-encoder via trusted provider.">
              <Select
                value={rrProvider()}
                options={providerOptions(rrProvider())}
                onValue={setRrProvider}
                disabled={memBusy() || props.busy}
              />
            </Field>
            <Field label="Reranker endpoint" hint="Optional loopback /rerank endpoint.">
              <Input
                mono
                value={rrEndpoint()}
                onValue={setRrEndpoint}
                placeholder="http://127.0.0.1:8080/rerank"
                disabled={memBusy() || props.busy}
              />
            </Field>
            <Field label="Reranker model" hint="e.g. bge-reranker-v2-m3, Qwen3-Reranker-0.6B">
              <Input
                mono
                value={rrModel()}
                onValue={setRrModel}
                placeholder="bge-reranker-v2-m3"
                disabled={memBusy() || props.busy}
              />
            </Field>
          </div>
          <div class="ws-row">
            <span />
            <div class="ws-actions">
              <Button variant="primary" busy={memBusy() || props.busy} onClick={saveConfig}>
                Save
              </Button>
            </div>
          </div>
            </div>
          </details>
        </div>
      </details>
    </section>
  );
}

export default function Settings() {
  const store = useStore();
  const status = () => store.status() ?? {};
  const models = () => store.models() ?? [];
  const model = () => store.defaultModel() || "";
  const [settings, setSettings] = createSignal({});
  const [storageTick, setStorageTick] = createSignal(0);
  const [proposals, setProposals] = createSignal([]);
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal(""), [loaded, setLoaded] = createSignal(false);
  const value = (key, fallback) => settings()[key] ?? fallback;

  async function loadCleanup() {
    setProposals((await rpc("reaper.proposals")) ?? []);
  }
  async function refresh(manual = false) {
    if (manual) setStorageTick((t) => t + 1);
    try {
      const [s] = await Promise.all([
        rpc("app.settings_get"),
        loadCleanup(),
        store.refetchStatus(),
        store.refetchModels(),
        store.refetchDefault(),
      ]);
      setSettings(s ?? {});
      setError("");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoaded(true);
    }
  }
  async function act(fn) {
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      await fn();
    } catch (e) {
      setError(String(e));
      toast(String(e), "danger");
    } finally {
      setBusy(false);
    }
  }
  function save(key, val) {
    act(async () => {
      await rpc("app.settings_set", { key, value: val });
      setSettings((s) => ({ ...s, [key]: String(val) }));
      toast("Setting saved", "good");
    });
  }
  function hours(key, val) {
    const n = Number(val);
    if (!Number.isInteger(n) || n < 1 || n > 8760) {
      setError("Hours must be an integer between 1 and 8760.");
      return;
    }
    save(key, n);
  }
  onMount(() => refresh(false));
  useVisibleEvent(
    "reaper:proposals",
    () => loadCleanup().catch((e) => setError(String(e))),
    { debounceMs: 300 },
  );
  useVisibleEvent(
    "reaper:changed",
    () => loadCleanup().catch((e) => setError(String(e))),
    { debounceMs: 300 },
  );
  useVisibleEvent(
    "settings",
    () => rpc("app.settings_get", {}, { local: true }).then((s) => setSettings(s ?? {})).catch(() => {}),
    { debounceMs: 150 },
  );
  return <Page title="Settings" sub="Workspace defaults, features, and cleanup policy" actions={<Button busy={busy()} onClick={() => refresh(true)}>Refresh</Button>}>
    <Show when={error()}><div class="ws-alert" role="alert">{error()}</div></Show>
    <Show when={loaded()} fallback={<p>Loading settings…</p>}><div class="ws-settings">
      <Storage refreshTick={storageTick()} />
      <section class="ws-section">
        <h2>General</h2>
        <Field label="Philium folder" hint="Managed repos and agent worktrees." title="Managed clones live in repos/. Agent worktrees live in worktrees/. Existing repositories stay where you added them.">
          <code class="ws-path">{status().root ?? "Unavailable"}</code>
          <Button disabled={!status().root} onClick={() => act(() => rpc("workspace.open_in", { path: status().root, app: "finder" }))}>Open folder</Button>
        </Field>
        <Field label="Default model" hint="Fallback model for new agents.">
          <Select value={model()} placeholder="Choose a model" options={models().filter(m => m.enabled !== 0).map(m => ({ value: m.ref || `${m.provider_id}/${m.model_id}`, label: `${m.provider_name} / ${m.display || m.model_id}${m.trust === "untrusted" ? " (Strict)" : ""}` }))} disabled={busy()} onValue={v => act(async () => { await rpc("providers.set_default", { ref: v }); await store.refetchDefault(); toast(`Default model: ${v}`, "good"); })} />
        </Field>
        <Show when={!models().length}><p class="ws-muted">No models available. Add a provider first.</p></Show>
        <Field label="Release engineering tools" hint="Show SQL contract checks, local Makefile checks, and offline packet tools in Review.">
          <Toggle
            on={value("release_engineering_tools", "false") === "true"}
            disabled={busy()}
            label="Enable release engineering tools"
            onChange={(v) => save("release_engineering_tools", v ? "true" : "false")}
          />
        </Field>
        <Field label="Workday sidebar item" hint="Pin the personal Workday activity log under System (always accessible via ⌘K).">
          <Toggle
            on={value("show_workday", "false") === "true"}
            disabled={busy()}
            label="Show Workday in sidebar"
            onChange={(v) => save("show_workday", v ? "true" : "false")}
          />
        </Field>
        <Field label="Notifications" hint="A system notification when an agent finishes, stops with a problem or waits for approval while Philium is in the background. It shows only the agent name, never its output. The first one may ask for permission.">
          <Toggle
            on={value("notifications", "false") === "true"}
            disabled={busy()}
            label="Notify me in the background"
            onChange={(v) => save("notifications", v ? "true" : "false")}
          />
        </Field>
      </section>
      <HelperModelsSection repos={store.repos() ?? []} models={models()} parentModel={model()} busy={busy()} />
      <StrictSandboxSection busy={busy()} />
      <PlaybooksSection busy={busy()} />
      <McpServersSection repos={store.repos() ?? []} />
      <MemoryEmbeddingsSection providers={store.providers()} busy={busy()} />
      <section class="ws-section">
        <h2>Teardown</h2>
        <Field label="Cleanup mode" hint="Unsaved or unmerged work always requires confirmation." title="Ask is the default. Auto removes eligible idle resources without prompting, except unsaved or unmerged work.">
          <Segmented value={value("teardown.mode", "ask")} options={[{ value: "ask", label: "Ask" }, { value: "auto", label: "Auto" }]} onChange={v => save("teardown.mode", v)} />
        </Field>
        <div class="ws-row">
          <Field label="Idle timeout (hours)" hint="Flag idle agent resources after this period.">
            <Input type="number" min="1" max="8760" value={value("teardown.idle_hours", "4")} onChange={e => hours("teardown.idle_hours", e.currentTarget.value)} />
          </Field>
          <Field label="Failed worktree retention (hours)" hint="Keep failed or stopped worktrees for inspection.">
            <Input type="number" min="1" max="8760" value={value("teardown.failed_hours", "24")} onChange={e => hours("teardown.failed_hours", e.currentTarget.value)} />
          </Field>
        </div>
        <Toggle on={value("teardown.stop_colima", "false") === "true"} disabled={busy()} label="Stop Colima when unused (only if started by Philium)" onChange={v => save("teardown.stop_colima", v ? "true" : "false")} />
        <p class="ws-muted" title="Containers and clusters survive app quit. Review worktrees stay until accepted or discarded. Cleanup deletes resources permanently; agent scratch space is removed with its owner.">
          Containers survive app quit; cleanup removes resources permanently.
        </p>
      </section>
      <section class="ws-section"><div class="ws-row"><div><h2>Cleanup</h2><p class="ws-muted small">{proposals().length ? `${proposals().length} ready to clean up. ` : ""}What agents left running, and what Philium could not stop, lives in Environments.</p></div><div class="ws-actions"><Show when={!status().pilot}><Button busy={busy()} onClick={() => act(async () => { const result = await rpc("reaper.sweep_now"); await loadCleanup(); if (result.errors?.length) setError(result.errors.map(e => e.error).join("\n")); else toast(`${result.removed.length} removed, ${result.proposals.length} pending confirmation`); })}>Sweep now</Button></Show><Button variant="primary" onClick={() => go("environments")}>Open Environments</Button></div></div></section>
      <section class="ws-section"><h2>About</h2>
        <div class="ws-kv"><span>Version</span><div class="ws-row update-row"><UpdateControls update={useUpdate()} version={status().version} showCheck /></div></div>
        <div class="ws-kv"><span>Desk runtime</span><Badge tone={status().desk?.ok ? "secure" : "strict"}>{status().desk?.ok ? "Running" : "Unavailable"}</Badge></div>
        <Show when={status().desk?.error}><p class="ws-alert">{status().desk.error}</p></Show>
        <div class="ws-kv"><span>Gateway</span><code class="ws-path">{status().gateway || "Not running"}</code></div>
        <div class="ws-kv"><span>Application data</span><code class="ws-path" title={status().data_dir}>{status().data_dir || "Unavailable"}</code></div>
      </section>
    </div></Show>
  </Page>;
}
