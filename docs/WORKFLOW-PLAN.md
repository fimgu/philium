# Philium: one coherent coding workspace

Status: **proposal, not implemented or approved as a broad rewrite**. Requested 2026-11-16. The stopped-agent history repair is separately approved and underway. The user controls installation and restart.

## Primary goal
Describe a piece of work, choose its repository and how it should run, then direct, inspect and review that work in one place, whether one named agent or a coordinated team performs it. Changing models, responding to approvals, stopping execution and reopening saved work must be ordinary, discoverable actions that do not lose context.

## Product diagnosis
The underlying pieces are more capable than their connection in the UI. Adding a Model dropdown and a Swarm button alone would leave the same fragmented workflow.

- Existing agents can change model through the backend, but the agent UI only displays the model. Effort is exposed indirectly as Fast / Normal / Deep.
- Helpers, an agent tree, mail, task leases, dependencies and review transitions already exist. They are useful primitives, not yet a complete user-directed team run.
- Passing `task_id` to spawn records a relationship; it does not atomically claim the task for that agent. A task link is not ownership.
- There is no implemented team concurrency/budget controller or Chat-to-agent handoff. These cannot be solved with UI labels alone.
- The same word Stop currently covers cancelling a turn in the composer and shutting down an agent in the header. Conversation viewing should not depend on a live process.
- Chat is intentionally tool-free. Its legitimate role is discussion without machine access, but the product does not connect discussion to execution.

## Proposed product model
**Work is the primary view. Agents are the persistent workers underneath it.** Preserve named agents, their individual HAL sessions, shells, worktrees, transcripts and ownership. Do not turn them into anonymous disposable chat tabs.

### Start work
One entry point: goal, repository/base (or explicitly no repository for discussion), execution choice, model and permissions. Offer:
- **Discuss:** no tools and no machine access. Existing Chat remains available until a deliberate migration is approved.
- **Solo:** one named coding agent, with visible model and effort.
- **Team:** a lead plus explicitly configured workers/reviewer, with a plan and execution allowance approved before workers start.

Discuss-to-code is an explicit transition: preview the context being shared, choose target repository and permissions, then confirm. Never silently grant tools to a chat or send an entire private transcript to another provider.

### While working
A stable workspace contains:
- Header: work title, exact repo/branch/base, execution status, model, effort and permissions. Team mode also shows active worker count and allowance.
- Main conversation: talk to the solo agent or team lead. Inspect a worker's conversation without losing the main task/draft.
- Context panels: Plan / Changes / Code / Terminal / Browser. These are views of the same work, not unrelated destinations.
- Review: exact diff and tested revision, outstanding findings, and an explicit decision to accept or send back. Completed tool execution is not completed work.
- Lifecycle actions with different labels: **Cancel turn**, **Shut down agent**, **Resume**, **Remove**. Removal remains separate and confirmation-protected. A team action states whether it affects one member or all owned workers.

Model switching must show the requested and effective model, preserve conversation/draft, and apply only at a safe turn boundary. Cross-provider Strict restrictions remain enforced; switching trust boundaries requires an explicitly created compatible agent, not relabeling or silently restarting the existing one. Do not carry browser grants into a new runtime/provider session.

### A real team, not a pile of chats
The team lead proposes tasks, dependencies, owners and expected checks. The user sees and approves the plan plus maximum active workers before execution. A scheduler claims ready tasks with leases, launches only admitted work, and records task attempts and results durably. Use isolated worktrees; file-area ownership reduces overlap but is not a substitute for detecting and reviewing integration conflicts. A reviewer must not approve its own changes.

Failure, waiting for permission, a completed turn, finished implementation, and accepted review are distinct states. Stop/resume must recover the plan without duplicate workers or lost claims. No recursive unlimited spawning, silent auto-merge, automatic push or release.

### Navigation
Make Work primary, keep Code as a directly reachable inspection tool, and retain the agent fleet as a secondary management view. Group diagnostics rather than making them the main user journey. Do not remove existing navigation until equivalent paths and state-preserving migration are proven.

## Phase 0: Verify documented primitives and freeze scope
**How this serves the primary goal:** the new workflow must connect real capabilities rather than promise controls that do not work.

Documentation/source discovery completed against base `57ee572` plus the separately scoped history repair in progress. Line numbers may move; function names are authoritative.

