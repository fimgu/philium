# Philium gap register

Philium (with HAL) already has: subagents, MCP, the macOS sandbox, several model
providers, skills and memory, worktrees, a ⌘K palette, image paste and `/fork`.

This register lists what is still missing, ranked by impact inside each area.
It started from three parallel code-reading audits (grep plus selected reads,
nothing executed). Line references are approximate. Every item has a status:

- **Claimed**: from the audit, not yet re-checked.
- **Verified**: a grounding pass confirmed it with file:line evidence.
- **Partly true / False**: grounding found the claim wrong or overstated.

Each item gets a grounding brief in `docs/gaps/<ID>.md` (see "Grounding brief
format" at the bottom). This file stays the index. Update the status column
when a brief lands.

All 22 items have source-grounding briefs as of 2026-11-23. Statuses below
describe the audit claims, not implementation completion or runtime acceptance.
Linked briefs carry the qualifications and remaining gaps; the original audit
descriptions below are retained as context and may be overstated.

## Where fixes can live

Agent-core items mostly sit in HAL (`~/Developer/hal`). `docs/LANES.md` treats
HAL as read-only for Philium lanes, so every agent-core brief must say which of
these it needs:

1. **Philium-only**: gateway (`src-tauri/src/gateway/`), `hal.rs` launch config,
   or the UI.
2. **HAL change**: needs an upstream HAL change. The brief names the HAL files
   and the smallest interface Philium needs.
3. **Both**: HAL exposes a primitive, Philium builds on it.

## Index

| ID  | Area  | Gap                                               | Status   |
|-----|-------|---------------------------------------------------|----------|
| A1  | Agent | Undo and checkpoints for file edits               | [Partly true](gaps/A1.md) (Philium proxy + undo UI done, HAL routes open) |
| A2  | Agent | Todo/plan tool and plan mode                      | [Partly true](gaps/A2.md) (plan latch done, UI buttons and HAL checklist tool open) |
| A3  | Agent | User-configurable tool hooks                      | [Verified](gaps/A3.md) (user lifecycle hooks) |
| A4  | Agent | Prompt caching on every request path              | [Partly true](gaps/A4.md) |
| A5  | Agent | Dollar cost tracking and price table              | [Partly closed](gaps/A5.md) (runtime price refresh, catalog fallback, cache-read pricing, included vs unknown; cache-write and session attribution open) |
| A6  | Agent | Web fetch/search and LSP/code-index tools         | [Partly true](gaps/A6.md) |
| A7  | Agent | Compaction controls                               | [Partly true](gaps/A7.md) (focused /compact done, window provenance open) |
| U1  | UX    | Per-hunk accept, reject, stage, revert            | [Partly closed](gaps/U1.md) (stage/unstage/revert per hunk and file; accept-as-reviewed decision open) |
| U2  | UX    | @-mentions in the composer                        | [Verified](gaps/U2.md) |
| U3  | UX    | Git and PR flow in the UI                         | [Verified](gaps/U3.md) (action flow, not read-side integration) |
| U4  | UX    | Mid-run steering                                  | [Verified](gaps/U4.md) |
| U5  | UX    | Cross-session search, fork from any message       | [Closed (Philium side)](gaps/U5.md) |
| U6  | UX    | Native OS notifications                           | [Verified](gaps/U6.md) |
| U7  | UX    | Shortcuts, onboarding, multi-agent grid           | [Partly closed](gaps/U7.md) (shortcut map in `src/lib/shortcuts.js`; onboarding readiness and pinned panes open) |
| I1  | Infra | CI                                                | [Verified](gaps/I1.md) (missing checked-in CI only) |
| I2  | Infra | Code signing and notarization                     | [Verified for release signing, partly true as phrased](gaps/I2.md) |
| I3  | Infra | Auto-update                                       | [Verified](gaps/I3.md) (Philium, not standalone HAL) |
| I4  | Infra | Secrets in plaintext JSON                         | [Verified](gaps/I4.md) (existing file-permission containment) |
| I5  | Infra | Sandbox is macOS-only but Linux/Windows ship      | [Partly true](gaps/I5.md) (safe refusal exists, shipping unverified) |
| I6  | Infra | Network blocking for sandboxed agents             | [Partly true](gaps/I6.md) (integration proof remains) |
| I7  | Infra | Logging and diagnostics                           | [Partly true](gaps/I7.md) |
| I8  | Infra | Frontend tests, lint, version mismatch            | [Partly true](gaps/I8.md) |

## Agent core (mostly HAL)

### A1. Undo and checkpoints for file edits
- **Problem**: HAL's edit, write and patch tools (`hal/src/tools/edit.rs`,
  `write.rs`, `patch.rs`) change files without taking a snapshot. A bad edit
  can only be fixed by hand or with git, and only if the file was committed.
