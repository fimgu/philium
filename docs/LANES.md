# Philium lane brief (for parallel workers)

Read `docs/SPEC.md` for the product. This file is the working contract.

## Ground rules
- Repo: `~/Developer/philium`. Several workers edit it at the same time.
  **Only edit files your lane owns.** Commit only your files:
  `git add <your paths> && git commit -m "..."`. Never `git add -A`, never
  reset/checkout/stash other files, never rewrite history.
- `Cargo.toml` / `package.json`: if you need a dependency, add one line and
  commit it immediately on its own (small, conflict-free commits).
- Never add `Co-authored-by` trailers or agent signatures to commits.
- HAL (`~/Developer/hal`) is read-only. Do not modify it.
- No em dashes in UI copy.

## Backend contract (`src-tauri/src/`)
- Each module: `pub fn init(ctx: &Arc<Ctx>) -> Result<(), String>` and
  `pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String>`.
  It is already wired into `main.rs` (`rpc("module.method", args)`). Do not edit `main.rs`
  unless your lane brief says so.
- `ctx.db` is SQLite (`db.rs`): `exec`, `query` (rows as JSON objects), `with`,
  `kv_get/kv_set`, `now()` (ms), `new_id()`. Schema is in `db.rs`; if you need a
  column/table, append `CREATE TABLE IF NOT EXISTS` / new table SQL in **your own
  module's `init`** (not db.rs) to avoid conflicts.
- Events to the UI: `ctx.emit("module:thing", payload)`. Audit log: `ctx.log_event`.
  Cross-module change signal: `ctx.notify("name")` / `ctx.changes.subscribe()`.
- Settings: `ctx.setting("key", "default")` reads `setting.key` in kv. UI writes via
  `rpc("app.settings_set", {key, value})`.
- Paths: `ctx.root` (= ~/Philium; worktrees in `root/worktrees`, repos in `root/repos`),
  `ctx.data_dir` (app data), `ctx.agent_dir(name)`.
- Providers: `providers::resolve(ctx, Some("prov/model"))`, `providers::hal_setup(ctx, hal_data_dir, &ConfigOpts{token, only, no_subscriptions, default_ref})`.
- Launch a HAL: `hal::Hal::start(hal::Launch{dir, config, cwd, env, allow_logins, wrap})`,
  then `h.call(Method, path, body)`; `h.base`, `h.token` for direct HTTP/SSE.
- Gateway: `ctx.gateway.issue_token(agent, Some(providers))`, `revoke_agent`.
  Every `gateway::Activity` has `agent`, `ttft_ms`, tokens, duration.
- Tests: `Ctx::test()` gives an in-memory context. `cargo test` must pass.
  Write real tests for logic (state machines, parsing, security rules).

## Frontend contract (`src/`)
- SolidJS. Each screen is `src/screens/<id>.jsx` default-exporting a component
  that returns `<Page title sub actions flush?>…</Page>`.
- UI kit: `src/ui/kit.jsx` (Button, Field, Input, TextArea, Select, Toggle,
  Segmented, Badge, Dot, Card, Stat, Empty, Shield, Page, Modal, confirm, toast,
  VirtualList, Meter, Bars). Icons: `src/ui/icons.jsx` (`<Icon name>`).
  Styles: `src/styles/components.css` classes. If you need new styles, create
  `src/styles/<lane>.css` and import it from your screen. Do not restyle shared classes.
- Backend: `import { rpc, useEvent, fmt } from "../lib/rpc.js"`.
  Navigation: `import { go, params, registerCommands } from "../lib/nav.js"`.
- Design: dark, quiet, self-explanatory. Every empty state explains what the
  screen is for and offers the first action. Security state is always visible
  (secure = green, strict/untrusted = amber, danger = red). Lists that can grow
  large use `VirtualList`. Never render untrusted text as HTML.
- `npx vite build` must succeed.

## Done means
- `cargo test` (in `src-tauri`) and `npx vite build` pass.
- Your module's rpc methods are documented at the top of the `.rs` file.
- Committed. Report back: methods, events, anything another lane must know.