Sources and copy-ready patterns:
- `docs/SPEC.md:5-23`: named-agent workstation, isolation, dynamic models, cleanup and visual goals.
- `src/screens/agents.jsx:37-96`: SpawnModal, live provider/model options and `agents.spawn` arguments.
- `src/screens/agents.jsx`, `Details` and `Agents.tree`: current read-only model, helper links and parent-child tree. Reuse them, do not build a duplicate agent hierarchy.
- `src-tauri/src/agents.rs`, `SpawnReq`, `spawn`, `provision`, `call`: runtime lifecycle and model/effort operations. `spawn` currently stores `task_id` without task claim.
- `src-tauri/src/board.rs`, `claim`, `submit_inner`, `release_agent`, mail dispatch: ownership leases, dependency gates, checked review transitions and coordination.
- `src-tauri/src/mcp.rs`, `agent_spawn`, `agent_stop`: caller-scoped child creation/cleanup and Strict-provider constraints.
- `src/screens/chat.jsx:57-81`, `src-tauri/src/main.rs:51-65`: tool-free Chat session creation and deny rules.
- `src/lib/drafts.js`, `src/lib/code-context.js`, `src/ui/code-inspector.jsx`: draft-only context handoff, explicit source/worktree/revision and per-agent draft isolation.

Allowed existing APIs, not a claim that new team APIs already exist:
- `providers.models`, `providers.list`, `providers.default` for live discovery/trust/defaults.
- `agents.spawn {name?, role?, model?, repo_id?, task_id?, brief?, parent_id?, strict?}`.
- `agents.get`, `agents.list`, `agents.events {id, after?, limit?}`.
- `agents.set_model {id, model}`, `agents.set_effort {id, effort}`, `agents.get_effort {id}`.
- `agents.send {id, text, images}`, `agents.cancel`, `agents.stop`, `agents.resume`, `agents.remove`.
- Existing board claim/release/submit/update/dependency and mail operations, subject to actor/ownership validation. Do not fabricate a claim on behalf of another agent from the human UI.
- MCP names are snake_case (`agent_spawn`, `mail_send`, `shell_run`); prose SPEC names are not callable APIs.

Verification: resolve the task-claim transaction boundary and runtime turn-state contract before implementation. Inventory all existing navigation and drafts that must survive. Declare platform/user-workflow acceptance rows before coding each phase.
Anti-pattern guards: no imagined `swarm.start`, no assumed budgets or queued runtime state, no direct HAL shortcut around Philium lifecycle/security, no wholesale rewrite of working subsystems.

## Phase 1: Complete control of one agent
**How this serves the primary goal:** solo work must be reliable and understandable before multiplying it into a team.

Implement:
1. Finish saved-history loading and local error/retry without a runtime, including stop/resume and stale-request safety.
2. Copy model discovery from SpawnModal into a persistent agent/work header. Surface actual effort values with understandable presets and show disabled/unavailable choices and trust restrictions.
3. Guard configuration changes at turn boundaries in the backend, not merely with a disabled UI. Save requested configuration only after acceptance; failed changes retain the effective model and draft. Allow saved configuration while stopped where the backend supports it, extending effort persistence deliberately if needed.
4. Separate Cancel turn, Shut down and Remove labels and scopes. No implicit startup merely to view history or configuration.

References: agent lifecycle/model methods and SpawnModal above; reuse existing action-local error and draft patterns.
Verification: existing and stopped agent model selection, unavailable/provider-error choice, active-turn change, Strict same-provider and forbidden cross-provider cases, effort persistence, failed change retains draft/model, cancel then second send, shutdown without removal, reopen and inspect history. Check actual provider route/model through a controlled test integration, not only a changed label.
Anti-pattern guards: no hidden provider-key exposure, silent trust downgrade, changed active-turn model, automatic restart or transcript deletion.

## Phase 2: Connect a task to its complete work context
**How this serves the primary goal:** the user should direct and review one piece of work without manually assembling unrelated pages.

Implement:
1. Use the existing board task as the work identity where possible. Specify a durable execution-attempt record only for lifecycle information the existing task/agent rows cannot represent. Avoid a second independent task board.
2. Add one task-to-agent launch/assignment operation with validation and a consistent claim/provisioning transaction or documented compensation path. Prevent duplicate clicks/retries from making duplicate agents or claiming work twice.
3. Build the workspace shell around existing conversation, source/diff, terminal, browser and task views. Explicitly bind every panel to the same task, selected agent and worktree; navigation preserves drafts and selected review context.
4. Add a previewed Discuss-to-code context handoff, copying the existing unsent code-draft pattern. Sharing with a provider remains deliberate.

