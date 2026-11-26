// Showcase fixtures for README marketing screenshots (scripts/render/showcase.mjs).
// Fictional data only: no real people, emails, employers, tokens or home paths.
import { Buffer } from "node:buffer";
const now = Date.now();
const min = 60000, hr = 3600000;
const HOME = "/Users/dev/Philium";

const agent = (id, name, state, extra = {}) => ({
  id, name, state, live: ["running", "waiting", "idle"].includes(state), strict: false, role: "worker",
  model: "claude/claude-opus-5-5", created_at: now - 50 * min, repo_id: "r1", branch: `agent/${name}`, ...extra,
});
const agents = [
  agent("a1", "lion", "running", { task_id: "t2", title: "Add rate limiting to checkout API" }),
  agent("a2", "otter", "running", { task_id: "t3", model: "chatgpt/gpt-6.1", title: "Migrate pricing page to server components" }),
  agent("a3", "heron", "waiting", { task_id: "t6", parent_id: "a1", title: "Write load tests for /api/checkout" }),
  agent("a4", "wren", "idle", { repo_id: "r2", title: "Triage flaky e2e suite" }),
  agent("a5", "ibis", "running", { repo_id: "r3", model: "local/qwen3-coder", title: "Generate OpenAPI client" }),
  agent("a6", "crane", "done", { title: "Dark mode tokens" }),
];
const task = (id, title, status, extra = {}) => ({ id, title, status, priority: 2, owner: null, depends_on: [], repo_id: "r1", detail: title, ...extra });
const tasks = [
  task("t1", "Add idempotency keys to order webhooks", "open", { priority: 1 }),
  task("t2", "Add rate limiting to checkout API", "claimed", { owner: "lion", owner_agent_id: "a1", priority: 1 }),
  task("t3", "Migrate pricing page to server components", "claimed", { owner: "otter", owner_agent_id: "a2" }),
  task("t6", "Write load tests for /api/checkout", "claimed", { owner: "heron", owner_agent_id: "a3", depends_on: ["t2"] }),
  task("t4", "Fix cart badge overflow on mobile", "in_review", { owner: "crane" }),
  task("t7", "Cache product images at the edge", "in_review", { owner: "wren" }),
  task("t5", "Dark mode design tokens", "done", { owner: "crane" }),
  task("t8", "Upgrade to Node 24 in CI", "done", { owner: "ibis" }),
  task("t9", "Search: typo tolerant matching", "open", { blocked: true, depends_on: ["t1"] }),
  task("t10", "Audit third-party scripts", "open", { priority: 3 }),
];

