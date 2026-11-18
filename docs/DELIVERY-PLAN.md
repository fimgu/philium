# Philium delivery plan

## Primary goal
Pick any registered repository, direct work, inspect what information a worker can see, run the right test in an approved environment, and make a decision from reproducible evidence. The workstation must stay responsive and must not disturb the operator's desktop. The product under test is **Philium**, not a candidate repository.

## Delivery sequence and gates

| Gate | Work owner and dependencies | Observable exit condition |
|---|---|---|
| 1. Read results locally | Review UI + evidence adapter. No external dependencies. | Choose a repo, open an exact-revision saved packet, read reported case outcomes and unknowns, switch repos without stale details. An imported pass is never a Philium run. Opening or parsing cannot execute, fetch, or publish. Verify included vs moved child PRs with a sanitized bundle fixture and a second unrelated project. Retain source labels. |
| 2. Context boundary | Runtime/security owner. Can proceed alongside 1. Required before agent dispatch. | Bind instructions, skills, recall, graph spans, cache and child requests to stable repo ID and recipient. Missing identity fails closed. No cross-repo marker in initial prompt, later tool calls or child dispatch, including same-basename repos. A PR cannot redefine its approval policy. Human can see where each input came from and what will leave the machine. |
| 3. Daily work loop | Workstation owner. Existing primitives in `WORKFLOW-PLAN.md`; do not reimplement working panels. | Start a task, choose/change an agent, inspect edits in a worktree, approve or stop, resume after restart without losing task, terminal or evidence. Switch repos during late responses. No focus theft, ghost jobs or duplicate starts. Test headlessly first, then separate native session. |
| 4. One local executor | Runtime + QA. Requires 1 and 2; build against a disposable synthetic repo first. | Explicit Run checks starts one pinned job; capture command, target, elapsed time, exit, stdout/stderr and cleanup. Test fail, cancel, timeout, dirty checkout, restart mid-run, and missing dependency. Resource budget and owner tags prevent exhausting the 16 GiB machine or deleting another job. Viewing never starts execution. |
| 5. Findings + independent verification | Review + QA. Requires 2 and 4. | A bounded isolated exploration logs one suspected finding and redacted evidence. Compare candidate and base; deduplicate, preserve failed attempts and let human confirm/dismiss. Nothing promoted to memory or posted automatically. Screenshots use disposable GUI sessions only. |
| 6. Private execution + nightly | Integration owner. Requires 4 and a named authorized target. | Pin source/artifact and bundle manifest, confine credentials, cap spend/concurrency, block protected namespace mutations, test create/use/delete and cancellation cleanup. Verify one release with multiple PRs and one unrelated repo; separate runner, inference, and publication grants. Human signs release; PR table update must be idempotent. |
| 7. Packaged daily-driver release | Release owner integrates preceding gates. | Exercise signed install, two real packaged restarts, update, interrupted update and rollback with preserved data in a separate OS session. Repeat a representative workday workload, measure latency and peak memory, compare before/after and settle budgets before signoff. Record exact builds, remaining blockers and a recovery path. No changes to the active workstation during validation. |

## How to run development

Use a single coordinator to own the interfaces, dependency order, evidence ledger and go/no-go call. Parallelize disjoint UI, runtime/privacy and headless QA work; integrate in small commits. Give any future worker one repo, one question, minimum context and an approved recipient. A larger swarm is not a substitute for distinct acceptance cases. Until there is a source-transfer grant, keep source and packet contents local. Product decisions requiring owner action: remote destination and permitted payload, posting policy, release signoff and time to install. None blocks gates 1 or 2.

For every increment, predeclare the user path, failure path, restart/scope boundaries and target environment; implement, run the named checks, record exact revision, commit, then move the ledger. Treat fixture tests, headless integration, native workflow and release approval as different evidence. Never average them into a readiness percentage. The first reviewer pilot uses saved metadata from #5541, not another review of its contents. No customer source, transcripts or raw packets belong in Philium's git fixtures.

## First increment matrix (declared before testing)

- Read-only packet preview: active workspace required; missing or nonmatching identity, malformed file, duplicate cases, invalid revision or stale row must not display an unqualified pass.
- Human-readable provenance: imported versus Philium-run, full SHA, first unresolved workflow, missing case as untested, and no implicit approval.
- Headless rendered UI: import synthetic data, switch repositories while open, check prior case text disappears and reopening the first repo requires explicit selection. No test, publish, or remote inspection of the candidate PR.
- Native packaged restart, full initial-prompt and retrieval isolation, bundle metadata import and runner execution are separate gates unless directly exercised and recorded.
