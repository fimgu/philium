// Memory: clean, self-explanatory workspace memory, notes, procedures, and maintenance.
import { For, Show, createSignal, createResource, createMemo, createEffect, on, onCleanup } from "solid-js";
import { Page, Button, Badge, Segmented, Empty, Input, TextArea, Select, Meter, confirm, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc } from "../lib/rpc.js";
import { useStore, activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import { MEMORY_CATEGORIES, categoryLabel, memoryTitle, fileChips, relTime, modelsSummary } from "../lib/memory-format.js";
import "../styles/surfaces-memory.css";

const TIER = {
  user_direct: { label: "Direct", tone: "secure" },
  verified_run: { label: "Verified", tone: "accent" },
  agent_inferred: { label: "Agent", tone: "" },
  untrusted: { label: "Untrusted", tone: "strict" },
};

const PROC_STATUS = {
  active: { label: "Active", tone: "secure" },
  candidate: { label: "Candidate", tone: "strict" },
  rolled_back: { label: "Rolled back", tone: "" },
};

function summarizeAssertions(assertions) {
  if (!assertions) return null;
  if (Array.isArray(assertions)) {
    const total = assertions.length;
    const passed = assertions.filter((a) => (typeof a === "boolean" ? a : Boolean(a?.passed ?? a?.ok))).length;
    return total ? { passed, total } : null;
  }
  if (typeof assertions === "object") {
    const vals = Object.values(assertions);
    const total = vals.length;
    const passed = vals.filter((v) => (typeof v === "boolean" ? v : Boolean(v?.passed ?? v?.ok))).length;
    return total ? { passed, total } : null;
  }
  return null;
}

export default function Memory() {
  const store = useStore();
  const [statusTab, setStatusTab] = createSignal("active");
  const [scopeMode, setScopeMode] = createSignal("auto");
  const [search, setSearch] = createSignal("");
  const [asOf, setAsOf] = createSignal("");
  const [budget, setBudget] = createSignal(2000);
  const [showAdd, setShowAdd] = createSignal(false);

  const [supersedeTarget, setSupersedeTarget] = createSignal(null);
  const [content, setContent] = createSignal("");
  const [category, setCategory] = createSignal("preference");
  const [factKey, setFactKey] = createSignal("");
  const [validFrom, setValidFrom] = createSignal("");
  const [validTo, setValidTo] = createSignal("");
  const [saving, setSaving] = createSignal(false);

  const [rulesDraft, setRulesDraft] = createSignal("");
  const [rulesSaving, setRulesSaving] = createSignal(false);

  const [wnReq, setWnReq] = createSignal("");
  const [wnDec, setWnDec] = createSignal("");
  const [wnFail, setWnFail] = createSignal("");
  const [wnNext, setWnNext] = createSignal("");
  const [wnSaving, setWnSaving] = createSignal(false);

  const [intTrigger, setIntTrigger] = createSignal("pre_release");
  const [intMatch, setIntMatch] = createSignal("");
  const [intText, setIntText] = createSignal("");
  const [intSaving, setIntSaving] = createSignal(false);

  const [conPreview, setConPreview] = createSignal(null);
  const [conBusy, setConBusy] = createSignal(false);
  const [procBusy, setProcBusy] = createSignal(false);
  const [reindexing, setReindexing] = createSignal(false);

  const [auditBusy, setAuditBusy] = createSignal(false);
  const [poisonResult, setPoisonResult] = createSignal(null);
  const [rbSource, setRbSource] = createSignal("");
  const [rbBusy, setRbBusy] = createSignal(false);
  const [rbPreview, setRbPreview] = createSignal(null);
  const [rebuildBusy, setRebuildBusy] = createSignal(false);
  const [rankerBusy, setRankerBusy] = createSignal(false);

  const effectiveScope = createMemo(() => {
    if (scopeMode() === "global") return "global";
    if (scopeMode() === "all") return "";
    return activeRepoId() ? `repo:${activeRepoId()}` : "";
  });

  const targetScope = createMemo(() => (activeRepoId() ? `repo:${activeRepoId()}` : "global"));

  const activeRepoName = createMemo(() => {
    const r = (store.repos() ?? []).find((x) => x.id === activeRepoId());
    return r?.name || "Workspace";
  });

  const [libStatus, { refetch: refetchStatus }] = createResource(() =>
    rpc("library.status", {}, { local: true }).catch(() => null),
  );
  const [activeCount, { refetch: refetchActiveCount }] = createResource(
    () => effectiveScope() || "*",
    (scope) =>
      scope === "*"
        ? Promise.resolve(null)
        : rpc("library.count", { scope, status: "active" }, { local: true })
            .then((r) => (typeof r?.count === "number" ? r.count : null))
            .catch(() => null),
  );
  const [models, { refetch: refetchModels }] = createResource(() =>
    rpc("library.models_status", {}, { local: true }).catch(() => null),
  );
  const modelsInfo = createMemo(() => modelsSummary(models()));
  let modelsTimer;
  createEffect(() => {
    clearTimeout(modelsTimer);
    if (modelsInfo().downloading) modelsTimer = setTimeout(refetchModels, 1500);
  });
  onCleanup(() => clearTimeout(modelsTimer));
  const [modelsBusy, setModelsBusy] = createSignal(false);
  async function downloadModels() {
    setModelsBusy(true);
    try { await rpc("library.models_download", { enabled: true }, { local: true }); }
    catch (e) { toast(String(e), "danger"); }
    finally { setModelsBusy(false); refetchModels(); }
  }
  const [memConfig, { refetch: refetchConfig }] = createResource(() =>
    rpc("library.config_get", {}, { local: true }).catch(() => null),
  );
  const [rankerStatus, { refetch: refetchRanker }] = createResource(() =>
    rpc("library.ranker_status", {}, { local: true }).catch(() => null),
  );
  const [conVersions, { refetch: refetchVersions }] = createResource(
    () => targetScope(),
    (scope) =>
      rpc("library.consolidate_versions", { scope }, { local: true })
        .then((r) => (Array.isArray(r) ? r : []))
        .catch(() => []),
  );
  const [items, { refetch: refetchItems }] = createResource(
    () => [effectiveScope(), statusTab(), search(), asOf().trim()],
    ([scope, status, q, at]) => {
      if (status === "procedures") return [];
      if (status === "superseded" && at) {
        return rpc("library.as_of", {
          scope: scope || null,
          repo_id: !scope && activeRepoId() ? activeRepoId() : null,
          at,
          q: q || "",
          limit: 150,
        }, { local: true })
          .then((r) => (Array.isArray(r) ? r : []))
          .catch(() => []);
      }
      return rpc("library.list", {
        scope: scope || null,
        status: status === "all" ? null : status,
        q,
        limit: 150,
      }, { local: true })
        .then((r) => (Array.isArray(r) ? r : []))
        .catch(() => []);
    },
  );
  const [procedures, { refetch: refetchProcedures }] = createResource(
    () => effectiveScope(),
    (scope) =>
      rpc("library.procedures", {
        scope: scope || null,
        limit: 100,
      }, { local: true })
        .then((r) => (Array.isArray(r) ? r : []))
        .catch(() => []),
  );
  const [slate, { refetch: refetchSlate }] = createResource(
    () => [activeRepoId() || "", search(), budget()],
    ([repoId, q, b]) =>
      rpc("library.context_slate", {
        repo_id: repoId || null,
        q,
        budget_tokens: b,
        strict: false,
        include_global: !repoId,
      }, { local: true }).catch(() => null),
  );
  const [rulesData, { refetch: refetchRules }] = createResource(() =>
    rpc("library.rules_get", {}, { local: true }).catch(() => null),
  );
  const [workingNote, { refetch: refetchNote }] = createResource(
    () => targetScope(),
    (scope) => rpc("library.working_note_get", { scope }, { local: true }).catch(() => null),
  );
  const [intentions, { refetch: refetchIntentions }] = createResource(
    () => (activeRepoId() ? `repo:${activeRepoId()}` : ""),
    (scope) =>
      rpc("library.intentions", { scope: scope || null }, { local: true })
        .then((r) => (Array.isArray(r) ? r : []))
        .catch(() => []),
  );

  createEffect(on(rulesData, (r) => { if (r?.rules) setRulesDraft(r.rules); }));
  createEffect(on(workingNote, (n) => {
    if (!n) return;
    setWnReq(n.requirements ?? "");
    setWnDec(n.decisions ?? "");
    setWnFail(n.failed_attempts ?? "");
    setWnNext(n.next_steps ?? "");
  }));

  const refreshAll = () => {
    refetchStatus();
    refetchActiveCount();
    refetchConfig();
    refetchItems();
    refetchProcedures();
    refetchSlate();
    refetchNote();
    refetchIntentions();
    refetchVersions();
    refetchRanker();
  };

  useVisibleEvent("library", refreshAll, { debounceMs: 250 });

  const filteredProcedures = createMemo(() => {
    const list = procedures() ?? [];
    const q = search().trim().toLowerCase();
    if (!q) return list;
    return list.filter((p) =>
      `${p.title || ""} ${p.steps || ""} ${p.verify_cmd || ""} ${p.trigger_hint || ""}`.toLowerCase().includes(q),
    );
  });

  const submitMemory = async (e) => {
    e?.preventDefault();
    const txt = content().trim();
    if (!txt || saving()) return;
    setSaving(true);
    try {
      const sup = supersedeTarget();
      if (sup) {
        await rpc("library.supersede", {
          old_id: sup.id,
          content: txt,
          category: category(),
          fact_key: factKey().trim() || sup.fact_key || null,
        });
        toast("Updated", "secure");
      } else {
        const saveScope = scopeMode() === "global" || !activeRepoId() ? "global" : `repo:${activeRepoId()}`;
        const vf = validFrom().trim();
        const vt = validTo().trim();
        await rpc("library.save", {
          content: txt,
          category: category(),
          scope: saveScope,
          fact_key: factKey().trim() || null,
          trust_tier: "user_direct",
          provenance: "operator:memory-screen",
          ...(vf ? { valid_from: vf } : {}),
          ...(vt ? { valid_to: vt } : {}),
        });
        toast("Saved", "secure");
      }
      setContent("");
      setFactKey("");
      setValidFrom("");
      setValidTo("");
      setSupersedeTarget(null);
      setShowAdd(false);
      refreshAll();
    } catch (err) {
      toast(String(err), "danger");
    } finally {
      setSaving(false);
    }
  };

  const beginSupersede = (m) => {
    setSupersedeTarget(m);
    setContent(m.content || "");
    setCategory(m.category || "preference");
    setFactKey(m.fact_key || "");
    setValidFrom(m.valid_from || "");
    setValidTo(m.valid_to || "");
    setShowAdd(true);
  };

  const promote = async (id) => {
    try { await rpc("library.promote", { id }); toast("Approved", "secure"); refreshAll(); }
    catch (e) { toast(String(e), "danger"); }
  };

  const quarantine = async (id) => {
    try {
      await rpc("library.quarantine", { id, reason: "blocked" });
      setPoisonResult((prev) =>
        prev?.flagged
          ? { ...prev, flagged: prev.flagged.filter((f) => f.id !== id), clean: prev.flagged.length <= 1 }
          : prev,
      );
      toast("Blocked");
      refreshAll();
    } catch (e) { toast(String(e), "danger"); }
  };

  const forget = async (id) => {
    try { await rpc("library.forget", { id }); toast("Deleted"); refreshAll(); }
    catch (e) { toast(String(e), "danger"); }
  };

  const mineProcedures = async () => {
    if (procBusy()) return;
    setProcBusy(true);
    try {
      const res = await rpc("library.procedure_mine", { repo_id: activeRepoId() || null });
      toast(`Mined ${res?.mined ?? 0}, updated ${res?.updated ?? 0}`, "secure");
      refetchProcedures();
      refetchSlate();
    } catch (err) { toast(String(err), "danger"); }
    finally { setProcBusy(false); }
  };

  const promoteProcedure = async (id) => {
    if (procBusy()) return;
    setProcBusy(true);
    try {
      await rpc("library.procedure_promote", { id });
      toast("Procedure approved", "secure");
      refetchProcedures();
      refetchSlate();
    } catch (err) { toast(String(err), "danger"); }
    finally { setProcBusy(false); }
  };

  const rollbackProcedure = async (id) => {
    if (procBusy()) return;
    setProcBusy(true);
    try {
      await rpc("library.procedure_rollback", { id });
      toast("Procedure rolled back");
      refetchProcedures();
      refetchSlate();
    } catch (err) { toast(String(err), "danger"); }
    finally { setProcBusy(false); }
  };

  const saveWorkingNote = async (e) => {
    e?.preventDefault();
    setWnSaving(true);
    try {
      await rpc("library.working_note_set", {
        scope: targetScope(),
        requirements: wnReq(),
        decisions: wnDec(),
        failed_attempts: wnFail(),
        next_steps: wnNext(),
        updated_by: "user",
      });
      toast("Notes saved", "secure");
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setWnSaving(false); }
  };

  const saveStandingRules = async (e) => {
    e?.preventDefault();
    setRulesSaving(true);
    try {
      await rpc("library.rules_set", { rules: rulesDraft() });
      toast("Rules saved", "secure");
      refetchRules();
    } catch (err) { toast(String(err), "danger"); }
    finally { setRulesSaving(false); }
  };

  const addIntention = async (e) => {
    e?.preventDefault();
    const txt = intText().trim();
    if (!txt || intSaving()) return;
    setIntSaving(true);
    try {
      await rpc("library.intention_add", {
        scope: targetScope(),
        trigger_kind: intTrigger(),
        trigger_match: intMatch().trim(),
        instruction: txt,
      });
      setIntMatch("");
      setIntText("");
      toast("Reminder added", "secure");
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setIntSaving(false); }
  };

  const removeIntention = async (id) => {
    try { await rpc("library.intention_remove", { id }); refreshAll(); }
    catch (err) { toast(String(err), "danger"); }
  };

  const runReindex = async () => {
    if (reindexing()) return;
    setReindexing(true);
    try {
      const res = await rpc("library.reindex", {});
      toast(`Reindexed ${res?.indexed ?? 0} (${res?.embed_model || res?.mode || "Lexical"})`, "secure");
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setReindexing(false); }
  };

  const runConsolidatePreview = async () => {
    setConBusy(true);
    try {
      const res = await rpc("library.consolidate_preview", { scope: targetScope() });
      setConPreview(res);
      if (!(res?.proposals ?? []).length) toast("No duplicates found", "secure");
    } catch (err) { toast(String(err), "danger"); }
    finally { setConBusy(false); }
  };

  const promoteConsolidation = async (id) => {
    setConBusy(true);
    try {
      const res = await rpc("library.consolidate_promote", { id });
      toast(`Cleaned ${res?.applied ?? 0} duplicate(s)`, "secure");
      setConPreview(null);
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setConBusy(false); }
  };

  const rollbackConsolidation = async () => {
    const ok = await confirm(
      "Rollback consolidation?",
      `Restores the previous memory snapshot for ${targetScope()}. Deleted rows remain scrubbed.`,
      { ok: "Rollback" },
    );
    if (!ok) return;
    setConBusy(true);
    try {
      const latest = (conVersions() ?? [])[0];
      await rpc("library.consolidate_rollback", {
        scope: targetScope(),
        ...(latest?.version_id ? { version_id: latest.version_id } : {}),
      });
      toast("Snapshot restored", "secure");
      setConPreview(null);
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setConBusy(false); }
  };

  const runPoisonAudit = async () => {
    if (auditBusy()) return;
    setAuditBusy(true);
    try {
      const res = await rpc("library.poison_audit", { scope: effectiveScope() || null });
      setPoisonResult(res ?? { scanned: 0, flagged: [], clean: true });
      const count = (res?.flagged ?? []).length;
      toast(count ? `${count} memory item(s) flagged` : `Clean (${res?.scanned ?? 0} scanned)`, count ? "strict" : "secure");
    } catch (err) { toast(String(err), "danger"); }
    finally { setAuditBusy(false); }
  };

  const runRollbackSource = async (dryRun) => {
    const raw = rbSource().trim();
    if (!raw || rbBusy()) return;
    const isSession = raw.toLowerCase().startsWith("session:");
    const payload = {
      ...(isSession ? { session_id: raw.slice(8).trim() } : { source: raw }),
      scope: effectiveScope() || null,
      dry_run: dryRun,
    };
    if (!dryRun) {
      const countHint = rbPreview() ? ` (${rbPreview().matched ?? 0} matched)` : "";
      const ok = await confirm(
        "Rollback source?",
        `Scrub derived memories and events matching "${raw}"${countHint}. User-direct memories are preserved unless explicitly targeted.`,
        { danger: true, ok: "Rollback source" },
      );
      if (!ok) return;
    }
    setRbBusy(true);
    try {
      const res = await rpc("library.rollback_source", payload);
      if (dryRun) {
        setRbPreview(res ?? { matched: 0, scrubbed_events: 0, dry_run: true });
      } else {
        setRbPreview(null);
        setRbSource("");
        toast(`Rolled back ${res?.rolled_back ?? 0}, scrubbed ${res?.scrubbed_events ?? 0} event(s)`, "secure");
        refreshAll();
      }
    } catch (err) { toast(String(err), "danger"); }
    finally { setRbBusy(false); }
  };

  const runRebuildViews = async () => {
    if (rebuildBusy()) return;
    setRebuildBusy(true);
    try {
      const res = await rpc("library.rebuild_views", { scope: effectiveScope() || null });
      toast(
        `Rebuilt ${res?.rebuilt_memories ?? 0} · removed ${res?.removed_orphan_vectors ?? 0} orphan vector(s)`,
        "secure",
      );
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setRebuildBusy(false); }
  };

  const runRankerTrain = async () => {
    if (rankerBusy()) return;
    setRankerBusy(true);
    try {
      const res = await rpc("library.ranker_train", { scope: effectiveScope() || null });
      await refetchRanker();
      toast(
        res?.active ? `Ranker v${res.version ?? 1} active` : `Ranker checked (${res?.samples ?? 0} samples)`,
        res?.active ? "secure" : "",
      );
    } catch (err) { toast(String(err), "danger"); }
    finally { setRankerBusy(false); }
  };

  const slatePct = () => {
    const s = slate();
    if (!s || !s.budget_tokens) return 0;
    return Math.min(100, Math.round((s.used_tokens * 100) / s.budget_tokens));
  };

  const pendingCount = () => Number(libStatus()?.proposed ?? 0);
  const procCount = () => (procedures() ?? []).length;
  const activeEmbedModel = () => memConfig()?.active_embed_model || libStatus()?.embed_model || "Lexical (FTS5)";
  const rerankerActive = () => Boolean(memConfig()?.reranker_active || memConfig()?.active_rerank_model);
  const assertionSummary = createMemo(() => summarizeAssertions(conPreview()?.eval_report?.assertions));

  return (
    <Page
      title="Memory"
      actions={
        <>
          <input
            type="search"
            class="input"
            style={{ width: "200px", height: "30px", "font-size": "12px" }}
            placeholder="Search…"
            value={search()}
            onInput={(e) => setSearch(e.currentTarget.value)}
          />
          <Button size="sm" variant="primary" onClick={() => { setSupersedeTarget(null); setValidFrom(""); setValidTo(""); setShowAdd(!showAdd()); }}>
            <Icon name="plus" size={12} /> Remember
          </Button>
        </>
      }
    >
      <div class="col" style={{ gap: "16px" }}>
        <div class="row" style={{ "flex-wrap": "wrap", gap: "10px" }}>
          <Segmented
            value={statusTab()}
            onChange={setStatusTab}
            options={[
              { value: "active", label: `Active (${activeCount() ?? libStatus()?.memories ?? 0})` },
              { value: "proposed", label: pendingCount() ? `Pending (${pendingCount()})` : "Pending" },
              { value: "superseded", label: "History" },
              { value: "quarantined", label: "Blocked" },
              { value: "procedures", label: procCount() ? `Procedures (${procCount()})` : "Procedures" },
            ]}
          />
          <div class="grow" />
          <Segmented
            value={scopeMode()}
            onChange={setScopeMode}
            options={[
              { value: "auto", label: activeRepoId() ? activeRepoName() : "All" },
              { value: "global", label: "Global" },
              { value: "all", label: "Everything" },
            ]}
          />
        </div>

        <Show when={models() && modelsInfo().state !== "ready"}>
          <div class="mem-models-banner">
            <span class="grow">
              <span class="dim">Local search models</span> <span class="dim" title={models()?.embed?.error || models()?.rerank?.error || ""}>{modelsInfo().text}</span>
            </span>
            <Show when={modelsInfo().action}>
              <Button size="sm" variant="ghost" busy={modelsBusy()} onClick={downloadModels}>{modelsInfo().action}</Button>
            </Show>
          </div>
        </Show>

        <Show when={showAdd()}>
          <form class="card col" style={{ gap: "10px" }} onSubmit={submitMemory}>
            <div class="row">
              <strong class="grow">{supersedeTarget() ? "Update memory" : "Remember"}</strong>
              <Button size="sm" variant="ghost" type="button" onClick={() => { setShowAdd(false); setSupersedeTarget(null); }}>Close</Button>
            </div>
            <TextArea rows={2} value={content()} onValue={setContent} placeholder="What should agents remember?" />
            <div class="row" style={{ "flex-wrap": "wrap" }}>
              <Input mono value={factKey()} onValue={setFactKey} placeholder="Key (optional, e.g. network.local)" style={{ flex: "1", "min-width": "180px" }} />
              <Select
                value={category()}
                onValue={setCategory}
                options={MEMORY_CATEGORIES}
              />
              <Button variant="primary" type="submit" busy={saving()} disabled={!content().trim()}>
                {supersedeTarget() ? "Update" : "Save"}
              </Button>
            </div>
            <Show when={!supersedeTarget()}>
              <div class="mem-validity-row">
                <Input
                  mono
                  value={validFrom()}
                  onValue={setValidFrom}
                  placeholder="Valid from (ISO, optional)"
                  aria-label="Valid from"
                />
                <Input
                  mono
                  value={validTo()}
                  onValue={setValidTo}
                  placeholder="Valid to (ISO, optional)"
                  aria-label="Valid to"
                />
              </div>
            </Show>
          </form>
        </Show>

        <div class="mem-grid">
          <div class="col" style={{ gap: "8px" }}>
            <Show when={statusTab() === "superseded"}>
              <div class="mem-asof-bar">
                <span class="xs dim">As of</span>
                <Input
                  mono
                  value={asOf()}
                  onValue={setAsOf}
                  placeholder="ISO timestamp (e.g. 2026-04-14T12:00:00Z)"
                  aria-label="As of timestamp"
                  style={{ flex: "1", "min-width": "180px", height: "28px" }}
                />
                <Show when={asOf().trim()}>
                  <Badge tone="accent">Historical view</Badge>
                  <Button size="sm" variant="ghost" onClick={() => setAsOf("")}>Clear</Button>
                </Show>
              </div>
            </Show>

            <Show
              when={statusTab() === "procedures"}
              fallback={
                <Show
                  when={(items() ?? []).length}
                  fallback={
                    <Empty
                      icon={<Icon name="brain" size={24} />}
                      title={
                        statusTab() === "superseded" && asOf().trim()
                          ? "No memories valid at that timestamp"
                          : `No ${statusTab() === "proposed" ? "pending" : statusTab()} memories`
                      }
                      body={
                        statusTab() === "superseded" && asOf().trim()
                          ? "Try an earlier or later ISO timestamp, or clear As of to view superseded entries."
                          : "Use Remember to save rules, corrections, or facts."
                      }
                    />
                  }
                >
                  <For each={items()}>
                    {(m) => {
                      const t = TIER[m.trust_tier] ?? TIER.agent_inferred;
                      const title = memoryTitle(m);
                      const chips = fileChips(m.files);
                      const when = relTime(m.updated_at || m.created_at);
                      return (
                        <div class={`mem-card hover-actions ${m.status}`}>
                          <div class="mem-head">
                            <strong class="mem-title grow">{m.title ? title : ""}</strong>
                            <Show when={m.category}><span class="mem-cat">{categoryLabel(m.category)}</span></Show>
                            <Show when={when}><span class="xs faint" title={m.updated_at || m.created_at}>{when}</span></Show>
                          </div>
                          <Show when={!m.title || (m.content && m.content.trim() !== title)}>
                            <div class="mem-body">{m.content}</div>
                          </Show>
                          <Show when={chips.shown.length}>
                            <div class="mem-files">
                              <For each={chips.shown}>{(f) => <span class="mem-file mono" title={f}>{f.split("/").pop()}</span>}</For>
                              <Show when={chips.more}><span class="mem-file mono">+{chips.more}</span></Show>
                            </div>
                          </Show>
                          <div class="mem-meta">
                            <Badge tone={t.tone}>{t.label}</Badge>
                            <Show when={m.fact_key}><span class="mem-key">{m.fact_key}</span></Show>
                            <Show when={m.valid_from || m.valid_to}>
                              <span class="mem-chip mono" title={`valid_from: ${m.valid_from || "—"}, valid_to: ${m.valid_to || "∞"}`}>
                                {(m.valid_from || "…").slice(0, 10)}→{m.valid_to ? m.valid_to.slice(0, 10) : "∞"}
                              </span>
                            </Show>
                            <Show when={m.scope && m.scope !== "global"}><span class="faint">{m.scope.replace(/^repo:/, "")}</span></Show>
                            <span class="grow" />
                            <div class="row card-actions" style={{ gap: "4px" }}>
                              <Show when={m.status === "proposed" || m.status === "quarantined"}>
                                <Button size="sm" variant="primary" onClick={() => promote(m.id)}><Icon name="check" size={11} /> Approve</Button>
                              </Show>
                              <Show when={m.status === "active"}>
                                <Button size="sm" variant="ghost" onClick={() => beginSupersede(m)}>Update</Button>
                                <Button size="sm" variant="ghost" onClick={() => quarantine(m.id)}>Block</Button>
                              </Show>
                              <Button size="sm" variant="ghost" title="Delete" onClick={() => forget(m.id)}>
                                <Icon name="trash" size={11} />
                              </Button>
                            </div>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </Show>
              }
            >
              <div class="row" style={{ gap: "8px" }}>
                <span class="xs dim grow">
                  Verified-run procedures ({filteredProcedures().length})
                </span>
                <Button size="sm" variant="primary" busy={procBusy()} onClick={mineProcedures}>
                  <Icon name="spark" size={11} /> Mine
                </Button>
              </div>
              <Show
                when={filteredProcedures().length}
                fallback={
                  <Empty
                    icon={<Icon name="layers" size={24} />}
                    title="No procedures yet"
                    body="Mine verified passing runs to extract repeatable procedures."
                    action={
                      <Button size="sm" variant="primary" busy={procBusy()} onClick={mineProcedures}>
                        Mine verified runs
                      </Button>
                    }
                  />
                }
              >
                <For each={filteredProcedures()}>
                  {(p) => {
                    const st = PROC_STATUS[p.status] ?? { label: p.status || "Candidate", tone: "" };
                    const cardTone = p.status === "candidate" ? "proposed" : p.status === "rolled_back" ? "superseded" : "";
                    const runs = p.support_count ?? (Array.isArray(p.verified_run_ids) ? p.verified_run_ids.length : 1);
                    return (
                      <div class={`mem-card ${cardTone}`}>
                        <div class="row" style={{ gap: "8px" }}>
                          <strong class="grow">{p.title}</strong>
                          <Badge tone={st.tone}>{st.label}</Badge>
                        </div>
                        <Show when={p.steps}>
                          <div class="mem-body">{p.steps}</div>
                        </Show>
                        <div class="mem-meta">
                          <span class="mem-chip mono">v{p.version ?? 1}</span>
                          <span class="mem-chip mono">runs={runs}</span>
                          <Show when={p.verify_cmd}>
                            <span class="mem-key" title={p.verify_cmd}>{p.verify_cmd}</span>
                          </Show>
                          <Show when={p.trigger_hint}>
                            <span class="faint">{p.trigger_hint}</span>
                          </Show>
                          <Show when={p.scope && p.scope !== "global"}>
                            <span class="faint">{p.scope.replace(/^repo:/, "")}</span>
                          </Show>
                          <span class="grow" />
                          <div class="row" style={{ gap: "4px" }}>
                            <Show when={p.status !== "active"}>
                              <Button size="sm" variant="primary" busy={procBusy()} onClick={() => promoteProcedure(p.id)}>
                                <Icon name="check" size={11} /> Approve
                              </Button>
                            </Show>
                            <Show when={p.status !== "rolled_back"}>
                              <Button size="sm" variant="ghost" busy={procBusy()} onClick={() => rollbackProcedure(p.id)}>
                                Rollback
                              </Button>
                            </Show>
                          </div>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </Show>
            </Show>
          </div>

          <div class="col" style={{ gap: "12px" }}>
            <div class="card col" style={{ gap: "10px" }}>
              <div class="row">
                <strong class="grow">Project notes</strong>
                <Badge>{activeRepoId() ? activeRepoName() : "Global"}</Badge>
              </div>
              <form class="note-fields" onSubmit={saveWorkingNote}>
                <TextArea rows={2} value={wnReq()} onValue={setWnReq} placeholder="Requirements" />
                <TextArea rows={2} value={wnDec()} onValue={setWnDec} placeholder="Decisions" />
                <TextArea rows={2} value={wnFail()} onValue={setWnFail} placeholder="Avoid / dead ends" />
                <TextArea rows={2} value={wnNext()} onValue={setWnNext} placeholder="Next step" />
                <div class="row" style={{ "justify-content": "flex-end" }}>
                  <Button size="sm" variant="primary" type="submit" busy={wnSaving()}>Save</Button>
                </div>
              </form>
            </div>

            <details class="card quiet-details">
              <summary class="row" style={{ cursor: "pointer" }}>
                <strong class="small grow">Advanced</strong>
                <span class="xs faint">{slate()?.used_tokens ?? 0} / {budget()} tok</span>
              </summary>
              <div class="col" style={{ gap: "12px", "margin-top": "12px" }}>
                <div class="col" style={{ gap: "6px" }}>
                  <div class="row">
                    <span class="xs dim grow">Context budget</span>
                    <span class="xs mono">{slate()?.used_tokens ?? 0} / {budget()}</span>
                  </div>
                  <Meter value={slatePct()} tone={slatePct() > 85 ? "strict" : "secure"} />
                  <div class="row" style={{ gap: "4px" }}>
                    <For each={[1000, 2000, 4000, 8000]}>
                      {(b) => <Button size="sm" variant={budget() === b ? "primary" : "ghost"} onClick={() => setBudget(b)}>{b / 1000}k</Button>}
                    </For>
                  </div>
                </div>

                <div class="mem-adv-group">
                  <div class="row" style={{ "flex-wrap": "wrap", gap: "6px" }}>
                    <span class="xs dim grow">Retrieval</span>
                    <Badge tone={activeEmbedModel() !== "Lexical (FTS5)" ? "secure" : ""}>
                      {activeEmbedModel()}
                    </Badge>
                    <Badge
                      tone={rerankerActive() ? "accent" : ""}
                      title={memConfig()?.active_rerank_model || "Cross-encoder reranker off"}
                    >
                      {rerankerActive() ? "Reranker" : "Reranker off"}
                    </Badge>
                    <Button size="sm" variant="ghost" busy={reindexing()} onClick={runReindex}>Reindex</Button>
                  </div>
                </div>

                <div class="mem-adv-group">
                  <span class="xs dim">Reminders</span>
                  <form class="row" style={{ gap: "6px", "flex-wrap": "wrap" }} onSubmit={addIntention}>
                    <Select
                      value={intTrigger()}
                      onValue={setIntTrigger}
                      options={[
                        { value: "pre_release", label: "Pre-release" },
                        { value: "on_pr", label: "On PR" },
                        { value: "on_error", label: "On error" },
                        { value: "on_session_start", label: "Start" },
                        { value: "path_match", label: "Path" },
                        { value: "on_compact", label: "Compact" },
                      ]}
                    />
                    <Input mono value={intMatch()} onValue={setIntMatch} placeholder="Match (optional)" style={{ width: "130px" }} />
                    <Input value={intText()} onValue={setIntText} placeholder="Reminder…" style={{ flex: "1", "min-width": "140px" }} />
                    <Button size="sm" variant="primary" type="submit" busy={intSaving()} disabled={!intText().trim()}>Add</Button>
                  </form>
                  <For each={intentions() ?? []}>
                    {(it) => (
                      <div class="row xs" style={{ padding: "4px 6px", background: "var(--bg-raised)", "border-radius": "6px" }}>
                        <Badge tone="accent">{it.trigger_kind}</Badge>
                        <Show when={it.trigger_match}><span class="mem-key">{it.trigger_match}</span></Show>
                        <span class="grow">{it.instruction}</span>
                        <button type="button" class="icon-btn" onClick={() => removeIntention(it.id)}><Icon name="trash" size={11} /></button>
                      </div>
                    )}
                  </For>
                </div>

                <div class="mem-adv-group">
                  <div class="row" style={{ gap: "6px" }}>
                    <span class="xs dim grow">Duplicates</span>
                    <Button size="sm" busy={conBusy()} onClick={runConsolidatePreview}>Check</Button>
                    <Show when={(conVersions() ?? []).length > 0}>
                      <Button size="sm" variant="ghost" busy={conBusy()} onClick={rollbackConsolidation}>Rollback</Button>
                    </Show>
                  </div>
                  <Show when={conPreview()}>
                    {(p) => (
                      <div class="col" style={{ gap: "6px" }}>
                        <div class="row" style={{ "flex-wrap": "wrap", gap: "6px" }}>
                          <span class="xs">{(p().proposals ?? []).length} duplicate(s)</span>
                          <Show when={assertionSummary()}>
                            {(a) => (
                              <Badge tone={a().passed === a().total ? "secure" : "strict"}>
                                Assertions {a().passed}/{a().total}
                              </Badge>
                            )}
                          </Show>
                          <Show when={p().eval_report?.replay_eval}>
                            {(re) => (
                              <Badge tone={re().passed ? "secure" : "strict"}>
                                Replay {re().cases_passed ?? 0}/{re().cases_run ?? 0}
                              </Badge>
                            )}
                          </Show>
                          <div class="grow" />
                          <Show when={(p().proposals ?? []).length > 0}>
                            <Show
                              when={p().eval_report?.passed}
                              fallback={<Badge tone="strict">Gate failed</Badge>}
                            >
                              <Button size="sm" variant="primary" busy={conBusy()} onClick={() => promoteConsolidation(p().id)}>
                                Clean
                              </Button>
                            </Show>
                          </Show>
                        </div>
                      </div>
                    )}
                  </Show>
                  <Show when={(conVersions() ?? []).length > 0}>
                    <span class="xs faint">{conVersions().length} snapshot version(s) available</span>
                  </Show>
                </div>

                <div class="mem-adv-group">
                  <div class="row" style={{ "flex-wrap": "wrap", gap: "6px" }}>
                    <span class="xs dim grow">Safety &amp; maintenance</span>
                    <Button size="sm" busy={auditBusy()} onClick={runPoisonAudit}>Poison audit</Button>
                    <Button size="sm" variant="ghost" busy={rebuildBusy()} onClick={runRebuildViews}>Rebuild index</Button>
                  </div>

                  <Show when={poisonResult()}>
                    {(r) => (
                      <Show
                        when={(r().flagged ?? []).length > 0}
                        fallback={
                          <div class="row" style={{ gap: "6px" }}>
                            <Badge tone="secure">Clean</Badge>
                            <span class="xs faint">{r().scanned ?? 0} scanned</span>
                          </div>
                        }
                      >
                        <div class="col" style={{ gap: "6px" }}>
                          <div class="row" style={{ gap: "6px" }}>
                            <Badge tone="danger">{r().flagged.length} flagged</Badge>
                            <span class="xs faint">{r().scanned ?? 0} scanned</span>
                          </div>
                          <For each={r().flagged}>
                            {(f) => (
                              <div class="mem-audit-item">
                                <div class="row" style={{ gap: "6px" }}>
                                  <Badge tone={f.severity === "high" || f.severity === "critical" ? "danger" : "strict"}>
                                    {f.severity || "flagged"}
                                  </Badge>
                                  <span class="xs faint grow">
                                    {Array.isArray(f.reasons) ? f.reasons.join(", ") : String(f.reasons || "")}
                                  </span>
                                  <Button size="sm" variant="ghost" onClick={() => quarantine(f.id)}>Block</Button>
                                </div>
                                <Show when={f.content_preview}>
                                  <div class="xs dim">{f.content_preview}</div>
                                </Show>
                              </div>
                            )}
                          </For>
                        </div>
                      </Show>
                    )}
                  </Show>

                  <div class="row" style={{ gap: "6px", "flex-wrap": "wrap" }}>
                    <Input
                      mono
                      value={rbSource()}
                      onValue={(v) => { setRbSource(v); setRbPreview(null); }}
                      placeholder="Source (or session:<id>)"
                      aria-label="Rollback source"
                      style={{ flex: "1", "min-width": "130px", height: "28px" }}
                    />
                    <Button size="sm" variant="ghost" busy={rbBusy()} disabled={!rbSource().trim()} onClick={() => runRollbackSource(true)}>
                      Preview
                    </Button>
                    <Button size="sm" variant="danger" busy={rbBusy()} disabled={!rbSource().trim()} onClick={() => runRollbackSource(false)}>
                      Rollback
                    </Button>
                  </div>
                  <Show when={rbPreview()}>
                    {(p) => (
                      <div class="row xs faint" style={{ gap: "6px" }}>
                        <Badge tone="strict">Dry run</Badge>
                        <span>{p().matched ?? 0} matched · {p().scrubbed_events ?? 0} event(s)</span>
                      </div>
                    )}
                  </Show>

                  <div class="row" style={{ gap: "6px" }}>
                    <span class="xs dim grow">Ranker</span>
                    <span class="xs mono faint">
                      {rankerStatus()?.active
                        ? `v${rankerStatus().version ?? 1} · ${rankerStatus().samples ?? 0} samples`
                        : `Baseline · ${rankerStatus()?.samples ?? 0} samples`}
                    </span>
                    <Button size="sm" variant="ghost" busy={rankerBusy()} onClick={runRankerTrain}>Train</Button>
                  </div>
                </div>

                <form class="mem-adv-group" onSubmit={saveStandingRules}>
                  <span class="xs dim">Standing rules</span>
                  <TextArea rows={6} value={rulesDraft()} onValue={setRulesDraft} class="mono" />
                  <div class="row" style={{ "justify-content": "flex-end" }}>
                    <Button size="sm" variant="primary" type="submit" busy={rulesSaving()}>Save rules</Button>
                  </div>
                </form>
              </div>
            </details>
          </div>
        </div>
      </div>
    </Page>
  );
}
