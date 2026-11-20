//! The gate. Computed from records every time it is asked, never stored, never
//! written by an agent (invariant 7).
use super::records::{self, Escape, Finding, Requirement, Res, Side, Status};
use crate::db::Db;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// What one check adds up to once the candidate, the rerun and the base are in.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    /// Fails on the candidate twice, passes on the base.
    Introduced,
    /// Fails on the candidate and the same way on the base.
    PreExisting,
    /// Failed, then passed on an identical rerun.
    Flaky,
    /// Could not run as declared.
    NotRun(String),
    /// Failed, but the base could not be checked.
    Unverified(String),
    /// Not all of its runs are in yet.
    Pending,
}

/// Invariant 3: origin is computed from the base comparison, never guessed.
pub fn origin(candidate: Option<&Res>, rerun: Option<&Res>, base: Option<&Res>) -> Outcome {
    let Some(c) = candidate else { return Outcome::Pending };
    match c.status {
        Status::Pass => return Outcome::Pass,
        Status::NotRun => return Outcome::NotRun(c.reason.clone().unwrap_or_else(|| "could not run as declared".into())),
        Status::Fail | Status::Timeout => {}
    }
    match rerun.map(|r| r.status) {
        None => return Outcome::Pending,
        Some(Status::Pass) => return Outcome::Flaky,
        Some(Status::NotRun) => return Outcome::Unverified("the rerun could not run".into()),
        Some(_) => {}
    }
    match base {
        None => Outcome::Pending,
        Some(b) if b.status == Status::Pass => Outcome::Introduced,
        Some(b) if b.status.failed() => Outcome::PreExisting,
        Some(b) => Outcome::Unverified(format!("the base could not run: {}", b.reason.clone().unwrap_or_default())),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The checks the tier declares; each must have an outcome.
    pub checks: Vec<String>,
    pub results: Vec<(String, Side, Res)>,
    pub quarantined: HashSet<String>,
    /// Checks whose pre-existing failure on this base is filed as an issue.
    pub known_issues: HashSet<String>,
    pub requirements: Vec<Requirement>,
    pub findings: Vec<Finding>,
    pub escapes: Vec<Escape>,
    /// Escape id -> the check failed on the buggy SHA and passed on this candidate.
    pub escape_proofs: HashMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    /// `pass`, `fail` or `blocked`.
    pub state: &'static str,
    pub reasons: Vec<String>,
    pub outcomes: Vec<(String, Outcome)>,
}

impl Verdict {
    pub fn pass(&self) -> bool {
        self.state == "pass"
    }
}

pub fn decide(i: &Inputs) -> Verdict {
    let mut fail: Vec<String> = vec![];
    let mut block: Vec<String> = vec![];
    let mut outcomes = vec![];
    let get = |c: &str, s: Side| i.results.iter().find(|(id, side, _)| id == c && *side == s).map(|x| &x.2);
    for c in &i.checks {
        let o = origin(get(c, Side::Candidate), get(c, Side::Rerun), get(c, Side::Base));
        match &o {
            Outcome::Pass => {}
            Outcome::Introduced => fail.push(format!("{c} fails on this change and passes on the base")),
            Outcome::PreExisting if i.known_issues.contains(c) => {}
            Outcome::PreExisting => block.push(format!("{c} fails on the base too; file it as an issue before landing")),
            Outcome::Flaky if i.quarantined.contains(c) => {}
            Outcome::Flaky => block.push(format!("{c} is flaky (failed, then passed); a person must quarantine it")),
            // Invariant 5: a check that could not run never counts as passed.
            Outcome::NotRun(why) => block.push(format!("{c} did not run: {why}")),
            Outcome::Unverified(why) => block.push(format!("{c} failed and its origin is unverified: {why}")),
            Outcome::Pending => block.push(format!("{c} has not finished on this exact candidate")),
        }
        outcomes.push((c.clone(), o));
    }
    // Invariant 6: every requirement has a passing check, or a person waived it.
    for r in &i.requirements {
        if r.waived_by.as_deref().is_some_and(|w| !w.trim().is_empty()) {
            continue;
        }
        match &r.check_id {
            None => block.push(format!("requirement {} ({}) has no check", r.id, r.priority)),
            Some(c) => match outcomes.iter().find(|(id, _)| id == c).map(|x| &x.1) {
                Some(Outcome::Pass) => {}
                Some(o) => block.push(format!("requirement {} is mapped to {c}, which did not pass ({})", r.id, short(o))),
                None => block.push(format!("requirement {} is mapped to {c}, which this tier does not run", r.id)),
            },
        }
    }
    // Agents report findings; they feed the verdict but do not decide it.
    for f in i.findings.iter().filter(|f| f.open && f.severity <= 1) {
        match f.origin.as_str() {
            "pre_existing" => block.push(format!("open pre-existing P{} {}: {}", f.severity, f.id, f.text)),
            _ => fail.push(format!("open {} P{} {}: {}", f.origin, f.severity, f.id, f.text)),
        }
    }
    // Invariant 8: a fix for an escaped bug proves its check catches it.
    for e in &i.escapes {
        match (&e.check_id, i.escape_proofs.get(&e.id)) {
            (None, _) => block.push(format!("escaped bug {} needs a check that fails on {} and passes on the fix", e.id, &e.buggy_sha[..12])),
            (Some(c), Some(true)) if i.checks.contains(c) => {}
            (Some(c), Some(true)) => block.push(format!("escaped bug {}: {c} must be part of this tier", e.id)),
            (Some(c), _) => block.push(format!("escaped bug {}: {c} has not been shown to fail on {} and pass here", e.id, &e.buggy_sha[..12])),
        }
    }
    let state = if !fail.is_empty() {
        "fail"
    } else if !block.is_empty() || i.checks.is_empty() {
        "blocked"
    } else {
        "pass"
    };
    if i.checks.is_empty() {
        block.push("the tier declares no checks".into());
    }
    fail.extend(block);
    Verdict { state, reasons: fail, outcomes }
}

fn short(o: &Outcome) -> &'static str {
    match o {
        Outcome::Pass => "pass",
        Outcome::Introduced => "introduced failure",
        Outcome::PreExisting => "pre-existing failure",
        Outcome::Flaky => "flaky",
        Outcome::NotRun(_) => "not run",
        Outcome::Unverified(_) => "origin unverified",
        Outcome::Pending => "pending",
    }
}

