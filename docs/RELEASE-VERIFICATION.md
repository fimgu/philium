# Verify a release PR

**Product design, not installed functionality.** Review gains one entry point: paste a PR, choose **Verify release**, and return for a decision. An ordinary PR and a release containing dozens of PRs use the same screen. The user should not have to write a prompt, build a swarm, or reconcile worker summaries. The existing packet reader and local checks remain available. Neither is a substitute for this workflow.

## What the user sees

On first use, configure a repository profile: trusted origin, product and resource catalogue, mandatory regressions, approved test drivers, named staging target and provenance source, isolated candidate recipe, allowed providers and payload classes, concurrency, budget and retention. Store it locally, version it, and show changes before granting new capabilities. A PR cannot edit its own acceptance policy. On subsequent runs, the URL and one click are sufficient within the enrolled scope. Missing facilities block only the cases that need them.

Illustrative screen, not live release data:

```
Review / Release #42                  Current head H2 · checked 10:41
Waiting for staging login             [Sign in to env01 · Firefox tab 4]
Local verification continues          8 passed · 2 running · 1 needs comparison

CLI pagination                        Candidate skips record 101
                                      Comparing base and candidate with seed 27
Resource connections                  4 of 7 types reconnected to the right source
Staging                               Deployed S1, not H2. Evidence kept separately.

[Workflows]    [Included changes]    [Evidence & cleanup]
```

Lead with the one action the user can take, not agent transcripts. From a row open its task, exact checkout, agent, running resource, bounded logs and evidence. Completed rows show observed behavior and a cleanup receipt. Show the change tree on demand. Notify for login, a decision-changing finding or a resolvable blocker. Opening a saved run is offline. **Verify release** starts work, never publication.

## Execution contract

A local coordinator owns the run and schedules dependency-ordered jobs from a durable journal. Models may propose cases, inspect authorized evidence and propose explanations. Only the coordinator allocates resources, accepts evidence, advances state and applies the decision rules. A vote among agents is not a verdict.

| Role | Inputs | Deliverable and boundary |
|---|---|---|
| Release planner | Trusted profile, live origin refs and metadata, local packet index | Exact revision snapshot, inclusion graph, frozen matrix. No staging or write credentials. |
| Environment coordinator | Approved target identities, pinned build inputs | In parallel: open the dedicated staging tab if permitted, verify deployed digest-to-revision provenance, and build one shared isolated candidate. Create a separate base when needed. Never deploy to make staging match. |
| Workflow leads | Pinned candidate and disjoint fixture grants | Run a small bounded pool of lanes. Each leaf case produces a direct observation or a specific reason it could not run. Leads cannot drop cases or expand grants. |
| Independent verifier | Frozen expectation, proposed reproduction, equivalent base/head manifests | Fresh fixtures and separate session. Collect both observations independently before seeing the author's attribution. No editing the candidate or grading its own fix. |
| Decision and cleanup service | Required rows, verified findings, fresh origin snapshot, resource ledger | Compute verdict, verify teardown, save private evidence and draft. No GitHub write or deployment credentials. |

Run states: `resolving → planning → executing → deciding → closed`. Attribution jobs start as soon as a finding exists. Login pauses a lane, not the run. Each attempt has an immutable identity, input manifest, owner, deadline and lease. On restart, reconcile the recorded resource identity before resuming or cleaning. An ambiguous launch is inspected, not blindly repeated. Cancellation stops new allocations and triggers owned cleanup. Release supersession cancels obsolete jobs safely and keeps their observations on the old generation.

## Resolve what actually shipped

Record origin repo identity, PR number, target branch tip, merge base, head and snapshot time. Freeze both the **diff baseline** and the **executable release comparator** (normally the pinned target tip, or the enrolled previous release when testing upgrades). Label them explicitly. A synthetic merge tested for mergeability is another revision, not the PR head.

1. Enumerate commits and actual net changes, with pagination. Join origin commit-to-PR associations and merge metadata to local Git ancestry. Walk included PRs recursively with a visited set. Model a graph, not a tree: a shared grandchild is one change with several paths.
2. For a merge commit, retain parents, integration result and any integration-only edits. For squash, rebase or cherry-pick, record mapped landed commits and patch evidence. A link in prose, a shared title or a patch match alone does not establish ancestry. Show `confirmed`, `candidate match`, `reverted/superseded`, or `unresolved`. Preserve direct commits, conflict resolutions and unassigned diff hunks as first-class changes.
3. Map net hunks to product workflows and dependencies. One row can cover several children and one child can require several rows. Deduplicate execution by case, revision and environment, not by PR number. An unmapped hunk must be assigned or explicitly accounted for before READY. Reverted features need relevant residue/compatibility checks, not credit for previously passing the original feature.
4. Re-resolve the origin before every verdict. Changed head, or changed target tip when it is the comparator, creates a new generation: show delta, freeze a new plan and run the applicable current-generation checks. If the origin cannot be reached, no current-head verdict. Preserve old packets as history, never roll forward a green badge by PR number.