- **Why it matters**: highest-impact trust gap. Claude Code, Cursor, Aider and
  Jcode all let you roll back an agent edit.
- **Done looks like**: every mutating file tool records the before-state per
  turn. The user can undo the last turn, or restore to any earlier turn, for
  one file or the whole turn. Works in worktrees and outside git repos.
  Covers shell-driven edits at least by detecting and warning about them.
- **Open questions**: snapshot store (shadow git repo vs content-addressed
  blobs), retention limits, and how undo interacts with `/fork`.

### A2. Todo/plan tool and plan mode
- **Problem**: no tool for the agent to keep a visible task list, and no
  read-only "plan first, then execute" mode.
- **Done looks like**: a todo tool whose state streams to the UI and survives
  reloads. A plan mode where mutating tools are refused until the user
  approves the plan.
- **Open questions**: does plan mode reuse the permission layer in
  `hal/src/security/permissions.rs`, or a separate tool filter?

### A3. User hooks (needs check)
- **Problem (claimed)**: no user-configurable before-tool or after-tool hooks.
  The only hook found is the sandbox-level `hal/src/security/exo_hook.rs`.
  A quick grep for `hooks` / `PreToolUse` in `hal/src/config.rs` and
  `hal/src/agent/runtime.rs` found nothing, which supports the claim.
- **Done looks like**: hooks configured per project or globally, running a
  user command on events (before tool, after tool, turn end, session start).
  A hook can block a tool call or add context. Hooks run inside the same
  sandbox policy as the agent and never see provider credentials.
- **Grounding must confirm**: what `exo_hook.rs` actually does, and whether it
  can be extended rather than adding a second mechanism.

### A4. Prompt caching (partly true)
- **Audit claim**: `cache_control` is absent from `hal/src/provider/anthropic.rs`,
  so prompts may not be cached.
- **Spot check**: Philium's gateway injects Anthropic ephemeral breakpoints
  (`src-tauri/src/gateway/translate.rs` `inject_cache_breakpoints`, around
  line 403: last tool, last system block, last two messages), and HAL routes
  models through the gateway. So the main Anthropic path is probably cached.
- **Grounding must confirm**: every path that can reach a Claude model
  (gateway `/messages`, Claude via `/chat/completions`, subscription bodies,
  any direct HAL provider call that bypasses the gateway) and whether cache
  hits are observable (cache read/write tokens recorded in gateway activity).
- **Done looks like**: cache read/write tokens shown per request, and a test
  per path proving breakpoints are present.

### A5. Dollar cost tracking
- **Problem**: budgets and stats are token-only. No price table, no per-agent
  or per-session dollar cost.
- **Done looks like**: cost per request, session and agent, including cache
  read/write pricing. Prices resolved at runtime from a live source, per the
  AGENTS.md "never stale pins" rule. Any fallback table lives in `pins.rs`.
  Subscription-billed requests shown as "included", not as a fake price.

### A6. Web fetch/search and code intelligence tools
- **Problem**: the agent has no web fetch or search tool and no LSP-style
  tool. Philium has `src-tauri/src/codeindex.rs` and `codeinspect.rs`, but the
  agent cannot call them.
- **Done looks like**: fetch (with network policy applied), search (provider
  resolved at runtime), and symbol tools (definition, references, outline)
  backed by codeindex/codeinspect, exposed to agents via MCP or a HAL tool.
- **Open questions**: expose Philium's index as a local MCP server (no HAL
  change) vs a native HAL tool.

### A7. Compaction controls
- **Problem**: compaction (`hal/src/agent/context.rs`, `compaction_request`
  around line 205) only summarises older history and keeps recent messages.
  No `/compact <instructions>`, no automatic threshold.
- **Done looks like**: manual `/compact` with optional focus instructions,
  an automatic threshold based on the model's real context window (resolved
  at runtime), and a visible marker in the transcript where compaction ran.

## UX

### U1. Per-hunk accept or reject
- **Problem**: hunks in the diff view can be selected to comment on, but not
  staged, reverted or applied.
- **Done looks like**: per-hunk and per-file stage, unstage, revert and
  accept, with keyboard shortcuts. Revert integrates with A1 checkpoints.

### U2. @-mentions in the composer (needs check)
- **Problem (claimed)**: no `@file`, `@symbol` or `@url` mentions.
- **Grounding must confirm**: `src/screens/agents.jsx` and `terminals.jsx`
  contain `@`, so check whether anything mention-like already exists.
- **Done looks like**: typing `@` opens fuzzy file and symbol search (backed
  by codeindex), URLs fetched under the network policy, and the attached
  context shown as chips before sending.

### U3. Git and PR flow
- **Problem**: no commit, push or open-PR from the UI.
- **Done looks like**: commit with an agent-drafted message, push, and open a
  PR from a worktree, showing CI status once I1 exists. Uses the user's own
  git and `gh` credentials, never ones exposed to agents.

