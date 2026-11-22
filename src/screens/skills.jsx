// Skills: Philium's own skill store plus the open-standard shared folders.
// Global: ~/Philium/skills, repo: <repo>/.philium/skills, shared (read in place): ~/.agents/skills
// and <repo>/.agents/skills. Agents see enabled skills via skill_list / skill_get.
import { For, Show, createSignal, createResource } from "solid-js";
import { Page, Button, Badge, Empty, Input, TextArea, Select, Toggle, Modal, Field, Menu, confirm, toast, cleanError } from "../ui/kit.jsx";
import { rpc } from "../lib/rpc.js";
import { activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";

const TEMPLATE = (name, desc) => `---\nname: ${name}\ndescription: ${desc || "What this skill is for and when to use it"}\n---\n\n# ${name}\n\n1. First step\n`;
const writable = (scope) => scope === "global" || scope.startsWith("repo:");
const scopeLabel = (scope) => scope === "global" ? null : scope.startsWith("repo:") ? "repo" : scope.startsWith("repo-shared:") ? "repo shared" : "shared";

// Read dropped items (files or folders) into [{path, b64}].
async function readDrop(dt) {
  const out = [];
  const toB64 = async (file) => {
    const buf = new Uint8Array(await file.arrayBuffer());
    let s = "";
    for (let i = 0; i < buf.length; i += 0x8000) s += String.fromCharCode(...buf.subarray(i, i + 0x8000));
    return btoa(s);
  };
  const walk = async (entry, prefix) => {
    if (out.length > 2000) throw new Error("Too many files dropped");
    if (entry.isFile) {
      const file = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ path: prefix + entry.name, b64: await toB64(file) });
    } else if (entry.isDirectory) {
      if (entry.name === ".git" || entry.name === "node_modules") return;
      const reader = entry.createReader();
      for (;;) {
        const batch = await new Promise((res, rej) => reader.readEntries(res, rej));
        if (!batch.length) break;
        for (const e of batch) await walk(e, `${prefix}${entry.name}/`);
      }
    }
  };
  const entries = [...dt.items].map((i) => i.webkitGetAsEntry?.()).filter(Boolean);
  if (entries.length) for (const e of entries) await walk(e, "");
  else for (const f of dt.files) out.push({ path: f.name, b64: await toB64(f) });
  return out;
}

