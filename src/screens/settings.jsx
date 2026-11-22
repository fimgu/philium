import { createSignal, createMemo, onMount, Show, For } from "solid-js";
import { Page, Button, Field, Input, TextArea, Select, Toggle, Segmented, Badge, VirtualList, confirm, toast } from "../ui/kit.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { useStore } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/workspace.css";
import { Storage } from "./storage.jsx";

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

export default function Settings() {
  const store = useStore();
  const status = () => store.status() ?? {};
  const models = () => store.models() ?? [];
  const model = () => store.defaultModel() || "";
  const [settings, setSettings] = createSignal({});
  const [storageTick, setStorageTick] = createSignal(0);
  const [ledger, setLedger] = createSignal([]), [proposals, setProposals] = createSignal([]), [orphans, setOrphans] = createSignal(null);
  const [filter, setFilter] = createSignal("live"), [agent, setAgent] = createSignal("");
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal(""), [loaded, setLoaded] = createSignal(false);
  const value = (key, fallback) => settings()[key] ?? fallback;

  const agentOptions = createMemo(() => {
    const seen = new Map();
    for (const a of store.agents() ?? []) {
      if (a?.id) seen.set(a.id, a.name ? `${a.name} (${a.id})` : a.id);
    }
    for (const r of ledger() ?? []) {
      if (r?.agent_id && !seen.has(r.agent_id)) {
        seen.set(r.agent_id, r.agent_name ? `${r.agent_name} (${r.agent_id})` : r.agent_id);
      }
    }
    return [
      { value: "", label: "All agents" },
      ...[...seen.entries()].map(([val, label]) => ({ value: val, label })),
    ];
  });

  let ledgerRequest = 0;
  async function loadLedger() {
    const request = ++ledgerRequest;
    const rows = await rpc("reaper.ledger", {
      ...(filter() ? { state: filter() } : {}),
      ...(agent().trim() ? { agent_id: agent().trim() } : {}),
    });
    if (request === ledgerRequest) setLedger(rows ?? []);
  }
  async function loadCleanup() {
    await Promise.all([loadLedger(), rpc("reaper.proposals").then((p) => setProposals(p ?? []))]);
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
  async function proposal(p, approve) {
    await act(async () => {
      if (
        approve &&
        !(await confirm(
          p.unsaved ? "Permanently discard unsaved work?" : "Approve cleanup?",
          `${p.kind}: ${p.handle}\n${p.reason}\n${
            p.unsaved
              ? "This can delete dirty files and unpushed or unmerged commits. There is no archive or undo."
              : "This resource will be permanently removed."
          }`,
          { danger: true, ok: p.unsaved ? "Discard and delete" : "Remove resource" },
        ))
      )
        return;
      const results = await rpc(approve ? "reaper.approve" : "reaper.dismiss", { ids: [p.id] });
      await loadCleanup();
      const failed = results.filter((r) => r.error || r.needs_confirmation);
      if (failed.length)
        setError(
          failed
            .map(
              (r) =>
                r.error ||
                "Work changed since this proposal. Review the updated unsaved-work warning and approve again.",
            )
            .join("\n"),
        );
    });
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
      </section>
      <StrictSandboxSection busy={busy()} />
      <PlaybooksSection busy={busy()} />
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
      <section class="ws-section"><div class="ws-row"><h2>Cleanup</h2><Show when={!status().pilot}><Button busy={busy()} onClick={() => act(async () => { const result = await rpc("reaper.sweep_now"); await loadCleanup(); if (result.errors?.length) setError(result.errors.map(e => e.error).join("\n")); else toast(`${result.removed.length} removed, ${result.proposals.length} pending confirmation`); })}>Sweep now</Button></Show></div>
        <h3>Pending cleanup proposals ({proposals().length})</h3><Show when={proposals().length} fallback={<p class="ws-muted">No pending cleanup proposals.</p>}><div class="ws-proposal-list"><VirtualList items={proposals()} rowHeight={150}>{p => <article class="ws-proposal"><div class="ws-row"><strong>{p.kind} · {p.agent_name || p.agent_id || "No owner"}</strong><Badge tone={p.unsaved ? "strict" : "secure"}>{p.unsaved ? "Unsaved work" : "Ready"}</Badge></div><code class="ws-path">{p.handle}</code><p>{p.reason}</p><div class="ws-actions"><Button disabled={busy()} onClick={() => proposal(p, true)}>{p.unsaved ? "Review and discard" : "Approve removal"}</Button><Button disabled={busy()} onClick={() => proposal(p, false)}>Dismiss</Button></div></article>}</VirtualList></div></Show>
        <h3>Resource ledger</h3><div class="ws-actions"><Select value={filter()} options={[{ value: "live", label: "Live" }, { value: "leaked", label: "Leaked" }, { value: "released", label: "Released" }, { value: "", label: "All states" }]} onValue={v => { setFilter(v); act(loadLedger); }} /><Select value={agent()} options={agentOptions()} onValue={v => { setAgent(v); act(loadLedger); }} /><Button onClick={() => act(loadLedger)}>Filter</Button></div><Show when={ledger().length} fallback={<p class="ws-muted">No matching resources.</p>}><div class="ws-ledger-list"><VirtualList items={ledger()} rowHeight={88}>{r => <article class="ws-ledger"><div class="ws-row"><strong>{r.kind} · {r.agent_name || r.agent_id || "Workspace"}</strong><Badge tone={r.state === "leaked" ? "danger" : r.state === "released" ? "secure" : "strict"}>{r.state}</Badge></div><code class="ws-path" title={r.handle}>{r.handle}</code><small>Registered {fmt.ago(r.created_at)} · {r.id}</small></article>}</VirtualList></div></Show>
        <div class="ws-row"><h3>Untracked resources</h3><Show when={!status().pilot}><Button busy={busy()} onClick={() => act(async () => setOrphans(await rpc("reaper.orphans")))}>Scan disk &amp; runtimes</Button></Show></div><p class="ws-muted" title="Lists Philium worktree folders, containers labelled philium.agent, and clusters named philium-*. Inspect ownership before deleting.">Scans Philium worktrees, labelled containers, and clusters.</p><Show when={orphans()}>{o => <><Show when={o().errors?.length}><div class="ws-alert">Some checks could not run:<For each={o().errors}>{e => <p>{e}</p>}</For></div></Show><Show when={o().resources?.length} fallback={<p>No untracked resources found.</p>}><div class="ws-ledger-list"><VirtualList items={o().resources} rowHeight={72}>{r => <article class="ws-ledger"><strong>{r.kind}</strong><code class="ws-path">{r.handle}</code><small>{r.action || r.agent || "Inspect before cleanup"}</small></article>}</VirtualList></div></Show></>}</Show>
      </section>
      <section class="ws-section"><h2>About</h2><p>Philium {status().version || "unknown version"}</p><div class="ws-row"><span>Desk runtime</span><Badge tone={status().desk?.ok ? "secure" : "strict"}>{status().desk?.ok ? "Running" : "Unavailable"}</Badge></div><Show when={status().desk?.error}><p class="ws-alert">{status().desk.error}</p></Show><Field label="Gateway"><code>{status().gateway}</code></Field><Field label="Application data"><code class="ws-path">{status().data_dir}</code></Field></section>
    </div></Show>
  </Page>;
}