### U4. Mid-run steering
- **Problem**: messages sent while an agent runs only queue until the turn ends.
- **Done looks like**: a message can be injected at the next tool boundary
  (steer) or queued (default), and the user picks which. Needs a HAL
  interface to accept input mid-turn.

### U5. Cross-session search and fork from any message
- **Problem**: no search across sessions. `/fork` only forks the current state.
- **Done looks like**: full-text search across all sessions and agents, and
  fork from any message in a transcript.

### U6. Native OS notifications
- **Problem**: only in-app badges and sound when a background agent finishes
  or needs approval.
- **Done looks like**: native notifications (Tauri notification plugin) for
  finish, failure and approval-needed, click-through to the agent, and
  respecting focus and Do Not Disturb.

### U7. Shortcuts, onboarding, multi-agent grid
- **Problem**: only ⌘K and ⌘1-9. No first-run onboarding. No side-by-side
  view of several agents.
- **Done looks like**: a documented shortcut map (new agent, send, stop,
  switch agent, open diff, approve), a first-run flow that ends with a
  working provider and a first agent, and a grid of 2 to 4 live agent chats.

## Infra and security

### I1. CI
- **Problem**: no `.github` folder in Philium or HAL. Nothing runs on push.
- **Done looks like**: on every push and PR, `cargo test` in `src-tauri`
  (including `pins::tests::no_stale_pins`), `npx vite build`, frontend lint
  and tests (I8), and a macOS build. Release builds signed (I2).

### I2. Signing and notarization
- **Problem**: `src-tauri/tauri.conf.json:25` sets `signingIdentity: "-"`
  (ad-hoc), so Gatekeeper warns and the sandbox entitlements story is weak.
- **Done looks like**: Developer ID signing, notarization and stapling in the
  release pipeline, with secrets held only in CI. Windows signing decided.

### I3. Auto-update
- **Problem**: no update mechanism.
- **Done looks like**: Tauri updater with signed manifests, an update prompt
  that never interrupts a running agent, and rollback on failed start.
  Depends on I1 and I2.

### I4. Secrets in plaintext JSON
- **Problem**: `src-tauri/src/secrets.rs` stores keys in a JSON file. The
  `keyring` crate is already in `Cargo.toml` (line 20) but deliberately unused.
  Any process running as the user can read the keys.
- **Grounding must find**: why keyring was rejected (prompt fatigue, sandbox
  access, headless use?) so the fix addresses that reason.
- **Done looks like**: secrets in the OS keychain (or encrypted at rest with a
  keychain-held key), one-time migration of the JSON file, and the existing
  test `vault_is_private_and_hidden_from_agents` still passing.

### I5. Sandbox is macOS-only
- **Problem**: `src-tauri/src/sandbox.rs:109` says Landlock is "not implemented
  yet", yet `tauri.conf.json:22` builds `msi`, `appimage` and `deb`.
- **Done looks like**: either Linux (Landlock + netns + seccomp) and Windows
  sandboxes exist, or non-macOS builds refuse to run untrusted agents and say
  so in the UI. Until then, stop shipping those targets or label them.

### I6. Network blocking for sandboxed agents
- **Problem**: unverified. The CSP only limits the web UI, not agent processes.
  HAL has `hal/src/security/exo_network.rs` and `exo_proxy`, but nothing shows
  a sandboxed agent is actually blocked.
- **Done looks like**: an automated test that launches a sandboxed agent and
  proves direct egress is denied and allowed hosts work through the proxy.

### I7. Logging and diagnostics
- **Problem**: only a custom `ctx.log_event`. No structured logs, rotation or
  diagnostics export.
- **Done looks like**: structured logs (`tracing`) with levels and rotation,
  HAL stderr tails included, and a "copy diagnostics" bundle with secrets
  redacted.

### I8. Frontend tests, lint, version mismatch
- **Problem**: no frontend tests or lint. `tauri.conf.json` says `0.1.0`,
  `package.json` says `1.0.0`.
- **Done looks like**: one version source of truth, a lint config, a test
  runner with tests for `src/lib/` logic, all wired into CI (I1).

## Grounding brief format

Each `docs/gaps/<ID>.md` contains:

1. **Verdict**: Verified, Partly true or False, with file:line evidence.
2. **Current state**: what exists today, in Philium and in HAL.
3. **Where the fix lives**: Philium-only, HAL change, or both (see above).
4. **Proposed design**: smallest change that meets "Done looks like".
5. **Acceptance checks**: concrete tests and real user workflows that prove it.
6. **Dependencies and risks**: other gap IDs, security impact, stale-pin risk.
7. **Size**: S, M, L or XL, with a one-line reason.