References: board ownership/dependency code, SpawnReq and spawn, current code-inspector target handling and draft helper.
Verification: goal -> repo -> one agent -> execute -> inspect exact diff/tests -> review decision -> revisit after shutdown; launch failure/claim rollback; duplicate submit; stale target response; draft/image preservation; peer task isolation; context-sharing preview with no automatic send.
Anti-pattern guards: attaching `task_id` without a real claim, migrating transcripts by blind concatenation, a new navigation label without a complete workflow, global errors for local panel failures.

## Phase 3: Add coordinated team execution
**How this serves the primary goal:** parallel agents should produce one reviewable result under user control, not more coordination work for the user.

Implement:
1. Add a proposed plan with explicit tasks, dependency edges, roles, model/effort choices and acceptance checks. Persist user approval and the execution allowance. Parent-child ownership and existing board leases remain the starting primitives.
2. Introduce the missing team-run/scheduler contracts explicitly: run identity, membership, attempts, admission, task lease/heartbeat ownership, blocked reasons and recovery. Validate cycles and reject self-review. Make worker creation idempotent and bound recursion/concurrency.
3. Enforce limits at all launch paths, including agent-initiated MCP helpers. A cap in the modal alone is not a cap. Show measured tokens/cost and transparent uncertainty; do not promise a hard monetary budget where subscription transport does not expose enforceable spend. Use enforceable worker/turn/token admission limits with documented in-flight overshoot and stop rules.
4. Reuse individual worktrees and the existing parent-child tree; add run-level progress, assignments, blocker/approval inbox and lead/worker navigation. Integration and conflicting patches require a visible review step with source revision and test evidence.
5. Team shutdown affects only owned processes/resources. Resume reconstructs existing attempts and claims rather than respawning duplicate workers. Preserve completed results and saved conversations.

References: MCP caller scoping, agent stop cleanup, board leases/dependencies/review/mail, current tree and worktree APIs. Scheduler/budget contracts are NEW work, not existing callable APIs.
Verification: lead + two workers + independent reviewer, one dependency blocked, one permission request, one worker failure/retry, concurrency ceiling across UI and MCP, task lease expiry, restart recovery, sibling isolation, deliberate conflicting edits, exact-head review, team stop/reconnect and owned-resource cleanup. Tests must prove coordination outcomes, not just three windows appearing.
Anti-pattern guards: unbounded helper spawn, multiple workers writing one shared checkout by default, pretending prompts enforce locks/budgets, auto-accepting a model's review claim, silent merge/push/deletion.

## Phase 4: Prove the complete user journey and migrate safely
**How this serves the primary goal:** the redesign is successful only when the actual product lets the user complete the work reliably.

Verify the full solo and team workflows through the real packaged app and integrations on the declared platforms. Initial local target is macOS arm64; other advertised targets need their own explicit acceptance coverage before a cross-platform readiness claim. Test repeated work, permission failures, provider errors, stop/resume/restart, model changes, recovered plans, exact-source queries/diffs, draft preservation and peer isolation.

Keep exact-head logs, redacted screenshots and checkpoint results. Keep failed/blocked/untested rows visible. Unit tests, fixture UI and packaging are supporting evidence, not substitutes for native acceptance. Apply the review gate for any actual PR approval; never invent a PR number for this proposal. The user performs installation/restart manually.

Only after the replacement paths pass should the old fragmented navigation be collapsed. Maintain a route/data compatibility path and a recoverable migration, with no deletion of conversations/worktrees.

## Not now
Full IDE/editor replacement; agent marketplace; automatic merges/releases; new credential/signing systems; per-agent VMs; speculative cross-repository orchestration; a visual team builder before the first real solo/team workflow works.

## Decisions to confirm before broad implementation
The proposed center is Work with named agents underneath it, not an agent fleet as the primary landing page. Team mode is user-approved coordination, not unrestricted autonomous spawning. Existing Chat becomes a connected discussion entry rather than being removed immediately. These are product proposals, not authorization to migrate the user's data or change the running application.
