// Inline Access Request approval cards and proactive Access Grant modal.
import { For, Show, createSignal, createResource, createMemo, createEffect, on } from "solid-js";
import { Button, Badge, Modal, Field, Input, Select, Segmented, Toggle, toast } from "./kit.jsx";
import { Icon } from "./icons.jsx";
import { rpc, fmt } from "../lib/rpc.js";
import { useVisibleEvent } from "../lib/visibility.js";
import {
  allowedScopesFor,
  scopeMeta,
  sanitizeScopes,
  defaultScopesForRequest,
  defaultScopesForGrant,
  requestAskedForWrite,
  availableLifetimes,
  defaultLifetime,
  normalizeTargetKind,
  targetKindLabel,
  inferTargetLabel,
  canApproveAccess,
  buildDecisionPayload,
  buildGrantPayload,
} from "../lib/access-helpers.js";
import "../styles/access.css";

function AccessRequestCard(props) {
  const req = () => props.request;
  const isStrict = () => Boolean(req().strict || props.strict);
  const effectiveRepoId = () => req().agent_repo_id || props.repoId || null;

  const [targetKind, setTargetKind] = createSignal(normalizeTargetKind(req().target_kind));
  const [targetRepoId, setTargetRepoId] = createSignal(req().resolved_repo_id || "");
  const [targetPath, setTargetPath] = createSignal(req().resolved_path || "");
  const [label, setLabel] = createSignal(
    req().resolved_repo_name || inferTargetLabel(req().target_kind, req().resolved_repo_name, req().resolved_path),
  );
  const [pickerOpen, setPickerOpen] = createSignal(!req().resolved_path);
  const [scopes, setScopes] = createSignal(
    defaultScopesForRequest(req().requested_scopes, req().target_kind, isStrict()),
  );
  const [lifetime, setLifetime] = createSignal(defaultLifetime());
  const [busy, setBusy] = createSignal(false);

  const allowed = createMemo(() => allowedScopesFor(targetKind(), isStrict()));
  const lifetimes = createMemo(() => availableLifetimes({ hasRepo: Boolean(effectiveRepoId()) }));
  const askedWrite = createMemo(() => requestAskedForWrite(req()));

  const selectedRepo = createMemo(() =>
    (props.catalog ?? []).find((r) => r.id === targetRepoId()) ?? null,
  );

  const resolvedTargetText = createMemo(() => {
    const repo = selectedRepo();
    if (targetKind() === "repo" && repo) return repo.name;
    if (label()) return label();
    if (targetPath()) return targetPath();
    return req().resolved_repo_name || req().resolved_path || req().target_hint || "";
  });

  const isResolved = createMemo(() =>
    Boolean(
      (targetKind() === "repo" && (targetRepoId() || targetPath())) ||
      (targetKind() !== "repo" && targetPath().trim()),
    ),
  );

  const canAllow = createMemo(() =>
    !busy() &&
    canApproveAccess({
      targetKind: targetKind(),
      targetRepoId: targetRepoId(),
      targetPath: targetPath(),
      scopes: scopes(),
      strict: isStrict(),
    }),
  );

  const updateKind = (nextKind) => {
    const k = normalizeTargetKind(nextKind);
    setTargetKind(k);
    if (k !== "repo") setTargetRepoId("");
    setScopes((cur) => {
      const next = sanitizeScopes(cur, k, isStrict());
      return next.length ? next : ["read"];
    });
  };

  const selectCatalogRepo = (repoId) => {
    if (!repoId) {
      setTargetRepoId("");
      return;
    }
    const repo = (props.catalog ?? []).find((r) => r.id === repoId);
    setTargetKind("repo");
    setTargetRepoId(repoId);
    setTargetPath(repo?.path || "");
    setLabel(repo?.name || "");
    setScopes((cur) => {
      const next = sanitizeScopes(cur, "repo", isStrict());
      return next.length ? next : defaultScopesForGrant("repo", isStrict());
    });
  };

  const pickFolder = async () => {
    try {
      const res = await rpc("app.pick_folder", { prompt: "Choose a folder to share with this agent" });
      if (res?.path) {
        setTargetKind("folder");
        setTargetRepoId("");
        setTargetPath(res.path);
        setLabel(inferTargetLabel("folder", "", res.path));
        setScopes((cur) => {
          const next = sanitizeScopes(cur, "folder", isStrict());
          return next.length ? next : ["read"];
        });
      }
    } catch (e) {
      toast(String(e), "danger");
    }
  };

  const onManualPath = (val) => {
    setTargetPath(val);
    if (targetKind() === "repo") {
      setTargetKind("folder");
      setTargetRepoId("");
      setScopes((cur) => {
        const next = sanitizeScopes(cur, "folder", isStrict());
        return next.length ? next : ["read"];
      });
    }
    setLabel(inferTargetLabel(targetKind(), "", val));
  };

  const toggleScope = (scopeId) => {
    setScopes((cur) =>
      cur.includes(scopeId) ? cur.filter((s) => s !== scopeId) : sanitizeScopes([...cur, scopeId], targetKind(), isStrict()),
    );
  };

  const decide = async (decision) => {
    if (busy()) return;
    setBusy(true);
    try {
      const payload = buildDecisionPayload({
        requestId: req().id,
        decision,
        targetKind: targetKind(),
        targetRepoId: targetRepoId(),
        targetPath: targetPath(),
        label: label() || inferTargetLabel(targetKind(), selectedRepo()?.name, targetPath()),
        scopes: scopes(),
        lifetime: lifetime(),
        strict: isStrict(),
      });
      await rpc("access.decide", payload, { action: true });
      toast(decision === "approve" ? "Access granted" : "Access denied", decision === "approve" ? "secure" : "");
      props.onDecided?.();
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class={`access-card ${isStrict() ? "is-strict" : ""}`} role="region" aria-label="Access request">
      <div class="access-card-head">
        <Icon name={isStrict() ? "shield" : "lock"} size={13} />
        <span class="access-card-title">Access request</span>
        <Badge>{targetKindLabel(targetKind())}</Badge>
        <Show when={isStrict()}>
          <Badge tone="strict"><Icon name="shield" size={10} /> Strict</Badge>
        </Show>
        <div class="grow" />
        <Show when={req().created_at}>
          <span class="xs faint">{fmt.ago(req().created_at)}</span>
        </Show>
      </div>

      <Show when={isStrict()}>
        <div class="access-strict-note" role="note">
          <Icon name="shield" size={12} />
          <span>Untrusted provider ({req().provider || "strict"}) — approved content may be sent to this provider.</span>
        </div>
      </Show>

      <Show when={req().reason}>
        <p class="access-reason">{req().reason}</p>
      </Show>

      <div class="access-target-row">
        <span class="access-target-label">Target</span>
        <code
          class={`access-target-path ${isResolved() ? "" : "unresolved"}`}
          title={targetPath() || req().target_hint || "Choose target"}
        >
          {resolvedTargetText() || "Select target…"}
        </code>
        <Show when={targetPath() && resolvedTargetText() !== targetPath()}>
          <span class="xs faint ellipsis" title={targetPath()}>{targetPath()}</span>
        </Show>
        <div class="grow" />
        <Button size="sm" variant="ghost" onClick={() => setPickerOpen(!pickerOpen())}>
          {pickerOpen() ? "Hide target" : "Change target"}
        </Button>
      </div>

      <Show when={pickerOpen()}>
        <div class="access-picker">
          <div class="access-picker-row">
            <Show when={(props.catalog ?? []).length > 0}>
              <select
                class="select"
                aria-label="Choose registered repository"
                value={targetKind() === "repo" ? targetRepoId() : ""}
                onChange={(e) => selectCatalogRepo(e.currentTarget.value)}
              >
                <option value="">Choose registered repo…</option>
                <For each={props.catalog ?? []}>
                  {(r) => <option value={r.id}>{r.name}</option>}
                </For>
              </select>
            </Show>
            <Button size="sm" onClick={pickFolder}>Choose folder…</Button>
          </div>
          <div class="access-picker-row">
            <input
              type="text"
              class="input mono"
              aria-label="Target path"
              placeholder="Or enter a folder or file path…"
              value={targetPath()}
              onInput={(e) => onManualPath(e.currentTarget.value)}
            />
            <Show when={targetKind() !== "repo"}>
              <Segmented
                value={targetKind()}
                onChange={updateKind}
                options={[
                  { value: "folder", label: "Folder" },
                  { value: "file", label: "File" },
                ]}
              />
            </Show>
          </div>
        </div>
      </Show>

      <div class="access-scopes" role="group" aria-label="Scopes">
        <For each={allowed()}>
          {(scopeId) => {
            const meta = scopeMeta(scopeId);
            const checked = () => scopes().includes(scopeId);
            const isCaution = Boolean(meta?.caution);
            return (
              <label
                class={`access-scope-chip ${checked() ? "on" : ""} ${isCaution ? "caution" : ""}`}
                title={meta?.description}
              >
                <input
                  type="checkbox"
                  checked={checked()}
                  onChange={() => toggleScope(scopeId)}
                />
                <span>{meta?.label ?? scopeId}</span>
                <Show when={isCaution && checked()}>
                  <span class="access-scope-caution-tag">Caution</span>
                </Show>
                <Show when={isCaution && !checked() && askedWrite()}>
                  <span class="access-scope-req-tag">requested</span>
                </Show>
              </label>
            );
          }}
        </For>
      </div>

      <div class="access-card-foot">
        <Segmented value={lifetime()} onChange={setLifetime} options={lifetimes()} />
        <div class="access-card-actions">
          <Button size="sm" variant="ghost" disabled={busy()} onClick={() => decide("deny")}>Deny</Button>
          <Button size="sm" variant="primary" busy={busy()} disabled={!canAllow()} onClick={() => decide("approve")}>
            Allow
          </Button>
        </div>
      </div>
    </div>
  );
}

export function AccessApprovalBar(props) {
  const [pending, { refetch }] = createResource(
    () => props.agentId || null,
    (id) => (id ? rpc("access.pending", { agent_id: id }, { local: true }).catch(() => []) : []),
  );

  const pendingList = createMemo(() =>
    (pending() ?? []).filter((r) => !r.status || r.status === "pending"),
  );

  const [catalog] = createResource(
    () => (pendingList().length > 0 ? props.repoId || "__all__" : null),
    (key) => {
      if (!key) return [];
      const args = key === "__all__" ? {} : { exclude_repo_id: key };
      return rpc("access.catalog", args, { local: true }).catch(() => []);
    },
  );

  useVisibleEvent("access:requested", (payload) => {
    const reqAgent = payload?.request?.agent_id ?? payload?.agent_id;
    if (!reqAgent || reqAgent === props.agentId) refetch();
  }, { debounceMs: 100 });

  useVisibleEvent("access:changed", (payload) => {
    if (!payload?.agent_id || payload.agent_id === props.agentId) refetch();
  }, { debounceMs: 100 });

  return (
    <Show when={pendingList().length > 0}>
      <div class="access-bar">
        <For each={pendingList()}>
          {(req) => (
            <AccessRequestCard
              request={req}
              repoId={props.repoId}
              strict={props.strict}
              catalog={catalog() ?? []}
              onDecided={refetch}
            />
          )}
        </For>
      </div>
    </Show>
  );
}

export function AccessGrantModal(props) {
  const isStrict = () => Boolean(props.strict);
  const forRepoScreen = () => Boolean(!props.agentId && props.sourceRepoId);

  const [targetKind, setTargetKind] = createSignal("folder");
  const [targetRepoId, setTargetRepoId] = createSignal("");
  const [targetPath, setTargetPath] = createSignal("");
  const [label, setLabel] = createSignal("");
  const [scopes, setScopes] = createSignal(defaultScopesForGrant("folder", false));
  const [lifetime, setLifetime] = createSignal(props.lifetime || defaultLifetime({ forRepoScreen: forRepoScreen() }));
  const [strictAllowed, setStrictAllowed] = createSignal(false);
  const [busy, setBusy] = createSignal(false);

  const [catalog] = createResource(
    () => (props.open ? props.sourceRepoId || "__all__" : null),
    (key) => {
      if (!key) return [];
      const args = key === "__all__" ? {} : { exclude_repo_id: key };
      return rpc("access.catalog", args, { local: true }).catch(() => []);
    },
  );

  createEffect(on(() => props.open, (open) => {
    if (!open) return;
    const defaultKind = forRepoScreen() ? "repo" : "folder";
    setTargetKind(defaultKind);
    setTargetRepoId("");
    setTargetPath("");
    setLabel("");
    setScopes(defaultScopesForGrant(defaultKind, isStrict()));
    setLifetime(props.lifetime || defaultLifetime({ forRepoScreen: forRepoScreen() }));
    setStrictAllowed(false);
    setBusy(false);
  }));

  const allowed = createMemo(() => allowedScopesFor(targetKind(), isStrict()));
  const selectedRepo = createMemo(() => (catalog() ?? []).find((r) => r.id === targetRepoId()) ?? null);

  const lifetimes = createMemo(() => {
    if (forRepoScreen()) return [{ value: "persistent", label: "Always for repo" }];
    return availableLifetimes({ hasRepo: Boolean(props.sourceRepoId) });
  });

  const onKindChange = (nextKind) => {
    const k = normalizeTargetKind(nextKind);
    setTargetKind(k);
    if (k !== "repo") setTargetRepoId("");
    else {
      const repo = selectedRepo();
      setTargetPath(repo?.path || "");
    }
    setScopes(defaultScopesForGrant(k, isStrict()));
  };

  const onSelectRepo = (repoId) => {
    setTargetRepoId(repoId);
    const repo = (catalog() ?? []).find((r) => r.id === repoId);
    setTargetPath(repo?.path || "");
    if (!label().trim() && repo?.name) setLabel(repo.name);
  };

  const pickFolder = async () => {
    try {
      const res = await rpc("app.pick_folder", { prompt: "Choose a folder to share" });
      if (res?.path) {
        setTargetPath(res.path);
        if (!label().trim()) setLabel(inferTargetLabel(targetKind(), "", res.path));
      }
    } catch (e) {
      toast(String(e), "danger");
    }
  };

  const toggleScope = (scopeId) => {
    setScopes((cur) =>
      cur.includes(scopeId)
        ? cur.filter((s) => s !== scopeId)
        : sanitizeScopes([...cur, scopeId], targetKind(), isStrict()),
    );
  };

  const canSubmit = createMemo(() =>
    !busy() &&
    canApproveAccess({
      targetKind: targetKind(),
      targetRepoId: targetRepoId(),
      targetPath: targetPath(),
      scopes: scopes(),
      strict: isStrict(),
    }),
  );

  const submit = async () => {
    if (!canSubmit()) return;
    setBusy(true);
    try {
      const payload = buildGrantPayload({
        sourceRepoId: props.sourceRepoId || null,
        agentId: props.agentId || null,
        targetKind: targetKind(),
        targetRepoId: targetRepoId(),
        targetPath: targetPath(),
        label: label().trim() || inferTargetLabel(targetKind(), selectedRepo()?.name, targetPath()),
        scopes: scopes(),
        lifetime: lifetime(),
        strict: isStrict(),
        strictAllowed: strictAllowed(),
      });
      const grant = await rpc("access.grant_create", payload, { action: true });
      toast("Access granted", "secure");
      props.onCreated?.(grant);
      props.onClose?.();
    } catch (e) {
      toast(String(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show when={props.open}>
      <Modal
        title={forRepoScreen() ? "Link repo or folder" : "Grant access"}
        badge={isStrict() ? <Badge tone="strict"><Icon name="shield" size={10} /> Strict</Badge> : null}
        onClose={() => !busy() && props.onClose?.()}
        footer={
          <>
            <Button variant="ghost" disabled={busy()} onClick={() => props.onClose?.()}>Cancel</Button>
            <Button variant="primary" busy={busy()} disabled={!canSubmit()} onClick={submit}>
              Grant access
            </Button>
          </>
        }
      >
        <div class="access-modal-form">
          <Show when={isStrict()}>
            <div class="access-strict-note" role="note">
              <Icon name="shield" size={12} />
              <span>Strict agent — approved content may be sent to its provider. Context and agent messaging are disabled.</span>
            </div>
          </Show>

          <Field label="Target type">
            <Segmented
              value={targetKind()}
              onChange={onKindChange}
              options={[
                { value: "repo", label: "Linked repo" },
                { value: "folder", label: "Folder" },
                { value: "file", label: "File" },
              ]}
            />
          </Field>

          <Show when={targetKind() === "repo"} fallback={
            <Field label={targetKind() === "file" ? "File path" : "Folder path"}>
              <div class="access-modal-row">
                <Input
                  mono
                  value={targetPath()}
                  onValue={setTargetPath}
                  placeholder={targetKind() === "file" ? "/path/to/spec.pdf" : "/path/to/shared-folder"}
                />
                <Show when={targetKind() === "folder"}>
                  <Button onClick={pickFolder}>Choose folder…</Button>
                </Show>
              </div>
            </Field>
          }>
            <Field label="Repository" hint={(catalog() ?? []).length ? undefined : "No other registered repositories found."}>
              <Select
                placeholder="Select a repository…"
                value={targetRepoId()}
                onValue={onSelectRepo}
                options={(catalog() ?? []).map((r) => ({ value: r.id, label: r.name }))}
              />
            </Field>
          </Show>

          <Field label="Label (optional)">
            <Input
              value={label()}
              onValue={setLabel}
              placeholder={inferTargetLabel(targetKind(), selectedRepo()?.name, targetPath()) || "Short display name"}
            />
          </Field>

          <Field label="Scopes">
            <div class="access-scopes" role="group" aria-label="Scopes">
              <For each={allowed()}>
                {(scopeId) => {
                  const meta = scopeMeta(scopeId);
                  const checked = () => scopes().includes(scopeId);
                  const isCaution = Boolean(meta?.caution);
                  return (
                    <label
                      class={`access-scope-chip ${checked() ? "on" : ""} ${isCaution ? "caution" : ""}`}
                      title={meta?.description}
                    >
                      <input
                        type="checkbox"
                        checked={checked()}
                        onChange={() => toggleScope(scopeId)}
                      />
                      <span>{meta?.label ?? scopeId}</span>
                      <Show when={isCaution && checked()}>
                        <span class="access-scope-caution-tag">Caution</span>
                      </Show>
                    </label>
                  );
                }}
              </For>
            </div>
          </Field>

          <Show when={!forRepoScreen()}>
            <Field label="Lifetime">
              <Segmented value={lifetime()} onChange={setLifetime} options={lifetimes()} />
            </Field>
          </Show>

          <Show when={forRepoScreen()}>
            <Toggle
              on={strictAllowed()}
              tone="strict"
              label="Allow Strict agents to use this grant"
              onChange={setStrictAllowed}
            />
          </Show>
        </div>
      </Modal>
    </Show>
  );
}
