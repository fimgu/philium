// Memory: clean, self-explanatory workspace memory, notes, and maintenance.
import { For, Show, createSignal, createResource, createMemo, createEffect, on } from "solid-js";
import { Page, Button, Badge, Segmented, Empty, Field, Input, TextArea, Select, Meter, toast } from "../ui/kit.jsx";
import { Icon } from "../ui/icons.jsx";
import { rpc } from "../lib/rpc.js";
import { useStore, activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/surfaces-memory.css";

const TIER = {
  user_direct: { label: "Direct", tone: "secure" },
  verified_run: { label: "Verified", tone: "accent" },
  agent_inferred: { label: "Agent", tone: "" },
  untrusted: { label: "Untrusted", tone: "strict" },
};

export default function Memory() {
  const store = useStore();
  const [statusTab, setStatusTab] = createSignal("active");
  const [scopeMode, setScopeMode] = createSignal("auto");
  const [search, setSearch] = createSignal("");
  const [budget, setBudget] = createSignal(2000);
  const [showAdd, setShowAdd] = createSignal(false);

  const [supersedeTarget, setSupersedeTarget] = createSignal(null);
  const [content, setContent] = createSignal("");
  const [category, setCategory] = createSignal("preference");
  const [factKey, setFactKey] = createSignal("");
  const [saving, setSaving] = createSignal(false);

  const [rulesDraft, setRulesDraft] = createSignal("");
  const [rulesSaving, setRulesSaving] = createSignal(false);

  const [wnReq, setWnReq] = createSignal("");
  const [wnDec, setWnDec] = createSignal("");
  const [wnFail, setWnFail] = createSignal("");
  const [wnNext, setWnNext] = createSignal("");
  const [wnSaving, setWnSaving] = createSignal(false);

  const [intTrigger, setIntTrigger] = createSignal("pre_release");
  const [intText, setIntText] = createSignal("");
  const [intSaving, setIntSaving] = createSignal(false);
  const [conPreview, setConPreview] = createSignal(null);
  const [conBusy, setConBusy] = createSignal(false);

  const effectiveScope = createMemo(() => {
    if (scopeMode() === "global") return "global";
    if (scopeMode() === "all") return "";
    return activeRepoId() ? `repo:${activeRepoId()}` : "";
  });

  const activeRepoName = createMemo(() => {
    const r = (store.repos() ?? []).find((x) => x.id === activeRepoId());
    return r?.name || "Workspace";
  });

  const [libStatus, { refetch: refetchStatus }] = createResource(() => rpc("library.status", {}, { local: true }).catch(() => null));
  const [items, { refetch: refetchItems }] = createResource(
    () => [effectiveScope(), statusTab(), search()],
    ([scope, status, q]) =>
      rpc("library.list", {
        scope: scope || null,
        status: status === "all" ? null : status,
        q,
        limit: 150,
      }, { local: true }).catch(() => []),
  );
  const [slate, { refetch: refetchSlate }] = createResource(
    () => [activeRepoId() || "", search(), budget()],
    ([repoId, q, b]) =>
      rpc("library.context_slate", {
        repo_id: repoId || null,
        q,
        budget_tokens: b,
        strict: false,
        include_global: true,
      }, { local: true }).catch(() => null),
  );
  const [rulesData, { refetch: refetchRules }] = createResource(() => rpc("library.rules_get", {}, { local: true }).catch(() => null));
  const [workingNote, { refetch: refetchNote }] = createResource(
    () => (activeRepoId() ? `repo:${activeRepoId()}` : "global"),
    (scope) => rpc("library.working_note_get", { scope }, { local: true }).catch(() => null),
  );
  const [intentions, { refetch: refetchIntentions }] = createResource(
    () => (activeRepoId() ? `repo:${activeRepoId()}` : ""),
    (scope) => rpc("library.intentions", { scope: scope || null }, { local: true }).catch(() => []),
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
    refetchItems();
    refetchSlate();
    refetchNote();
    refetchIntentions();
  };

  useVisibleEvent("library", refreshAll, { debounceMs: 250 });

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
        const targetScope = scopeMode() === "global" || !activeRepoId() ? "global" : `repo:${activeRepoId()}`;
        await rpc("library.save", {
          content: txt,
          category: category(),
          scope: targetScope,
          fact_key: factKey().trim() || null,
          trust_tier: "user_direct",
          provenance: "operator:memory-screen",
        });
        toast("Saved", "secure");
      }
      setContent("");
      setFactKey("");
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
    setShowAdd(true);
  };

  const promote = async (id) => {
    try { await rpc("library.promote", { id }); toast("Approved", "secure"); refreshAll(); }
    catch (e) { toast(String(e), "danger"); }
  };

  const quarantine = async (id) => {
    try { await rpc("library.quarantine", { id, reason: "blocked" }); toast("Blocked"); refreshAll(); }
    catch (e) { toast(String(e), "danger"); }
  };

  const forget = async (id) => {
    try { await rpc("library.forget", { id }); toast("Deleted"); refreshAll(); }
    catch (e) { toast(String(e), "danger"); }
  };

  const saveWorkingNote = async (e) => {
    e?.preventDefault();
    setWnSaving(true);
    try {
      const scope = activeRepoId() ? `repo:${activeRepoId()}` : "global";
      await rpc("library.working_note_set", {
        scope,
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
      const scope = activeRepoId() ? `repo:${activeRepoId()}` : "global";
      await rpc("library.intention_add", { scope, trigger_kind: intTrigger(), instruction: txt });
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

  const runConsolidatePreview = async () => {
    setConBusy(true);
    try {
      const scope = activeRepoId() ? `repo:${activeRepoId()}` : "global";
      const res = await rpc("library.consolidate_preview", { scope });
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

  const slatePct = () => {
    const s = slate();
    if (!s || !s.budget_tokens) return 0;
    return Math.min(100, Math.round((s.used_tokens * 100) / s.budget_tokens));
  };

  const pendingCount = () => Number(libStatus()?.proposed ?? 0);

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
          <Button size="sm" variant="primary" onClick={() => { setSupersedeTarget(null); setShowAdd(!showAdd()); }}>
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
              { value: "active", label: `Active (${libStatus()?.memories ?? 0})` },
              { value: "proposed", label: pendingCount() ? `Pending (${pendingCount()})` : "Pending" },
              { value: "superseded", label: "History" },
              { value: "quarantined", label: "Blocked" },
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
                options={[
                  { value: "preference", label: "Rule" },
                  { value: "correction", label: "Correction" },
                  { value: "fact", label: "Fact" },
                  { value: "procedure", label: "Procedure" },
                ]}
              />
              <Button variant="primary" type="submit" busy={saving()} disabled={!content().trim()}>
                {supersedeTarget() ? "Update" : "Save"}
              </Button>
            </div>
          </form>
        </Show>

        <div class="mem-grid">
          <div class="col" style={{ gap: "8px" }}>
            <Show
              when={(items() ?? []).length}
              fallback={<Empty icon={<Icon name="brain" size={24} />} title={`No ${statusTab() === "proposed" ? "pending" : statusTab()} memories`} body="Use Remember to save rules, corrections, or facts." />}
            >
              <For each={items()}>
                {(m) => {
                  const t = TIER[m.trust_tier] ?? TIER.agent_inferred;
                  return (
                    <div class={`mem-card hover-actions ${m.status}`}>
                      <div class="mem-body">{m.content}</div>
                      <div class="mem-meta">
                        <Badge tone={t.tone}>{t.label}</Badge>
                        <Show when={m.fact_key}><span class="mem-key">{m.fact_key}</span></Show>
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
              <div class="col" style={{ gap: "14px", "margin-top": "12px" }}>
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

                <div class="col" style={{ gap: "6px" }}>
                  <span class="xs dim">Reminders</span>
                  <form class="row" style={{ gap: "6px" }} onSubmit={addIntention}>
                    <Select
                      value={intTrigger()}
                      onValue={setIntTrigger}
                      options={[
                        { value: "pre_release", label: "Pre-release" },
                        { value: "on_pr", label: "On PR" },
                        { value: "on_error", label: "On error" },
                        { value: "on_session_start", label: "Start" },
                      ]}
                    />
                    <Input value={intText()} onValue={setIntText} placeholder="Reminder…" style={{ flex: "1" }} />
                    <Button size="sm" variant="primary" type="submit" busy={intSaving()} disabled={!intText().trim()}>Add</Button>
                  </form>
                  <For each={intentions() ?? []}>
                    {(it) => (
                      <div class="row xs" style={{ padding: "4px 6px", background: "var(--bg-raised)", "border-radius": "6px" }}>
                        <Badge tone="accent">{it.trigger_kind}</Badge>
                        <span class="grow">{it.instruction}</span>
                        <button type="button" class="icon-btn" onClick={() => removeIntention(it.id)}><Icon name="trash" size={11} /></button>
                      </div>
                    )}
                  </For>
                </div>

                <div class="col" style={{ gap: "6px" }}>
                  <div class="row">
                    <span class="xs dim grow">Duplicates</span>
                    <Button size="sm" busy={conBusy()} onClick={runConsolidatePreview}>Check</Button>
                  </div>
                  <Show when={conPreview()?.proposals?.length}>
                    <div class="row">
                      <span class="xs">{conPreview().proposals.length} duplicate(s)</span>
                      <div class="grow" />
                      <Button size="sm" variant="primary" busy={conBusy()} onClick={() => promoteConsolidation(conPreview().id)}>Clean</Button>
                    </div>
                  </Show>
                </div>

                <form class="col" style={{ gap: "6px" }} onSubmit={saveStandingRules}>
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
