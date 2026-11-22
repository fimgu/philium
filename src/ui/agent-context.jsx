// Agent Memory side panel: quiet, immediate memory & notes right next to chat.
import { For, Show, createSignal, createResource, createEffect, on } from "solid-js";
import { Button, Badge, Meter, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc } from "../lib/rpc.js";
import { useVisibleEvent } from "../lib/visibility.js";
import "../styles/surfaces-memory.css";

const TIER = {
  user_direct: { label: "Direct", tone: "secure" },
  verified_run: { label: "Verified", tone: "accent" },
  agent_inferred: { label: "Agent", tone: "" },
  untrusted: { label: "Untrusted", tone: "strict" },
};

export function AgentContext(props) {
  const [newText, setNewText] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [noteOpen, setNoteOpen] = createSignal(false);
  const [req, setReq] = createSignal("");
  const [dec, setDec] = createSignal("");
  const [fail, setFail] = createSignal("");
  const [nextSteps, setNextSteps] = createSignal("");
  const [noteSaving, setNoteSaving] = createSignal(false);

  const scope = () => (props.repoId ? `repo:${props.repoId}` : "global");

  const [slate, { refetch: refetchSlate }] = createResource(
    () => [props.repoId || "", !!props.strict, props.modelRef || ""],
    ([repoId, strict, modelRef]) =>
      rpc("library.context_slate", {
        repo_id: repoId || null,
        q: "",
        model_ref: modelRef || null,
        strict,
        include_global: !repoId,
      }, { local: true }).catch(() => null),
  );

  const [proposed, { refetch: refetchProposed }] = createResource(
    () => props.repoId || "global",
    () => rpc("library.list", { scope: scope(), status: "proposed", limit: 20 }, { local: true }).catch(() => []),
  );

  const [note, { refetch: refetchNote }] = createResource(
    () => scope(),
    (s) => rpc("library.working_note_get", { scope: s }, { local: true }).catch(() => null),
  );

  createEffect(on(note, (n) => {
    if (!n) return;
    setReq(n.requirements ?? "");
    setDec(n.decisions ?? "");
    setFail(n.failed_attempts ?? "");
    setNextSteps(n.next_steps ?? "");
  }));

  const refreshAll = () => { refetchSlate(); refetchProposed(); refetchNote(); };
  useVisibleEvent("library", refreshAll, { debounceMs: 250 });

  const saveDirect = async (e) => {
    e?.preventDefault();
    const text = newText().trim();
    if (!text || saving()) return;
    setSaving(true);
    try {
      await rpc("library.save", {
        content: text,
        category: "preference",
        scope: scope(),
        trust_tier: "user_direct",
        provenance: `operator:agent:${props.name || "focus"}`,
      });
      setNewText("");
      toast("Saved", "secure");
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setSaving(false); }
  };

  const saveNote = async (e) => {
    e?.preventDefault();
    if (noteSaving()) return;
    setNoteSaving(true);
    try {
      await rpc("library.working_note_set", {
        scope: scope(),
        requirements: req(),
        decisions: dec(),
        failed_attempts: fail(),
        next_steps: nextSteps(),
        updated_by: "user",
      });
      toast("Notes saved", "secure");
      setNoteOpen(false);
      refreshAll();
    } catch (err) { toast(String(err), "danger"); }
    finally { setNoteSaving(false); }
  };

  const promote = async (id) => {
    try { await rpc("library.promote", { id }); toast("Approved", "secure"); refreshAll(); }
    catch (err) { toast(String(err), "danger"); }
  };

  const forget = async (id) => {
    try { await rpc("library.forget", { id }); refreshAll(); }
    catch (err) { toast(String(err), "danger"); }
  };

  const pct = () => {
    const s = slate();
    return s?.budget_tokens ? Math.min(100, Math.round((s.used_tokens * 100) / s.budget_tokens)) : 0;
  };

  const fmtBudget = () => {
    const b = slate()?.budget_tokens ?? 2000;
    return b % 1000 === 0 ? `${b / 1000}k` : `${(b / 1000).toFixed(1)}k`;
  };

  const hasNote = () => {
    const n = note();
    return !!(n && (n.requirements || n.decisions || n.failed_attempts || n.next_steps));
  };

  return (
    <div class="ctx-panel">
      <div class="ctx-header">
        <Icon name="brain" size={14} />
        <strong class="grow">Memory</strong>
        <Badge tone={props.strict ? "strict" : "secure"}>
          {props.strict ? "Strict" : props.repoName || "Global"}
        </Badge>
      </div>

      <Show when={props.strict}>
        <div class="shield strict small">
          <Icon name="shield" size={13} />
          Disabled in Strict mode.
        </div>
      </Show>

      <Show when={!props.strict}>
        <div class="col" style={{ gap: "4px" }}>
          <div class="row xs dim">
            <span class="grow">Context</span>
            <Show when={slate()?.reranker}>
              <span
                class="ctx-rerank-tag"
                title={typeof slate().reranker === "string" ? `Reranked (${slate().reranker})` : "Cross-encoder reranked"}
              >
                Reranked
              </span>
            </Show>
            <span class="mono">{slate()?.used_tokens ?? 0} / {fmtBudget()}</span>
          </div>
          <Meter value={pct()} tone={pct() > 85 ? "strict" : "secure"} />
        </div>

        <form class="row" style={{ gap: "6px" }} onSubmit={saveDirect}>
          <input
            class="input grow"
            style={{ height: "30px", "font-size": "12px" }}
            placeholder="Remember for this workspace…"
            value={newText()}
            onInput={(e) => setNewText(e.currentTarget.value)}
          />
          <Button size="sm" variant="primary" type="submit" busy={saving()} disabled={!newText().trim()}>Save</Button>
        </form>

        <Show when={(proposed() ?? []).length > 0}>
          <div class="ctx-section">
            <div class="ctx-section-head">
              <span class="grow">Pending ({proposed().length})</span>
            </div>
            <For each={proposed()}>
              {(m) => (
                <div class="mem-card proposed">
                  <div class="mem-body">{m.content}</div>
                  <div class="row" style={{ "justify-content": "flex-end" }}>
                    <Button size="sm" variant="ghost" onClick={() => forget(m.id)}>Dismiss</Button>
                    <Button size="sm" variant="primary" onClick={() => promote(m.id)}><Icon name="check" size={11} /> Approve</Button>
                  </div>
                </div>
              )}
            </For>
          </div>
        </Show>

        <div class="ctx-section">
          <div class="ctx-section-head">
            <span class="grow">Notes</span>
            <Button size="sm" variant="ghost" onClick={() => setNoteOpen(!noteOpen())}>
              {noteOpen() ? "Done" : hasNote() ? "Edit" : "Add"}
            </Button>
          </div>
          <Show when={noteOpen()} fallback={
            <Show when={hasNote()}>
              <div class="mem-card">
                <Show when={note()?.requirements}><div class="xs"><b class="dim">Req:</b> {note().requirements}</div></Show>
                <Show when={note()?.decisions}><div class="xs"><b class="dim">Decided:</b> {note().decisions}</div></Show>
                <Show when={note()?.failed_attempts}><div class="xs"><b class="dim">Avoid:</b> {note().failed_attempts}</div></Show>
                <Show when={note()?.next_steps}><div class="xs"><b class="dim">Next:</b> {note().next_steps}</div></Show>
              </div>
            </Show>
          }>
            <form class="note-fields" onSubmit={saveNote}>
              <textarea class="textarea" placeholder="Requirements" value={req()} onInput={(e) => setReq(e.currentTarget.value)} />
              <textarea class="textarea" placeholder="Decisions" value={dec()} onInput={(e) => setDec(e.currentTarget.value)} />
              <textarea class="textarea" placeholder="Avoid" value={fail()} onInput={(e) => setFail(e.currentTarget.value)} />
              <textarea class="textarea" placeholder="Next step" value={nextSteps()} onInput={(e) => setNextSteps(e.currentTarget.value)} />
              <div class="row" style={{ "justify-content": "flex-end" }}>
                <Button size="sm" variant="primary" type="submit" busy={noteSaving()}>Save</Button>
              </div>
            </form>
          </Show>
        </div>

        <Show when={(slate()?.procedures ?? []).length > 0}>
          <details class="ctx-section quiet-details">
            <summary class="ctx-section-head" style={{ cursor: "pointer" }}>
              <span class="grow">Procedures ({slate().procedures.length})</span>
              <span class="faint">▾</span>
            </summary>
            <div class="col" style={{ gap: "6px", "margin-top": "6px" }}>
              <For each={slate().procedures}>
                {(p) => (
                  <div class="mem-card">
                    <div class="mem-body xs"><strong>{p.title}</strong></div>
                    <Show when={p.steps}>
                      <div class="xs dim" style={{ "white-space": "pre-wrap" }}>{p.steps}</div>
                    </Show>
                    <div class="mem-meta">
                      <span class="mem-chip mono">v{p.version ?? 1}</span>
                      <Show when={p.support_count != null}>
                        <span class="mem-chip mono">runs={p.support_count}</span>
                      </Show>
                      <Show when={p.verify_cmd}>
                        <span class="mem-key">{p.verify_cmd}</span>
                      </Show>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </details>
        </Show>

        <div class="ctx-section">
          <div class="ctx-section-head">
            <span class="grow">Active ({(slate()?.items ?? []).length})</span>
          </div>
          <Show when={(slate()?.items ?? []).length} fallback={<div class="xs faint">Nothing saved yet.</div>}>
            <For each={slate()?.items ?? []}>
              {(item) => {
                const t = TIER[item.trust_tier] ?? TIER.agent_inferred;
                return (
                  <div class="mem-card hover-actions">
                    <div class="mem-body">{item.content}</div>
                    <div class="mem-meta">
                      <Badge tone={t.tone}>{t.label}</Badge>
                      <Show when={item.fact_key}><span class="mem-key">{item.fact_key}</span></Show>
                      <span class="grow" />
                      <button type="button" class="icon-btn card-actions" title="Delete" onClick={() => forget(item.id)}>
                        <Icon name="trash" size={11} />
                      </button>
                    </div>
                  </div>
                );
              }}
            </For>
          </Show>
        </div>
      </Show>
    </div>
  );
}
