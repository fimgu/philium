//! A release review led by an orchestrator. The person pastes a pull request; an orchestrator
//! writes a plan of items in tiers P0 to P5, helpers do the items and record pass, fail or
//! blocked with what they ran, and the orchestrator writes a report.
//!
//! Everything recorded here is an agent's word. It is shown to the person, grouped and counted,
//! and the person signs off. It never replaces what Philium checks itself in the same screen.
use crate::{board, ctx::Ctx, db::now};
use rusqlite::params;
use serde_json::{json, Value};
use std::sync::Arc;

/// What each tier means. Used in the orchestrator's brief and shown beside the plan.
pub const TIERS: [(&str, &str); 6] = [
    ("P0", "can lose data, break security or stop the release from installing: migrations, permissions, sign-in, conflicts with the target"),
    ("P1", "the main behaviour each pull request changes"),
    ("P2", "neighbouring behaviour that could regress"),
    ("P3", "error handling and edge cases"),
    ("P4", "configuration, deployment files and documentation"),
    ("P5", "polish: listed, not verified"),
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
/// the old items stay on the board under their own pull request.
pub fn start(ctx: &Ctx, repo: &str, pr: i64, generation: i64) -> Result<(), String> {
    ctx.db.kv_set(&run_key(repo), &json!({"pr": pr, "generation": generation, "started": now()}).to_string())?;
    ctx.emit("release:review", json!({ "repo_id": repo }));
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
    let list = items.as_array().filter(|a| (1..=40).contains(&a.len())).ok_or("Send items: 1 to 40 entries, each {title, tier, detail}")?;
    let existing = ctx.db.query("SELECT count(*) AS n FROM tasks WHERE repo_id=?1 AND review_gen=?2 AND status!='cancelled'", params![repo, gen])?;
    if existing.first().and_then(|r| r["n"].as_i64()).unwrap_or(0) + list.len() as i64 > 60 {
        return Err("A review holds at most 60 items".into());
    }
    // Check everything before creating anything, so a bad entry does not leave half a plan.
    let mut checked = Vec::new();
    for (i, it) in list.iter().enumerate() {
        let title = it["title"].as_str().map(str::trim).filter(|t| !t.is_empty() && t.len() <= 160).ok_or_else(|| format!("Item {}: title is required, at most 160 characters", i + 1))?;
        let tier = it["tier"].as_i64().filter(|t| (0..=5).contains(t)).ok_or_else(|| format!("Item {}: tier is a number from 0 (P0) to 5 (P5)", i + 1))?;
        let detail = it["detail"].as_str().map(str::trim).filter(|d| !d.is_empty()).ok_or_else(|| format!("Item {}: detail says what to verify and how", i + 1))?;
        checked.push((title, tier, detail));
    }
    let mut out = Vec::new();
    for (title, tier, detail) in checked {
        let priority = ["urgent", "high", "normal", "low", "low", "low"][tier as usize];
        let task = board::create(ctx, me, &format!("P{tier} {title}"), detail, Some(priority), Some(repo), &[], None)?;
        let id = task["id"].as_str().ok_or("Task not created")?;
        ctx.db.exec("UPDATE tasks SET review_pr=?1, review_gen=?2, review_tier=?3 WHERE id=?4", params![pr, gen, tier, id])?;
        out.push(json!({"id": id, "tier": format!("P{tier}"), "title": title}));
    }
    ctx.emit("release:review", json!({ "repo_id": repo }));
    Ok(json!({"items": out, "next": "Hand each item to a helper with agent_spawn {task_id, brief}, P0 first, at most two helpers at a time. Each helper records its outcome with review_result."}))
}

/// One item's outcome, with what was run and seen. Recorded by the agent that did the item or by
/// the review's orchestrator.
pub fn result(ctx: &Arc<Ctx>, agent_id: &str, me: &str, row: &Value, task_id: &str, outcome: &str, origin: Option<&str>, evidence: &str) -> Result<Value, String> {
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
    ctx.db.exec("UPDATE tasks SET review_result=?1, review_evidence=?2, review_origin=?3, updated_at=?4 WHERE id=?5", params![outcome, format!("{me}: {evidence}"), origin, now(), task_id])?;
    // Hand the item in when this agent holds it, so the board shows it done by its owner.
    let handed_in = board::submit(ctx, agent_id, task_id, &format!("{}: {evidence}", outcome.to_uppercase()), None).is_ok();
    ctx.emit("release:review", json!({ "repo_id": repo }));
    Ok(json!({"recorded": outcome, "origin": origin, "handed_in": handed_in}))
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
    Ok(json!({"saved": true, "next": "Tell the person the verdict in two sentences. They read the report in Review."}))
}

/// The verdict the items add up to. Failing or unchecked P0 and P1 items decide it; the lower
/// tiers inform. A failure the release introduced, or one whose origin is unverified, means do
/// not ship; one that fails the same way on the target is not this release's doing, so it leaves
/// the release not ready for the person to judge.
pub fn headline(items: &[Value]) -> &'static str {
    let key: Vec<&Value> = items.iter().filter(|i| i["tier"].as_i64().unwrap_or(9) <= 1).collect();
    if items.is_empty() {
        "none"
    } else if key.iter().any(|i| i["result"] == "fail" && i["origin"] != "pre_existing") {
        "do_not_ship"
    } else if key.is_empty() || key.iter().any(|i| i["result"] != "pass") {
        "not_ready"
    } else {
        "passed"
    }
}

