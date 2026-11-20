//! Proving the gate: rerun it on commits that shipped a known bug and count how
//! many it catches. A bug counts as caught when its check fails on the buggy
//! commit and, when a fix is named, passes on the fix (so the check is not
//! simply always red). A gate change that lowers the catch rate is rejected.
use super::config::Pipeline;
use super::records::{self, Status};
use super::runner::{self, Host};
use crate::db::{now, Db};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

pub const FILE: &str = ".philium/replay.yaml";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pipeline_replays(
  repo TEXT NOT NULL, digest TEXT NOT NULL, bug TEXT NOT NULL, buggy_sha TEXT NOT NULL, fixed_sha TEXT,
  check_id TEXT NOT NULL, buggy_status TEXT NOT NULL, fixed_status TEXT, caught INTEGER NOT NULL, at INTEGER NOT NULL,
  buggy_output TEXT, control_output TEXT);
CREATE TABLE IF NOT EXISTS pipeline_replay_bar(repo TEXT PRIMARY KEY, rate REAL NOT NULL, caught INTEGER NOT NULL, total INTEGER NOT NULL, digest TEXT NOT NULL, at INTEGER NOT NULL);
";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReplaySet {
    pub version: u32,
    pub bugs: Vec<Bug>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bug {
    pub id: String,
    /// The commit the bug shipped in.
    pub buggy: String,
    /// The commit that fixed it, when known.
    #[serde(default)]
    pub fixed: Option<String>,
    /// The check expected to catch it.
    pub check: String,
    pub text: String,
    #[serde(default)]
    pub source: Option<String>,
    /// The commit before the bug, as the base of the change that shipped it.
    /// Defaults to the buggy commit's first parent.
    #[serde(default)]
    pub base: Option<String>,
    /// Files laid over both commits before the check runs, typically the
    /// regression tests the fix added (the Defects4J way to replay a bug).
    #[serde(default)]
    pub overlay: Option<Overlay>,
    /// Text the failure on the buggy commit must show (a file, a test name),
    /// so a check failing for another reason is never counted as a catch.
    #[serde(default)]
    pub expect: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub from: String,
    pub paths: Vec<String>,
}

impl ReplaySet {
    pub fn parse(text: &str, p: &Pipeline) -> Result<ReplaySet, String> {
        let s: ReplaySet = serde_yaml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
        if s.version != 1 {
            return Err(format!("{FILE}: version {} is not supported", s.version));
        }
        let mut ids = BTreeSet::new();
        for b in &s.bugs {
            if !ids.insert(&b.id) {
                return Err(format!("{FILE}: bug {} appears twice", b.id));
            }
            if let Some(o) = &b.overlay {
                if o.paths.is_empty() || o.paths.iter().any(|p| p.is_empty() || p.starts_with('/') || p.split('/').any(|x| x == "..")) {
                    return Err(format!("{FILE}: bug {}: overlay.paths are paths inside the repo", b.id));
                }
            }
            for (what, sha) in [("buggy", Some(&b.buggy)), ("fixed", b.fixed.as_ref()), ("base", b.base.as_ref()), ("overlay.from", b.overlay.as_ref().map(|o| &o.from))] {
                if let Some(sha) = sha {
                    if !records::is_sha(sha) {
                        return Err(format!("{FILE}: bug {}: {what} must be a full 40-character SHA", b.id));
                    }
                }
            }
            if p.check(&b.check).is_none() {
                return Err(format!("{FILE}: bug {} names check {}, which the pipeline does not declare", b.id, b.check));
            }
        }
        Ok(s)
    }
}

fn init(db: &Db) -> Result<(), String> {
    db.with(|c| c.execute_batch(SCHEMA))?;
    for col in ["buggy_output", "control_output"] {
        let _ = db.exec(&format!("ALTER TABLE pipeline_replays ADD COLUMN {col} TEXT"), []);
    }
    Ok(())
}

pub fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[allow(clippy::too_many_arguments)]
pub fn remember(db: &Db, repo: &str, digest: &str, bug: &str, buggy: &str, fixed: Option<&str>, check: &str, buggy_status: Status, fixed_status: Option<Status>) -> Result<bool, String> {
    init(db)?;
    let caught = buggy_status.failed() && fixed_status.is_none_or(|s| s == Status::Pass);
    db.exec(
        "INSERT INTO pipeline_replays(repo,digest,bug,buggy_sha,fixed_sha,check_id,buggy_status,fixed_status,caught,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![repo, digest, bug, buggy, fixed, check, buggy_status.as_str(), fixed_status.map(|s| s.as_str()), caught, now()],
    )?;
    Ok(caught)
}

