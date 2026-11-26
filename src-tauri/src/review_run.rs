//! A release review led by an orchestrator. The person pastes a pull request; an orchestrator
//! writes a plan of checks, riskiest first, helpers do the checks and record pass, fail or
//! blocked with what they ran, and the orchestrator writes a report. Every failure is a finding
//! with a severity, P0 to P5, and an origin: whether this release caused it.
//!
//! Everything recorded here is an agent's word. It is shown to the person, grouped and counted,
//! and the person signs off. It never replaces what Philium checks itself in the same screen.
use crate::{board, ctx::Ctx, db::now};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::Arc;

/// How bad a finding is. P0 and P1 block the release; P2 to P5 should be looked at, in
/// decreasing importance. Used in the brief and shown beside each finding.
pub const SEVERITY: [(&str, &str); 6] = [
    ("P0", "blocks the release: data loss, a security hole, or the release does not install or upgrade"),
    ("P1", "blocks the release: a main behaviour is broken"),
    ("P2", "look at soon: a real problem with a workaround"),
    ("P3", "look at: an edge case or error handling"),
    ("P4", "minor"),
    ("P5", "cosmetic"),
];
const RESULTS: [&str; 3] = ["pass", "fail", "blocked"];
/// Where a failure comes from, decided by running the same check on the target as well.
const ORIGINS: [&str; 3] = ["introduced", "pre_existing", "unverified"];
const ORIGIN_RULE: &str = "introduced (fails on the head, passes on the target), pre_existing (fails the same way on the target) or unverified (the target could not be checked, or fails differently)";

/// A finding's origin in the words an agent may use for it.
fn origin_of(raw: &str) -> Option<&'static str> {
    let w = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match w.as_str() {
        "introduced" => Some("introduced"),
        "pre_existing" | "preexisting" => Some("pre_existing"),
        "unverified" | "origin_unverified" => Some("unverified"),
        _ => None,
    }
}

fn run_key(repo: &str) -> String {
    format!("review.run.{repo}")
}
/// The review in progress for a checkout: which pull request it is about.
pub fn current(ctx: &Ctx, repo: &str) -> Option<Value> {
    ctx.db.kv_get(&run_key(repo)).and_then(|s| serde_json::from_str(&s).ok())
}
/// Started by the person from Review. A new run for another pull request replaces the old one;
/// the old items stay on the board under their own pull request. Starting the same revision
/// again replaces its plan: the earlier run's unchecked items are cancelled, and what it did
/// check stays on the board, outside the new list.
pub fn start(ctx: &Ctx, repo: &str, pr: i64, generation: i64) -> Result<(), String> {
    ctx.db.exec(
        "UPDATE tasks SET status='cancelled', updated_at=?3 WHERE repo_id=?1 AND review_gen=?2 AND review_result IS NULL AND status!='cancelled'",
        params![repo, generation, now()],
    )?;
    ctx.db.kv_set(&run_key(repo), &json!({"pr": pr, "generation": generation, "started": now()}).to_string())?;
    ctx.emit("board:changed", json!({ "repo_id": repo }));
    ctx.emit("release:review", json!({ "repo_id": repo }));
    Ok(())
}

/// On start, bring the board in line with the reviews: an item with a pass or a fail is finished,
/// and an unchecked item left from an earlier run of the review now in progress is cancelled.
/// Earlier versions left both open.
pub fn tidy(ctx: &Ctx) -> Result<(), String> {
    ctx.db.exec("DELETE FROM leases WHERE kind='task' AND key IN (SELECT id FROM tasks WHERE review_pr IS NOT NULL AND review_result IN ('pass','fail') AND status NOT IN ('done','cancelled'))", [])?;
    ctx.db.exec("UPDATE tasks SET status='done',lease_expires_at=NULL WHERE review_pr IS NOT NULL AND review_result IN ('pass','fail') AND status NOT IN ('done','cancelled')", [])?;
    for r in ctx.db.query("SELECT substr(k,12) AS repo, v FROM kv WHERE k LIKE 'review.run.%'", [])? {
        let (Some(repo), Some(run)) = (r["repo"].as_str(), r["v"].as_str().and_then(|v| serde_json::from_str::<Value>(v).ok())) else { continue };
        let (Some(gen), Some(started)) = (run["generation"].as_i64(), run["started"].as_i64()) else { continue };
        ctx.db.exec(
            "UPDATE tasks SET status='cancelled' WHERE repo_id=?1 AND review_gen=?2 AND review_result IS NULL AND created_at<?3 AND status!='cancelled'",
            params![repo, gen, started],
        )?;
    }
    Ok(())
}

