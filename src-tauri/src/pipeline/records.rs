//! What the pipeline observed, and nothing else. Only the runner writes check
//! results; people and agents add inputs (requirements, findings, quarantines,
//! issue links, waivers), never results or verdicts.
//!
//! Invariant 1, exact SHA: every result belongs to a run that records the full
//! head, base and candidate SHAs, and its own SHA must be the run's candidate or
//! base. A new head for a pull request makes every earlier result stale.
use crate::db::{new_id, now, Db};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pipeline_runs(
  id TEXT PRIMARY KEY, repo TEXT NOT NULL, pr INTEGER, tier TEXT NOT NULL,
  head_sha TEXT NOT NULL, base_sha TEXT NOT NULL, candidate_sha TEXT NOT NULL,
  config_from TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0, started INTEGER NOT NULL, finished INTEGER,
  CHECK(length(head_sha)=40 AND length(base_sha)=40 AND length(candidate_sha)=40));
CREATE TABLE IF NOT EXISTS pipeline_results(
  run_id TEXT NOT NULL REFERENCES pipeline_runs(id) ON DELETE CASCADE,
  check_id TEXT NOT NULL, side TEXT NOT NULL CHECK(side IN ('candidate','rerun','base')),
  sha TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pass','fail','timeout','not_run')),
  reason TEXT, exit_code INTEGER, duration_ms INTEGER, output TEXT, recorded INTEGER NOT NULL,
  PRIMARY KEY(run_id, check_id, side));
CREATE TABLE IF NOT EXISTS pipeline_heads(repo TEXT NOT NULL, pr INTEGER NOT NULL, head_sha TEXT NOT NULL, seen INTEGER NOT NULL, PRIMARY KEY(repo, pr));
CREATE TABLE IF NOT EXISTS pipeline_quarantine(repo TEXT NOT NULL, check_id TEXT NOT NULL, by TEXT NOT NULL, reason TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(repo, check_id));
CREATE TABLE IF NOT EXISTS pipeline_known_issues(repo TEXT NOT NULL, check_id TEXT NOT NULL, base_sha TEXT NOT NULL, issue TEXT NOT NULL, by TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(repo, check_id, base_sha));
CREATE TABLE IF NOT EXISTS pipeline_requirements(
  repo TEXT NOT NULL, pr INTEGER NOT NULL, id TEXT NOT NULL, source TEXT NOT NULL, text TEXT NOT NULL,
  priority TEXT NOT NULL, check_id TEXT, waived_by TEXT, waiver_reason TEXT, added INTEGER NOT NULL,
  PRIMARY KEY(repo, pr, id));
CREATE TABLE IF NOT EXISTS pipeline_findings(
  repo TEXT NOT NULL, pr INTEGER NOT NULL, id TEXT NOT NULL, severity INTEGER NOT NULL CHECK(severity BETWEEN 0 AND 5),
  origin TEXT NOT NULL CHECK(origin IN ('introduced','pre_existing','unverified')), open INTEGER NOT NULL,
  text TEXT NOT NULL, by TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(repo, pr, id));
CREATE TABLE IF NOT EXISTS pipeline_enrolled(repo TEXT NOT NULL, environment TEXT NOT NULL, by TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(repo, environment));
CREATE TABLE IF NOT EXISTS pipeline_lends(repo TEXT NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL, by TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(repo, name));
CREATE TABLE IF NOT EXISTS pipeline_escapes(
  repo TEXT NOT NULL, id TEXT NOT NULL, buggy_sha TEXT NOT NULL, fix_pr INTEGER, check_id TEXT,
  text TEXT NOT NULL, found INTEGER NOT NULL, PRIMARY KEY(repo, id), CHECK(length(buggy_sha)=40));
";