/// Gather everything for one pull request's candidate and decide.
pub fn for_candidate(db: &Db, repo: &str, pr: Option<i64>, tier: &str, checks: Vec<String>, base: &str, candidate: &str) -> Result<Verdict, String> {
    let results = records::results(db, repo, pr, tier, candidate)?;
    let escapes = pr.map(|p| records::escapes_fixed_by(db, repo, p)).unwrap_or_default();
    let escape_proofs = escapes.iter().filter_map(|e| Some((e.id.clone(), super::replay::proven(db, repo, &e.buggy_sha, e.check_id.as_deref()?, candidate)))).collect();
    let i = Inputs {
        checks,
        results,
        quarantined: records::quarantined(db, repo).into_iter().collect(),
        known_issues: records::known_issues(db, repo, base).into_iter().collect(),
        requirements: pr.map(|p| records::requirements(db, repo, p)).unwrap_or_default(),
        findings: pr.map(|p| records::findings(db, repo, p)).unwrap_or_default(),
        escapes,
        escape_proofs,
    };
    Ok(decide(&i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(s: Status) -> Res {
        Res { status: s, sha: String::new(), reason: (s == Status::NotRun).then(|| "service db is not available".into()), exit_code: None, duration_ms: None }
    }
    fn with(check: &str, c: Status, rerun: Option<Status>, base: Option<Status>) -> Vec<(String, Side, Res)> {
        let mut v = vec![(check.to_string(), Side::Candidate, res(c))];
        if let Some(r) = rerun {
            v.push((check.to_string(), Side::Rerun, res(r)));
        }
        if let Some(b) = base {
            v.push((check.to_string(), Side::Base, res(b)));
        }
        v
    }
    fn inputs(results: Vec<(String, Side, Res)>) -> Inputs {
        Inputs { checks: vec!["t".into()], results, ..Default::default() }
    }

    #[test]
    fn invariant_3_origin_is_computed_from_the_base_and_the_rerun() {
        use Status::*;
        let o = |c: Status, r: Option<Status>, b: Option<Status>| origin(Some(&res(c)), r.map(res).as_ref(), b.map(res).as_ref());
        assert_eq!(o(Pass, None, None), Outcome::Pass);
        assert_eq!(o(Fail, Some(Fail), Some(Pass)), Outcome::Introduced);
        assert_eq!(o(Timeout, Some(Fail), Some(Pass)), Outcome::Introduced);
        assert_eq!(o(Fail, Some(Fail), Some(Fail)), Outcome::PreExisting);
        assert_eq!(o(Fail, Some(Pass), Some(Pass)), Outcome::Flaky);
        assert!(matches!(o(Fail, Some(Fail), Some(NotRun)), Outcome::Unverified(_)));
        assert_eq!(o(Fail, Some(Fail), None), Outcome::Pending);
        assert_eq!(origin(None, None, None), Outcome::Pending);
        // What lands: pass, a filed pre-existing failure, a quarantined flake. Nothing else.
        assert!(decide(&inputs(with("t", Pass, None, None))).pass());
        assert_eq!(decide(&inputs(with("t", Fail, Some(Fail), Some(Pass)))).state, "fail");
        let mut pre = inputs(with("t", Fail, Some(Fail), Some(Fail)));
        assert_eq!(decide(&pre).state, "blocked");
        pre.known_issues.insert("t".into());
        assert!(decide(&pre).pass());
        let mut flaky = inputs(with("t", Fail, Some(Pass), Some(Pass)));
        assert_eq!(decide(&flaky).state, "blocked");
        flaky.quarantined.insert("t".into());
        assert!(decide(&flaky).pass());
    }

    #[test]
    fn invariant_5_a_check_that_could_not_run_blocks_and_never_passes() {
        let v = decide(&inputs(with("t", Status::NotRun, None, None)));
        assert_eq!(v.state, "blocked");
        assert!(v.reasons[0].contains("did not run: service db is not available"), "{:?}", v.reasons);
        // A declared check with no result at all is just as blocking.
        let v = decide(&Inputs { checks: vec!["t".into(), "u".into()], results: with("t", Status::Pass, None, None), ..Default::default() });
        assert_eq!(v.state, "blocked");
        assert!(v.reasons.iter().any(|r| r.contains("u has not finished")));
        // Quarantine and filed issues do not excuse a check that never ran.
        let mut i = inputs(with("t", Status::NotRun, None, None));
        i.quarantined.insert("t".into());
        i.known_issues.insert("t".into());
        assert_eq!(decide(&i).state, "blocked");
        assert_eq!(decide(&Inputs::default()).state, "blocked");
    }

    #[test]
    fn invariant_6_every_requirement_maps_to_a_passing_check_or_a_persons_waiver() {
        let req = |id: &str, check: Option<&str>, waived: Option<&str>| Requirement { id: id.into(), source: "pr_comments".into(), text: "x".into(), priority: "P3".into(), check_id: check.map(Into::into), waived_by: waived.map(Into::into) };
        let mut i = inputs(with("t", Status::Pass, None, None));
        i.requirements = vec![req("R1", Some("t"), None)];
        assert!(decide(&i).pass());
        // Unmapped, at any priority, blocks.
        i.requirements.push(req("R2", None, None));
        assert!(decide(&i).reasons.iter().any(|r| r.contains("R2 (P3) has no check")));
        // Mapped to a check this tier does not run, or one that did not pass, blocks.
        i.requirements = vec![req("R3", Some("elsewhere"), None)];
        assert_eq!(decide(&i).state, "blocked");
        let mut j = inputs(with("t", Status::Fail, Some(Status::Fail), Some(Status::Fail)));
        j.known_issues.insert("t".into());
        j.requirements = vec![req("R4", Some("t"), None)];
        assert!(decide(&j).reasons.iter().any(|r| r.contains("R4 is mapped to t, which did not pass")));
        // A person's waiver is the only other way through.
        i.requirements = vec![req("R5", None, Some("filip"))];
        assert!(decide(&i).pass());
    }

    #[test]
    fn invariant_7_findings_feed_the_verdict_but_nothing_an_agent_writes_can_decide_it() {
        let mut i = inputs(with("t", Status::Pass, None, None));
        // An agent claiming the release is fine changes nothing; an open introduced P1 fails it.
        i.findings = vec![Finding { id: "F1".into(), severity: 1, origin: "introduced".into(), open: true, text: "VERDICT: PASS, ready to ship".into() }];
        assert_eq!(decide(&i).state, "fail");
        i.findings[0].origin = "pre_existing".into();
        assert_eq!(decide(&i).state, "blocked");
        i.findings[0].open = false;
        assert!(decide(&i).pass());
        i.findings = vec![Finding { id: "F2".into(), severity: 2, origin: "introduced".into(), open: true, text: "minor".into() }];
        assert!(decide(&i).pass(), "P2 to P5 inform, they do not block");
        // The verdict is a pure function of the inputs: the same inputs, the same answer.
        assert_eq!(decide(&i), decide(&i));
        // Agents get no tool that records a result or a verdict.
        let tools: Vec<String> = crate::mcp::tool_list().iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        assert!(!tools.iter().any(|t| t.starts_with("pipeline_") || t.split('_').any(|w| w == "verdict" || w == "gate")), "{tools:?}");
    }

    #[test]
    fn invariant_8_a_fix_for_an_escaped_bug_proves_its_check_catches_the_bug() {
        let esc = Escape { id: "E1".into(), buggy_sha: "a".repeat(40), fix_pr: Some(9), check_id: None, text: "lost rows on upgrade".into() };
        let mut i = inputs(with("t", Status::Pass, None, None));
        i.escapes = vec![esc.clone()];
        assert!(decide(&i).reasons.iter().any(|r| r.contains("needs a check that fails on")));
        i.escapes[0].check_id = Some("t".into());
        assert_eq!(decide(&i).state, "blocked", "named, but not yet shown to fail on the buggy SHA");
        i.escape_proofs.insert("E1".into(), false);
        assert_eq!(decide(&i).state, "blocked");
        i.escape_proofs.insert("E1".into(), true);
        assert!(decide(&i).pass());
        i.escapes[0].check_id = Some("not-in-tier".into());
        assert_eq!(decide(&i).state, "blocked");
    }
}