/// What Review shows for one revision of a pull request: the items in tier order, the verdict
/// they add up to and the report, if any. A new head is a new revision and starts empty.
pub fn items(ctx: &Ctx, repo: &str, pr: i64, gen: i64) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT t.id,t.title,t.status,t.review_tier AS tier,t.review_result AS result,t.review_origin AS origin,t.review_evidence AS evidence,COALESCE(a.name,t.assigned,t.owner) AS owner \
         FROM tasks t LEFT JOIN agents a ON a.id=COALESCE(t.owner_agent_id,t.assigned_agent_id) \
         WHERE t.repo_id=?1 AND t.review_gen=?2 AND t.status!='cancelled' ORDER BY t.review_tier, t.created_at",
        params![repo, gen],
    )?;
    let report = ctx.db.kv_get(&format!("review.report.{repo}.{gen}")).and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    let run = current(ctx, repo).filter(|r| r["generation"].as_i64() == Some(gen)).unwrap_or(Value::Null);
    Ok(json!({"pr": pr, "headline": headline(&rows), "items": rows, "report": report, "run": run,
        "tiers": TIERS.iter().map(|(t, d)| json!({"tier": t, "means": d})).collect::<Vec<_>>()}))
}

/// What the orchestrator is told. `facts` is what Philium already established about the release.
pub fn brief(pr: i64, base: &str, head: &str, facts: &str) -> String {
    let tiers = TIERS.iter().map(|(t, d)| format!("   {t}: {d}")).collect::<Vec<_>>().join("\n");
    format!(
        "You are leading the review of release pull request #{pr} for this checkout. The person pasted it and expects you to organise the whole review. Do not hand work back to them.\n\n\
         The release: target {base}, head {head}. {facts}\n\
         See it with `git log --oneline {base}..{head}` and `git diff --stat {base}...{head}`.\n\n\
         Rules for you and every helper: this is a read-only review. Do not push, comment, approve, merge, deploy, or change any branch or shared environment. Leave other people's worktrees and branches alone.\n\n\
         1. Plan. Read what changed, then call `review_plan` once with every item worth verifying, each in a tier:\n{tiers}\n\
         \x20  Give each landed pull request at least one P1 item for its main behaviour. Add release-wide items: migrations apply in order on an empty database, conflicts with the target, deployment files. Each item's detail says what to verify and how: the test or command to run, or what to read.\n\
         2. Delegate. For each item call `agent_spawn` with its task_id and a brief that repeats the item, these rules, and step 3. P0 first. At most two helpers at a time; start the next when one reports.\n\
         3. Each helper must actually do the check, then call `review_result` with the item's task_id, a result of pass, fail or blocked, and evidence: the exact commands it ran and what they showed, on which commit. Blocked means it could not check; the evidence then says what it needs. A guess is not a pass.\n\
         \x20  A fail is a finding, and every finding says whether this release caused it. Run the same check on the target too (`git checkout --detach {base}`, run it, then `git checkout --detach {head}`), and send origin: introduced if it fails on the head and passes on the target, pre_existing if it fails the same way on the target, unverified if the target could not be checked or fails differently. The evidence shows both runs. A conflict with the target is always introduced. For an introduced finding, start the evidence with the landed pull request that brings it in, for example \"Introduced by #1234:\", when `git log {base}..{head} -- <file>` shows it; otherwise say it is not attributed.\n\
         4. Small items you may do yourself and record with `review_result`.\n\
         5. Finish. When every item has a result, call `review_report` with a thorough report in Markdown: the verdict first (do not ship if any P0 or P1 failure is introduced or origin unverified; not ready if any P0 or P1 failure is pre-existing, or any P0 or P1 is blocked or unchecked; otherwise ready for the person's sign-off), then a table of the items with tier, result, origin and one line of evidence, then what failed and why, what could not be checked and what it needs, and risks you noticed that no item covers. Label every finding in the report, risks included: introduced, pre-existing or origin unverified. Then tell the person the verdict in two sentences."
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
        let p = plan(&ctx, "lead", &lead, &json!([{"title": "Migrations", "tier": 1, "detail": "Run them on an empty database."}])).unwrap();
        let id = p["items"][0]["id"].as_str().unwrap();
        let ran = "Ran make migrate on the head and on the target: both fail at step 61 with the same error.";
        let e = result(&ctx, "x", "lead", &lead, id, "fail", None, ran).unwrap_err();
        assert!(e.contains("needs its origin") && e.contains("pre_existing"), "{e}");
        assert!(result(&ctx, "x", "lead", &lead, id, "fail", Some("maybe"), ran).is_err());
        assert_eq!(result(&ctx, "x", "lead", &lead, id, "fail", Some("Pre-existing"), ran).unwrap()["origin"], "pre_existing");
        let v = items(&ctx, "r", 7, 1).unwrap();
        assert_eq!((v["items"][0]["origin"].as_str(), v["headline"].as_str()), (Some("pre_existing"), Some("not_ready")));
        // A pass carries no origin, whatever is sent.
        assert_eq!(result(&ctx, "x", "lead", &lead, id, "pass", Some("introduced"), ran).unwrap()["origin"], Value::Null);
    }

    #[test]
    fn failing_or_unchecked_p0_and_p1_items_decide_the_verdict() {
        let item = |tier: i64, result: Value| json!({"tier": tier, "result": result});
        assert_eq!(headline(&[]), "none");
        assert_eq!(headline(&[item(0, json!("pass")), item(1, json!("pass")), item(3, json!("fail"))]), "passed");
        assert_eq!(headline(&[item(0, json!("pass")), item(1, json!("fail"))]), "do_not_ship");
        let found = |tier: i64, origin: &str| json!({"tier": tier, "result": "fail", "origin": origin});
        assert_eq!(headline(&[item(0, json!("pass")), found(1, "introduced")]), "do_not_ship");
        assert_eq!(headline(&[item(0, json!("pass")), found(1, "unverified")]), "do_not_ship");
        assert_eq!(headline(&[item(0, json!("pass")), found(1, "pre_existing")]), "not_ready");
        assert_eq!(headline(&[item(0, json!("pass")), item(1, json!("pass")), found(2, "introduced")]), "passed");
        assert_eq!(headline(&[item(0, json!("pass")), item(1, json!("blocked"))]), "not_ready");
        assert_eq!(headline(&[item(0, json!("pass")), item(1, Value::Null)]), "not_ready");
        // A plan with nothing in P0 or P1 has verified nothing that matters.
        assert_eq!(headline(&[item(4, json!("pass"))]), "not_ready");
    }

    #[test]
    fn the_brief_names_the_release_the_tiers_and_the_three_tools() {
        let b = brief(5542, "aaaa", "bbbb", "5 pull requests landed in it: #1, #2.");
        for needle in ["#5542", "target aaaa, head bbbb", "P0: can lose data", "P5: polish", "review_plan", "review_result", "review_report", "At most two helpers", "read-only review",
            "git checkout --detach aaaa", "Introduced by #1234", "introduced if it fails on the head and passes on the target", "pre_existing if it fails the same way", "Label every finding"] {
            assert!(b.contains(needle), "{needle}");
        }
    }
}