/// Invariant 8's proof: the check failed on the buggy SHA and passed on this candidate.
pub fn proven(db: &Db, repo: &str, buggy: &str, check: &str, candidate: &str) -> bool {
    if init(db).is_err() || records::init(db).is_err() {
        return false;
    }
    let failed_on_bug = db
        .query("SELECT 1 FROM pipeline_replays WHERE repo=?1 AND buggy_sha=?2 AND check_id=?3 AND buggy_status IN ('fail','timeout') LIMIT 1", params![repo, buggy, check])
        .map(|r| !r.is_empty())
        .unwrap_or(false);
    let passes_here = db
        .query(
            "SELECT 1 FROM pipeline_results r JOIN pipeline_runs u ON u.id=r.run_id WHERE u.repo=?1 AND u.candidate_sha=?2 AND r.check_id=?3 AND r.side='candidate' AND r.status='pass' LIMIT 1",
            params![repo, candidate, check],
        )
        .map(|r| !r.is_empty())
        .unwrap_or(false);
    failed_on_bug && passes_here
}

#[derive(Debug, Clone, Serialize)]
pub struct BugResult {
    pub id: String,
    pub check: String,
    pub buggy: Status,
    pub fixed: Option<Status>,
    pub caught: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub caught: usize,
    pub total: usize,
    pub rate: f64,
    pub bugs: Vec<BugResult>,
    /// The catch rate a previous gate reached, when this one falls below it.
    pub rejected_below: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
async fn one(p: &Pipeline, repo: &Path, sha: &str, base: &str, changed: &[String], check: &str, gate_dir: &Path, overlay: Option<&Overlay>, host: &Host) -> Result<runner::Outcome, String> {
    let c = p.check(check).ok_or_else(|| format!("no check {check}"))?;
    let ov = overlay.map(|o| (o.from.as_str(), o.paths.as_slice()));
    let mut outs = super::gate::side(p, &[c], repo, sha, base, changed, gate_dir, ov, host, &|_| {}).await?;
    Ok(outs.pop().map(|x| x.1).ok_or("no result")?)
}

pub(crate) fn parent(repo: &Path, sha: &str) -> Result<String, String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(["rev-parse", &format!("{sha}^")]).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!("{} has no parent; name the bug's base", &sha[..12]));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Run the gate's checks against every known bug and hold the catch rate to the bar.
#[allow(clippy::too_many_arguments)]
pub async fn run(db: &Db, repo_key: &str, repo: &Path, p: &Pipeline, set: &ReplaySet, digest: &str, gate_dir: &Path, host: &Host, say: &(dyn Fn(String) + Sync)) -> Result<Report, String> {
    init(db)?;
    let mut bugs = vec![];
    for b in &set.bugs {
        say(format!("{}: {} on {}", b.id, b.check, &b.buggy[..12]));
        let base = match &b.base {
            Some(x) => x.clone(),
            None => parent(repo, &b.buggy)?,
        };
        // The bug's scope: what its change touched, plus any laid-over tests. The
        // control is checked over exactly the same files.
        let mut changed = super::gate::changed_files(repo, &base, &b.buggy)?;
        for f in b.overlay.iter().flat_map(|o| o.paths.iter()) {
            if !changed.contains(f) {
                changed.push(f.clone());
            }
        }
        let on_bug = one(p, repo, &b.buggy, &base, &changed, &b.check, gate_dir, b.overlay.as_ref(), host).await?;
        // The control: the fix when known, else the commit before the bug. A check
        // that also fails there proves nothing about this bug.
        let on_fix = match &b.fixed {
            Some(f) => Some(one(p, repo, f, &b.buggy, &changed, &b.check, gate_dir, b.overlay.as_ref(), host).await?),
            None => Some(one(p, repo, &base, &base, &changed, &b.check, gate_dir, b.overlay.as_ref(), host).await?),
        };
        // With an expected text, the bug's own signature decides: it shows on the buggy
        // commit, and not on the control. Without one, the control must pass.
        let shows = |o: &runner::Outcome, t: &str| o.output.contains(t);
        // The bug's signature gone from the control counts as the control passing,
        // whether the control is the fix or the commit before the bug.
        let control_status = match (&b.expect, on_fix.as_ref()) {
            (Some(t), Some(c)) if c.status != Status::NotRun && !shows(c, t) && on_bug.status.failed() && shows(&on_bug, t) => Some(Status::Pass),
            _ => on_fix.as_ref().map(|o| o.status),
        };
        let bug_status = match &b.expect {
            Some(t) if on_bug.status.failed() && !shows(&on_bug, t) => Status::Pass,
            _ => on_bug.status,
        };
        let caught = remember(db, repo_key, digest, &b.id, &b.buggy, b.fixed.as_deref(), &b.check, bug_status, control_status)?;
        db.exec(
            "UPDATE pipeline_replays SET buggy_output=?1, control_output=?2 WHERE rowid=(SELECT max(rowid) FROM pipeline_replays WHERE repo=?3 AND bug=?4)",
            params![on_bug.output, on_fix.as_ref().map(|o| o.output.clone()), repo_key, b.id],
        )?;
        let reason = if on_bug.status.failed() && bug_status == Status::Pass {
            Some(format!("failed on the buggy commit, but not with \"{}\": it failed for another reason", b.expect.as_deref().unwrap_or("")))
        } else if caught {
            None
        } else if on_bug.status == Status::NotRun {
            Some(format!("did not run on the buggy commit: {}", on_bug.reason.clone().unwrap_or_default()))
        } else if !on_bug.status.failed() {
            Some("passed on the buggy commit: the gate misses this bug".into())
        } else if let Some(f) = on_fix.as_ref().filter(|f| f.status != Status::Pass) {
            let place = if b.fixed.is_some() { "the fix" } else { "the commit before the bug" };
            Some(format!("{} on {place} too, so it does not single out this bug{}", f.status.as_str(), f.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()))
        } else {
            None
        };
        say(format!("{}: {}", b.id, if caught { "caught" } else { "missed" }));
        bugs.push(BugResult { id: b.id.clone(), check: b.check.clone(), buggy: on_bug.status, fixed: on_fix.map(|o| o.status), caught, reason });
    }
    let caught = bugs.iter().filter(|b| b.caught).count();
    let total = bugs.len();
    let rate = if total == 0 { 0.0 } else { caught as f64 / total as f64 };
    let bar = db.query("SELECT rate FROM pipeline_replay_bar WHERE repo=?1", [repo_key])?.pop().and_then(|r| r["rate"].as_f64());
    let rejected_below = bar.filter(|b| rate + 1e-9 < *b);
    if rejected_below.is_none() {
        db.exec(
            "INSERT INTO pipeline_replay_bar(repo,rate,caught,total,digest,at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(repo) DO UPDATE SET rate=excluded.rate,caught=excluded.caught,total=excluded.total,digest=excluded.digest,at=excluded.at",
            params![repo_key, rate, caught as i64, total as i64, digest, now()],
        )?;
    }
    Ok(Report { caught, total, rate, bugs, rejected_below })
}

