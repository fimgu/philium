import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { createRoot, createSignal, createMemo } from "solid-js/dist/solid.js";
import { workspaceAgents, selectedAgentId, spawnRepoDefault } from "./agent-workspace.js";
import { repoLabels } from './repo-labels.js';

test('same-name checkouts remain identifiable without long paths for unrelated repos', () => {
  const rows = [{id:'a',name:'app',path:'/a/src/app'}, {id:'b',name:'app',path:'/b/src/app'}, {id:'c',name:'solo',path:'/a/solo'}];
  assert.deepEqual([...repoLabels(rows).values()], ['a/src/app','b/src/app','solo']);
  const agents = [{id:'old',repo_name:'app'}, {id:'known',repo_name:'app',cwd:'/b/src/app/lib'}];
  assert.deepEqual(workspaceAgents(agents,'a',rows), []);
  assert.deepEqual(workspaceAgents(agents,'b',rows), [agents[1]]);
  assert.deepEqual(workspaceAgents(agents,'',rows), agents);
});

const repos = [{ id: "pcb", name: "watch-pcb", path: "/fixture/watch-pcb" }, { id: "phi", name: "Philium", path: "/fixture/philium" }];
const agents = [
  { id: "lion", name: "lion", repo_id: "phi", repo_name: "Philium", live: true, state: "waiting", warnings: [{ kind: "clash", text: "Philium warning" }] },
  { id: "otter", name: "otter", repo_id: "pcb", repo_name: "watch-pcb", live: true, state: "waiting", warnings: [{ kind: "health", text: "PCB warning" }] },
];

test("explicit repo identity overrides stale name and cwd hints", () => {
  const rows = workspaceAgents([...agents, { id: "wrong", repo_id: "phi", repo_name: "watch-pcb", cwd: "/fixture/watch-pcb" }], "pcb", repos);
  assert.deepEqual(rows.map(a => a.id), ["otter"]);
  assert.deepEqual(workspaceAgents(agents, "missing", repos), []);
  assert.equal(workspaceAgents(agents, "", repos), agents);
});

test("legacy cwd fallback requires a directory boundary, not a prefix sibling", () => {
  const rows = workspaceAgents([
    { id: "root", cwd: "/fixture/watch-pcb" },
    { id: "child", cwd: "/fixture/watch-pcb/worktree" },
    { id: "sibling", cwd: "/fixture/watch-pcb-other" },
    { id: "named", repo_name: "watch-pcb" },
  ], "pcb", repos);
  assert.deepEqual(rows.map(a => a.id), ["root", "child", "named"]);
});

test("selection belongs to filtered rows or falls back, never to a hidden global agent", () => {
  assert.equal(selectedAgentId([agents[1]], "lion"), "otter");
  assert.equal(selectedAgentId(agents, "otter"), "otter");
  assert.equal(selectedAgentId([], "lion"), undefined);
});

const source = readFileSync(new URL("../screens/agents.jsx", import.meta.url), "utf8");
const start = source.indexOf("export default function Agents() {") + "export default function Agents() {".length;
const setup = source.slice(start, source.indexOf("  return (", start));

test("actual Agents reactive setup isolates focus, warnings, counts and batch approval on workspace changes", async () => {
  let api, selectRepo, dispose;
  const calls = [];
  createRoot(cleanup => {
    dispose = cleanup;
    const [activeRepoId, setRepo] = createSignal("phi"); selectRepo = setRepo;
    const context = {
      useStore: () => ({ agents: () => agents, repos: () => repos, refetchAgents() {} }),
      createSignal, createMemo, workspaceAgents, selectedAgentId, activeRepoId,
      params: () => ({ id: "lion" }), registerCommands() {},
      localStorage: { getItem: () => null, setItem() {} },
      rpc: async (method, args) => { calls.push({ method, args }); return { resolved: 1 }; },
      toast() {}, go() {},
    };
    api = vm.runInNewContext(`${setup}\n({selected, telemetry, filtered, approveAllWaiting, setSearch})`, context);
  });
  assert.equal(api.selected(), "lion");
  assert.equal(api.telemetry().total, 1);
  selectRepo("pcb");
  assert.equal(api.selected(), "otter");
  assert.equal(api.telemetry().warnings, 1);
  assert.equal(api.telemetry().clashes, 0);
  assert.equal(api.telemetry().allAlerts[0].agent.id, "otter");
  assert.equal(api.telemetry().waiting, 1);
  await api.approveAllWaiting();
  assert.equal(calls[0].method, "agents.batch_permission");
  assert.equal(calls[0].args.repo_id, "pcb");
  api.setSearch("lion");
  assert.equal(api.selected(), undefined);
  assert.equal(api.filtered().length, 0);
  // Search does not hide workspace-wide triage counts.
  assert.equal(api.telemetry().total, 1);
  selectRepo("missing");
  assert.equal(api.selected(), undefined);
  assert.equal(api.telemetry().allAlerts.length, 0);
  dispose();
});

test("Spawn defaults to selected workspace and preserves explicit parent/scratch overrides", () => {
  assert.equal(spawnRepoDefault({}, "pcb"), "pcb");
  assert.equal(spawnRepoDefault({ repo_id: "phi" }, "pcb"), "phi");
  assert.equal(spawnRepoDefault({ repo_id: "" }, "pcb"), "");
  assert.equal(spawnRepoDefault(undefined, undefined), "");
  assert.match(source, /createSignal\(spawnRepoDefault\(props\.preset, activeRepoId\(\)\)\)/);
  assert.match(source, /repo_id: repo\(\) \|\| null/);
});

test("alerts are a non-growing top row inside a column and legacy broadcast is removed", () => {
  const screen = source.slice(start);
  assert.match(screen, /class="agents-workspace" style=\{\{ display: "flex", "flex-direction": "column"/);
  assert.match(screen, /class="fleet-triage-strip" style=\{\{ flex: "none" \}\}/);
  assert.ok(screen.indexOf('class="fleet-triage-strip"') < screen.indexOf('class="agents-workspace-content"'));
  assert.doesNotMatch(screen, /agents\.broadcast|fleet-broadcast-form|broadcastText/);
  assert.match(screen, /<AgentView id=\{id\(\)\} agent=\{filtered\(\)\.find/);
  assert.match(screen, /No agents in this workspace/);
});