pub fn init(db: &Db) -> Result<(), String> {
    db.with(|c| c.execute_batch(SCHEMA))
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Candidate,
    Rerun,
    Base,
}
impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Candidate => "candidate",
            Side::Rerun => "rerun",
            Side::Base => "base",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Timeout,
    NotRun,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Timeout => "timeout",
            Status::NotRun => "not_run",
        }
    }
    fn parse(s: &str) -> Status {
        match s {
            "pass" => Status::Pass,
            "fail" => Status::Fail,
            "timeout" => Status::Timeout,
            _ => Status::NotRun,
        }
    }
    pub fn failed(self) -> bool {
        matches!(self, Status::Fail | Status::Timeout)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Res {
    pub status: Status,
    pub sha: String,
    pub reason: Option<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// What a run is about. `pr` is absent for a branch or replay run.
#[derive(Debug, Clone)]
pub struct RunKey<'a> {
    pub repo: &'a str,
    pub pr: Option<i64>,
    pub tier: &'a str,
    pub head: &'a str,
    pub base: &'a str,
    pub candidate: &'a str,
    /// The gate came from the base commit, not a local file.
    pub pinned: bool,
}

pub fn start_run(db: &Db, k: &RunKey, config_from: &str) -> Result<String, String> {
    for (what, s) in [("head", k.head), ("base", k.base), ("candidate", k.candidate)] {
        if !is_sha(s) {
            return Err(format!("the {what} must be a full 40-character commit SHA, not {s}"));
        }
    }
    init(db)?;
    let id = new_id();
    db.exec(
        "INSERT INTO pipeline_runs(id,repo,pr,tier,head_sha,base_sha,candidate_sha,config_from,pinned,started) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![id, k.repo, k.pr, k.tier, k.head, k.base, k.candidate, config_from, k.pinned, now()],
    )?;
    if let Some(pr) = k.pr {
        see_head(db, k.repo, pr, k.head)?;
    }
    Ok(id)
}

pub fn finish_run(db: &Db, run: &str) -> Result<(), String> {
    db.exec("UPDATE pipeline_runs SET finished=?2 WHERE id=?1", params![run, now()]).map(|_| ())
}

/// Record one result. The SHA must be the one this side of the run is about.
#[allow(clippy::too_many_arguments)]
pub fn record(db: &Db, run: &str, check: &str, side: Side, sha: &str, status: Status, reason: Option<&str>, exit_code: Option<i64>, duration_ms: Option<i64>, output: &str) -> Result<(), String> {
    let r = db.query("SELECT base_sha,candidate_sha FROM pipeline_runs WHERE id=?1", [run])?.pop().ok_or("no such run")?;
    let want = if side == Side::Base { &r["base_sha"] } else { &r["candidate_sha"] };
    if want.as_str() != Some(sha) {
        return Err(format!("a {} result must be for {}, not {sha}", side.as_str(), want.as_str().unwrap_or("")));
    }
    db.exec(
        "INSERT INTO pipeline_results(run_id,check_id,side,sha,status,reason,exit_code,duration_ms,output,recorded) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(run_id,check_id,side) DO UPDATE SET sha=excluded.sha,status=excluded.status,reason=excluded.reason,exit_code=excluded.exit_code,duration_ms=excluded.duration_ms,output=excluded.output,recorded=excluded.recorded",
        params![run, check, side.as_str(), sha, status.as_str(), reason, exit_code, duration_ms, output, now()],
    )?;
    Ok(())
}

/// The newest head seen for a pull request. Anything recorded for another head is stale.
pub fn see_head(db: &Db, repo: &str, pr: i64, head: &str) -> Result<(), String> {
    if !is_sha(head) {
        return Err(format!("a head must be a full commit SHA, not {head}"));
    }
    init(db)?;
    db.exec(
        "INSERT INTO pipeline_heads(repo,pr,head_sha,seen) VALUES(?1,?2,?3,?4) ON CONFLICT(repo,pr) DO UPDATE SET head_sha=excluded.head_sha,seen=excluded.seen WHERE pipeline_heads.head_sha!=excluded.head_sha",
        params![repo, pr, head, now()],
    )?;
    Ok(())
}

pub fn current_head(db: &Db, repo: &str, pr: i64) -> Option<String> {
    init(db).ok()?;
    db.query("SELECT head_sha FROM pipeline_heads WHERE repo=?1 AND pr=?2", params![repo, pr]).ok()?.pop()?["head_sha"].as_str().map(str::to_string)
}

/// The latest result of each side of each check, from runs of this exact
/// candidate. For a pull request, only runs of its current head count.
pub fn results(db: &Db, repo: &str, pr: Option<i64>, tier: &str, candidate: &str) -> Result<Vec<(String, Side, Res)>, String> {
    init(db)?;
    let head = match pr {
        Some(pr) => match current_head(db, repo, pr) {
            Some(h) => Some(h),
            None => return Ok(vec![]),
        },
        None => None,
    };
    let rows = db.query(
        "SELECT r.check_id,r.side,r.sha,r.status,r.reason,r.exit_code,r.duration_ms FROM pipeline_results r JOIN pipeline_runs u ON u.id=r.run_id
         WHERE u.repo=?1 AND u.tier=?2 AND u.candidate_sha=?3 AND (?4 IS NULL OR (u.pr=?5 AND u.head_sha=?4))
         ORDER BY r.recorded",
        params![repo, tier, candidate, head, pr],
    )?;
    let mut out: Vec<(String, Side, Res)> = vec![];
    for r in rows {
        let side = match r["side"].as_str() {
            Some("base") => Side::Base,
            Some("rerun") => Side::Rerun,
            _ => Side::Candidate,
        };
        let check = r["check_id"].as_str().unwrap_or("").to_string();
        let res = Res {
            status: Status::parse(r["status"].as_str().unwrap_or("")),
            sha: r["sha"].as_str().unwrap_or("").to_string(),
            reason: r["reason"].as_str().map(str::to_string),
            exit_code: r["exit_code"].as_i64(),
            duration_ms: r["duration_ms"].as_i64(),
        };
        out.retain(|(c, s, _)| !(c == &check && *s == side));
        out.push((check, side, res));
    }
    Ok(out)
}

/// A person enrolls an environment for read-only observation. Until then its checks are not_run.
pub fn enroll(db: &Db, repo: &str, env: &str, by: &str) -> Result<(), String> {
    if by.trim().is_empty() {
        return Err("enrolling an environment names the person".into());
    }
    init(db)?;
    db.exec("INSERT OR REPLACE INTO pipeline_enrolled(repo,environment,by,at) VALUES(?1,?2,?3,?4)", params![repo, env, by, now()]).map(|_| ())
}
pub fn enrolled(db: &Db, repo: &str, env: &str) -> bool {
    let _ = init(db);
    db.query("SELECT 1 FROM pipeline_enrolled WHERE repo=?1 AND environment=?2", params![repo, env]).map(|r| !r.is_empty()).unwrap_or(false)
}

/// Every run behind this candidate's results read its gate from the base commit.
pub fn pinned(db: &Db, repo: &str, pr: Option<i64>, tier: &str, candidate: &str) -> bool {
    let head = match pr {
        Some(p) => match current_head(db, repo, p) {
            Some(h) => Some(h),
            None => return false,
        },
        None => None,
    };
    db.query(
        "SELECT min(u.pinned) AS p, count(*) AS n FROM pipeline_runs u WHERE u.repo=?1 AND u.tier=?2 AND u.candidate_sha=?3 AND (?4 IS NULL OR (u.pr=?5 AND u.head_sha=?4))
         AND EXISTS(SELECT 1 FROM pipeline_results r WHERE r.run_id=u.id)",
        params![repo, tier, candidate, head, pr],
    )
    .ok()
    .and_then(|r| r.first().cloned())
    .is_some_and(|r| r["n"].as_i64().unwrap_or(0) > 0 && r["p"].as_i64() == Some(1))
}

/// A person lends a host folder to a repo's checks, read-only. Never a home folder
/// itself, a folder that contains it, or one of the places keys and logins live.
pub fn lend(db: &Db, repo: &str, name: &str, path: &std::path::Path, by: &str, home: &std::path::Path) -> Result<(), String> {
    if by.trim().is_empty() {
        return Err("lending a folder names the person".into());
    }
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !real.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if home.starts_with(&real) {
        return Err("a home folder, or a folder that holds one, is never lent".into());
    }
    for private in [".ssh", ".gnupg", ".aws", ".config", ".docker", ".kube", ".netrc", "Library/Keychains", "Library/Application Support", "Library/Cookies", "Philium Pilot"] {
        let p = home.join(private);
        if real.starts_with(&p) || p.starts_with(&real) {
            return Err(format!("{} holds or sits in {private}, where keys and logins live; it is never lent", path.display()));
        }
    }
    init(db)?;
    db.exec("INSERT OR REPLACE INTO pipeline_lends(repo,name,path,by,at) VALUES(?1,?2,?3,?4,?5)", params![repo, name, real.display().to_string(), by, now()]).map(|_| ())
}
pub fn lent(db: &Db, repo: &str, name: &str) -> Option<std::path::PathBuf> {
    let _ = init(db);
    db.query("SELECT path FROM pipeline_lends WHERE repo=?1 AND name=?2", params![repo, name]).ok()?.pop()?["path"].as_str().map(std::path::PathBuf::from)
}

/// People and agents add inputs; each is attributed.
pub fn quarantine(db: &Db, repo: &str, check: &str, by: &str, reason: &str) -> Result<(), String> {
    init(db)?;
    db.exec("INSERT OR REPLACE INTO pipeline_quarantine(repo,check_id,by,reason,at) VALUES(?1,?2,?3,?4,?5)", params![repo, check, by, reason, now()]).map(|_| ())
}
pub fn quarantined(db: &Db, repo: &str) -> Vec<String> {
    let _ = init(db);
    db.query("SELECT check_id FROM pipeline_quarantine WHERE repo=?1", [repo]).unwrap_or_default().iter().filter_map(|r| r["check_id"].as_str().map(str::to_string)).collect()
}
pub fn file_known_issue(db: &Db, repo: &str, check: &str, base: &str, issue: &str, by: &str) -> Result<(), String> {
    init(db)?;
    db.exec("INSERT OR REPLACE INTO pipeline_known_issues(repo,check_id,base_sha,issue,by,at) VALUES(?1,?2,?3,?4,?5,?6)", params![repo, check, base, issue, by, now()]).map(|_| ())
}
pub fn known_issues(db: &Db, repo: &str, base: &str) -> Vec<String> {
    let _ = init(db);
    db.query("SELECT check_id FROM pipeline_known_issues WHERE repo=?1 AND base_sha=?2", params![repo, base]).unwrap_or_default().iter().filter_map(|r| r["check_id"].as_str().map(str::to_string)).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub source: String,
    pub text: String,
    pub priority: String,
    pub check_id: Option<String>,
    pub waived_by: Option<String>,
}

pub fn add_requirement(db: &Db, repo: &str, pr: i64, r: &Requirement) -> Result<(), String> {
    init(db)?;
    db.exec(
        "INSERT INTO pipeline_requirements(repo,pr,id,source,text,priority,check_id,waived_by,added) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(repo,pr,id) DO UPDATE SET text=excluded.text,priority=excluded.priority,check_id=COALESCE(excluded.check_id,pipeline_requirements.check_id)",
        params![repo, pr, r.id, r.source, r.text, r.priority, r.check_id, r.waived_by, now()],
    )
    .map(|_| ())
}

/// Only a person waives a requirement, with a reason.
pub fn waive_requirement(db: &Db, repo: &str, pr: i64, id: &str, person: &str, reason: &str) -> Result<(), String> {
    if person.trim().is_empty() || reason.trim().is_empty() {
        return Err("a waiver names the person and the reason".into());
    }
    let n = db.exec("UPDATE pipeline_requirements SET waived_by=?4, waiver_reason=?5 WHERE repo=?1 AND pr=?2 AND id=?3", params![repo, pr, id, person, reason])?;
    if n == 0 {
        return Err(format!("no requirement {id}"));
    }
    Ok(())
}

pub fn requirements(db: &Db, repo: &str, pr: i64) -> Vec<Requirement> {
    let _ = init(db);
    db.query("SELECT id,source,text,priority,check_id,waived_by FROM pipeline_requirements WHERE repo=?1 AND pr=?2 ORDER BY added,id", params![repo, pr])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Requirement>(v).ok())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: i64,
    pub origin: String,
    pub open: bool,
    pub text: String,
}