fn repo_of(row: &Value) -> Result<&str, String> {
    row["repo_id"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| "A release review belongs to a registered checkout".to_string())
}

/// The orchestrator's plan: every item worth verifying, each in a tier, as tasks on the board.
pub fn plan(ctx: &Arc<Ctx>, me: &str, row: &Value, items: &Value) -> Result<Value, String> {
    let repo = repo_of(row)?;
    let run = current(ctx, repo).ok_or("No release review is in progress for this checkout. The person starts one in Review.")?;
    if row["role"].as_str() != Some("orchestrator") {
        return Err("Only the orchestrator leading the review writes its plan".into());
    }
    let pr = run["pr"].as_i64().ok_or("The review has no pull request")?;
    let gen = run["generation"].as_i64().ok_or("The review has no revision")?;
    let list = items.as_array().filter(|a| (1..=40).contains(&a.len())).ok_or("Send items: 1 to 40 checks, riskiest first, each {title, detail}")?;
    let existing = ctx.db.query("SELECT count(*) AS n FROM tasks WHERE repo_id=?1 AND review_gen=?2 AND status!='cancelled'", params![repo, gen])?;
    if existing.first().and_then(|r| r["n"].as_i64()).unwrap_or(0) + list.len() as i64 > 60 {
        return Err("A review holds at most 60 items".into());
    }
    // Check everything before creating anything, so a bad entry does not leave half a plan.
    let mut checked = Vec::new();
    for (i, it) in list.iter().enumerate() {
        let title = it["title"].as_str().map(str::trim).filter(|t| !t.is_empty() && t.len() <= 160).ok_or_else(|| format!("Item {}: title is required, at most 160 characters", i + 1))?;
        let detail = it["detail"].as_str().map(str::trim).filter(|d| !d.is_empty()).ok_or_else(|| format!("Item {}: detail says what to verify and how", i + 1))?;
        checked.push((title, detail));
    }
    let mut out = Vec::new();
    for (title, detail) in checked {
        let task = board::create(ctx, me, title, detail, Some("high"), Some(repo), &[], None)?;
        let id = task["id"].as_str().ok_or("Task not created")?;
        ctx.db.exec("UPDATE tasks SET review_pr=?1, review_gen=?2 WHERE id=?3", params![pr, gen, id])?;
        out.push(json!({"id": id, "title": title}));
    }
    ctx.emit("release:review", json!({ "repo_id": repo }));
    Ok(json!({"items": out, "next": "Hand each check to a helper with agent_spawn {task_id, brief}, in this order, at most two helpers at a time. Each helper records its outcome with review_result."}))
}