/// The replay set for a gate: the file at the same commit as the pipeline, else a local file.
pub fn load(repo: &Path, at: &str, local: Option<&Path>, p: &Pipeline) -> Result<(ReplaySet, String), String> {
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(["show", &format!("{at}:{FILE}")]).output().map_err(|e| format!("git: {e}"))?;
    let text = if out.status.success() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        let f = local.ok_or_else(|| format!("{at} has no {FILE}, and no local replay file was given"))?;
        std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?
    };
    Ok((ReplaySet::parse(&text, p)?, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::gate::tests::{commit, pipeline, repo};
    use crate::pipeline::runner::tests::{host, tmp};

    #[tokio::test]
    async fn a_bug_the_commits_own_tests_miss_is_caught_by_the_fixs_tests_or_the_gates_own_script() {
        use crate::pipeline::gate::tests::git as g;
        let root = tmp("overlay");
        let (r, _) = repo(&root);
        std::fs::write(r.join("value"), "2").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-qm", "good"]);
        // The bug ships with no test that notices it.
        std::fs::write(r.join("value"), "1").unwrap();
        g(&r, &["commit", "-qam", "bug"]);
        let bug = g(&r, &["rev-parse", "HEAD"]);
        // The fix adds the regression test.
        std::fs::create_dir_all(r.join("tests")).unwrap();
        std::fs::write(r.join("tests/value_test.sh"), "[ \"$(cat value)\" = 2 ]\n").unwrap();
        std::fs::write(r.join("value"), "2").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-qm", "fix"]);
        let fix = g(&r, &["rev-parse", "HEAD"]);
        // The gate keeps its own script in its .philium folder.
        let gate = root.join("gate");
        std::fs::create_dir_all(gate.join("checks")).unwrap();
        std::fs::write(gate.join("checks/value.sh"), "[ \"$(cat value)\" = 2 ]\n").unwrap();
        let p = Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}]\ntiers: {{ fast: [ {{ id: tests, run: \"for t in tests/*.sh; do [ -e \\\"$t\\\" ] || exit 0; sh \\\"$t\\\" || exit 1; done\" }}, {{ id: gate-script, run: \"sh ${{gate}}/checks/value.sh\" }} ] }}\n",
            crate::pipeline::config::host_platform()
        ))
        .unwrap();
        let mut h = host(&root);
        h.caches = root.join("caches");
        let db = crate::pipeline::records::tests::db();
        let text = format!(
            "version: 1\nbugs:\n  - {{ id: as-shipped, buggy: {bug}, fixed: {fix}, check: tests, text: the commit's own tests }}\n  - {{ id: with-fix-tests, buggy: {bug}, fixed: {fix}, check: tests, text: the fix's test laid over, overlay: {{ from: {fix}, paths: [tests/value_test.sh] }} }}\n  - {{ id: gate-owned, buggy: {bug}, fixed: {fix}, check: gate-script, text: the gate's own script }}\n"
        );
        let set = ReplaySet::parse(&text, &p).unwrap();
        let rep = run(&db, "r", &r, &p, &set, "o1", &gate, &h, &|_| {}).await.unwrap();
        // No fix known: the bug's own text must show on the buggy commit and not before it.
        let sig = format!(
            "version: 1\nbugs:\n  - {{ id: right-reason, buggy: {bug}, check: gate-script, expect: value, text: t }}\n  - {{ id: wrong-reason, buggy: {bug}, check: gate-script, expect: something-else, text: t }}\n"
        );
        std::fs::write(gate.join("checks/value.sh"), "[ \"$(cat value)\" = 2 ] || { echo 'bad value'; exit 1; }\n").unwrap();
        let rs = run(&db, "r", &r, &p, &ReplaySet::parse(&sig, &p).unwrap(), "o2", &gate, &h, &|_| {}).await.unwrap();
        let got: Vec<(&str, bool)> = rs.bugs.iter().map(|b| (b.id.as_str(), b.caught)).collect();
        assert_eq!(got, vec![("right-reason", true), ("wrong-reason", false)], "{:?}", rs.bugs);
        assert!(rs.bugs[1].reason.as_deref().unwrap().contains("for another reason"));
        let caught: Vec<(&str, bool)> = rep.bugs.iter().map(|b| (b.id.as_str(), b.caught)).collect();
        assert_eq!(caught, vec![("as-shipped", false), ("with-fix-tests", true), ("gate-owned", true)], "{:?}", rep.bugs);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn the_gate_is_measured_against_real_bugs_and_a_weaker_gate_is_rejected() {
        let root = tmp("replay");
        let (r, _) = repo(&root);
        let ok = commit(&r, &["readme"], &[]);
        let bug_a = commit(&r, &["broken-a"], &[]);
        let fix_a = commit(&r, &[], &["broken-a"]);
        let bug_b = commit(&r, &["broken-b"], &[]);
        let _ = ok;
        let shared = root.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        let mut h = host(&root);
        h.caches = shared.clone();
        let p = pipeline(&shared);
        let db = crate::pipeline::records::tests::db();
        let text = format!(
            "version: 1\nbugs:\n  - {{ id: lost-rows, buggy: {bug_a}, fixed: {fix_a}, check: a, text: upgrade lost rows }}\n  - {{ id: b-broke, buggy: {bug_b}, check: b, text: b broke }}\n  - {{ id: wrong-check, buggy: {bug_b}, check: a, text: the wrong check misses it }}\n"
        );
        let set = ReplaySet::parse(&text, &p).unwrap();
        let gate = root.join("gate");
        std::fs::create_dir_all(&gate).unwrap();
        let rep = run(&db, "r", &r, &p, &set, "g1", &gate, &h, &|_| {}).await.unwrap();
        assert_eq!((rep.caught, rep.total), (2, 3));
        assert!(rep.rejected_below.is_none());
        let missed = rep.bugs.iter().find(|b| b.id == "wrong-check").unwrap();
        assert!(!missed.caught && missed.reason.as_deref().unwrap().contains("misses this bug"));
        // A gate whose check for b no longer catches anything is held to the old bar.
        let weaker = Pipeline::parse(&format!("version: 1\nbranch: main\nplatforms: [{}]\ntiers: {{ fast: [ {{ id: a, run: sh check.sh a }}, {{ id: b, run: \"true\" }} ] }}\n", crate::pipeline::config::host_platform())).unwrap();
        let set2 = ReplaySet::parse(&text, &weaker).unwrap();
        let rep2 = run(&db, "r", &r, &weaker, &set2, "g2", &gate, &h, &|_| {}).await.unwrap();
        assert_eq!(rep2.caught, 1);
        assert!(rep2.rejected_below.is_some_and(|b| (b - 2.0 / 3.0).abs() < 1e-9));
        // Invariant 8's proof needs the failure on the bug and a pass on the fix candidate.
        assert!(!proven(&db, "r", &bug_a, "a", &fix_a), "no gate run on the fix yet");
        // Bad replay files are refused.
        assert!(ReplaySet::parse("version: 1\nbugs: [ { id: x, buggy: abc, check: a, text: t } ]", &p).unwrap_err().contains("40-character"));
        assert!(ReplaySet::parse(&format!("version: 1\nbugs: [ {{ id: x, buggy: {bug_a}, check: nope, text: t }} ]"), &p).unwrap_err().contains("does not declare"));
        std::fs::remove_dir_all(&root).ok();
    }
}
