// Providers + live gateway activity. All text via textContent (untrusted data).
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const $ = (s) => document.querySelector(s);
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

// ---------- navigation ----------
export function initNav(onShowChat) {
  for (const b of document.querySelectorAll("#nav button")) {
    b.onclick = () => show(b.dataset.view, onShowChat);
  }
}
function show(view, onShowChat) {
  for (const b of document.querySelectorAll("#nav button")) b.classList.toggle("on", b.dataset.view === view);
  for (const v of ["chat", "providers", "activity"]) $(`#view-${v}`).hidden = v !== view;
  if (view === "providers") loadProviders();
  if (view === "activity") $("#act-badge").textContent = "";
  if (view === "chat") onShowChat?.();
}

// ---------- providers ----------
const KIND = { openai_chat: "OpenAI-compatible", openai_responses: "OpenAI Responses", anthropic: "Anthropic" };
let editing = null;
let onProvidersChanged = () => {};

export async function loadProviders() {
  const { providers, default: def } = await invoke("providers_list");
  const list = $("#provider-list");
  list.replaceChildren();
  if (!providers.length) {
    list.append(el("p", "meta", "No providers yet. Add one with its base URL, model and API key."));
  }
  for (const p of providers) {
    const row = el("div", "prov");
    const info = el("div", "info");
    const name = el("strong", null, p.name);
    if (p.id === def) name.append(el("span", "pill ok", "default"));
    name.append(el("span", p.has_key ? "pill" : "pill bad", p.has_key ? "key in keychain" : "no key"));
    name.append(el("span", p.guard === "off" ? "pill warn" : "pill", `guard: ${p.guard}`));
    info.append(name, el("div", null, `${p.id} · ${KIND[p.kind]} · ${p.model} · ${p.base_url}`));
    const result = el("span", "meta");
    const test = el("button", null, "Test");
    test.onclick = async () => {
      result.textContent = "testing…";
      try { result.textContent = `✓ ${await invoke("provider_test", { id: p.id })}`; }
      catch (e) { result.textContent = `✗ ${e}`; }
    };
    const edit = el("button", null, "Edit");
    edit.onclick = () => openProvider(p, p.id === def);
    const del = el("button", "deny", "Delete");
    del.onclick = async () => {
      if (!confirm(`Delete ${p.name} and its stored key?`)) return;
      await invoke("provider_delete", { id: p.id });
      await loadProviders();
      onProvidersChanged();
    };
    row.append(info, result, test, edit, del);
    list.append(row);
  }
  return providers;
}

function openProvider(p, isDefault) {
  editing = p?.id ?? null;
  const f = $("#provider-form");
  f.reset();
  $("#provider-error").textContent = "";
  const e = f.elements;
  e.id.readOnly = !!p;
  if (p) {
    for (const k of ["name", "id", "kind", "base_url", "model", "context_window", "guard"]) e[k].value = p[k];
    e.api_key.placeholder = p.has_key ? "unchanged (stored in keychain)" : "not set";
  } else {
    e.api_key.placeholder = "stored in keychain";
  }
  e.make_default.checked = !!isDefault;
  $("#provider-dialog").showModal();
}

export function initProviders(changed) {
  onProvidersChanged = changed;
  $("#add-provider").onclick = () => openProvider(null, false);
  const f = $("#provider-form");
  f.elements.name.oninput = () => {
    if (!editing) f.elements.id.value = f.elements.name.value.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
  };
  $("#provider-ok").onclick = async (ev) => {
    ev.preventDefault();
    if (!f.reportValidity()) return;
    const e = f.elements;
    const provider = {
      id: e.id.value.trim(), name: e.name.value.trim(), kind: e.kind.value,
      base_url: e.base_url.value.trim(), model: e.model.value.trim(),
      context_window: Number(e.context_window.value) || 128000, guard: e.guard.value,
    };
    const key = e.api_key.value;
    $("#provider-ok").disabled = true;
    $("#provider-error").textContent = "saving… (HAL restarts if the model list changed)";
    try {
      await invoke("provider_save", { provider, apiKey: key ? key : null, makeDefault: e.make_default.checked });
      e.api_key.value = "";
      $("#provider-dialog").close();
      await loadProviders();
      onProvidersChanged();
    } catch (err) {
      $("#provider-error").textContent = String(err);
    } finally {
      $("#provider-ok").disabled = false;
    }
  };
}

