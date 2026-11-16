// Fixture sets for the render harness. EMPTY = fresh install, POPULATED = lived-in.
import { Buffer } from "node:buffer";
const now = Date.now();
const min = 60000;
const status = { root: "/Users/demo/Philium", desk: { ok: true }, version: "0.1.0" };

export const EMPTY = {
  "app.status": status,
  "app.settings_get": {},
  "app.disk": {},
  "app.events": [],
  "providers.default": "",
  "providers.models": [],
  "stats.summary": { totals: { requests: 0 }, by_agent: [], by_provider: [], by_model: [] },
  "stats.timeseries": { points: [] },
  "stats.efficiency": {},
  "codeindex.status": { error: "No repository selected" },
  "codeindex.tree": { files: [] },
  "codeindex.graph": { nodes: [], edges: [] },
  "codeindex.clashes": { clashes: [] },
  "hal GET /sessions": [],
};

const agent = (id, name, state, extra = {}) => ({
  id, name, state, live: ["running", "waiting", "idle"].includes(state), strict: false, role: "worker",
  model: "claude/claude-opus-5-5", created_at: now - 90 * min, ...extra,
});
const agents = [
  agent("a1", "lion", "running", { task_id: "t2", repo_id: "r1" }),
  agent("a2", "otter", "waiting", { repo_id: "r1" }),
  agent("a3", "heron", "idle", { parent_id: "a1" }),
  agent("a4", "fox", "failed", { model: "local/qwen3-coder" }),
  agent("a5", "moth", "stopped", { strict: true, model: "local/qwen3-coder" }),
  agent("a6", "crane", "done"),
];
const task = (id, title, status, extra = {}) => ({ id, title, status, priority: 2, owner: null, depends_on: [], detail: "Details for " + title, ...extra });
const tasks = [
  task("t1", "Add retry to gateway streaming", "open", { priority: 1 }),
  task("t2", "Refactor worktree cleanup", "claimed", { owner: "lion" }),
  task("t3", "Fix sidebar overflow", "review", { owner: "otter" }),
  task("t4", "Write onboarding copy", "done", { owner: "crane" }),
  task("t5", "Index ignore rules", "open", { blocked: true, depends_on: ["t2"] }),
];
const rows = [];
for (let i = 0; i < 24; i++) rows.push({
  id: 100 - i, ts: now - i * 40000, agent: ["lion", "otter", "", "fox"][i % 4], provider: ["claude", "chatgpt", "local"][i % 3],
  model: ["claude-opus-5-5", "gpt-6.1", "qwen3-coder"][i % 3], status: i === 5 ? 500 : 200, ttft_ms: 300 + i * 20,
  input_tokens: 1200 + i * 50, output_tokens: 300 + i * 10, tokens_in: 1200 + i * 50, tokens_out: 300 + i * 10,
  cost: i % 3 === 2 ? null : 0.01 * (i + 1), guard: i % 7 === 0 ? "redacted" : "", guard_actions: i % 7 === 0 ? 1 : 0, duration_ms: 1500 + i * 30, avg_tps: 55, started_ms: now - i * 40000,
});
const statRow = (name, i) => ({ id: name, name, agent: name, provider: name, provider_id: name, model: name, requests: 120 - i * 20, tokens_in: 90000 - i * 10000, tokens_out: 24000 - i * 3000, input_tokens: 90000 - i * 10000, output_tokens: 24000 - i * 3000, cost: 4.2 - i, avg_ttft_ms: 420 + i * 90, avg_tps: 60 - i * 5, error_rate: i * 0.01, guard_actions: i });
const points = Array.from({ length: 24 }, (_, i) => ({ t: now - (24 - i) * 3600000, ts: now - (24 - i) * 3600000, requests: 10 + ((i * 7) % 13), tokens_in: 8000 + i * 600, tokens_out: 2000 + i * 100, cost: 0.2 + (i % 5) * 0.1 }));
const tree = { files: ["src/main.rs", "src/db.rs", "src/gateway.rs", "src/agents.rs", "src/ui/kit.jsx"].map((path, i) => ({ path, lines: 100 + i * 40, syms: [{ name: "init", kind: "function", line: 3 }, { name: "Config", kind: "struct", line: 20 }], language: path.endsWith("rs") ? "rust" : "js" })) };