/// One item's outcome, with what was run and seen. Recorded by the agent that did the item or by
/// the review's orchestrator.
pub fn result(ctx: &Arc<Ctx>, agent_id: &str, me: &str, row: &Value, task_id: &str, outcome: &str, severity: Option<i64>, origin: Option<&str>, evidence: &str) -> Result<Value, String> {
    let repo = repo_of(row)?;
    let outcome = outcome.trim().to_ascii_lowercase();
    if !RESULTS.contains(&outcome.as_str()) {
        return Err("result is pass, fail or blocked".into());
    }
    // Every finding says whether this release caused it. Only a failure is a finding.
    let origin = match (outcome.as_str(), origin.filter(|o| !o.trim().is_empty())) {
        ("fail", None) => return Err(format!("A failure needs its origin. Run the same check on the target too, then send origin: {ORIGIN_RULE}.")),
        ("fail", Some(o)) => Some(origin_of(o).ok_or_else(|| format!("origin is {ORIGIN_RULE}"))?),
        _ => None,
    };
    let severity = match (outcome.as_str(), severity) {
        ("fail", Some(s)) if (0..=5).contains(&s) => Some(s),
        ("fail", _) => return Err("A failure needs its severity, 0 to 5: 0 or 1 block the release (0: data loss, security, does not install or upgrade; 1: a main behaviour is broken), 2 to 5 should be looked at, in decreasing importance.".into()),
        _ => None,
    };
    debug_assert!(origin.map_or(true, |o| ORIGINS.contains(&o)));
    let evidence = evidence.trim();
    if evidence.len() < 20 || evidence.len() > 6000 {
        return Err("evidence says what you ran and what it showed, on which commit: 20 to 6000 characters".into());
    }
    let item = ctx.db.query("SELECT id,owner_agent_id,assigned_agent_id FROM tasks WHERE id=?1 AND repo_id=?2 AND review_pr IS NOT NULL", params![task_id, repo])?.pop().ok_or("That is not an item of this checkout's release review")?;
    let holds = item["owner_agent_id"].as_str() == Some(agent_id) || item["assigned_agent_id"].as_str() == Some(agent_id);
    if !holds && row["role"].as_str() != Some("orchestrator") {
        return Err("Record the outcome of the item you were given, or let the orchestrator record it".into());
    }
    ctx.db.exec("UPDATE tasks SET review_result=?1, review_evidence=?2, review_origin=?3, review_severity=?4, updated_at=?5 WHERE id=?6", params![outcome, format!("{me}: {evidence}"), origin, severity, now(), task_id])?;
    // A pass or a fail finishes the item on the board. A blocked item is handed in when this agent
    // holds it, so it waits in review for the person.
    let handed_in = if outcome == "blocked" {
        board::submit(ctx, agent_id, task_id, &format!("BLOCKED: {evidence}"), None).is_ok()
    } else {
        board::close_checked(ctx, agent_id, task_id, &outcome).is_ok()
    };
    ctx.emit("release:review", json!({ "repo_id": repo }));
    Ok(json!({"recorded": outcome, "severity": severity.map(|s| format!("P{s}")), "origin": origin, "handed_in": handed_in}))
}

/// The orchestrator's written report for the review in progress.
pub fn report(ctx: &Ctx, me: &str, row: &Value, text: &str) -> Result<Value, String> {
    let repo = repo_of(row)?;
    let run = current(ctx, repo).ok_or("No release review is in progress for this checkout")?;
    let text = text.trim();
    if text.len() < 80 || text.len() > 40_000 {
        return Err("The report is Markdown, 80 to 40000 characters: verdict first, then the items, what failed, what could not be checked".into());
    }
    let gen = run["generation"].as_i64().unwrap_or(0);
    ctx.db.kv_set(&format!("review.report.{repo}.{gen}"), &json!({"by": me, "at": now(), "text": text}).to_string())?;
    ctx.emit("release:review", json!({ "repo_id": repo }));
    crate::attention::announce(ctx);
    Ok(json!({"saved": true, "next": "Tell the person the verdict in two sentences. They read the report in Review."}))
}

/// The verdict the checks add up to. A P0 or P1 finding this release introduced, or whose origin
/// is unverified, means do not ship. A pre-existing P0 or P1, or any check that is blocked or
/// not done, leaves the release not ready: an unchecked item can hide a blocker. P2 to P5
/// findings inform.
pub fn headline(items: &[Value]) -> &'static str {
    let blocker = |i: &&Value| i["result"] == "fail" && i["severity"].as_i64().map_or(true, |s| s <= 1);
    if items.is_empty() {
        "none"
    } else if items.iter().filter(blocker).any(|i| i["origin"] != "pre_existing") {
        "do_not_ship"
    } else if items.iter().any(|i| blocker(&i) || (i["result"] != "pass" && i["result"] != "fail")) {
        "not_ready"
    } else {
        "passed"
    }
}