// ---------- activity ----------
const rows = new Map();
let selected = null;
const fmtBytes = (n) => (n == null ? "" : n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`);
const fmtTok = (a) => (a.input_tokens == null && a.output_tokens == null ? "" : `${a.input_tokens ?? "?"} / ${a.output_tokens ?? "?"}`);

function upsertActivity(a) {
  let tr = rows.get(a.id);
  if (!tr) {
    tr = el("tr");
    tr.onclick = () => select(a.id);
    rows.set(a.id, tr);
    $("#act-table tbody").prepend(tr);
    while (rows.size > 500) {
      const oldest = Math.min(...rows.keys());
      rows.get(oldest).remove();
      rows.delete(oldest);
    }
  }
  const findings = Object.entries(a.findings ?? {}).map(([k, v]) => `${k}×${v}`).join(", ");
  const status = a.state === "pending" ? (a.stream ? "streaming…" : "waiting…") : a.status ?? a.state;
  const cells = [
    [new Date(a.started_ms).toLocaleTimeString(), ""],
    [a.provider, ""],
    [a.model ?? "", ""],
    [String(status), a.state],
    [a.guard === "pass" ? "pass" : `${a.guard}: ${findings}`, a.guard === "pass" ? "" : a.guard],
    [fmtTok(a), ""],
    [a.duration_ms == null ? "" : `${(a.duration_ms / 1000).toFixed(1)} s`, ""],
    [`${fmtBytes(a.req_bytes)} / ${fmtBytes(a.resp_bytes)}`, ""],
  ];
  tr.replaceChildren(...cells.map(([t, c]) => el("td", c, t)));
  tr.title = a.error ?? "";
  tr.classList.toggle("sel", a.id === selected);
  updateTotals();
  if (a.id === selected && a.state !== "pending") select(a.id);
  if ($("#view-activity").hidden && (a.guard === "blocked" || a.guard === "redacted" || a.state === "error") && a.state !== "pending") {
    const b = $("#act-badge");
    b.textContent = String((Number(b.textContent) || 0) + 1);
  }
}

const totals = new Map();
function updateTotals() {
  // Recomputed from rendered rows' data cache.
  let n = 0, inT = 0, outT = 0, red = 0, blk = 0;
  for (const a of totals.values()) {
    n++; inT += a.input_tokens ?? 0; outT += a.output_tokens ?? 0;
    if (a.guard === "redacted") red++;
    if (a.guard === "blocked") blk++;
  }
  $("#act-totals").textContent = `${n} requests · ${inT.toLocaleString()} tokens sent · ${outT.toLocaleString()} received · ${red} redacted · ${blk} blocked`;
}

async function select(id) {
  selected = id;
  for (const [k, tr] of rows) tr.classList.toggle("sel", k === id);
  const d = await invoke("activity_detail", { id });
  const box = $("#act-detail");
  box.replaceChildren();
  if (!d) return box.append(el("p", "meta", "No longer in memory."));
  const s = d.summary;
  box.append(el("strong", null, `#${s.id} ${s.method} ${s.provider}${s.path}`));
  if (s.error) box.append(el("p", "note err", s.error));
  const pretty = (t) => { try { return JSON.stringify(JSON.parse(t), null, 2); } catch { return t; } };
  box.append(el("h3", null, "Request as sent upstream (after guard)"), el("pre", null, pretty(d.request)));
  box.append(el("h3", null, "Response"), el("pre", null, d.response_head || "(none yet)"));
  if (d.response_tail) box.append(el("h3", null, "Response (end)"), el("pre", null, d.response_tail));
}

export async function initActivity() {
  $("#act-clear").onclick = async () => {
    await invoke("activity_clear");
    rows.clear(); totals.clear();
    $("#act-table tbody").replaceChildren();
    $("#act-detail").replaceChildren(el("p", "meta", "Cleared."));
    updateTotals();
  };
  await listen("activity", ({ payload }) => { totals.set(payload.id, payload); upsertActivity(payload); });
  for (const a of await invoke("activity_list")) { totals.set(a.id, a); upsertActivity(a); }
  $("#act-badge").textContent = "";
}