export default function Skills() {
  const repo = () => activeRepoId() || null;
  const [data, { refetch }] = createResource(() => ({ r: repo() }), ({ r }) => rpc("skills.list", { repo_id: r }, { fresh: true }));
  useVisibleEvent("library", () => refetch(), { debounceMs: 250 });
  const [sel, setSel] = createSignal(null); // { name, scope, body, isNew, source }
  const [q, setQ] = createSignal("");
  const [dialog, setDialog] = createSignal(null); // "url" | "create" | "preview"
  const [url, setUrl] = createSignal("");
  const [nm, setNm] = createSignal("");
  const [desc, setDesc] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [prev, setPrev] = createSignal(null); // {token, skills}
  const [choice, setChoice] = createSignal({}); // id -> {on, action}
  const [over, setOver] = createSignal(false);
  const skills = () => (data()?.skills || []).filter((s) => !q() || `${s.name} ${s.description}`.toLowerCase().includes(q().toLowerCase()));

  const openPreview = (p) => {
    const c = {};
    for (const s of p.skills) c[s.id] = { on: !s.problems.length && (p.skills.length === 1 || !s.exists), action: s.exists ? "skip" : "skip" };
    setChoice(c);
    setPrev(p);
    setDialog("preview");
  };
  const stage = async (args) => {
    setBusy(true);
    try { openPreview(await rpc("skills.stage", args, { action: true })); }
    catch (e) { toast(cleanError(e), "bad"); }
    finally { setBusy(false); }
  };
  const upload = async () => {
    const r = await rpc("skills.pick_upload", {}, { action: true });
    if (r.path) await stage({ path: r.path });
  };
  const closePreview = () => {
    if (prev()) rpc("skills.discard", { token: prev().token }, { action: true }).catch(() => {});
    setPrev(null);
    setDialog(null);
  };
  const install = async () => {
    const items = Object.entries(choice()).filter(([, c]) => c.on).map(([id, c]) => ({ id, action: c.action }));
    setBusy(true);
    try {
      const r = await rpc("skills.install", { token: prev().token, items, repo_id: repo() }, { action: true });
      toast(r.installed.length ? `Installed ${r.installed.join(", ")}` : "Nothing installed", "secure");
      setPrev(null);
      setDialog(null);
      refetch();
    } catch (e) { toast(cleanError(e), "bad"); }
    finally { setBusy(false); }
  };
  const create = () => {
    const n = nm().trim();
    setSel({ name: n, scope: "global", body: TEMPLATE(n, desc().trim()), isNew: true });
    setDialog(null);
  };
  const onDrop = async (e) => {
    e.preventDefault();
    setOver(false);
    try {
      const files = await readDrop(e.dataTransfer);
      if (files.length) await stage({ files });
    } catch (x) { toast(cleanError(x), "bad"); }
  };

  const save = async () => {
    const s = sel();
    await rpc("skills.save", { name: s.name.trim(), scope: s.scope, body: s.body, create: s.isNew, repo_id: repo() }, { action: true });
    toast(`Saved ${s.name}`, "secure");
    setSel({ ...s, isNew: false });
    refetch();
  };
  const del = async () => {
    const s = sel();
    if (!(await confirm(`Delete ${s.name}?`, "Removes the skill folder from disk.", { danger: true, ok: "Delete" }))) return;
    await rpc("skills.delete", { name: s.name, scope: s.scope, repo_id: repo() }, { action: true });
    setSel(null);
    refetch();
  };
  const toggle = async (s, on) => {
    await rpc("skills.set_enabled", { name: s.name, scope: s.scope, enabled: on, repo_id: repo() }, { action: true });
    refetch();
  };
  const update = async (s) => {
    await rpc("skills.update", { name: s.name, repo_id: repo() }, { action: true });
    toast(`Updated ${s.name}`, "secure");
    const fresh = await rpc("skills.get", { name: s.name, repo_id: repo() }, { fresh: true });
    setSel({ ...sel(), body: fresh.body });
    refetch();
  };

  const addMenu = [
    { label: "Upload…", onClick: upload },
    { label: "From URL…", onClick: () => { setUrl(""); setDialog("url"); } },
    { label: "Create…", onClick: () => { setNm(""); setDesc(""); setDialog("create"); } },
  ];

  return (
    <Page title="Skills" sub={data()?.global_dir}
      actions={<div class="row gap-4"><Show when={busy()}><span class="xs dim">Working…</span></Show><span class="xs">Add skill</span><Menu title="Add skill" items={addMenu} /></div>}>
      <div class="row gap-12" style={{ "align-items": "flex-start", height: "100%", outline: over() ? "2px dashed var(--accent, #888)" : "none" }}
        onDragOver={(e) => { e.preventDefault(); setOver(true); }} onDragLeave={() => setOver(false)} onDrop={onDrop}>
        <div class="col gap-8" style={{ width: "320px", "flex-shrink": 0 }}>
          <Input placeholder="Filter skills" value={q()} onValue={setQ} />
          <Show when={skills().length} fallback={<Empty title="No skills yet" body="Add one, or drop a .zip, .skill, folder or SKILL.md here." />}>
            <For each={skills()}>{(s) => (
              <div class={`nav-item ${sel()?.name === s.name && sel()?.scope === s.scope ? "on" : ""}`} style={{ display: "flex", gap: "8px", "align-items": "flex-start", opacity: s.enabled ? 1 : 0.55 }}>
                <button style={{ "text-align": "left", flex: 1, background: "none", border: 0, padding: 0, color: "inherit" }}
                  onClick={() => setSel({ name: s.name, scope: s.scope, body: s.body, isNew: false, source: s.source })}>
                  <div class="row gap-4"><b>{s.name}</b><Show when={scopeLabel(s.scope)}><Badge>{scopeLabel(s.scope)}</Badge></Show><Show when={s.source}><Badge>url</Badge></Show></div>
                  <div class="xs dim">{s.description}</div>
                </button>
                <Toggle on={s.enabled} onChange={(v) => toggle(s, v).catch((e) => toast(cleanError(e), "bad"))} />
              </div>
            )}</For>
          </Show>
        </div>
        <div class="col gap-8" style={{ flex: 1 }}>
          <Show when={sel()} fallback={<Empty title="Select a skill" body="Use one in any agent composer with /skill <name>." />}>
            {(s) => (<>
              <div class="row gap-8">
                <Input mono placeholder="name" value={s().name} disabled={!s().isNew} onValue={(v) => setSel({ ...s(), name: v, body: s().isNew ? s().body.replace(/^name: .*$/m, `name: ${v}`) : s().body })} />
                <Show when={s().isNew && repo()}>
                  <Select value={s().scope} onValue={(v) => setSel({ ...s(), scope: v })}
                    options={[{ value: "global", label: "Global" }, { value: `repo:${repo()}`, label: "This repo" }]} />
                </Show>
                <Show when={writable(s().scope)} fallback={<span class="xs dim">Shared skill, read-only here</span>}>
                  <Button onClick={save}>Save</Button>
                  <Show when={s().source && s().scope === "global"}><Button variant="ghost" onClick={() => update(s()).catch((e) => toast(cleanError(e), "bad"))}>Update</Button></Show>
                  <Show when={!s().isNew}><Button variant="ghost" onClick={del}>Delete</Button></Show>
                </Show>
              </div>
              <Show when={s().source}><div class="xs dim mono">Source: {s().source.url}{s().source.path ? ` (${s().source.path})` : ""}</div></Show>
              <TextArea class="mono" style={{ "min-height": "60vh" }} value={s().body} disabled={!writable(s().scope)} onValue={(v) => setSel({ ...s(), body: v })} />
            </>)}
          </Show>
        </div>
      </div>

      <Show when={dialog() === "url"}>
        <Modal title="Add skill from URL" onClose={() => !busy() && setDialog(null)}
          footer={<Button busy={busy()} disabled={!url().trim()} onClick={() => stage({ url: url().trim() })}>Fetch</Button>}>
          <Field label="Repository URL" hint="Any git repository, or a link to a folder inside one (…/tree/<branch>/<path>).">
            <Input value={url()} onValue={setUrl} placeholder="https://github.com/owner/repo/tree/main/skills" />
          </Field>
        </Modal>
      </Show>

      <Show when={dialog() === "create"}>
        <Modal title="Create skill" onClose={() => setDialog(null)}
          footer={<Button disabled={!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(nm().trim()) || !desc().trim()} onClick={create}>Continue</Button>}>
          <Field label="Name" hint="Letters, digits, - and _"><Input mono value={nm()} onValue={setNm} placeholder="my-skill" /></Field>
          <Field label="Description" hint="What it does and when an agent should use it"><Input value={desc()} onValue={setDesc} /></Field>
        </Modal>
      </Show>

      <Show when={dialog() === "preview" && prev()}>
        <Modal wide title="Review before installing" onClose={() => !busy() && closePreview()}
          footer={<Button busy={busy()} disabled={!Object.values(choice()).some((c) => c.on)} onClick={install}>Install</Button>}>
          <div class="col gap-12">
            <For each={prev().skills}>{(s) => (
              <div class="col gap-4" style={{ "border-bottom": "1px solid var(--line, #333)", "padding-bottom": "8px" }}>
                <div class="row gap-8">
                  <input type="checkbox" checked={choice()[s.id]?.on} disabled={s.problems.length > 0}
                    onChange={(e) => setChoice({ ...choice(), [s.id]: { ...choice()[s.id], on: e.target.checked } })} />
                  <b>{s.name}</b><span class="xs dim mono">{s.id || "."}</span>
                  <Show when={s.exists}>
                    <Badge>exists</Badge>
                    <Select value={choice()[s.id]?.action} onValue={(v) => setChoice({ ...choice(), [s.id]: { ...choice()[s.id], action: v } })}
                      options={[{ value: "skip", label: "Skip" }, { value: "rename", label: "Install as new name" }, { value: "replace", label: "Replace existing" }]} />
                  </Show>
                </div>
                <div class="xs">{s.description || <i class="dim">no description</i>}</div>
                <For each={s.problems}>{(p) => <div class="xs" style={{ color: "var(--bad, #d55)" }}>{p}</div>}</For>
                <Show when={s.files.some((f) => f.script)}><div class="xs" style={{ color: "var(--warn, #c90)" }}>Contains scripts. Agents may run them; review before installing.</div></Show>
                <div class="xs mono dim">
                  <For each={s.files}>{(f) => <div>{f.path}{f.script ? "  ⚠ script" : ""}</div>}</For>
                </div>
              </div>
            )}</For>
          </div>
        </Modal>
      </Show>
    </Page>
  );
}
