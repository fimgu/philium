import { invoke } from "@tauri-apps/api/core";

const $ = (s) => document.querySelector(s);
const hal = (method, path, body) => invoke("hal", { method, path, body });

const state = { current: null, after: 0, busy: false, poll: null, models: [] };

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text; // never innerHTML: model output is untrusted
  return e;
}

function setStatus(text, bad = false) {
  const s = $("#status");
  s.textContent = text;
  s.classList.toggle("bad", bad);
}

async function refreshSessions() {
  const list = await hal("GET", "/sessions");
  const sessions = Array.isArray(list) ? list : list.sessions ?? [];
  const ul = $("#sessions");
  ul.replaceChildren();
  for (const s of sessions) {
    const li = el("li", s.id === state.current ? "active" : "", s.title || s.id.slice(0, 8));
    li.title = s.workspace ?? "";
    li.onclick = () => openSession(s);
    ul.append(li);
  }
}

async function loadModels() {
  try {
    const m = await hal("GET", "/models");
    const items = Array.isArray(m) ? m : m.models ?? [];
    state.models = items.map((x) => (typeof x === "string" ? x : x.id ?? x.name)).filter(Boolean);
  } catch (e) {
    console.warn(e);
  }
}

function openSession(s) {
  state.current = s.id;
  state.after = 0;
  $("#title").textContent = s.title || s.id;
  $("#meta").textContent = `${s.model ?? ""} · ${s.workspace ?? ""}`;
  $("#timeline").replaceChildren();
  $("#permissions").replaceChildren();
  $("#input").disabled = $("#send").disabled = false;
  $("#cancel").disabled = false;
  refreshSessions();
  pollEvents();
}

// ---------- timeline rendering ----------
let streamEl = null;
const tools = new Map();

function add(node) {
  const tl = $("#timeline");
  const atBottom = tl.scrollHeight - tl.scrollTop - tl.clientHeight < 40;
  tl.append(node);
  if (atBottom) tl.scrollTop = tl.scrollHeight;
}

function render(ev) {
  if ($("#raw").checked) {
    const pre = el("pre", "raw", JSON.stringify(ev, null, 2));
    add(pre);
  }
  switch (ev.type) {
    case "message.created": {
      if (ev.role === "user") add(el("div", "msg user", ev.content ?? ""));
      break;
    }
    case "model.delta":
      if (!streamEl) add((streamEl = el("div", "msg assistant", "")));
      state.streamed = true;
      streamEl.textContent += ev.delta;
      break;
    case "model.started":
      break;
    case "model.finished": {
      if (streamEl && !streamEl.textContent) streamEl.remove();
      streamEl = null;
      const u = ev.usage ?? {};
      add(el("div", "usage", `↑ ${u.prompt_tokens ?? 0} tok sent · ↓ ${u.completion_tokens ?? 0} tok received · ${ev.finish_reason ?? ""}`));
      break;
    }
    case "tool.started": {
      const box = el("details", "tool");
      const sum = el("summary", null, `⚙ ${ev.tool}`);
      box.append(sum, el("pre", "args", JSON.stringify(ev.arguments, null, 2)));
      const out = el("pre", "out", "");
      box.append(out);
      tools.set(ev.call_id, { box, out, sum });
      add(box);
      break;
    }
    case "tool.output": {
      const t = tools.get(ev.call_id);
      if (t) t.out.textContent += ev.data ?? "";
      break;
    }
    case "tool.finished": {
      const t = tools.get(ev.call_id);
      if (t) {
        t.sum.textContent = `${ev.ok ? "✓" : "✗"} ${ev.tool} · ${ev.duration_ms ?? "?"}ms`;
        t.box.classList.add(ev.ok ? "ok" : "err");
        if (!t.out.textContent && ev.result != null)
          t.out.textContent = typeof ev.result === "string" ? ev.result : JSON.stringify(ev.result, null, 2);
      }
      break;
    }
    case "file.changed":
      add(el("div", "note", `✎ ${ev.path ?? JSON.stringify(ev)}`));
      break;
    case "permission.requested":
      showPermission(ev);
      break;
    case "permission.resolved":
      document.querySelector(`[data-rid="${CSS.escape(ev.request_id ?? "")}"]`)?.remove();
      add(el("div", "note", `permission ${ev.decision}: ${ev.request_id}`));
      break;
    case "turn.started":
      state.busy = true;
      state.streamed = false;
      break;
    case "assistant.completed":
      state.busy = false;
      if (!state.streamed && ev.text) add(el("div", "msg assistant", ev.text));
      break;
    case "turn.cancelled":
    case "turn.failed":
      state.busy = false;
      add(el("div", "note err", `${ev.type} ${ev.error ?? ""}`));
      break;
    case "loop.detected":
    case "runtime_policy.decision":
      add(el("div", "note warn", `${ev.type}: ${JSON.stringify(ev)}`));
      break;
  }
}