const tev = (seq, type, o) => ({ seq, type, ...o });
const diffText = `diff --git a/src/api/checkout/route.ts b/src/api/checkout/route.ts
--- a/src/api/checkout/route.ts
+++ b/src/api/checkout/route.ts
@@ -1,9 +1,21 @@
 import { NextResponse } from "next/server";
+import { rateLimit } from "@/lib/rate-limit";
 import { createOrder } from "@/lib/orders";
 
+const limiter = rateLimit({ window: "1m", max: 20 });
+
 export async function POST(req: Request) {
+  const ip = req.headers.get("x-forwarded-for") ?? "anon";
+  const { ok, retryAfter } = await limiter.check(ip);
+  if (!ok) {
+    return NextResponse.json(
+      { error: "Too many requests" },
+      { status: 429, headers: { "Retry-After": String(retryAfter) } },
+    );
+  }
   const body = await req.json();
-  const order = await createOrder(body);
+  const order = await createOrder(body, { idempotencyKey: req.headers.get("idempotency-key") });
   return NextResponse.json(order, { status: 201 });
 }
diff --git a/src/lib/rate-limit.ts b/src/lib/rate-limit.ts
new file mode 100644
--- /dev/null
+++ b/src/lib/rate-limit.ts
@@ -0,0 +1,14 @@
+import { redis } from "./redis";
+
+export function rateLimit({ window, max }: { window: string; max: number }) {
+  const ttl = parseWindow(window);
+  return {
+    async check(key: string) {
+      const k = \`rl:\${key}\`;
+      const n = await redis.incr(k);
+      if (n === 1) await redis.expire(k, ttl);
+      return { ok: n <= max, retryAfter: ttl };
+    },
+  };
+}
`;
const baseEvents = [
  tev(1, "message.created", { role: "user", content: "Add rate limiting to `POST /api/checkout`: 20 requests per minute per client, return 429 with Retry-After. Reuse our Redis client and add tests." }),
  tev(2, "model.delta", { delta: "I'll start by looking at the checkout route and the existing Redis helper." }),
  tev(3, "model.finished", { usage: {} }),
  tev(4, "tool.started", { call_id: "c1", tool: "mcp_philium_shell_run", arguments: { command: "rg -n \"redis\" src/lib | head" } }),
  tev(5, "tool.output", { call_id: "c1", data: "src/lib/redis.ts:3:export const redis = new Redis(process.env.REDIS_URL)\nsrc/lib/cache.ts:1:import { redis } from \"./redis\";\n" }),
  tev(6, "tool.finished", { call_id: "c1", ok: true, duration_ms: 180, result: "2 matches" }),
  tev(7, "tool.started", { call_id: "c2", tool: "read", arguments: { file_path: "src/api/checkout/route.ts" } }),
  tev(8, "tool.finished", { call_id: "c2", ok: true, duration_ms: 12, result: "14 lines" }),
  tev(9, "model.delta", { delta: "There's no limiter yet. I'll add a small fixed-window limiter in `src/lib/rate-limit.ts` backed by `redis.incr` + `expire`, then wire it into the route." }),
  tev(10, "model.finished", { usage: {} }),
  tev(11, "tool.started", { call_id: "c3", tool: "write", arguments: { file_path: "src/lib/rate-limit.ts" } }),
  tev(12, "tool.finished", { call_id: "c3", ok: true, duration_ms: 20, result: "wrote 14 lines" }),
  tev(13, "file.changed", { path: "src/lib/rate-limit.ts", diff: "@@ -0,0 +1,4 @@\n+export function rateLimit({ window, max }) {\n+  const ttl = parseWindow(window);\n+  return { async check(key) { /* incr + expire */ } };\n+}" }),
  tev(14, "tool.started", { call_id: "c4", tool: "edit", arguments: { file_path: "src/api/checkout/route.ts" } }),
  tev(15, "tool.finished", { call_id: "c4", ok: true, duration_ms: 15, result: "1 edit applied" }),
  tev(16, "file.changed", { path: "src/api/checkout/route.ts", diff: "@@ -5,3 +5,9 @@\n export async function POST(req: Request) {\n+  const { ok, retryAfter } = await limiter.check(ip);\n+  if (!ok) return tooMany(retryAfter);\n   const body = await req.json();" }),
  tev(17, "tool.started", { call_id: "c5", tool: "mcp_philium_shell_run", arguments: { command: "pnpm vitest run src/api/checkout" } }),
  tev(18, "tool.output", { call_id: "c5", data: " ✓ checkout/route.test.ts (6 tests) 412ms\n ✓ lib/rate-limit.test.ts (4 tests) 38ms\n\n Test Files  2 passed (2)\n      Tests  10 passed (10)\n" }),
  tev(19, "tool.finished", { call_id: "c5", ok: true, duration_ms: 2300, result: "10 passed" }),
  tev(20, "model.delta", { delta: "All 10 tests pass. Summary:\n\n- **New** `src/lib/rate-limit.ts`: fixed window limiter on Redis\n- **Route** returns `429` with `Retry-After` after 20 req/min per client\n- Passed the `idempotency-key` header through to `createOrder`\n\nI've asked **heron** to write k6 load tests against the new limit." }),
  tev(21, "model.finished", { usage: {} }),
  tev(22, "assistant.completed", { text: "" }),
];

