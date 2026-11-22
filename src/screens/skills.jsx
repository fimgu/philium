// Skills: Philium's own skill store. Global skills live in ~/Philium/skills/<name>/SKILL.md,
// repo skills in <repo>/.philium/skills. Agents see them via skill_list / skill_get.
import { For, Show, createSignal, createResource } from "solid-js";
import { Page, Button, Badge, Empty, Input, TextArea, Select, confirm, toast } from "../ui/kit.jsx";
import { rpc } from "../lib/rpc.js";
import { activeRepoId } from "../lib/store.js";
import { useVisibleEvent } from "../lib/visibility.js";

const TEMPLATE = (name) => `---\nname: ${name}\ndescription: What this skill is for and when to use it\n---\n\n# ${name}\n\n1. First step\n`;

export default function Skills() {
  const repo = () => activeRepoId() || null;
  const [data, { refetch }] = createResource(() => ({ r: repo() }), ({ r }) => rpc("skills.list", { repo_id: r }, { fresh: true }));
  useVisibleEvent("library", () => refetch(), { debounceMs: 250 });
  const [sel, setSel] = createSignal(null); // { name, scope, body, isNew }
  const [q, setQ] = createSignal("");
  const skills = () => (data()?.skills || []).filter((s) => !q() || `${s.name} ${s.description}`.toLowerCase().includes(q().toLowerCase()));

  const startNew = () => setSel({ name: "", scope: "global", body: TEMPLATE("new-skill"), isNew: true });
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
  const runImport = async () => {
    const r = await rpc("skills.import", { repo_id: repo() }, { action: true });
    toast(r.imported.length ? `Imported ${r.imported.length} skills` : "Nothing new to import", "secure");
    refetch();
  };

  return (
    <Page title="Skills" sub={data()?.global_dir}
      actions={<>
        <Show when={!data()?.imported_at}><Button variant="ghost" onClick={runImport}>Import from Claude / Jcode</Button></Show>
        <Button onClick={startNew}>New skill</Button>
      </>}>
      <div class="row gap-12" style={{ "align-items": "flex-start", height: "100%" }}>
        <div class="col gap-8" style={{ width: "300px", "flex-shrink": 0 }}>
          <Input placeholder="Filter skills" value={q()} onValue={setQ} />
          <Show when={skills().length} fallback={<Empty title="No skills yet" body="Create one, or import from ~/.claude/skills and ~/.jcode/skills." />}>
            <For each={skills()}>{(s) => (
              <button class={`nav-item ${sel()?.name === s.name && sel()?.scope === s.scope ? "on" : ""}`} style={{ "text-align": "left", display: "block", width: "100%" }}
                onClick={() => setSel({ name: s.name, scope: s.scope, body: s.body, isNew: false })}>
                <div class="row gap-4"><b>{s.name}</b><Show when={s.scope !== "global"}><Badge>repo</Badge></Show></div>
                <div class="xs dim">{s.description}</div>
              </button>
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
                <Button onClick={save}>Save</Button>
                <Show when={!s().isNew}><Button variant="ghost" onClick={del}>Delete</Button></Show>
              </div>
              <TextArea class="mono" style={{ "min-height": "60vh" }} value={s().body} onValue={(v) => setSel({ ...s(), body: v })} />
            </>)}
          </Show>
        </div>
      </div>
    </Page>
  );
}