function showPermission(ev) {
  const box = el("div", "perm");
  box.dataset.rid = ev.request_id;
  box.append(el("strong", null, `Approve ${ev.action}?`), el("code", null, ev.resource ?? ""));
  const yes = el("button", "allow", "Allow");
  const no = el("button", "deny", "Deny");
  const decide = async (verb) => {
    try {
      await hal("POST", `/sessions/${state.current}/permissions/${ev.request_id}/${verb}`, {});
      box.remove();
    } catch (e) {
      setStatus(String(e), true);
    }
  };
  yes.onclick = () => decide("approve");
  no.onclick = () => decide("deny");
  box.append(yes, no);
  $("#permissions").append(box);
}

async function pollEvents() {
  clearTimeout(state.poll);
  const id = state.current;
  if (!id) return;
  try {
    const evs = await hal("GET", `/sessions/${id}/events?after=${state.after}&limit=500`);
    if (id !== state.current) return;
    for (const ev of evs) {
      state.after = Math.max(state.after, ev.seq);
      render(ev);
    }
    setStatus(state.busy ? "working…" : "idle");
  } catch (e) {
    setStatus(String(e), true);
  }
  state.poll = setTimeout(pollEvents, state.busy ? 150 : 1000);
}

// ---------- actions ----------
$("#composer").onsubmit = async (e) => {
  e.preventDefault();
  const text = $("#input").value.trim();
  if (!text || !state.current) return;
  $("#input").value = "";
  try {
    state.busy = true;
    await hal("POST", `/sessions/${state.current}/messages`, { text });
    pollEvents();
  } catch (err) {
    setStatus(String(err), true);
  }
};
$("#input").onkeydown = (e) => {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) $("#composer").requestSubmit();
};
$("#cancel").onclick = () => state.current && hal("POST", `/sessions/${state.current}/cancel`, {}).catch((e) => setStatus(String(e), true));
$("#raw").onchange = () => {
  state.after = 0;
  $("#timeline").replaceChildren();
  pollEvents();
};

$("#new-session").onclick = () => {
  const sel = $("#new-form").elements.model;
  sel.replaceChildren(el("option", null, "(default)"));
  sel.firstChild.value = "";
  for (const m of state.models) {
    const o = el("option", null, m);
    o.value = m;
    sel.append(o);
  }
  $("#new-dialog").showModal();
};
$("#new-dialog").onclose = async () => {
  if ($("#new-dialog").returnValue !== "ok") return;
  const f = new FormData($("#new-form"));
  const body = { workspace: f.get("workspace"), permission_mode: f.get("permission_mode") };
  if (f.get("model")) body.model = f.get("model");
  try {
    const s = await hal("POST", "/sessions", body);
    openSession(s.session ?? s);
  } catch (e) {
    setStatus(String(e), true);
  }
};

(async function boot() {
  try {
    await hal("GET", "/health");
    setStatus("HAL ready");
    await loadModels();
    await refreshSessions();
  } catch (e) {
    setStatus(`HAL unavailable: ${e}`, true);
  }
})();
