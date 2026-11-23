//! Human-driven Git publish steps for a worktree: commit selected paths, push the
//! branch, open a pull request. Each is a separate RPC the user confirms in the UI.
//! Runs the user's own git/gh on the host; nothing here reaches agents or HAL.
//!
//! - `commit {paths, message, expect_head}`: stages exactly `paths` (argv-separated) and
//!   commits only those paths. Rejects when HEAD moved since the panel was loaded.
//! - `push {}`: pushes the current branch to its upstream remote, or the only remote,
//!   with normal fast-forward rules. Never forces.
//! - `pr {base, title, body}`: returns the existing PR for this head if there is one,
//!   otherwise runs `gh pr create`.
use serde_json::{json, Value};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(120);

fn run(bin: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    run_raw(bin, dir, args).map(|s| s.trim().to_string())
}

fn run_raw(bin: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run {bin}: {e}"))?;
    let start = Instant::now();
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{bin} took too long and was stopped."));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err: String = String::from_utf8_lossy(&out.stderr).trim().chars().take(2000).collect();
        return Err(if err.is_empty() { format!("{bin} failed") } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run("git", dir, args)
}

/// Branch, HEAD, remote and changed paths for the publish panel.
pub(crate) fn state(dir: &Path) -> Result<Value, String> {
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
    let head = git(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let status = run_raw("git", dir, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let files: Vec<Value> = status
        .split('\0')
        .filter(|l| l.len() > 3)
        .map(|l| json!({"status": l[..2].trim(), "path": &l[3..]}))
        .collect();
    let remote = remote_for(dir, &branch).ok();
    let ahead = remote
        .as_ref()
        .and_then(|r| git(dir, &["rev-list", "--count", &format!("{r}/{branch}..HEAD")]).ok());
    Ok(json!({"branch": branch, "head": head, "files": files, "remote": remote, "ahead": ahead}))
}

fn remote_for(dir: &Path, branch: &str) -> Result<String, String> {
    if let Ok(r) = git(dir, &["config", "--get", &format!("branch.{branch}.remote")]) {
        if !r.is_empty() {
            return Ok(r);
        }
    }
    let remotes = git(dir, &["remote"])?;
    let list: Vec<&str> = remotes.lines().filter(|l| !l.is_empty()).collect();
    match list.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err("This repository has no remote to push to.".into()),
        _ => Err("This branch has no upstream and the repository has several remotes. Set an upstream first.".into()),
    }
}

pub(crate) fn commit(dir: &Path, paths: &[String], message: &str, expect_head: Option<&str>) -> Result<Value, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Write a commit message first.".into());
    }
    if paths.is_empty() {
        return Err("Choose at least one file to commit.".into());
    }
    if paths.iter().any(|p| p.is_empty() || p.starts_with('-') || p.split('/').any(|s| s == "..")) {
        return Err("One of the chosen paths is not valid.".into());
    }
    if let Some(expect) = expect_head.filter(|s| !s.is_empty()) {
        if git(dir, &["rev-parse", "HEAD"]).unwrap_or_default() != expect {
            return Err("The branch changed since you opened this. Refresh and review again.".into());
        }
    }
    let mut add = vec!["add", "--all", "--"];
    add.extend(paths.iter().map(String::as_str));
    git(dir, &add)?;
    let mut c = vec!["commit", "--quiet", "-m", message, "--"];
    c.extend(paths.iter().map(String::as_str));
    git(dir, &c)?;
    Ok(json!({"head": git(dir, &["rev-parse", "HEAD"])?}))
}

pub(crate) fn push(dir: &Path) -> Result<Value, String> {
    let branch = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "Not on a branch, so there is nothing to push.".to_string())?;
    let remote = remote_for(dir, &branch)?;
    git(dir, &["push", "--set-upstream", "--", &remote, &format!("refs/heads/{branch}:refs/heads/{branch}")])?;
    Ok(json!({"remote": remote, "branch": branch}))
}

/// `gh pr create` argv for a head branch. Separate so it can be checked without `gh`.
pub(crate) fn pr_create_args<'a>(base: &'a str, head: &'a str, title: &'a str, body: &'a str) -> Vec<&'a str> {
    vec!["pr", "create", "--base", base, "--head", head, "--title", title, "--body", body]
}

pub(crate) fn pr(dir: &Path, base: &str, title: &str, body: &str) -> Result<Value, String> {
    if title.trim().is_empty() || base.trim().is_empty() {
        return Err("A pull request needs a title and a base branch.".into());
    }
    let head = git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "Not on a branch.".to_string())?;
    if let Ok(existing) = run("gh", dir, &["pr", "view", &head, "--json", "url,state"]) {
        if let Ok(v) = serde_json::from_str::<Value>(&existing) {
            if v["state"] == "OPEN" {
                return Ok(json!({"url": v["url"], "existing": true}));
            }
        }
    }
    let url = run("gh", dir, &pr_create_args(base, &head, title.trim(), body))?;
    Ok(json!({"url": url.lines().last().unwrap_or(""), "existing": false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(tag: &str) -> Tmp {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "philium-publish-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    #[test]
    fn commit_only_chosen_paths_then_push() {
        let remote = tmp("remote");
        git(&remote.0, &["init", "-q", "--bare"]).unwrap();
        let w = tmp("work");
        let p = &w.0;
        for a in [&["init", "-q", "-b", "feature"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.test"], &["config", "commit.gpgsign", "false"]] {
            git(p, a).unwrap();
        }
        git(p, &["remote", "add", "origin", remote.0.to_str().unwrap()]).unwrap();
        std::fs::write(p.join("a.txt"), "a").unwrap();
        git(p, &["add", "a.txt"]).unwrap();
        git(p, &["commit", "-qm", "init"]).unwrap();
        std::fs::write(p.join("keep.txt"), "k").unwrap();
        std::fs::write(p.join("other.txt"), "o").unwrap();
        let s = state(p).unwrap();
        assert_eq!(s["files"].as_array().unwrap().len(), 2);
        let head = s["head"].as_str().unwrap().to_string();

        assert!(commit(p, &[], "m", None).is_err());
        assert!(commit(p, &["keep.txt".into()], " ", None).is_err());
        assert!(commit(p, &["--all".into()], "m", None).is_err());
        assert!(commit(p, &["keep.txt".into()], "m", Some("deadbeef")).unwrap_err().contains("changed"));
        commit(p, &["keep.txt".into()], "Add keep", Some(&head)).unwrap();
        let left = state(p).unwrap();
        assert_eq!(left["files"][0]["path"], "other.txt");

        let r = push(p).unwrap();
        assert_eq!(r["remote"], "origin");
        assert_eq!(git(&remote.0, &["log", "-1", "--format=%s", "feature"]).unwrap(), "Add keep");
        assert_eq!(state(p).unwrap()["ahead"], "0");
    }

    #[test]
    fn pr_args_are_argv_separated() {
        let a = pr_create_args("main", "feature", "Title; rm -rf", "body");
        assert_eq!(a[5], "feature");
        assert_eq!(a[7], "Title; rm -rf");
    }
}
