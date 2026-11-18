# Local checks and model picker acceptance plan

Implementation scope: one human-triggered, local-only check at a time, detached source worktree, exact revision and optional same-repo task, capped stdout/stderr, timeout/cancel, saved evidence, and provider-first model selection. No provider calls, automatic installs, remote runners, CI approval or release automation. A check command is trusted local code, not a security sandbox. Native Windows execution must fail closed until process-tree ownership is implemented.

Required coverage, declared before testing:

| Workflow | Environment | Required observation | Status |
|---|---|---|---|
| Start check from Review, inspect pinned revision/argv/timeout, save result | macOS source app backend + headless UI | Explicit confirmation, actual command stdout/stderr/exit, task link and persisted result | Untested |
| Failure, missing executable, output cap, timeout, cancel | macOS subprocesses | Correct terminal states, bounded output, all owned descendants stopped | Untested |
| Parent process interruption, fresh process recovery | macOS subprocesses + SQLite | Worker notices closed pipe, kills children, persisted run is interrupted, no false pass | Untested |
| Dirty/revision mismatch, concurrent starts, cross-repo task and run IDs | macOS backend | Refused without execution, peer records unaffected | Untested |
| Worktree cleanup and reopening result | macOS backend | Only owned snapshot removed, saved evidence retained | Untested |
| Provider accordion, global search, current selection, keyboard and small viewport | Headless Chromium actual UI | Selecting provider does not select model, search reveals matches, Escape returns focus, trust/locked restrictions retained | Untested |
| Checkout switch during async calls | Headless Chromium actual UI | No stale results or cross-checkout commands | Untested |
| Packaged native graphical workflow | Disposable macOS graphical session | Repeat actual Review run/cancel, model selection and quit/reopen | Blocked: no disposable graphical session established; live user session must not be touched |
| Windows process lifecycle | Native Windows | Start/cancel/timeout/restart with descendant cleanup | Blocked: native environment unavailable; runner must explicitly reject Windows |

Regression checks: complete Rust suite, JS unit suites, Vite build and existing headless checks where available. Supporting backend/headless checks are not native desktop acceptance. Commit SHA and exact observations will be recorded after implementation. No GitHub review or approval is requested.

Progress, 18 November 2026 (partial implementation, not release acceptance): `scripts/check-local-worker.py` exercised successful, failed, oversized-output, missing-command, timed-out, cancelled and owner-pipe-closed Make runs on macOS arm64. It checked peer separation, stale/dirty refusal, saved rows and removal of its fixture directory. The implementation uses a detached local clone instead of registering a worktree in the source repo. An initial smoke test caught the Apple make shim attempting a host cache write; resolving make via xcrun fixed the smoke test without widening write access. Rust and JS regressions and a Vite build were exercised during development. Full Review/RPC/graphical acceptance, a representative Exo suite and live Kubernetes policy enforcement remain unproved.

The tenant JSON in Review is an offline preview only. Neither this preview nor snapshot cleanup deletes kind clusters or Colima profiles. For managed environments the intended policy is: bind checkout, task, runtime endpoint and immutable resource identity at creation; release on finish/cancel or reconcile an expired lease after restart; verify absence on the same runtime. Reused names, unknown ownership, active leases and external/shared profiles must block deletion. Never infer ownership from a kubeconfig context or a cluster name. No live cleanup enabled by this change.
