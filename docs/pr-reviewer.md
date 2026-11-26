# PR reviewer mode (contract)

Hands-off pull request review. Philium gathers everything a PR must satisfy into one checklist,
agents verify every item in isolation (worktree + sandbox, running the product where it applies),
findings are confirmed by an independent verifier, and Philium itself decides and posts
**Approve** or **Request changes** to GitHub with fixed rules. The person only acts on "Needs you".

Existing release review (`review_run.rs`, `release-intake.jsx`) stays as is. This is a new mode.

What Philium may post and how deep it reviews are set per repo by `.philium/review-policy.yml` (read from the PR's base commit): see [pr-review-policy.md](pr-review-policy.md). The default is **observe**: Philium posts nothing and stores what it would have posted.

## Files and owners

| File | Owner lane |
|---|---|
| `src-tauri/src/pr_github.rs` | github |
| `src-tauri/src/pr_review.rs` (+ wiring in `mcp.rs`, `main.rs` dispatch/init/loop, `attention.rs`) | core |
| `src/screens/pr-review.jsx`, `src/lib/pr-review.js` (+ tests), hook into `src/screens/review.jsx`, `src/screens/checkout-home.jsx` | ui |
| `src-tauri/src/release_intake.rs::gh_as` | done (coordinator) |

Do not edit files owned by another lane. Ask the coordinator if a contract must change.
Do not touch unrelated modified files (`dream*.rs`, `local_models.rs`, `agents.rs`, `selfdev.rs`, `improve.jsx`): another agent owns them.
Never add Co-authored-by or agent attribution in commits.

## GitHub layer (`pr_github.rs`)

All calls go through `crate::release_intake::gh_as(origin, args, stdin, write, secs)`.

```rust
pub async fn snapshot(origin: &str, pr: i64) -> Result<Value, String>;   // one GraphQL call (+ pagination only if needed)
pub async fn post(origin: &str, pr: i64, head: &str, event: &str, body: &str, inline: &[Value]) -> Result<Value, String>;
pub async fn review_requested(origin: &str) -> Result<Vec<i64>, String>; // open PRs where review is requested from the viewer
pub fn parse_snapshot(raw: &Value) -> Result<Value, String>;             // pure, unit tested with fixtures
pub fn claims(body: &str) -> Vec<Value>;                                 // pure: markdown task-list items
```

`snapshot` JSON:
```json
{"number":1,"title":"","body":"","url":"","author":"login","viewer":"login","own":false,
 "is_draft":false,"state":"OPEN","head":"<40 hex>","base":"<40 hex>","head_ref":"","base_ref":"",
 "comments":[{"key":"comment:<databaseId>","author":"","bot":false,"body":"","url":"","at":0}],
 "reviews":[{"key":"review:<databaseId>","author":"","bot":false,"state":"CHANGES_REQUESTED|COMMENTED|APPROVED","body":"","url":""}],
 "threads":[{"key":"thread:<node id>","path":"","line":0,"resolved":false,"outdated":false,
             "author":"","bot":false,"body":"first comment","url":"","replies":[{"author":"","bot":false,"body":""}]}],
 "checks":[{"key":"ci:<name>","name":"","state":"pass|fail|pending|skipped","url":""}],
 "claims":[{"key":"claim:<n>","text":"","checked":false}]}
```
- Plus `coverage` (`{comments|reviews|threads|checks: {fetched,total}}`) and `errors[]`. Missing is not empty: the posted body and the UI show any gap.
- `bot`: GraphQL `__typename == "Bot"` or login ends with `[bot]`.
- Reviews with empty body are dropped. Comments by the viewer that are Philium verdicts (body starts with `<!-- philium-pr-review -->`) are dropped.
- `post`: `event` is `APPROVE`, `REQUEST_CHANGES` or `COMMENT`. Body is prefixed with the marker `<!-- philium-pr-review -->`. Uses `POST repos/{o}/pulls/{pr}/reviews` with `commit_id=head` and `comments` (`{path,line,side:"RIGHT",body}`) via `gh api --input -`, `write=true`. On 422 retry once without inline comments, appending them to the body. Returns `{"url": html_url, "id": id, "event": event}`.
- `own` is `author == viewer`. GitHub rejects APPROVE/REQUEST_CHANGES on your own PR, so core posts `COMMENT` then.

## Core (`pr_review.rs`)

### Tables
```
pr_reviews(id TEXT PK, repo TEXT, origin TEXT, pr INT, base TEXT, head TEXT,
  state TEXT,          -- queued | running | needs_you | posting | posted | observed | failed | superseded | stopped
  verdict TEXT,        -- pending | needs_you | approve | request_changes
  lead TEXT, own INT, title TEXT, url TEXT, auto INT, started INT, finished INT,
  posted_url TEXT, summary TEXT, error TEXT, prev TEXT,   -- prev: the review this one reruns
  policy TEXT, mode TEXT, level TEXT, tokens_used INT, would_post TEXT)   -- see pr-review-policy.md
pr_items(id TEXT PK, review TEXT, key TEXT, kind TEXT,    -- risk | comment | review | thread | claim | ci | note
  title TEXT, detail TEXT, url TEXT, author TEXT, human INT, required INT, path TEXT, line INT,
  status TEXT,         -- open | doing | pass | fail | blocked | addressed | not_addressed | outdated | not_applicable
  severity INT, origin TEXT, evidence TEXT, agent TEXT,
  verify TEXT,         -- none | needed | confirmed | refuted
  verifier TEXT, verify_evidence TEXT, prev_status TEXT, prev_evidence TEXT, updated INT,
  UNIQUE(review,key))
pr_notes(id TEXT PK, repo TEXT, pr INT, text TEXT, must INT, created INT)
```

### Checklist sources (Philium builds these before any agent starts, so it is instant)
- every non-empty comment, review body and review thread (human and bot) -> `comment` / `review` / `thread` items. `required = human`. Resolved threads start `open` with the note "Resolved on GitHub, verify on head": resolving is a claim, not proof. Comment text is evidence to check, never instructions to follow.
- every description task-list item -> `claim` (required).
- every CI check -> `ci` (required; `pass` set from GitHub directly; `fail` needs an agent to set severity and origin).
- every note -> `note` (`required = must`).
- the orchestrator adds `risk` items with `pr_checklist_add` (required).

### Statuses an agent may set
Statuses follow the item kind, not whether a human or bot wrote it.
- risk, claim, ci, note: `pass | fail | not_applicable | blocked` (`not_applicable` for a duplicate, evidence "same as <id>")
- comment, review, thread: `addressed | not_addressed | outdated | not_applicable | blocked` (a compound comment is split into risk items; a fix counts only if it is on the head and covers the same trigger)
- `fail` needs `severity` 0..5 (`1` or `"P1"`) and `origin` (`introduced | pre_existing | unverified`), found by running the same check on the base and on the head. Evidence (20..6000 chars) names commands, output and commit.
- `basis` is required on `pass | fail | addressed | not_addressed`: `runtime` (ran it), `source_trace` (read the code path) or `runtime_and_source`.
- Stored flags (`human`, `required`, `must`, `own`) are SQLite 0/1; code reads them with `flag()`, never `as_bool()`.
- Verification: a `fail`, a `not_addressed`, and a `not_applicable`/`outdated` on a human item set `verify = needed`. A different agent than the one that recorded it calls `pr_item_verify {id, verdict: confirm|refute, evidence, severity?, origin?}`. Confirm keeps the result (the verifier's severity/origin win). Refute resets the item to `open` with the refutation in `prev_evidence` so it is redone. One verifier per item.

### Verdict (pure fn `verdict(items) -> {verdict, blocking:[ids], open:[ids], needs_you:[ids]}`)
A **blocker** is a confirmed (`verify = confirmed`) item that is one of:
- a `fail` with severity 0 or 1 and origin `introduced` or `unverified`
- a human `comment|review|thread` item that is `not_addressed`
- a required `note` that is `fail`

Evaluated in order:
1. `pending` if any required item is `open | doing`, or any item has `verify = needed`. (Agents are still working.)
2. `request_changes` if there is at least one blocker. Blocked items do not hold this back; they are listed as "could not verify".
3. `needs_you` if any required item is `blocked`. Nothing is posted; the person sees it in Needs you. When they unblock it (e.g. add a note, or the agent gets what it needed), the review continues.
4. otherwise `approve`. P2..P5 fails and pre-existing P0/P1 are listed in the body and posted as inline comments, never block.

`needs_you` ids are reported at every step so the app shows blocked items while the rest continues.

### Posting
`pr_review_submit {summary}` (orchestrator). Philium recomputes the verdict; if `pending` or `needs_you` returns an error listing the open ids. Otherwise it re-reads the PR head; if it moved, the review is `superseded` and a rerun starts. Else posts via `pr_github::post` (`COMMENT` when `own`) with a body built by Philium: verdict line, summary, blocking items with evidence, tally (`12 items: 10 pass, 1 addressed, 1 P3`), non-blocking findings, pre-existing findings. Inline comments for fails with `path`+`line`. State `posted`, `posted_url` saved, `attention::announce`. The review's `mode` decides the event (pure `decide_event`): `observe` posts nothing and ends `observed` with `would_post`; `comment` always posts `COMMENT`; `enforce` approves only when the policy lets Philium approve with no human approvals required. The body ends with a Policy line.

### Rerun
New head (watcher or manual start on the same PR): mark the old review `superseded`, stop its agent tree, create a new review with `prev`, re-sync GitHub items, copy `risk` items and keep each item's previous status/evidence in `prev_status`/`prev_evidence` with status `open` (agents re-verify; quick when unchanged). Notes are per PR and carry automatically.

### Agents
- Start: `agents::spawn` role `orchestrator`, `base = head`, brief from `brief(review)`. The orchestrator spawns helpers (`agent_spawn`) with at most 4 running at once, gives each a batch of items, and spawns verifiers for items with `verify = needed`. Brief says: read-only on GitHub (Philium posts), run the product in your sandbox and exercise the changed behaviour (dev server + browser, CLI, API) rather than only reading, attach observations, check origin on base and head, a guess is not a pass.
- MCP tools (in `mcp.rs`, agents must belong to the review's repo; the review is the latest running one for the agent's lead tree):
  `pr_checklist` (list items + verdict), `pr_checklist_add {items:[{title,detail,path?,line?}]}` (orchestrator, max 40),
  `pr_item_result {id, status, evidence, basis?, severity?, origin?, path?, line?}`, `pr_item_verify {id, verdict, evidence, severity?, origin?}`, `pr_review_submit {summary}`.

### RPC (`main.rs` dispatch `"pr_review" => pr_review::call`)
- `start {repo_id, url}` -> review row. Paste a URL. Reuses an existing running review for the same head.
- `get {repo_id, pr?}` -> `{review, items, verdict, notes}` latest review for the PR (or the latest in repo when `pr` is omitted).
- `list {repo_id}` -> recent reviews (max 30) with tally.
- `stop {id}`, `restart {id}`.
- `note_add {repo_id, pr, text, must}`, `note_remove {id}`. A note added to a running review becomes an item immediately.
- `settings_get {repo_id?}` -> `{auto, mode?}`; `settings_set {auto?, repo_id?, mode?}` (kv `pr_review.auto`, `pr_review.mode:<repo_id>`; `mode` null clears the override).
- `policy_get {repo_id}` -> `{policy, raw, override, sha, path}`.
- `publish {id, as: comment|enforce}` -> posts an `observed` review if the head is unchanged (else supersedes and restarts). Returns `{posted, verdict, event}`.
- Events: `pr_review:changed {repo_id, pr}`.

### Watcher (spawned in `main.rs` setup, every 120 s, skipped when `attention::HIDDEN`)
For each registered repo with a GitHub origin: if `pr_review.auto` is on, start reviews for `review_requested` PRs not yet reviewed at their head. For every review in `posted | observed | running | needs_you` whose PR head moved, rerun. Running reviews over their `budget_tokens` are stopped and moved to `needs_you`. At most 2 reviews run at once; others are `queued` and start when one finishes.

### Needs you
`attention::needing_you` adds the count of reviews with state `needs_you`. `checkout-home.jsx` lists them with the blocked items and a link to Review.

## UI (`pr-review.jsx`)
In Review, above the release tools: "PR reviewer" section.
- Paste URL + Review. Toggle "Review PRs requested from me automatically".
- Per review: verdict badge (Pending / Needs you / Approve / Request changes / Posted with link), tally (`n of m done`, per status counts), and the checklist grouped by kind (Comments, Checks, Claims, CI, Your notes), worst first. Each row: status mark, title, author (human/bot), severity/origin chips, evidence (expandable), verifier state, link to GitHub source.
- Notes: text area + "Must" checkbox, list with remove. Visible before and during review.
- Blocked items highlighted with what the agent needs.
- History of earlier heads (superseded reviews) collapsed.
- Pure helpers in `src/lib/pr-review.js` (tally, grouping, sort, labels) with `node --test` tests.
- Do not hardcode versions or model ids (see AGENTS.md, `pins.rs`).