const tev = (seq, type, o) => ({ seq, type, ...o });
const chatEvents = [
  tev(2, "message.created", { role: "user", content: "Summarise the failing test and show the screenshot I attached.", images: [{ mime: "image/png", name: "shot.png" }] }),
  tev(3, "tool.started", { call_id: "c1", tool: "mcp_philium_shell_run", arguments: { command: "cargo test --lib gateway" } }),
  tev(4, "tool.output", { call_id: "c1", data: "running 12 tests\ntest gateway::retry ... FAILED\n" }),
  tev(5, "tool.finished", { call_id: "c1", ok: false, duration_ms: 4200, result: "1 failed" }),
  tev(6, "model.delta", { delta: "The `gateway::retry` test fails because the backoff is capped below the first retry delay. " }),
  tev(7, "model.delta", { delta: "Raising the cap to 2s fixes it." }),
  tev(8, "model.finished", { usage: {} }),
  tev(9, "file.changed", { path: "src-tauri/src/gateway.rs", diff: "@@ -10,3 +10,3 @@\n-const CAP: u64 = 100;\n+const CAP: u64 = 2000;" }),
  tev(10, "assistant.completed", { text: "" }),
];
const sessions = [
  { id: "s1", title: "Gateway retry failing", model: "claude/claude-opus-5-5", updated_at: now - 5 * min },
  { id: "s2", title: "Plan the onboarding flow", model: "chatgpt/gpt-6.1", updated_at: now - 3600000 },
];
const provs = [
  { id: "claude", name: "Claude", kind: "claude_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 6, account: { email: "demo@philium.dev", plan: "Max" }, models_cached_at: now },
  { id: "chatgpt", name: "ChatGPT", kind: "chatgpt_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 5, account: { email: "demo@philium.dev", plan: "Plus" }, models_cached_at: now },
  { id: "openrouter", name: "OpenRouter", kind: "openai_chat", base_url: "https://openrouter.ai/api/v1", has_key: true, trusted: false, enabled: true, guard: "strict", model_count: 120, models_cached_at: now },
  { id: "local", name: "Local Ollama", kind: "openai_chat", base_url: "http://127.0.0.1:11434/v1", has_key: false, trusted: false, enabled: true, guard: "strict", model_count: 3, models_error: "connection refused" },
];
const models = [
  { provider_id: "claude", provider_name: "Claude", model_id: "claude-opus-5-5", ref: "claude/claude-opus-5-5", display: "Opus 5.5", enabled: 1, context_window: 200000, price_in: 15, price_out: 75 },
  { provider_id: "chatgpt", provider_name: "ChatGPT", model_id: "gpt-6.1", ref: "chatgpt/gpt-6.1", display: "GPT 6.1", enabled: 1, context_window: 400000, price_in: 5, price_out: 20 },
  { provider_id: "local", provider_name: "Local Ollama", model_id: "qwen3-coder", ref: "local/qwen3-coder", display: "Qwen3 Coder", enabled: 1, trust: "untrusted" },
];
const repos = [
  { id: "r1", name: "philium", path: "/Users/demo/Developer/philium", default_branch: "main", integration_branch: null, branch: "main", dirty: false, worktree_count: 2 },
  { id: "r2", name: "hal", path: "/Users/demo/Developer/hal", default_branch: "main", branch: "main", dirty: true, worktree_count: 0 },
];
const worktrees = [
  { id: "w1", repo_id: "r1", branch: "agent/lion", path: "/Users/demo/Philium/worktrees/lion", agent_name: "lion", created_at: now - 3600000, status: { unsaved: true, dirty_files: ["src/a.jsx", "src/b.css"], ahead: 2, behind: 0, unpushed_commits: 2, unmerged_commits: 2 } },
  { id: "w2", repo_id: "r1", branch: "agent/otter", path: "/Users/demo/Philium/worktrees/otter", agent_name: "otter", created_at: now - 7200000, status: { unsaved: false, dirty_files: [], ahead: 0, behind: 1, unpushed_commits: 0, unmerged_commits: 0 } },
];
const terms = [
  { id: "term1", title: "cargo test", agent_id: "a1", agent: "lion", state: "running", cmd: "cargo test", cwd: "/Users/demo/Philium/worktrees/lion", created_at: now - 600000 },
  { id: "term2", title: "npm run build", agent_id: "a2", agent: "otter", state: "exited", exit_code: 0, cmd: "npm run build", created_at: now - 900000 },
  { id: "term3", title: "shell", state: "exited", exit_code: 1, cmd: "zsh", created_at: now - 1200000 },
];

export const POPULATED = {
  ...EMPTY,
  "app.settings_get": { theme: "dark", sweep_hours: "24" },
  "app.disk": { worktrees: { bytes: 1.4e9 }, repos: { bytes: 6.2e8 }, data: { bytes: 2.1e8 } },
  "app.events": [
    { id: 1, kind: "agent.spawned", ts: now - 20 * min, summary: "lion started", agent: "lion" },
    { id: 2, kind: "guard.blocked", ts: now - 10 * min, summary: "Secret removed from request", agent: "otter" },
    { id: 3, kind: "task.claimed", ts: now - 5 * min, summary: "lion claimed t2" },
  ],
  "providers.default": "claude/claude-opus-5-5",
  "providers.list": provs,
  "providers.models": models,
  "agents.list": agents,
  "browser.tabs": [
    { id: "b1", url: "https://docs.rs/tauri/latest/tauri/", title: "tauri - Rust", agent: null, visible: false, loading: false, error: null },
    { id: "b2", url: "https://github.com/tauri-apps/wry/issues", title: "Issues · tauri-apps/wry", agent: "lion", visible: false, loading: true, error: null },
    { id: "b3", url: "http://localhost:5173/", title: "", agent: "otter", visible: false, loading: false, error: "Blocked a `file:` link. Only http and https pages can open here." },
  ],
  "agents.get": agents[0],
  "agents.names": ["badger", "wren", "ibis", "newt"],
  "agents.events": chatEvents,
  "agents.diff": { diff: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n" },
  "board.list": tasks,
  "board.get": { ...tasks[1], check_results: [], playbook_progress: null },
  "board.playbooks": [],
  "board.mail_threads": [],
  "board.mail_inbox": [],
  "terminals.list": terms,
  "terminals.buffer": { data: Buffer.from("$ cargo test\r\nrunning 12 tests\r\ntest result: ok. 12 passed\r\n").toString("base64") },
  "workspace.repos_list": repos,
  "workspace.worktrees": worktrees,
  "workspace.branches": [{ name: "main" }, { name: "agent/lion" }, { name: "origin/main" }],
  "workspace.worktree_status": worktrees[0].status,
  "reaper.proposals": [{ id: "p1", kind: "worktree", handle: "/Users/demo/Philium/worktrees/old", reason: "Agent removed", state: "proposed" }],
  "reaper.ledger": [{ id: "l1", kind: "worktree", handle: "/Users/demo/Philium/worktrees/lion", state: "live", created_at: now - 3600000 }],
  "activity.list": rows,
  "stats.summary": {
    totals: { requests: 342, tokens_in: 1.2e6, tokens_out: 3.4e5, cost: 12.8, error_rate: 0.012, avg_tps: 58, avg_ttft_ms: 410, guard_actions: 7, unpriced_requests: 12 },
    by_agent: ["lion", "otter", "heron"].map(statRow), by_provider: ["claude", "chatgpt", "local"].map(statRow), by_model: ["claude-opus-5-5", "gpt-6.1", "qwen3-coder"].map(statRow),
  },
  "stats.timeseries": points,
  "stats.efficiency": { cached_share: 0.42, cache_available: true, output_input_ratio: 0.28, cost_per_: 0.04 },
  "codeindex.status": { files: 5, symbols: 42, indexed_at: now - 60000, state: "ready" },
  "codeindex.tree": tree,
  "codeindex.graph": { nodes: tree.files.map((f, i) => ({ id: f.path, label: f.path.split("/").pop(), path: f.path, size: 5 + i })), edges: [{ source: "src/main.rs", target: "src/db.rs" }, { source: "src/main.rs", target: "src/gateway.rs" }, { source: "src/agents.rs", target: "src/db.rs" }] },
  "codeindex.clashes": { clashes: [{ a: "lion", b: "otter", severity: "high", detail: ["src/gateway.rs"] }] },
  "hal GET /sessions": sessions,
  "hal GET /sessions/s1/events": chatEvents,
  "hal GET /sessions/s2/events": [],
};

// ---- robustness sets ----
// FAILING: every call rejects with realistic Rust/Tauri error strings.
export const FAILING = {
  __fail: [
    "database is locked",
    "Error: gateway: connection refused (os error 61)",
    "No such file or directory (os error 2)",
    "HAL not running: harness offline",
    "providers: keychain access denied: User canceled the authorization",
  ],
};
// SLOW: populated data, every call delayed.
export const SLOW = { ...POPULATED, __delay: 4000 };

const long = (n, w = "very-long-segment") => Array.from({ length: n }, () => w).join("-");
const uni = "日本語のタイトル 🚀 Ünïcödé مرحبا עברית ";
const hugeAgents = Array.from({ length: 1000 }, (_, i) => ({
  ...agents[i % agents.length], id: "h" + i, name: i % 7 === 0 ? long(12) + i : `agent-${i}`,
}));
const hugeTasks = Array.from({ length: 1000 }, (_, i) => ({
  ...tasks[i % tasks.length], id: "ht" + i, title: i % 3 === 0 ? uni.repeat(6) + i : long(20, "title") + i,
  owner: i % 5 ? "lion" : long(10), depends_on: Array.from({ length: i % 4 }, (_, k) => "x" + k),
}));
const hugeRows = Array.from({ length: 1000 }, (_, i) => ({ ...rows[i % rows.length], id: 5000 - i, model: long(12, "model") }));
const hugeRepos = Array.from({ length: 200 }, (_, i) => ({
  ...repos[i % 2], id: "hr" + i, name: long(10, "repo") + i, path: "/Users/demo/" + long(30, "deep/directory"),
}));
const hugeTerms = Array.from({ length: 60 }, (_, i) => ({ ...terms[i % 3], id: "ht" + i, title: uni + long(6) }));
const hugeMsg = "A".repeat(3000) + " " + uni.repeat(80) + "\n" + "/Users/demo/" + long(80, "path");
const hugeEvents = [
  tev(1, "message.created", { role: "user", content: hugeMsg }),
  tev(2, "tool.started", { call_id: "c1", tool: "mcp_philium_shell_run", arguments: { command: long(80, "cmd --flag") } }),
  tev(3, "tool.finished", { call_id: "c1", ok: true, duration_ms: 99999, result: "x".repeat(5000) }),
  tev(4, "assistant.completed", { text: hugeMsg }),
  tev(5, "file.changed", { path: "src/" + long(60, "deep/dir") + "/file.rs", diff: "+" + "y".repeat(4000) }),
];
export const HUGE = {
  ...POPULATED,
  "agents.list": hugeAgents,
  "board.list": hugeTasks,
  "activity.list": hugeRows,
  "workspace.repos_list": hugeRepos,
  "terminals.list": hugeTerms,
  "agents.events": hugeEvents,
  "hal GET /sessions": Array.from({ length: 300 }, (_, i) => ({ id: "s" + i, title: i % 2 ? uni.repeat(5) : long(30, "chat"), model: "claude/claude-opus-5-5", updated_at: now - i * 1000 })),
  "hal GET /sessions/s0/events": hugeEvents,
  "providers.list": Array.from({ length: 30 }, (_, i) => ({ ...provs[i % 4], id: "p" + i, name: long(8, "Provider") + i })),
};