/// What Review shows for one revision of a pull request: the items in tier order, the verdict
/// they add up to and the report, if any. A new head is a new revision and starts empty, and so
/// does starting the review of the same revision again.
pub fn items(ctx: &Ctx, repo: &str, pr: i64, gen: i64) -> Result<Value, String> {
    let run = current(ctx, repo).filter(|r| r["generation"].as_i64() == Some(gen)).unwrap_or(Value::Null);
    let since = run["started"].as_i64().unwrap_or(0);
    let rows = ctx.db.query(
        "SELECT t.id,t.title,t.status,t.review_result AS result,t.review_origin AS origin,t.review_evidence AS evidence,COALESCE(a.name,t.assigned,t.owner) AS owner, \
         CASE WHEN t.review_result='fail' THEN COALESCE(t.review_severity,t.review_tier) END AS severity \
         FROM tasks t LEFT JOIN agents a ON a.id=COALESCE(t.owner_agent_id,t.assigned_agent_id) \
         WHERE t.repo_id=?1 AND t.review_gen=?2 AND t.status!='cancelled' AND t.created_at>=?3 ORDER BY t.created_at",
        params![repo, gen, since],
    )?;
    let report = ctx.db.kv_get(&format!("review.report.{repo}.{gen}")).and_then(|s| serde_json::from_str::<Value>(&s).ok()).filter(|r| r["at"].as_i64().unwrap_or(0) >= since).unwrap_or(Value::Null);
    Ok(json!({"pr": pr, "headline": headline(&rows), "items": rows, "report": report, "run": run,
        "severity": SEVERITY.iter().map(|(t, d)| json!({"level": t, "means": d})).collect::<Vec<_>>()}))
}

