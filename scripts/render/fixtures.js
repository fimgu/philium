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
  "access.pending": [],
  "providers.default": "",
  "providers.models": [],
  "activity.quotas": {},
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
  model_ref: "claude/claude-opus-5-5", created_at: now - 90 * min, ...extra,
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
  task("t2", "Refactor worktree cleanup", "claimed", { owner: "lion", owner_agent_id:"a1", repo_id:"r1" }),
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
  tev(2, "message.created", { role: "user", content: "Summarise the failing test and show the screenshot I attached.", images: 1, attachments: [{ sha256: "abababababababababababababababababababababababababababababababab", media_type: "image/png", name: "shot.png", size: 68 }] }),
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
const sec = Math.floor(now / 1000);
const win = (name, used, resetIn, extra = {}) => ({ name, scope: "account", used_percent: used, resets_at: sec + resetIn, models: [], projected_full_at: null, ...extra });
const usageAccounts = {
  checked_at: now,
  accounts: [
    { provider_id: "claude", provider_name: "Claude", kind: "claude_oauth", account: "d***@philium.dev", plan: "Max", enabled: true, stale: false, limit_reached: false, error: null, key_status: null,
      windows: [win("5-hour window", 13, 4 * 3600 + 32 * 60), win("7-day window", 82, 23 * 3600 + 2 * 60, { projected_full_at: sec + 9 * 3600 }), win("7-day Opus window", 26, 23 * 3600 + 2 * 60, { scope: "model", models: ["opus"] })],
      banked_resets: null, details: [["Extra usage (long context)", "disabled"], ["Session resets", "Not eligible"]], last_used_ms: now - 4 * min, fetched_at: now - 12000 },
    { provider_id: "chatgpt", provider_name: "ChatGPT", kind: "chatgpt_oauth", account: "d***@philium.dev", plan: "self_serve_business_prolite", enabled: true, stale: false, limit_reached: false, error: null, key_status: null,
      windows: [win("7-day window", 35, 2 * 86400 + 20 * 3600)], banked_resets: { available: 1, next_available_at: null }, details: [], last_used_ms: now - 40 * min, fetched_at: now - 12000 },
    { provider_id: "openrouter", provider_name: "OpenRouter", kind: "openai_chat", account: null, plan: null, enabled: true, stale: false, limit_reached: false, error: null, key_status: "valid",
      windows: [], banked_resets: null, details: [], last_used_ms: null, fetched_at: now - 12000 },
    { provider_id: "local", provider_name: "Local Ollama", kind: "openai_chat", account: null, plan: null, enabled: true, stale: false, limit_reached: false, error: "Connection refused", key_status: null,
      windows: [], banked_resets: null, details: [], last_used_ms: null, fetched_at: now - 12000 },
  ],
};
const insights = {
  range: "24h",
  totals: { requests: 342, errors: 4, tokens_in: 1.2e6, tokens_out: 3.4e5, tokens_cached: 7.1e5, cost: 12.8 },
  spend: { today: { cost: 3.42, requests: 88, unpriced: 2, included: 61 }, week: { cost: 12.8, requests: 342, unpriced: 12, included: 230 }, month: { cost: 41.06, requests: 1310, unpriced: 30, included: 902 } },
  models: [
    { provider_id: "claude", model: "claude-opus-5-5", requests: 140, p50_ttft_ms: 820, p95_ttft_ms: 2400, avg_tps: 61.2, cache_hit: 0.71, error_rate: 0.007, cost: 0 },
    { provider_id: "chatgpt", model: "gpt-6.1", requests: 120, p50_ttft_ms: 540, p95_ttft_ms: 1900, avg_tps: 74.5, cache_hit: 0.48, error_rate: 0.017, cost: 0 },
    { provider_id: "local", model: "qwen3-coder", requests: 82, p50_ttft_ms: 190, p95_ttft_ms: 600, avg_tps: 38.1, cache_hit: null, error_rate: 0.061, cost: null },
  ],
  heatmap: Array.from({ length: 7 }, (_, d) => Array.from({ length: 24 }, (_, h) => (h >= 9 && h <= 18 && d !== 2 && d !== 3 ? ((h * 7 + d * 3) % 11) + 2 : h > 20 ? (h + d) % 3 : 0))),
  repos: [{ id: "r1", name: "philium", requests: 220, tokens: 9.1e5, cost: 8.2 }, { id: "r2", name: "jcode", requests: 90, tokens: 4.4e5, cost: 3.9 }, { id: null, name: null, requests: 32, tokens: 1.1e5, cost: 0.7 }],
  agents: [{ id: "lion", name: "lion", requests: 150, tokens: 7.2e5, cost: 6.1 }, { id: "otter", name: "otter", requests: 110, tokens: 5.0e5, cost: 4.4 }, { id: "heron", name: "heron", requests: 82, tokens: 2.3e5, cost: 2.3 }],
};
const provs = [
  { id: "claude", name: "Claude", kind: "claude_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 6, account: { email: "demo@philium.dev", plan: "Max" }, models_cached_at: now },
  { id: "chatgpt", name: "ChatGPT", kind: "chatgpt_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 5, account: { email: "demo@philium.dev", plan: "Plus" }, models_cached_at: now },
  { id: "openrouter", name: "OpenRouter", kind: "openai_chat", base_url: "https://openrouter.ai/api/v1", has_key: true, trusted: false, enabled: true, guard: "strict", model_count: 120, models_cached_at: now },
  { id: "local", name: "Local Ollama", kind: "openai_chat", base_url: "http://127.0.0.1:11434/v1", has_key: false, trusted: false, enabled: true, guard: "strict", model_count: 3, models_error: "connection refused" },
];
const models = [
  { provider_id: "claude", provider_name: "Claude", model_id: "claude-opus-5-5", ref: "claude/claude-opus-5-5", display: "Opus 5.5", enabled: 1, effort_details: [{ value: "low", label: "Fast" }, { value: "default", label: "Default" }], context_window: 200000, price_in: 15, price_out: 75 },
  { provider_id: "chatgpt", provider_name: "ChatGPT", model_id: "gpt-6.1", ref: "chatgpt/gpt-6.1", display: "GPT 6.1", enabled: 1, context_window: 400000, price_in: 5, price_out: 20 },
  { provider_id: "local", provider_name: "Local Ollama", model_id: "qwen3-coder", ref: "local/qwen3-coder", display: "Qwen3 Coder", enabled: 1, trust: "untrusted" },
];
const repos = [
  { id: "r1", name: "philium", path: "/Users/demo/Developer/philium", default_branch: "main", integration_branch: null, branch: "main", dirty: false, worktree_count: 2, remote_url: "git@github.com:demo/philium.git" },
  { id: "r2", name: "hal", path: "/Users/demo/Developer/hal", default_branch: "main", branch: "main", dirty: true, worktree_count: 0, remote_url: "https://github.com/demo/hal.git" },
  { id: "r3", name: "philium", path: "/Users/demo/pilot/philium", default_branch: "main", remote_url: "git@github.com:demo/philium.git" },
];
const worktrees = [
  { id: "w1", repo_id: "r1", agent_id: "a1", branch: "agent/lion", path: "/Users/demo/Philium/worktrees/lion", agent_name: "lion", created_at: now - 3600000, status: { unsaved: true, dirty_files: ["src/a.jsx", "src/b.css"], ahead: 2, behind: 0, unpushed_commits: 2, unmerged_commits: 2 } },
  { id: "w2", repo_id: "r1", agent_id: "a2", branch: "agent/otter", path: "/Users/demo/Philium/worktrees/otter", agent_name: "otter", created_at: now - 7200000, status: { unsaved: false, dirty_files: [], ahead: 0, behind: 1, unpushed_commits: 0, unmerged_commits: 0 } },
];
const terms = [
  { id: "term0", title: "lion shell", agent_id: "a1", agent: "lion", state: "running", cmd: "bash", cwd: "/Users/demo/Philium/worktrees/lion", created_at: now - 700000 },
  { id: "term1", title: "lion: npm run dev", agent_id: "a1", agent: "lion", state: "running", cmd: "npm run dev", cwd: "/Users/demo/Philium/worktrees/lion", created_at: now - 600000 },
  { id: "term2", title: "npm run build", agent_id: "a2", agent: "otter", state: "exited", exit_code: 0, cmd: "npm run build", created_at: now - 900000 },
  { id: "term3", title: "shell", state: "exited", exit_code: 1, cmd: "zsh", created_at: now - 1200000 },
];

// Realistic code graph: ~300 files across modules, weighted edges, a couple of agent changes.
const codeGraph = (() => {
  let s = 7; const rnd = () => ((s = Math.imul(s ^ (s >>> 15), 2246822507) + 0x6d2b79f5 | 0) >>> 0) / 4294967296;
  const mods = [["src-tauri/src", 70, "rs"], ["src/screens", 40, "jsx"], ["src/ui", 34, "jsx"], ["src/lib", 26, "js"], ["src/styles", 22, "css"], ["crates/hal", 38, "rs"], ["crates/gateway", 30, "rs"], ["scripts/render", 12, "mjs"], ["docs", 10, "md"], ["tests", 18, "rs"]];
  const words = ["agent", "board", "chat", "code", "db", "gateway", "hal", "index", "kit", "nav", "rpc", "sandbox", "shell", "stats", "terminal", "worktree", "provider", "guard", "events", "session", "router", "layout", "theme", "cache", "store", "auth", "config", "util", "model", "graph"];
  const nodes = [];
  for (const [dir, n, ext] of mods) for (let i = 0; i < n; i++) {
    const name = `${words[(i * 7 + dir.length) % words.length]}${i >= words.length ? "_" + i : ""}.${ext}`;
    nodes.push({ id: `${dir}/${name}`, label: name, kind: "file", path: `${dir}/${name}`, group: dir, rank: rnd(), syms: 2 + Math.floor(rnd() ** 2 * 60), changed_by: [] });
  }
  const edges = [], seen = new Set();
  const hubs = nodes.filter((_, i) => i % 9 === 0);
  for (const a of nodes) {
    const k = 1 + Math.floor(rnd() * 4);
    for (let j = 0; j < k; j++) {
      const same = rnd() < 0.7;
      const pool = same ? nodes.filter((b) => b.group === a.group) : hubs;
      const b = pool[Math.floor(rnd() ** 1.6 * pool.length)];
      if (!b || b === a || seen.has(a.id + b.id)) continue;
      seen.add(a.id + b.id);
      edges.push({ source: a.id, target: b.id, weight: 1 + Math.floor(rnd() ** 3 * 24), kind: "uses" });
    }
  }
  const deg = new Map();
  for (const e of edges) { const s0 = deg.get(e.source) ?? [0, 0], t0 = deg.get(e.target) ?? [0, 0]; s0[1] += e.weight; t0[0] += e.weight; deg.set(e.source, s0); deg.set(e.target, t0); }
  for (const n of nodes) { const [i, o] = deg.get(n.id) ?? [0, 0]; n.refs_in = i; n.refs_out = o; n.weight = i + o; }
  nodes[3].changed_by = [{ worktree_id: "w1", agent: "lion" }];
  nodes[75].changed_by = [{ worktree_id: "w2", agent: "otter" }];
  return { nodes, edges, truncated: false, level: "file" };
})();

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
  "agents.events": [...chatEvents, ...["browser_open", "browser_navigate", "browser_snapshot"].flatMap((tool, i) => [
    { type: "tool.started", seq: 1001 + i * 2, call_id: `browser-call-${i}`, tool: `mcp_philium_${tool}`, arguments: tool === "browser_snapshot" ? {} : { url: "https://example.com/permission-test" } },
    { type: "permission.requested", seq: 1002 + i * 2, request_id: `browser-perm-${i}`, call_id: `browser-call-${i}`, action: "mcp", resource: `philium/${tool}` },
  ])],
  "agents.diff": { diff: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n" },
  "board.list": tasks,
  "board.get": { ...tasks[1], check_results: [], playbook_progress: null },
  "board.playbooks": [],
  "board.mail_threads": [],
  "board.mail_inbox": [],
  "agents.shell": "term0",
  "terminals.list": terms,
  "terminals.buffer": { data: Buffer.from("\x1b[38;5;110mlion\x1b[38;5;244m lion $\x1b[0m cargo test -q\r\nrunning 112 tests\r\n........................................\r\ntest result: \x1b[32mok\x1b[0m. 112 passed; 0 failed\r\n\x1b[38;5;110mlion\x1b[38;5;244m lion $\x1b[0m git status -s\r\n M src/screens/agents.jsx\r\n?? src/ui/termview.jsx\r\n\x1b[38;5;110mlion\x1b[38;5;244m lion $\x1b[0m ").toString("base64") },
  "workspace.repos_list": repos,
  "workspace.worktrees": worktrees,
  "workspace.branches": [{ name: "main", current: true, commit: "a1b2c3d4e5", date: now - 3600000 }, { name: "agent/lion", commit: "b2c3d4e5f6", date: now - 600000 }, { name: "agent/otter", commit: "c3d4e5f6a7" }, { name: "origin/main", commit: "a1b2c3d4e5" }, { name: "origin/HEAD" }],
  "workspace.worktree_status": worktrees[0].status,
  "sandbox.grants_get": { grants: [{ path: "/Users/demo/Documents/Parts", write: false }], private: [] },
  "agents.get_effort": "default",
  "reaper.proposals": [{ id: "p1", kind: "worktree", handle: "/Users/demo/Philium/worktrees/old", reason: "Agent removed", state: "proposed" }],
  "reaper.ledger": [{ id: "l1", kind: "worktree", handle: "/Users/demo/Philium/worktrees/lion", state: "live", created_at: now - 3600000 }],
  "activity.list": rows,
  "activity.quotas": {
    chatgpt: {
      observed_at: Math.floor(now / 1000),
      windows: [{ label: "7d", used_percent: 28, resets_at: Math.floor(now / 1000) + 259200 }],
    },
  },
  "stats.summary": {
    totals: { requests: 342, tokens_in: 1.2e6, tokens_out: 3.4e5, cost: 12.8, error_rate: 0.012, avg_tps: 58, avg_ttft_ms: 410, guard_actions: 7, unpriced_requests: 12 },
    by_agent: ["lion", "otter", "heron"].map(statRow), by_provider: ["claude", "chatgpt", "local"].map(statRow), by_model: ["claude-opus-5-5", "gpt-6.1", "qwen3-coder"].map(statRow),
  },
  "stats.timeseries": points,
  "stats.efficiency": [],
  "stats.insights": insights,
  "usage.accounts": usageAccounts,
  "codeindex.status": { files: 5, symbols: 42, indexed_at: now - 60000, state: "ready" },
  "codeindex.tree": tree,
  "codeindex.graph": codeGraph,
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
