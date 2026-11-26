# PR reviewer policy (contract)

Extends [pr-reviewer.md](pr-reviewer.md). Inspired by Claude Ship's approval policy, observe mode and review levels. Philium still runs the product; this only decides **how deep** to review and **what Philium may post**.

## Policy file

`.philium/review-policy.yml`, read with `git show <base_sha>:.philium/review-policy.yml` from the review's **base** commit, never from the PR head. If the PR changes the file, add a `risk` item "PR changes the review policy" (required) so a human sees it. The candidate never loosens its own rules.

```yaml
version: 1
mode: observe            # observe | comment | enforce   (default observe)
level: standard          # quick | standard | deep       (default standard)
budget_tokens: 2000000   # optional per-review cap; omitted = no cap
rules:                   # first match per changed file; strictest wins across files
  - purpose: "Gateway and auth need a person"
    paths: ["src-tauri/src/gateway/**", "src-tauri/src/pins.rs"]
    level: deep
    philium: comments    # approves | comments   (default approves)
    humans: 1            # people who must approve besides Philium (default 0)
  - purpose: "Docs are routine"
    paths: ["docs/**", "**/*.md"]
    level: quick
```

- Glob: `**` any depth, `*` within one segment, `?` one char. Implement in `pr_policy.rs`, no new crate.
- Missing file: defaults (`observe`, `standard`, `approves`, `humans: 0`, no cap). Invalid file: review still runs in `observe` with a `note` item "Policy file invalid: <error>" and `policy_error` set. Never fail open to `enforce`.
- Strictest across matched rules: level `deep > standard > quick`, `comments` beats `approves`, max `humans`. Files matching no rule use top-level `level`, `approves`, `humans: 0`.
- `version` other than 1 is invalid.

## Effective policy

`pr_policy::resolve(yaml: Option<&str>, changed: &[String]) -> Effective` (pure, unit tested):
```json
{"mode":"observe","level":"standard","philium":"approves","humans":0,"budget_tokens":null,
 "matched":[{"purpose":"","paths":["..."],"files":3}],"source":"base:<sha>|default|invalid","error":null}
```
Stored on `pr_reviews` as new columns `policy TEXT` (the JSON) and `mode TEXT`, `level TEXT`, plus `tokens_used INT DEFAULT 0`, `would_post TEXT`. Add via the existing migration style in `init`.

A per-repo override in kv `pr_review.mode:<repo_id>` (set from UI) wins over the file's `mode` **only toward safer**: observe < comment < enforce. The UI can lower, never raise above the file. With no file, the kv may set any mode (the user owns the repo setting).

## Mode at submit (`pr_review_submit`)

Compute `event` as today, then:
- `observe`: post nothing. `state='observed'`, `would_post` = the event, body saved in `summary`. Emit/announce as today. Return `{"observed": true, "would_post": event}`.
- `comment`: always post `COMMENT` (findings and inline still included).
- `enforce`: `APPROVE` only if `philium == approves` **and** `humans == 0`; otherwise `COMMENT` with a line "Philium found no blockers. Policy needs N human approval(s): <purposes>." `REQUEST_CHANGES` stays as today. Own PR stays `COMMENT`.
- Body gets a "Policy" line: mode, level, source, matched purposes.

`observed` is a terminal state like `posted` for the watcher (rerun on moved head, no auto restart otherwise). Add an RPC `publish {id}` that posts a stored observed review **only if** the PR head still equals the review head (else supersede/restart as submit does), using the mode `comment` or `enforce` rules above per the user's choice `{id, as: "comment"|"enforce"}`; `enforce` still bounded by the file.

## Level

Brief changes (`brief`):
- `quick`: read-only. One orchestrator, no helpers (`agent_spawn` not used), basis `source_trace` acceptable, run only CI already reported plus unit tests touched by the diff. There is no independent verifier on a quick review: a result that would need verification (`fail`, `not_addressed`, or dismissing a person's comment as `not_applicable`/`outdated`) is recorded as `blocked` with the finding in its evidence, so a required one puts the review in `needs_you` for the person.
- `standard`: today's brief.
- `deep`: up to 6 helpers, every risk item must have `basis` `runtime` or `runtime_and_source` to `pass` (enforce in `pr_item_result`: a `pass` with `source_trace` on a deep review is rejected with a teaching message).

## Budget

`budget_tokens`: sum the review agent tree's tokens (orchestrator + descendants) from whatever Philium already records per agent; find it, do not add a second meter. Check in the watcher (every cycle, even while the window is hidden), in `pr_item_result` and at the start of `pr_review_submit`. Review membership is persisted as agents are discovered, so a deleted helper's spend still counts and `tokens_used` never decreases. When exceeded: stop the agent tree, `state='needs_you'`, add a blocked `note` item "Token budget N reached at M". Never silently stop.

## RPC additions
- `get`/`list` rows include `mode`, `level`, `policy`, `tokens_used`, `would_post`.
- `policy_get {repo_id}` -> effective policy for the repo default branch with no changed files plus raw file text or null, and kv override.
- `settings_set {auto, repo_id?, mode?}` -> sets `pr_review.mode:<repo_id>`.
- `publish {id, as}` above.

## UI (`pr-review.jsx`, helpers in `src/lib/pr-review.js`)
- Mode selector per repo (Observe / Comment / Enforce) with the file's ceiling shown; Enforce disabled above the ceiling.
- Policy panel: source (base sha / default / invalid + error), level, matched rules with purpose and file counts, humans required.
- Review row: mode + level chips, token use vs budget, and for `observed`: "Would post: Approve|Request changes|Comment" with **Publish as comment** and **Publish** buttons.
- Fixture data for the render harness covers observed, enforce-with-humans and invalid policy.
- No hardcoded versions or model IDs (`pins.rs` rule).