/// What the orchestrator is told. `facts` is what Philium already established about the release.
pub fn brief(pr: i64, base: &str, head: &str, facts: &str) -> String {
    let severity = SEVERITY.iter().map(|(t, d)| format!("   {t}: {d}")).collect::<Vec<_>>().join("\n");
    format!(
        "You are leading the review of release pull request #{pr} for this checkout. The person pasted it and expects you to organise the whole review. Do not hand work back to them.\n\n\
         The release: target {base}, head {head}. {facts}\n\
         See it with `git log --oneline {base}..{head}` and `git diff --stat {base}...{head}`.\n\n\
         Rules for you and every helper: this is a read-only review. Do not push, comment, approve, merge, deploy, or change any branch or shared environment. Leave other people's worktrees and branches alone.\n\n\
         1. Plan. Read what changed, then call `review_plan` once with every check worth doing, riskiest first: migrations on an empty and on an existing database, permissions and sign-in, conflicts with the target, then the main behaviour of each landed pull request (at least one check each), then neighbouring behaviour, error handling, configuration and deployment files. Each check's detail says what to verify and how: the test or command to run, or what to read.\n\
         2. Delegate. For each check call `agent_spawn` with its task_id, a `job` (`explore` to read and run checks, `code` to write code, `verify` to confirm a finding) and a brief that repeats the check, these rules and step 3. In plan order, at most two helpers at a time; start the next when one reports.\n\
         3. Each helper must actually do the check, then call `review_result` with the check's task_id, a result of pass, fail or blocked, and evidence: the exact commands it ran and what they showed, on which commit. Blocked means it could not check; the evidence then says what it needs. A guess is not a pass.\n\
         \x20  A fail is a finding. Give it a severity from 0 to 5:\n{severity}\n\
         \x20  And say whether this release caused it: run the same check on the target too (`git checkout --detach {base}`, run it, then `git checkout --detach {head}`), and send origin: introduced if it fails on the head and passes on the target, pre_existing if it fails the same way on the target, unverified if the target could not be checked or fails differently. The evidence shows both runs. A conflict with the target is always introduced. For an introduced finding, start the evidence with the landed pull request that brings it in, for example \"Introduced by #1234:\", when `git log {base}..{head} -- <file>` shows it; otherwise say it is not attributed.\n\
         4. Small checks you may do yourself and record with `review_result`.\n\
         5. Finish. When every check has a result, call `review_report` with a thorough report in Markdown: the verdict first (do not ship if any P0 or P1 finding is introduced or origin unverified; not ready if a P0 or P1 finding is pre-existing, or any check is blocked or not done; otherwise ready for the person's sign-off), then the findings from P0 down, each with severity, origin and one line of evidence, then the checks that passed, what could not be checked and what it needs, and risks you noticed that no check covers, each with a severity and an origin. Then tell the person the verdict in two sentences."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_recorded_only_with_its_origin() {
        let ctx = Ctx::test();
        board::init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r','r','/nonexistent','main',0)", []).unwrap();
        start(&ctx, "r", 7, 1).unwrap();
        let lead = json!({"repo_id": "r", "role": "orchestrator"});
        let p = plan(&ctx, "lead", &lead, &json!([{"title": "Migrations", "detail": "Run them on an empty database."}])).unwrap();
        let id = p["items"][0]["id"].as_str().unwrap();
        let ran = "Ran make migrate on the head and on the target: both fail at step 61 with the same error.";
        let e = result(&ctx, "x", "lead", &lead, id, "fail", Some(1), None, ran).unwrap_err();
        assert!(e.contains("needs its origin") && e.contains("pre_existing"), "{e}");
        assert!(result(&ctx, "x", "lead", &lead, id, "fail", Some(1), Some("maybe"), ran).is_err());
        let e = result(&ctx, "x", "lead", &lead, id, "fail", None, Some("introduced"), ran).unwrap_err();
        assert!(e.contains("needs its severity") && e.contains("block the release"), "{e}");
        assert!(result(&ctx, "x", "lead", &lead, id, "fail", Some(6), Some("introduced"), ran).is_err());
        let r = result(&ctx, "x", "lead", &lead, id, "fail", Some(1), Some("Pre-existing"), ran).unwrap();
        assert_eq!((r["origin"].as_str(), r["severity"].as_str()), (Some("pre_existing"), Some("P1")));
        let v = items(&ctx, "r", 7, 1).unwrap();
        assert_eq!((v["items"][0]["origin"].as_str(), v["items"][0]["severity"].as_i64(), v["headline"].as_str()), (Some("pre_existing"), Some(1), Some("not_ready")));
        // A pass carries no origin and no severity, whatever is sent.
        let r = result(&ctx, "x", "lead", &lead, id, "pass", Some(0), Some("introduced"), ran).unwrap();
        assert_eq!((r["origin"].clone(), r["severity"].clone()), (Value::Null, Value::Null));
    }

    #[test]
    fn a_checked_item_is_finished_on_the_board_and_leftovers_are_tidied_on_start() {
        let ctx = Ctx::test();
        board::init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r','r','/nonexistent','main',0)", []).unwrap();
        let lead = json!({"repo_id": "r", "role": "orchestrator"});
        start(&ctx, "r", 7, 1).unwrap();
        let p = plan(&ctx, "lead", &lead, &json!([{"title": "Merges", "detail": "Merge onto the target."}, {"title": "Staging", "detail": "Try it on staging."}, {"title": "Docs", "detail": "Read the notes."}])).unwrap();
        let id = |i: usize| p["items"][i]["id"].as_str().unwrap().to_string();
        let status = |id: &str| ctx.db.query("SELECT status FROM tasks WHERE id=?1", [id]).unwrap()[0]["status"].as_str().unwrap().to_string();
        result(&ctx, "x", "lead", &lead, &id(0), "pass", None, None, "git merge-tree on the head: no conflicts.").unwrap();
        result(&ctx, "x", "lead", &lead, &id(1), "blocked", None, None, "Needs a sign-in to the staging cluster.").unwrap();
        assert_eq!((status(&id(0)), status(&id(1)), status(&id(2))), ("done".into(), "open".into(), "open".into()));
        // An earlier version left the checked item open; the run then started again.
        ctx.db.exec("UPDATE tasks SET status='open'", []).unwrap();
        ctx.db.exec("UPDATE tasks SET created_at=created_at-60000", []).unwrap();
        tidy(&ctx).unwrap();
        assert_eq!((status(&id(0)), status(&id(1)), status(&id(2))), ("done".into(), "open".into(), "cancelled".into()));
    }

    #[test]
    fn starting_the_same_review_again_replaces_its_plan_and_report() {
        let ctx = Ctx::test();
        board::init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r','r','/nonexistent','main',0)", []).unwrap();
        let lead = json!({"repo_id": "r", "role": "orchestrator"});
        start(&ctx, "r", 7, 1).unwrap();
        let p = plan(&ctx, "lead", &lead, &json!([{"title": "Merges", "detail": "Merge onto the target."}, {"title": "Migrations", "detail": "Run them."}])).unwrap();
        let done = p["items"][0]["id"].as_str().unwrap().to_string();
        result(&ctx, "x", "lead", &lead, &done, "pass", None, None, "git merge-tree on the head: no conflicts.").unwrap();
        report(&ctx, "lead", &lead, &"Verdict: not ready. The migrations check was not done before the agents stopped. ".repeat(2)).unwrap();
        // The first run was earlier.
        ctx.db.exec("UPDATE tasks SET created_at=created_at-60000", []).unwrap();
        let mut r: Value = serde_json::from_str(&ctx.db.kv_get("review.report.r.1").unwrap()).unwrap();
        r["at"] = json!(r["at"].as_i64().unwrap() - 60_000);
        ctx.db.kv_set("review.report.r.1", &r.to_string()).unwrap();
        let mut run = current(&ctx, "r").unwrap();
        run["started"] = json!(run["started"].as_i64().unwrap() - 60_000);
        ctx.db.kv_set(&run_key("r"), &run.to_string()).unwrap();
        assert_eq!(items(&ctx, "r", 7, 1).unwrap()["items"].as_array().unwrap().len(), 2);
        start(&ctx, "r", 7, 1).unwrap();
        let v = items(&ctx, "r", 7, 1).unwrap();
        assert_eq!((v["items"].as_array().unwrap().len(), v["report"].clone(), v["headline"].as_str()), (0, Value::Null, Some("none")));
        // What the first run checked stays on the board; what it left undone is cancelled.
        let left = ctx.db.query("SELECT status, review_result FROM tasks ORDER BY created_at, title", []).unwrap();
        let states: Vec<(String, Value)> = left.iter().map(|t| (t["status"].as_str().unwrap().to_string(), t["review_result"].clone())).collect();
        assert!(states.contains(&("cancelled".to_string(), Value::Null)), "{states:?}");
        assert!(states.iter().any(|(s, r)| s != "cancelled" && r == "pass"), "{states:?}");
        plan(&ctx, "lead", &lead, &json!([{"title": "Migrations", "detail": "Run them again."}])).unwrap();
        assert_eq!(items(&ctx, "r", 7, 1).unwrap()["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn failing_or_unchecked_p0_and_p1_items_decide_the_verdict() {
        let pass = || json!({"result": "pass"});
        let found = |severity: i64, origin: &str| json!({"result": "fail", "severity": severity, "origin": origin});
        assert_eq!(headline(&[]), "none");
        assert_eq!(headline(&[pass(), pass()]), "passed");
        // P0 and P1 findings block when this release caused them, or when that is not known.
        assert_eq!(headline(&[pass(), found(1, "introduced")]), "do_not_ship");
        assert_eq!(headline(&[pass(), found(0, "unverified")]), "do_not_ship");
        assert_eq!(headline(&[pass(), json!({"result": "fail", "origin": "introduced"})]), "do_not_ship");
        // A blocker that was already on the target is shown, and the person decides.
        assert_eq!(headline(&[pass(), found(1, "pre_existing")]), "not_ready");
        // P2 to P5 findings inform.
        assert_eq!(headline(&[pass(), found(2, "introduced"), found(5, "unverified")]), "passed");
        // A check that could not be done can hide a blocker.
        assert_eq!(headline(&[pass(), json!({"result": "blocked"})]), "not_ready");
        assert_eq!(headline(&[pass(), json!({"result": null})]), "not_ready");
    }

    #[test]
    fn the_brief_names_the_release_the_severities_and_the_three_tools() {
        let b = brief(5542, "aaaa", "bbbb", "5 pull requests landed in it: #1, #2.");
        for needle in ["#5542", "target aaaa, head bbbb", "P0: blocks the release", "P1: blocks the release", "P5: cosmetic", "riskiest first", "review_plan", "review_result", "review_report", "at most two helpers", "read-only review",
            "git checkout --detach aaaa", "Introduced by #1234", "introduced if it fails on the head and passes on the target", "pre_existing if it fails the same way", "each with severity, origin"] {
            assert!(b.contains(needle), "{needle}");
        }
    }
}