pub fn add_finding(db: &Db, repo: &str, pr: i64, f: &Finding, by: &str) -> Result<(), String> {
    init(db)?;
    db.exec(
        "INSERT OR REPLACE INTO pipeline_findings(repo,pr,id,severity,origin,open,text,by,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![repo, pr, f.id, f.severity, f.origin, f.open, f.text, by, now()],
    )
    .map(|_| ())
}

pub fn findings(db: &Db, repo: &str, pr: i64) -> Vec<Finding> {
    let _ = init(db);
    db.query("SELECT id,severity,origin,open,text FROM pipeline_findings WHERE repo=?1 AND pr=?2", params![repo, pr])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mut v| {
            v["open"] = Value::Bool(v["open"].as_i64() == Some(1));
            serde_json::from_value::<Finding>(v).ok()
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Escape {
    pub id: String,
    pub buggy_sha: String,
    pub fix_pr: Option<i64>,
    pub check_id: Option<String>,
    pub text: String,
}

/// A bug found after landing. The pull request that fixes it must add a check
/// that fails on `buggy_sha` and passes on the fix (invariant 8).
pub fn add_escape(db: &Db, repo: &str, e: &Escape) -> Result<(), String> {
    if !is_sha(&e.buggy_sha) {
        return Err("an escaped bug names the full SHA it shipped in".into());
    }
    init(db)?;
    db.exec(
        "INSERT OR REPLACE INTO pipeline_escapes(repo,id,buggy_sha,fix_pr,check_id,text,found) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![repo, e.id, e.buggy_sha, e.fix_pr, e.check_id, e.text, now()],
    )
    .map(|_| ())
}

pub fn escapes_fixed_by(db: &Db, repo: &str, pr: i64) -> Vec<Escape> {
    let _ = init(db);
    db.query("SELECT id,buggy_sha,fix_pr,check_id,text FROM pipeline_escapes WHERE repo=?1 AND fix_pr=?2", params![repo, pr])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Escape>(v).ok())
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn db() -> Db {
        Db::open(&std::env::temp_dir().join(format!("philium-pipe-{}.db", new_id()))).unwrap()
    }
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn invariant_1_every_result_is_tied_to_exact_shas_and_a_new_head_makes_old_results_stale() {
        let db = db();
        // Short or abbreviated SHAs are refused outright.
        assert!(start_run(&db, &RunKey { repo: "r", pr: Some(1), tier: "fast", head: "abc123", base: B, candidate: A, pinned: true }, "f").is_err());
        let run = start_run(&db, &RunKey { repo: "r", pr: Some(1), tier: "fast", head: A, base: B, candidate: A, pinned: true }, "f").unwrap();
        // A result must be for the SHA its side is about.
        assert!(record(&db, &run, "unit", Side::Candidate, B, Status::Pass, None, Some(0), None, "").is_err());
        assert!(record(&db, &run, "unit", Side::Base, A, Status::Pass, None, Some(0), None, "").is_err());
        record(&db, &run, "unit", Side::Candidate, A, Status::Pass, None, Some(0), Some(5), "ok").unwrap();
        record(&db, &run, "unit", Side::Base, B, Status::Fail, None, Some(1), Some(5), "").unwrap();
        let r = results(&db, "r", Some(1), "fast", A).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].2.sha, A);
        // A newer head arrives: everything recorded for the old head no longer counts.
        see_head(&db, "r", 1, C).unwrap();
        assert!(results(&db, "r", Some(1), "fast", A).unwrap().is_empty());
        assert_eq!(current_head(&db, "r", 1).as_deref(), Some(C));
    }

    #[test]
    fn inputs_are_attributed_and_waivers_need_a_person_and_a_reason() {
        let db = db();
        add_requirement(&db, "r", 1, &Requirement { id: "UPG-01".into(), source: "pr_comments".into(), text: "Upgrade keeps data".into(), priority: "P0".into(), check_id: None, waived_by: None }).unwrap();
        assert!(waive_requirement(&db, "r", 1, "UPG-01", "", "x").is_err());
        assert!(waive_requirement(&db, "r", 1, "nope", "filip", "x").is_err());
        waive_requirement(&db, "r", 1, "UPG-01", "filip", "covered by the release check").unwrap();
        assert_eq!(requirements(&db, "r", 1)[0].waived_by.as_deref(), Some("filip"));
        add_finding(&db, "r", 1, &Finding { id: "B01".into(), severity: 0, origin: "introduced".into(), open: true, text: "leak".into() }, "heron").unwrap();
        assert!(findings(&db, "r", 1)[0].open);
        assert!(add_escape(&db, "r", &Escape { id: "e1".into(), buggy_sha: "short".into(), fix_pr: Some(2), check_id: None, text: "x".into() }).is_err());
    }
}