const statRow = (name, i, extra = {}) => ({ id: name, name, agent: name, provider: name, provider_id: name, model: name, requests: 480 - i * 70, tokens_in: 2.4e6 - i * 3.1e5, tokens_out: 4.1e5 - i * 5e4, input_tokens: 2.4e6 - i * 3.1e5, output_tokens: 4.1e5 - i * 5e4, cost: +(18.4 - i * 2.7).toFixed(2), avg_ttft_ms: 380 + i * 70, p50_ttft_ms: 340 + i * 60, avg_tps: 72 - i * 6, error_rate: i * 0.004, guard_actions: i % 3, ...extra });
const points = Array.from({ length: 24 }, (_, i) => ({ t: now - (24 - i) * hr, ts: now - (24 - i) * hr, requests: 18 + Math.round(14 * Math.sin(i / 3.2) + 14) + (i > 8 && i < 19 ? 22 : 0), tokens_in: 60000 + i * 4200 + (i % 5) * 9000, tokens_out: 11000 + i * 700, cost: +(0.6 + (i % 6) * 0.22 + (i > 8 && i < 19 ? 0.9 : 0)).toFixed(2) }));
const rows = Array.from({ length: 30 }, (_, i) => ({
  id: 900 - i, ts: now - i * 47000, agent: ["lion", "otter", "heron", "ibis", "wren"][i % 5], provider: ["claude", "chatgpt", "claude", "local", "chatgpt"][i % 5],
  model: ["claude-opus-5-5", "gpt-6.1", "claude-sonnet-5-5", "qwen3-coder", "gpt-6.1"][i % 5], status: 200, ttft_ms: 280 + (i * 37) % 400,
  input_tokens: 9000 + i * 310, output_tokens: 600 + i * 41, tokens_in: 9000 + i * 310, tokens_out: 600 + i * 41,
  cost: i % 5 === 3 ? 0 : +(0.02 + (i % 7) * 0.013).toFixed(3), guard: "", guard_actions: 0, duration_ms: 2100 + i * 90, avg_tps: 64, started_ms: now - i * 47000,
}));
const provs = [
  { id: "claude", name: "Claude", kind: "claude_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 6, account: { email: "you@example.com", plan: "Max" }, models_cached_at: now },
  { id: "chatgpt", name: "ChatGPT", kind: "chatgpt_oauth", signed_in: true, trusted: true, enabled: true, guard: "redact", model_count: 5, account: { email: "you@example.com", plan: "Pro" }, models_cached_at: now },
  { id: "openrouter", name: "OpenRouter", kind: "openai_chat", base_url: "https://openrouter.ai/api/v1", has_key: true, trusted: false, enabled: true, guard: "strict", model_count: 120, models_cached_at: now },
  { id: "local", name: "Local Ollama", kind: "openai_chat", base_url: "http://127.0.0.1:11434/v1", has_key: false, trusted: false, enabled: true, guard: "strict", model_count: 3, models_cached_at: now },
];
const models = [
  { provider_id: "claude", provider_name: "Claude", model_id: "claude-opus-5-5", ref: "claude/claude-opus-5-5", display: "Opus 5.5", enabled: 1, context_window: 200000, price_in: 15, price_out: 75 },
  { provider_id: "claude", provider_name: "Claude", model_id: "claude-sonnet-5-5", ref: "claude/claude-sonnet-5-5", display: "Sonnet 5.5", enabled: 1, context_window: 200000, price_in: 3, price_out: 15 },
  { provider_id: "chatgpt", provider_name: "ChatGPT", model_id: "gpt-6.1", ref: "chatgpt/gpt-6.1", display: "GPT 6.1", enabled: 1, context_window: 400000, price_in: 5, price_out: 20 },
  { provider_id: "local", provider_name: "Local Ollama", model_id: "qwen3-coder", ref: "local/qwen3-coder", display: "Qwen3 Coder", enabled: 1, trust: "untrusted" },
];
const repos = [
  { id: "r1", name: "web", path: `${HOME}/repos/acme/web`, default_branch: "main", branch: "main", dirty: false, worktree_count: 3, remote_url: "git@github.com:acme/web.git" },
  { id: "r2", name: "mobile", path: `${HOME}/repos/acme/mobile`, default_branch: "main", branch: "main", dirty: false, worktree_count: 1, remote_url: "git@github.com:acme/mobile.git" },
  { id: "r3", name: "api-sdk", path: `${HOME}/repos/acme/api-sdk`, default_branch: "main", branch: "main", dirty: true, worktree_count: 1, remote_url: "git@github.com:acme/api-sdk.git" },
];
const wt = (id, agent_id, name, st) => ({ id, repo_id: "r1", agent_id, branch: `agent/${name}`, path: `${HOME}/worktrees/${name}`, agent_name: name, created_at: now - hr, status: st });
const worktrees = [
  wt("w1", "a1", "lion", { unsaved: true, dirty_files: ["src/api/checkout/route.ts", "src/lib/rate-limit.ts"], ahead: 3, behind: 0, unpushed_commits: 3, unmerged_commits: 3 }),
  wt("w2", "a2", "otter", { unsaved: true, dirty_files: ["app/pricing/page.tsx"], ahead: 1, behind: 0, unpushed_commits: 1, unmerged_commits: 1 }),
  wt("w3", "a3", "heron", { unsaved: false, dirty_files: [], ahead: 0, behind: 0, unpushed_commits: 0, unmerged_commits: 0 }),
];
const terms = [
  { id: "term0", title: "lion: pnpm test --watch", agent_id: "a1", agent: "lion", state: "running", cmd: "pnpm test --watch", cwd: `${HOME}/worktrees/lion`, created_at: now - 20 * min, workspace_id: "r1" },
  { id: "term1", title: "otter: pnpm dev", agent_id: "a2", agent: "otter", state: "running", cmd: "pnpm dev", cwd: `${HOME}/worktrees/otter`, created_at: now - 30 * min, workspace_id: "r1" },
  { id: "term2", title: "heron: k6 run load.js", agent_id: "a3", agent: "heron", state: "running", cmd: "k6 run load.js", created_at: now - 8 * min, workspace_id: "r1" },
  { id: "term3", title: "pnpm build", agent_id: "a6", agent: "crane", state: "exited", exit_code: 0, cmd: "pnpm build", created_at: now - 2 * hr, workspace_id: "r1" },
];
const termBuf = Buffer.from([
  "\x1b[38;5;110macme/web\x1b[38;5;244m agent/lion $\x1b[0m pnpm vitest run",
  "",
  " \x1b[46;30m RUN \x1b[0m \x1b[36mv3.2.0\x1b[0m",
  "",
  " \x1b[32m✓\x1b[0m src/lib/rate-limit.test.ts \x1b[2m(4 tests)\x1b[0m \x1b[33m38ms\x1b[0m",
  " \x1b[32m✓\x1b[0m src/api/checkout/route.test.ts \x1b[2m(6 tests)\x1b[0m \x1b[33m412ms\x1b[0m",
  " \x1b[32m✓\x1b[0m src/lib/orders.test.ts \x1b[2m(11 tests)\x1b[0m \x1b[33m96ms\x1b[0m",
  " \x1b[32m✓\x1b[0m src/components/Cart.test.tsx \x1b[2m(8 tests)\x1b[0m \x1b[33m210ms\x1b[0m",
  "",
  "\x1b[2m Test Files \x1b[0m \x1b[1;32m4 passed\x1b[0m (4)",
  "\x1b[2m      Tests \x1b[0m \x1b[1;32m29 passed\x1b[0m (29)",
  "\x1b[2m   Duration \x1b[0m 1.84s",
  "",
  "\x1b[38;5;110macme/web\x1b[38;5;244m agent/lion $\x1b[0m git status -s",
  " \x1b[31mM\x1b[0m src/api/checkout/route.ts",
  "\x1b[31m??\x1b[0m src/lib/rate-limit.ts",
  "\x1b[31m??\x1b[0m src/lib/rate-limit.test.ts",
  "\x1b[38;5;110macme/web\x1b[38;5;244m agent/lion $\x1b[0m ",
].join("\r\n")).toString("base64");

const mem = (id, title, content, category, extra = {}) => ({ id, title, content, category, status: "active", scope: "repo:r1", trust_tier: "verified", created_at: now - 3 * 86400000, updated_at: now - 2 * hr, files: [], ...extra });
const memories = [
  mem("m1", "Checkout API conventions", "Route handlers live in src/api/**/route.ts and return NextResponse.json. Errors use { error } bodies.", "convention"),
  mem("m2", "Redis client", "Use the shared client from src/lib/redis.ts. Never construct a new Redis() in request handlers.", "fact", { files: ["src/lib/redis.ts"] }),
  mem("m3", "Test runner", "Unit tests run with pnpm vitest run. E2E uses Playwright via pnpm e2e; it needs the dev server on :3000.", "procedure"),
  mem("m4", "Pricing page", "Pricing data comes from the CMS at build time; revalidate every 10 minutes.", "fact"),
  mem("m5", "Design tokens", "Colors come from tokens.css. Dark mode uses the [data-theme=dark] selector, not media queries.", "convention"),
  mem("m6", "Flaky e2e: cart drawer", "The cart drawer test is flaky on CI because of an animation. Wait for [data-open=true].", "gotcha", { trust_tier: "observed" }),
  mem("m7", "Release process", "Merges to main deploy preview builds. Tag vX.Y.Z to ship to production.", "procedure"),
];

export const SHOWCASE = {
  "app.status": { root: HOME, desk: { ok: true }, version: "1.0.0" },
  "app.settings_get": { theme: "dark", sweep_hours: "24" },
  "app.disk": { worktrees: { bytes: 2.3e9 }, repos: { bytes: 9.1e8 }, data: { bytes: 3.4e8 } },
  "app.events": [
    { id: 1, kind: "agent.spawned", ts: now - 42 * min, summary: "lion started on Add rate limiting to checkout API", agent: "lion" },
    { id: 2, kind: "agent.spawned", ts: now - 30 * min, summary: "otter started on Migrate pricing page", agent: "otter" },
    { id: 3, kind: "guard.blocked", ts: now - 18 * min, summary: "API key redacted from a prompt", agent: "otter" },
    { id: 4, kind: "task.claimed", ts: now - 9 * min, summary: "heron claimed Write load tests for /api/checkout" },
    { id: 5, kind: "task.review", ts: now - 4 * min, summary: "crane moved Fix cart badge overflow to review" },
  ],
  "providers.default": "claude/claude-opus-5-5",
  "providers.list": provs,
  "providers.models": models,
  "activity.quotas": {
    claude: { windows: [{ label: "5h", used_percent: 46, resets_at: now / 1000 + 2.4 * 3600 }, { label: "Weekly", used_percent: 31, resets_at: now / 1000 + 4 * 86400 }] },
    chatgpt: { windows: [{ label: "5h", used_percent: 22, resets_at: now / 1000 + 3.1 * 3600 }, { label: "Weekly", used_percent: 58, resets_at: now / 1000 + 2 * 86400 }] },
  },
  "agents.list": agents,
  "agents.get": agents[0],
  "agents.names": ["badger", "newt", "kite", "lynx"],
  "agents.events": baseEvents,
  "agents.diff": { patch: diffText, stat: " src/api/checkout/route.ts | 14 +++++++++++++-\n src/lib/rate-limit.ts      | 14 ++++++++++++++\n 2 files changed, 27 insertions(+), 1 deletion(-)" },
  "agents.shell": "term0",
  "agents.get_effort": "high",
  "board.list": tasks,
  "board.get": { ...tasks[1], check_results: [], playbook_progress: null },
  "board.playbooks": [], "board.mail_threads": [], "board.mail_inbox": [],
  "terminals.list": terms,
  "terminals.buffer": { data: termBuf },
  "workspace.repos_list": repos,
  "workspace.worktrees": worktrees,
  "workspace.branches": [{ name: "main", current: true, commit: "4f2a9c1e0b", date: now - hr }, { name: "agent/lion", commit: "9b1d77aa20", date: now - 5 * min }, { name: "agent/otter", commit: "c3e08f5d11" }],
  "workspace.worktree_status": worktrees[0].status,
  "reaper.proposals": [], "reaper.ledger": [],
  "workspace.overview": {
    repo_id: "r1", path: `${HOME}/repos/acme/web`, pilot: true,
    tasks: tasks.filter((t) => t.status !== "done"),
    agents: agents.filter((a) => a.repo_id === "r1").map((a) => ({ ...a, role: a.name === "lion" ? "coordinator" : a.role })),
    worktrees: worktrees.map((w) => ({ ...w, handle: w.branch })),
    resources: [
      { owner: "heron", kind: "container", handle: "k6-runner", state: "live" },
      { owner: "otter", kind: "dev_server", handle: "localhost:3001", state: "live" },
    ],
  },
  "activity.list": rows,
  "stats.summary": {
    totals: { requests: 1842, tokens_in: 9.6e6, tokens_out: 1.42e6, cost: 61.37, error_rate: 0.004, avg_tps: 68, avg_ttft_ms: 395, guard_actions: 3, unpriced_requests: 210, tokens_cached: 6.05e6, p50_ttft_ms: 352 },
    by_agent: ["lion", "otter", "heron", "ibis", "wren"].map((n, i) => statRow(n, i)),
    by_provider: ["claude", "chatgpt", "local"].map((n, i) => statRow(n, i)),
    by_model: [["claude-opus-5-5", "claude"], ["gpt-6.1", "chatgpt"], ["claude-sonnet-5-5", "claude"], ["qwen3-coder", "local"]].map(([n, p], i) => statRow(n, i, { provider_id: p })),
  },
  "stats.timeseries": points,
  "stats.efficiency": { cached_share: 0.63, cache_available: true, output_input_ratio: 0.15, cost_per_: 0.033 },
  "codeindex.status": { files: 412, symbols: 3870, indexed_at: now - 2 * min, state: "ready" },
  "codeindex.tree": { files: [] },
  "codeindex.graph": { nodes: [], edges: [] },
  "codeindex.clashes": { clashes: [] },
  "skills.list": {
    global_dir: "~/.philium/skills",
    skills: [
      { name: "review-pr", description: "Review a pull request with real workflow testing and a coverage matrix.", scope: "global", enabled: true },
      { name: "release", description: "Bump the version, tag, publish and watch the release land.", scope: "global", enabled: true },
      { name: "nextjs-conventions", description: "Routing, data fetching and caching rules for acme/web.", scope: "repo", enabled: true, body: "---\nname: nextjs-conventions\ndescription: Routing, data fetching and caching rules for acme/web.\n---\n\n# Next.js conventions for acme/web\n\n## Routing\n- Pages live in `app/`, API handlers in `src/api/**/route.ts`.\n- Prefer server components. Add `\"use client\"` only for interactivity.\n\n## Data fetching\n- Fetch on the server with `fetch(url, { next: { revalidate: 600 } })`.\n- Never call the database from a client component.\n\n## Caching\n- Tag cached fetches and call `revalidateTag` from webhooks.\n- Static assets go through the image CDN.\n\n## Checks before you finish\n1. `pnpm lint && pnpm typecheck`\n2. `pnpm vitest run`\n3. Open the changed page in the browser and check the console.\n" },
      { name: "db-migrations", description: "Write reversible Postgres migrations and verify them on a branch database.", scope: "repo", enabled: true },
      { name: "k6-load-test", description: "Generate and run k6 load tests against a local endpoint.", scope: "global", enabled: true },
      { name: "accessibility-audit", description: "Run axe and keyboard checks on changed pages.", scope: "global", enabled: false },
      { name: "changelog", description: "Draft release notes from merged PRs.", scope: "global", enabled: true },
    ],
  },
  "library.list": memories,
  "library.count": { count: memories.length },
  "library.procedures": [],
  "library.models_status": { embed: { state: "ready" }, rerank: { state: "ready" } },
  "library.config_get": {},
  "library.ranker_status": null,
  "library.consolidate_versions": [],
  "library.context_slate": null,
  "library.rules_get": null,
  "library.working_note_get": { text: "Checkout hardening this week: rate limits, idempotency, load tests." },
  "library.intentions": [{ id: "i1", trigger_kind: "file", trigger_match: "src/api/**", instruction: "Add a rate limit check to every new public route." }],
  "library.dream_status": {
    enabled: true, running: false, last_dreamed_at: now - 3 * hr, daily_tokens: 200000, used_today: 41000,
    actions: [
      { id: "d1", kind: "merge", summary: "Merged two notes about the Redis client into one", reason: "Duplicate facts" },
      { id: "d2", kind: "promote", summary: "Promoted \"Test runner\" to verified", reason: "Confirmed by 6 sessions" },
      { id: "d3", kind: "correct", summary: "Corrected Node version in CI notes (22 → 24)", reason: "Superseded by t8" },
      { id: "d4", kind: "new", summary: "Added \"Flaky e2e: cart drawer\" from wren's session", reason: "Recurring failure" },
      { id: "d5", kind: "retire", summary: "Retired note about the old pricing API", reason: "Endpoint removed" },
    ],
  },
  "pr_review.settings_get": { auto: true },
  "pr_review.list": [],
  // Repo default branch has an invalid policy file: UI caps the mode at Observe.
  "pr_review.policy_get": { policy: { mode: "observe", level: "standard", philium: "approves", humans: 0, budget_tokens: null, matched: [], source: "invalid", error: "version 2 is not supported" }, raw: "version: 2\nmode: enforce\n", override: null },
  "pr_review.get": {
    review: {
      id: "rv1", pr: 482, state: "observed", verdict: "request_changes", would_post: "REQUEST_CHANGES",
      mode: "observe", level: "deep", tokens_used: 1340000,
      // Enforce-with-humans policy from the base commit: gateway rule needs a person.
      policy: JSON.stringify({ mode: "enforce", level: "deep", philium: "comments", humans: 1, budget_tokens: 2000000, source: "base:9f3c2a17e4b0", error: null,
        matched: [{ purpose: "Gateway and auth need a person", paths: ["src-tauri/src/gateway/**"], files: 3 }, { purpose: "Docs are routine", paths: ["docs/**"], files: 2 }] }),
      coverage: { comments: { fetched: 9, total: 9 }, reviews: { fetched: 2, total: 2 }, threads: { fetched: 4, total: 4 }, checks: { fetched: 7, total: 7 } } },
    summary: "Adds a Redis rate limiter to `POST /api/checkout`. The approach is sound, but the limiter keys on `x-forwarded-for` without trusting the proxy chain, so clients can spoof it.",
    notes: [{ id: "n1", text: "Make sure 429 responses include Retry-After", must: true }],
    items: [
      { id: "i1", kind: "risk", status: "fail", severity: 1, title: "Client IP can be spoofed", detail: "x-forwarded-for is read raw. Use the platform's trusted client IP.", origin: "introduced", basis: "reproduced" },
      { id: "i2", kind: "risk", status: "pass", title: "Limiter TTL set only on first hit", detail: "incr + expire on n === 1 is correct for a fixed window." },
      { id: "i3", kind: "risk", status: "doing", title: "Behaviour when Redis is down", detail: "Verifying fail-open vs fail-closed." },
      { id: "i4", kind: "thread", status: "addressed", title: "Use a shared limiter helper", author: "reviewer-1", detail: "Moved to src/lib/rate-limit.ts." },
      { id: "i5", kind: "claim", status: "pass", title: "\"Returns 429 with Retry-After\"", detail: "Verified with a local burst of 25 requests." },
      { id: "i6", kind: "ci", status: "pass", title: "CI: test, lint, typecheck", detail: "7 of 7 checks green." },
      { id: "i7", kind: "note", status: "pass", title: "Make sure 429 responses include Retry-After", detail: "Header present." },
    ],
  },
  "checks.list": [], "checks.remembered": null,
  "selfdev.source": `${HOME}/repos/philium`,
  "selfdev.list": [{ id: "sd1" }, { id: "sd2" }, { id: "sd3" }],
  "selfdev.get:sd1": { id: "sd1", request: "Show token cost next to each agent in the sidebar", state: "building", agent_exists: true, agent_id: "a5", agent_name: "ibis", agent_state: "running", files: ["src/main.jsx", "src/lib/store.js"], created_at: now - 6 * min },
  "selfdev.get:sd2": { id: "sd2", request: "Add keyboard shortcut to jump between agents", state: "preview", preview: "active", tests: "passed", build: "passed", files: ["src/lib/keys.js", "src/screens/agents.jsx", "src/styles/agents.css"], agent_name: "crane", agent_state: "done", created_at: now - 50 * min },
  "selfdev.get:sd3": { id: "sd3", request: "Group board columns by repository", state: "accepted", tests: "passed", build: "passed", files: ["src/screens/board.jsx"], created_at: now - 26 * hr },
  "hal GET /sessions": [],
};
export const SHOWCASE_EVENTS = baseEvents;
