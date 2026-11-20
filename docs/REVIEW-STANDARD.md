# Review standard

One set of words, levels and lists for every pull request review, whoever does it: Philium's
Review, a Jcode agent or a person. Version 2, agreed between Philium and Jcode on 2026-11-20.
Philium's Review and Jcode's `review-pr` skill and gate both follow it.

## 1. Read everything, and remember where you stopped

Read these before planning, and again before any verdict:

1. Every comment and review on the pull request.
2. Every pull request into the same base branch (`gh pr list --base <branch>`), including fix
   pull requests that point back to this one.
3. Findings issues from the previous release on the same line.
4. Issues that mention the pull request or the release.
5. CI results.

For each source, record the last comment read and its time (a read cursor). A verdict is stale
as soon as the head moves or any source has something newer than its cursor.

Everything people report goes in:

- Every bug a person reports goes into the bug list, before it is checked.
- A bug count without a list ("22 P2s") becomes one placeholder row per count, until the real
  rows replace it.
- Every test plan becomes check rows, one per case, keeping the plan's IDs and priorities. A
  plan is worked through, not sampled.

## 2. Required sweeps

A test plan can miss things, so every review also runs these, each with a named owner:

1. **Role exposure:** call each new or changed endpoint as each role, and check the response for
   secrets and for fields that role should not see.
2. **Fresh install.**
3. **Upgrade from the last release.**
4. **Mixed versions** while old and new replicas or workers run side by side.
5. **Base parity:** every failure is also run on the base (see section 5).

Every worker gets both lists, not only its slice of the diff.

## 3. Two lists

The **check list** is what to try. The **bug list** is what is wrong. A failed check opens a bug,
and every ❌ links to one. A bug can also exist without a check.

Both lists are append-only: rows are added, never deleted. Dropping a check needs a reason and a
name.

**IDs.** Our own: `<pr>-C01` for checks and `<pr>-B01` for bugs. A reporter's own IDs are kept
and prefixed with the source: `jort:B01`, `debarshi:UPG-01`, `#5571:F03`. Nothing is
renumbered. A bug belongs to the list of the pull request being shipped, even when another
pull request fixes it.

Check row: ID · check (what to do, what should happen) · priority P0–P5 · result · head ·
run by · environment · evidence · artifacts.

Bug row: ID · bug (one line, where) · severity P0–P5 · the reporter's own level, if different ·
origin · found by, with link · status · evidence.

**Statuses:** open · fixed in `<sha>` · verified fixed on `<head>` · deferred to `<issue>`
(who decided) · won't fix (who decided) · not a bug (why).

- Only rerunning the check that found the bug, on this head, makes it verified fixed. A fix that
  still lists caveats is "fixed", and each caveat becomes its own check row.
- Deferral needs an issue and a named owner. A deferred bug is listed in the verdict but does
  not block.

## 4. Severity

| Level | Meaning | Blocks the release |
|---|---|---|
| P0 | Data loss, a security hole or exposed secret, or the release does not install, upgrade or start | Yes |
| P1 | A main feature is broken with no workaround, or install/upgrade needs an undocumented manual step | Yes |
| P2 | Broken with a workaround, or a secondary feature broken, or install/upgrade needs a documented manual step | Only without a decision |
| P3 | Wrong in edge cases, poor error handling | No |
| P4 | Tooling, lint, docs, small UI defects | No |
| P5 | Cosmetic or wording | No |

Every open P2 needs a recorded decision: fix before release, or ship as a known issue linked to
an issue. The owner can make any P2 blocking. When we regrade a reporter's level, say why.

## 5. Origin

Run the same check, set up the same way, on the base.

- **introduced**: fails on the head, passes on the base.
- **pre-existing**: fails the same way on the base.
- **origin unverified**: the base could not be checked, or fails differently.

## 6. Evidence

- Every result records the exact head, who ran it, where, and when (with time zone).
- A P0 or P1 row needs a real workflow and a saved artifact whose own content shows the claim,
  recorded with its SHA-256. Unit tests support a check but never settle a P0 or P1 row.