## Freeze observable workflows, not test names

Each required row records change IDs, intended behavior, platform/client/resource type, environment class, fixture and role identities, ordered checkpoints, expected output, failure criteria, cleanup owner and estimate. Build rows from the authoritative support catalogue, live diff, child changes, core regressions and previously observed risks. An unsupported resource and a supported-but-unavailable resource are different. Plan additions are versioned and visible. Removing a requirement requires a recorded scope decision, not a worker retry. Reuse evidence only when revision, case definition, inputs, actual environment/provenance and observation match the reuse policy. Read old failures first.

Example release profile lanes (selected by change, not hard-coded into Philium):

| Lane | What constitutes evidence |
|---|---|
| Access/JIT | Distinct requester and approvers, correct visibility, individual votes, denied before threshold and actual access after. |
| CLI and console | Separate exit code/stdout/stderr, filters and reserved endpoint names, changing multipage data and prune survivors. Real browser last-record identity on large, skipped and shrinking lists, including each affected integration and inventory route. |
| Resource lifecycle | One row per supported type and required client/platform. Register, connect, return a unique source marker, disconnect, verify owned handles and a live peer, reconnect and issue a fresh command. |
| Extraction and upgrade | Inspect persisted events for clean, interrupted and abandoned sessions. Verify streaming, memory bounds, restart behavior and old-schema compatibility with explicit thresholds. |
| Credentials and artifacts | Disposable quoted MySQL credential succeeds and is absent from retained errors. Resolve the actual image of a live SSH session. Compare app and background release artifacts by digest without publishing. |

Keep unit/build/static results separately. All lanes may share the immutable candidate build, not fixture users, sessions, mutable datasets or destructive access. Lock exclusive resources or allocate another instance of that same build for restart, migration and load cases. Run only enrolled staging-safe cases on staging. Staging must identify the actual serving revisions, including relevant background workers and mixed rollouts. An unproven revision is `unknown`, not `head`. Local kind or headless evidence cannot satisfy a staging or native-browser requirement of a different class.

## Attribute findings before assigning blame

Capture the smallest workflow that demonstrates the impact. Pin seed, inputs, driver/toolchain, permissions, platform, resource type and starting data. The verifier runs equivalent trials against clean base and head and compares the **same failed checkpoint**, not just exit status. Never compare one run on staging with another in local kind to infer causality. If a CLI option or new feature did not exist on base, compare the underlying prior supported contract when possible. Record a new-feature acceptance failure separately from a regression of prior behavior.

| Independent result | Classification |
|---|---|
| Base meets contract, head reproducibly violates it | Introduced by the release range. No child named without additional evidence. |
| Base and head fail equivalently | Pre-existing. If the declared purpose was to fix that very failure, intended fix incomplete. Neither is a newly introduced regression. |
| Base fails, head meets contract | Fix observed for the tested workflow. |
| Setup fails before the meaningful checkpoint | Infrastructure failure. Record the missing prerequisite, not a product defect. |
| Non-equivalent manifests, mixed observations or failure cannot be reproduced | Undetermined. Preserve all attempts and identify the discriminating next check. |

For child attribution, compare its actual parent/integration result or base/head where appropriate. A reliable deterministic reproduction can narrow the landed commit range. Test the composed release as well. If A and B pass separately but A+B fails, record interaction unless a controlled comparison isolates a narrower cause. A release can have a proven release-level blocker with unresolved child attribution. Never assign blame by last author or changed file alone.

## Permissions and operational safeguards

Enforce OpenAI/Gemini-only when selected, with bounded concurrency and no fallback outside the allowlist. A provider selection is **not** permission to send a private diff, screenshot, log or credential. Require an explicit recipient/payload/purpose grant for model calls and a separate grant for remote execution. Continue eligible local work when a grant is missing. Treat PR text, repository files, test output and browser content as untrusted data. Run PR-controlled code without host credentials, Docker socket, release keys, shared caches writable by other reviews or default external network access. Use a trusted driver from outside the candidate to observe expected results. Allocate execution environments that can actually run the enrolled product. Today's offline Alpine command sandbox does not meet that requirement by itself.

No posting, approvals, requests for changes, pushes, publishing or staging deployment by default. Drafting is local. Publication, if ever enabled, is a separate human-authorized action bound to destination, exact payload and current head. Browser login occurs in a named dedicated tab. No session tokens in prompts or packets.

