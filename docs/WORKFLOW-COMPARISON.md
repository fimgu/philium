# Can Philium replace this workflow?

Decision as of 18 November 2026. Implementation compared: Philium `29600aa`; representative project: Exo `ca5ad617d`, including Sentinel under `deploy/sentinel`. This is a comparison of known workflows, not an audit of every tool on the Mac. No Exo suite, cluster, installer or release job was run for this report.

**No, not even at the code level.** Philium can be the front door to a checkout today. Running a command inside its terminal is useful integration, not the same as Philium scheduling it, identifying its inputs, saving its result, recovering after interruption or cleaning up after it. Keep the project's existing suites and native tools as execution engines even when Philium eventually owns the workflow.

| Job today | What changes in Philium | Parity / decision |
|---|---|---|
| Switch across similarly named checkouts; reconstruct task, branch and shell context | Home brings unfinished tasks, agents, worktrees and recorded resources together. New terminals start by registered checkout ID. | **Use now** for local coordination. Packaged tests checked two same-name peers, actual terminal working directories and persisted associations. This is not restoration of a live shell process. |
| Plan work, split it among agents, approve, stop and resume | Board, per-agent worktrees, hierarchy and repo-scoped tools reduce manual handoffs. The helper isolation check exercised a local recipient. | **Partial.** Local task coordination exists. Do not equate repository isolation with consent to send a payload to a provider. Explicit recipient/payload grants are missing. |
| Navigate source, edit, debug and inspect changes | Indexed symbols, graph and worktree diffs bring review context next to the task. | **Companion to an editor.** Code inspection is not editor/debugger parity. Keep the editor and debugger where they are better. |
| Build and test Exo/Sentinel | Open the right terminal and run the existing Make targets, including `build-sentinel` and `test-sentinel`. | **Wrapper, not a test runner.** Still missing a pinned, bounded job with timeout, cancellation, persisted output, exit status and restart recovery. Terminal output alone is not review evidence. |
| Inspect kind, Docker, kube contexts and resource ownership | Home answers “what did Philium record for this agent?” with an owner, handle and ledger state. | **Inventory only.** Not live pod health, logs, cluster-wide capacity or everything created by shell commands. Keep kubectl and existing cluster tooling. Pilot kind mutations and host-wide cleanup are blocked. |
| Verify Sentinel install and service behavior | The terminal can invoke existing project tooling, including `sentinel-linux-vm-test`. | **Not replaced.** Native macOS and Linux lifecycle tests need disposable hosts. A healthy kind pod says nothing about a native upgrade, restart or uninstall. |
| Review a PR, babysit it to merge, ship and watch the result | Review reads a saved packet locally and separates imported claims from checks Philium actually ran. | **Reader, not release automation.** Independent base/candidate findings, authorized private nightlies and a demonstrated review-to-release-to-post-release loop are missing. Keep the existing review, CI, release and post-release workflows. |
| Clean up and return tomorrow | Worktree status and cleanup proposals expose some ownership and risk. Terminal associations, tasks and the HAL session survived fixture restarts. | **Partial.** A ledger entry isn't proof an external object is safe to delete. Native graphical recovery, interruption/rollback and a responsive Exo/Sentinel workday on the 16 GiB Mac are unproven. |

## Highest-value next increment

Take a single existing Exo/Sentinel suite all the way from **task → exact checkout/revision → Run checks → saved pass/fail evidence → review**. Verify failure, cancellation, timeout, restart and cleanup before expanding to a nightly or native installer. Then compare the same representative task against today's workflow: time to a trustworthy verdict, manual handoffs, wrong-checkout actions and peak memory. Fewer places to look is a demonstrable design change; faster builds, better agents and lower RAM are hypotheses until measured.

## Evidence map

- Local workflow baseline and host constraints: `docs/DAILY-DRIVER-REVIEW.md`; delivery requirements: `docs/DELIVERY-PLAN.md`.
- Dashboard and terminals: `src/screens/checkout-home.jsx`, `src-tauri/src/workspace.rs` (`overview`), `src-tauri/src/terminals.rs`.
- Cluster controls and resource tracking: `src-tauri/src/agents.rs` (`kind_list`, `kind_create`, `kind_delete`), `src-tauri/src/reaper.rs`.
- Saved evidence and proposed execution path: `src/screens/review.jsx`, `docs/REVIEW-ARCHITECTURE.md`.
- Packaged checkpoint and its limits: `docs/PILOT.md`, `scripts/check-pilot.py`, `src-tauri/src/pilot.rs`. Existing regression results at the compared revision: 168 Rust passed, 7 ignored; 65 JavaScript passed. Not a workday benchmark.