- Before any verdict, a second reader spot-checks every P0 and P1 artifact against its
  observation.
- A result someone else posted counts as evidence for their head only.
- **Blocked** means the owner's input or decision is needed, and says exactly what. A missing
  setup is not blocked: the reviewer builds it and runs the check.

## 7. The verdict

A tool computes it from the lists. A person or agent never writes it.

- **Stale**: the head moved, or a source has something past its read cursor. Read again first.
- **Do not ship**: an open P0 or P1 bug that is introduced or origin unverified.
- **Not ready**: any of
  - an open pre-existing P0 or P1 that is not deferred;
  - a P0 or P1 check that is not ✅ on this head;
  - a P0 or P1 placeholder row;
  - an open P2 without a decision;
  - a P0 or P1 artifact not yet spot-checked.
- **Ready**: none of the above. Ready is not approved. Approval is a separate step the owner
  authorizes.

"Open" means any status other than verified fixed, deferred, won't fix or not a bug.

## 8. A new head

- The bug list carries over. Open bugs are checked again; fixed ones become verified fixed on
  this head, or reopen.
- P0 and P1 checks, sweeps, and every check whose code changed are rerun. A check with no
  recorded code area counts as changed.
- Other checks show "✅ on `<old sha>`" in their own column, never counted for this head.

## 9. The report

1. The verdict in one line, with the head and when each source was last read.
2. The bug list as one copyable table, P0 first.
3. The check list as one copyable table: ✅ pass, ❌ fail (with its bug ID), ⬜ not run or
   blocked (with what is needed).
4. What was not checked, and why.

The summary posted on the pull request shows every P0 and P1 row, counts for the rest and where
the full list is. A GitHub comment holds at most 65,536 characters, and an unfixed leak is never
described there in detail.

## 10. Where the lists live

One folder per pull request: `~/.jcode/review-evidence/<repo>-pr<number>/`, holding
`ledger.jsonl`, `ledger.lock` and one packet folder per head under `heads/<sha>/`.

`ledger.jsonl` is the single source of truth. Each tool keeps its own view of it: Philium in
its database, live; Jcode through its gate; the pull request through the generated summary.

**Writing.** Take an exclusive lock on `ledger.lock` (`flock`), read the last `seq`, append one
line, flush, release. Never rewrite or delete a line. A reader ignores an unfinished last line.

**Each line** is one JSON object:

```json
{"v": 1, "seq": 42, "at": "2026-11-20T09:41:07+01:00", "by": "heron", "tool": "philium", "type": "bug_update", "id": "5542-B03", "status": "fixed", "sha": "35278fa1ef", "note": "fixed in #5587"}
```

| type | fields |
|---|---|
| `pr` | `repo`, `number`, `base` (always the first line) |
| `source` | `source` (short name), `url`, `kind`: `thread`, `pr`, `issue` or `ci` |
| `cursor` | `source`, `last_id`, `last_at` |
| `head` | `sha` |
| `check_add` | `id`, `title`, `priority`, `from` (plan or source), `sweep`, `area` (paths), `owner` |
| `check_result` | `id`, `head`, `result`: `pass`, `fail`, `not_run` or `blocked`; `run_by`, `environment`, `evidence`, `artifacts` (`path`, `sha256`), `bug`, `needs` |
| `check_drop` | `id`, `reason` |
| `bug_add` | `id`, `title`, `severity`, `reported_severity`, `origin`, `found_by`, `link`, `evidence`; a placeholder has `placeholder: {"count": 22}` |
| `bug_update` | `id` and any of `status`, `sha`, `head`, `issue`, `decided_by`, `decision` (`fix_before_release` or `known_issue`), `severity`, `origin`, `reason`, `note` |
| `spot_check` | `id`, `head`, `agrees`, `note` |

The current tables are the log replayed in `seq` order, the later line winning field by field.
Both tools are tested against the same sample log, so they build the same tables and the same
verdict.
