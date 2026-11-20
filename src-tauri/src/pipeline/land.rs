//! Invariant 2, tested = landed: the branch only ever moves to the exact SHA the
//! gate passed, and only if the branch is still where the candidate was built from.
//! Nothing is pushed unless asked; by default this only says what it would do.
use super::records;
use super::verdict::Verdict;
use crate::db::Db;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Plan {
    pub remote: String,
    pub branch: String,
    pub from: String,
    pub to: String,
    pub argv: Vec<String>,
    pub pushed: bool,
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null"]).args(args).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// Move `branch` on `remote` from `base` to `candidate`, if and only if the gate
/// passed on exactly `candidate`. `verdict` is computed by the caller from records.
pub fn land(db: &Db, repo_key: &str, repo: &Path, remote: &str, branch: &str, base: &str, candidate: &str, verdict: &Verdict, push: bool) -> Result<Plan, String> {
    let _ = db;
    let _ = repo_key;
    for (what, s) in [("base", base), ("candidate", candidate)] {
        if !records::is_sha(s) {
            return Err(format!("landing needs the full {what} SHA"));
        }
    }
    if !verdict.pass() {
        return Err(format!("not landing {}: the gate is {} ({})", &candidate[..12], verdict.state, verdict.reasons.join("; ")));
    }
    // The candidate must contain the base: landing is a fast-forward, never a rewrite.
    git(repo, &["merge-base", "--is-ancestor", base, candidate]).map_err(|_| format!("{} does not contain the base {}; rebuild the candidate", &candidate[..12], &base[..12]))?;
    let argv: Vec<String> = ["git", "push", "--porcelain", remote, &format!("{candidate}:refs/heads/{branch}"), &format!("--force-with-lease=refs/heads/{branch}:{base}")].iter().map(|s| s.to_string()).collect();
    let mut plan = Plan { remote: remote.into(), branch: branch.into(), from: base.into(), to: candidate.into(), argv, pushed: false };
    if !push {
        return Ok(plan);
    }
    let args: Vec<&str> = plan.argv[1..].iter().map(String::as_str).collect();
    git(repo, &args).map_err(|e| format!("the branch moved since the candidate was built, so nothing landed; rebuild the candidate ({e})"))?;
    plan.pushed = true;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::gate::tests::{commit, git as g, repo};
    use crate::pipeline::records::{record, start_run, RunKey, Side, Status};
    use crate::pipeline::runner::tests::tmp;
    use crate::pipeline::verdict;

    #[test]
    fn invariant_2_the_branch_only_moves_to_the_exact_sha_that_passed() {
        let root = tmp("land");
        let (r, base) = repo(&root);
        let remote = root.join("remote.git");
        std::process::Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&remote).output().unwrap();
        g(&r, &["remote", "add", "up", remote.to_str().unwrap()]);
        g(&r, &["push", "-q", "up", "main"]);
        let candidate = commit(&r, &["feature"], &[]);
        let other = commit(&r, &["untested"], &[]);
        let db = crate::pipeline::records::tests::db();
        let decide = |cand: &str| verdict::for_candidate(&db, "r", Some(1), "full", vec!["unit".into()], &base, cand).unwrap();
        let tip = || g(&remote, &["rev-parse", "refs/heads/main"]);
        // No passing record for the candidate: nothing moves.
        assert!(land(&db, "r", &r, "up", "main", &base, &candidate, &decide(&candidate), true).unwrap_err().contains("gate is blocked"));
        assert_eq!(tip(), base);
        let run = start_run(&db, &RunKey { repo: "r", pr: Some(1), tier: "full", head: &other, base: &base, candidate: &candidate }, "t").unwrap();
        record(&db, &run, "unit", Side::Candidate, &candidate, Status::Pass, None, Some(0), None, "").unwrap();
        // A pass on the candidate says nothing about another SHA.
        assert!(land(&db, "r", &r, "up", "main", &base, &other, &decide(&other), true).is_err());
        // Without asking to push, it only says what it would do.
        let plan = land(&db, "r", &r, "up", "main", &base, &candidate, &decide(&candidate), false).unwrap();
        assert!(!plan.pushed && plan.argv.contains(&format!("--force-with-lease=refs/heads/main:{base}")));
        assert_eq!(tip(), base);
        // Someone else moves the branch first: the lease refuses, nothing lands.
        let side = root.join("side");
        std::process::Command::new("git").arg("clone").arg("-q").arg(&remote).arg(&side).output().unwrap();
        g(&side, &["config", "user.email", "t@t"]);
        g(&side, &["config", "user.name", "t"]);
        std::fs::write(side.join("theirs"), "x").unwrap();
        g(&side, &["add", "."]);
        g(&side, &["commit", "-qm", "theirs"]);
        g(&side, &["push", "-q", "origin", "main"]);
        let moved = tip();
        assert!(land(&db, "r", &r, "up", "main", &base, &candidate, &decide(&candidate), true).unwrap_err().contains("branch moved"));
        assert_eq!(tip(), moved);
        // Put the branch back where the candidate was built: now it lands, exactly there.
        g(&side, &["push", "-q", "--force", "origin", &format!("{base}:refs/heads/main")]);
        let plan = land(&db, "r", &r, "up", "main", &base, &candidate, &decide(&candidate), true).unwrap();
        assert!(plan.pushed);
        assert_eq!(tip(), candidate);
        // A candidate that does not contain the base is never pushed.
        let fresh = g(&r, &["commit-tree", "-m", "orphan", &g(&r, &["rev-parse", "HEAD^{tree}"])]);
        let run2 = start_run(&db, &RunKey { repo: "r", pr: None, tier: "full", head: &fresh, base: &candidate, candidate: &fresh }, "t").unwrap();
        record(&db, &run2, "unit", Side::Candidate, &fresh, Status::Pass, None, Some(0), None, "").unwrap();
        let v = verdict::for_candidate(&db, "r", None, "full", vec!["unit".into()], &candidate, &fresh).unwrap();
        assert!(land(&db, "r", &r, "up", "main", &candidate, &fresh, &v, true).unwrap_err().contains("does not contain the base"));
        std::fs::remove_dir_all(&root).ok();
    }
}