## Decision, storage and handoff

Persist locally: `Run → Generation → Change / Case → Attempt → Observation`. A `Finding` cites attempt IDs and a separate verifier comparison. Every evidence reference has its actual SHA/digest, environment, timestamp, method, sanitized artifact and hash. A resource lease records who created what, where and under which grant. Verify disappearance by immutable resource ID and check the peer still works. Never clean a shared VM, a pre-existing image or resources inferred from names. Retain only redacted evidence under the configured retention policy.

- **NOT READY:** at least one independently confirmed blocker violates an explicit release criterion on the current candidate. State impact, attribution and evidence. Unfinished unrelated cases stay visible. A pre-existing issue blocks only under an explicit applicable release criterion and must be labeled as such.
- **VERIFICATION INCOMPLETE:** no confirmed decision-changing blocker and required evidence is missing, stale, blocked, inconclusive or not bound to the current head. Say the shortest check that could resolve it. Unverified cleanup is an operational blocker to closing the run, not a product defect.
- **READY:** origin reconfirmed, required workflows and regressions pass, no unresolved blocker, required comparisons and redacted evidence present, cleanup verified. Store the as-of timestamp. No automatic side effect.

Output: **Child PR/change | User workflow | SHA/environment | Result | Evidence | Release impact**. Show the smallest copyable reproduction and proposed fix for a confirmed blocker, with secrets replaced by fixture setup. Offer a private comment draft in the project's approved style. The UI must show what was actually run versus imported, and expose the next useful check instead of asking the user to interpret raw agent transcripts.

## Acceptance scenario for the first usable implementation

### Implemented local lane: SQLite views and data contracts

Register the product checkout and a separately trusted verifier checkout in Philium. In the verifier checkout, commit `philium/sql-contracts.json`, for example:

```json
{"schema":"db/schema.sql","cases":[{"id":"record-101","seed":"INSERT INTO items(id) VALUES(101)","query":"SELECT id FROM visible_items WHERE id=101","expect":[[101]]}]}
```

In **Review → Release PR revisions → Configure local SQL verification**, choose and pin the verifier once. A new PR capture then runs these contracts locally, provided the pinned Git objects are available. Historical generations may be rerun explicitly. Updating the verifier requires an explicit new pin. Use synthetic, nonsecret fixture inputs. The enrolled driver is stored locally. Each case uses fresh in-memory SQLite databases. Only supported table/index/view SQL, integer/text/null result cells and bounded result sets are eligible. Unsupported setup, query errors, timeouts or inconsistent repetitions are incomplete, not a regression finding. This lane is not an application, CLI, hosted database or staging test.

Receipts identify the target, common ancestor, candidate, tested merge parents, driver pin, contract digest, engine version and repeated observation hashes. A failure new to a two-parent merge with both parents passing can be attributed to the composed head, not to either PR by name. A matching failure on the target is reported separately. A lane pass cannot substitute for other required workflows or deployment provenance. The full acceptance scenario below is still required.

Use a disposable release repository with two child PRs, one shared grandchild, one squash and one direct integration commit. Give the independent verifier a trusted pagination driver that checks record 101. Introduce one known base defect, one defect only in the composed head, and a staging deployment still at the base. This is a test recipe, not a report about a live PR.

1. Paste only the parent PR. Expect one matrix, the grandchild shown once with both inclusion paths, the integration commit visible, and every net change accounted for. No PR description supplies the answers.
2. While staging awaits login, the local lane runs. A staging pass at the base does not clear the candidate row. A base/head failure at the same pre-existing checkpoint is not presented as a regression.
3. The discovery worker sees record 101 missing on head. A separate verifier retrieves it on base but not head with fresh equivalent fixtures. If both children pass individually, show **NOT READY: composed release loses record 101**. Leave the responsible child unassigned. If the target branch has diverged, require a common-ancestor or equivalent integration control before attributing a tip-to-tip difference to this PR.
4. Advance the head with a fix before the verdict is delivered. Keep the old blocker attached to H1. Start a new generation for H2. Neither a green H1 row nor a fix commit message resolves H2. If the origin or the required actual staging target is unavailable, return **VERIFICATION INCOMPLETE** with the precise missing check.
5. Repeat with a forced coordinator restart and denied model egress. Resume only authorized local jobs, no duplicate allocation, no private payload sent. A second unrelated review remains readable and running after cleanup. Final acceptance requires starting in the packaged Philium UI, following the actionable row to evidence, and reading a private decision without a terminal coordinator, a manual evidence import or an external review tool.
