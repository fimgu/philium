//! One tier on one candidate: run every check, rerun failures, run what still
//! fails on the base with identical fixtures, record it all, then compute the
//! verdict from the records.
use super::config::{Gate, Pipeline, Tier};
use super::records::{self, RunKey, Side};
use super::runner::{self, Host};
use super::verdict::{self, Verdict};
use crate::db::Db;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

pub struct Target<'a> {
    /// How the records name the repo (from its remote, never typed into code).
    pub key: &'a str,
    pub path: &'a Path,
    pub pr: Option<i64>,
    pub head: &'a str,
    pub base: &'a str,
    pub candidate: &'a str,
}

fn label() -> String {
    format!("philium-{}", &crate::db::new_id()[..8]).to_ascii_lowercase()
}

/// Run `checks` on one SHA in a fresh checkout with fresh services.
/// The files a change adds or edits, from its base to its candidate. Every side of
/// a run gets this same list, so the base is checked over the same scope.
pub fn changed_files(repo: &Path, base: &str, candidate: &str) -> Result<Vec<String>, String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(["diff", "--name-only", "--diff-filter=ACMR", &format!("{base}...{candidate}")]).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!("could not list the change's files: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn side(p: &Pipeline, checks: &[&super::config::Check], repo: &Path, sha: &str, base: &str, changed: &[String], gate_dir: &Path, overlay: Option<(&str, &[String])>, host: &Host, say: &(dyn Fn(String) + Sync)) -> Result<Vec<(String, runner::Outcome)>, String> {
    let label = label();
    let root = host.scratch.join(&label);
    let work = root.join("src");
    let rundir = root.join("run");
    std::fs::create_dir_all(&rundir).map_err(|e| e.to_string())?;
    let result = async {
        runner::checkout(repo, sha, &work)?;
        if let Some((from, paths)) = overlay {
            runner::overlay(&work, from, paths)?;
        }
        let needed: BTreeSet<String> = checks.iter().flat_map(|c| c.needs.iter().cloned()).collect();
        if !needed.is_empty() {
            say(format!("starting {} on {}", needed.iter().cloned().collect::<Vec<_>>().join(", "), &sha[..12]));
        }
        let list = rundir.join("changed.txt");
        std::fs::write(&list, changed.iter().map(|f| format!("{f}\n")).collect::<String>()).map_err(|e| e.to_string())?;
        let extra: HashMap<String, String> = [("sha", sha.to_string()), ("base", base.to_string()), ("gate", gate_dir.display().to_string()), ("changed", list.display().to_string())].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let services = runner::up(p, &needed, &label, &work, &rundir, host, &extra).await;
        let pre = runner::preflight(&work, &rundir, Some(gate_dir), host).await;
        if let Err(e) = &pre {
            say(e.clone());
        }
        let mut out = vec![];
        for c in checks {
            say(format!("{} on {}", c.id, &sha[..12]));
            let on_host = p.platform_for(c, &host.platform) == host.platform;
            let o = match (&pre, on_host) {
                (Err(e), true) => runner::Outcome { status: super::records::Status::NotRun, reason: Some(e.clone()), exit_code: None, duration_ms: 0, output: String::new() },
                _ => runner::run_check(p, c, &work, &rundir, &services, host).await,
            };
            say(format!("{} on {}: {}{}", c.id, &sha[..12], o.status.as_str(), o.reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()));
            out.push((c.id.clone(), o));
        }
        services.down().await;
        Ok::<_, String>(out)
    }
    .await;
    std::fs::remove_dir_all(&root).ok();
    result
}

fn store(db: &Db, run: &str, side_: Side, sha: &str, outs: &[(String, runner::Outcome)]) -> Result<(), String> {
    for (id, o) in outs {
        records::record(db, run, id, side_, sha, o.status, o.reason.as_deref(), o.exit_code, Some(o.duration_ms), &o.output)?;
    }
    Ok(())
}

pub async fn run_tier(db: &Db, t: &Target<'_>, g: &Gate, tier: Tier, host: &Host, say: &(dyn Fn(String) + Sync)) -> Result<(String, Verdict), String> {
    let (p, config_from) = (&g.pipeline, g.from.as_str());
    let checks: Vec<&super::config::Check> = p.tier(tier).iter().collect();
    let run = records::start_run(db, &RunKey { repo: t.key, pr: t.pr, tier: tier.as_str(), head: t.head, base: t.base, candidate: t.candidate, pinned: g.pinned }, config_from)?;
    let gate_root = host.scratch.join(format!("{}-gate", label()));
    let gate_dir = g.materialize(&gate_root)?;
    let result = sides(db, t, p, &checks, &run, config_from, &gate_dir, host, say).await;
    std::fs::remove_dir_all(&gate_root).ok();
    result?;
    records::finish_run(db, &run)?;
    let v = verdict::for_candidate(db, t.key, t.pr, tier.as_str(), checks.iter().map(|c| c.id.clone()).collect(), t.base, t.candidate)?;
    Ok((run, v))
}

#[allow(clippy::too_many_arguments)]
async fn sides(db: &Db, t: &Target<'_>, p: &Pipeline, checks: &[&super::config::Check], run: &str, config_from: &str, gate_dir: &Path, host: &Host, say: &(dyn Fn(String) + Sync)) -> Result<(), String> {
    let changed = changed_files(t.path, t.base, t.candidate)?;
    let first = side(p, checks, t.path, t.candidate, t.base, &changed, gate_dir, None, host, say).await?;
    store(db, run, Side::Candidate, t.candidate, &first)?;
    // A failure is run again on the same SHA: passing the second time is a flake, not a pass.
    let failed: Vec<&super::config::Check> = checks.iter().copied().filter(|c| first.iter().any(|(id, o)| id == &c.id && o.status.failed())).collect();
    if !failed.is_empty() {
        let again = side(p, &failed, t.path, t.candidate, t.base, &changed, gate_dir, None, host, say).await?;
        store(db, run, Side::Rerun, t.candidate, &again)?;
        // What failed twice runs on the base, same fixtures, to compute its origin.
        let twice: Vec<&super::config::Check> = failed.iter().copied().filter(|c| again.iter().any(|(id, o)| id == &c.id && o.status.failed())).collect();
        if !twice.is_empty() {
            let base = side(p, &twice, t.path, t.base, t.base, &changed, gate_dir, None, host, say).await?;
            store(db, run, Side::Base, t.base, &base)?;
        }
    }
    // A fix for an escaped bug: its check also runs on the SHA the bug shipped in.
    if let Some(pr) = t.pr {
        for e in records::escapes_fixed_by(db, t.key, pr) {
            if let Some(c) = e.check_id.as_deref().and_then(|id| p.check(id)) {
                let outs = side(p, &[c], t.path, &e.buggy_sha, t.base, &changed, gate_dir, None, host, say).await?;
                if let Some((_, o)) = outs.first() {
                    super::replay::remember(db, t.key, config_from, &e.id, &e.buggy_sha, None, &c.id, o.status, None, o.status.failed())?;
                }
            }
        }
    }
    Ok(())
}

/// Summary lines for a person: one per check, then the verdict and why.
pub fn report(v: &Verdict) -> String {
    let mut s = String::new();
    for (id, o) in &v.outcomes {
        let mark = match o {
            verdict::Outcome::Pass => "✅",
            verdict::Outcome::Introduced => "❌",
            _ => "⬜",
        };
        s.push_str(&format!("{mark} {id}: {}\n", serde_json::to_value(o).ok().and_then(|x| x["outcome"].as_str().map(str::to_string)).unwrap_or_default()));
    }
    s.push_str(&format!("gate: {}{}\n", v.state, if v.pinned { "" } else { " (unpinned config: read from a local file, not the base commit; it can never land)" }));
    for r in &v.reasons {
        s.push_str(&format!("  - {r}\n"));
    }
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pipeline::records::Status;
    use crate::pipeline::runner::tests::{host, tmp};

    pub fn git(dir: &Path, args: &[&str]) -> String {
        let o = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A repo whose `check.sh` decides each check's fate from files in the commit.
    pub fn repo(root: &Path) -> (std::path::PathBuf, String) {
        let r = root.join("repo");
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        git(&r, &["config", "user.email", "t@t"]);
        git(&r, &["config", "user.name", "t"]);
        std::fs::write(r.join("check.sh"), "#!/bin/sh\n# fail when a file named after the check exists\n[ ! -e \"broken-$1\" ]\n").unwrap();
        std::fs::write(r.join("flaky.sh"), "#!/bin/sh\n# fails the first time in a given folder, passes after\nf=\"$2/flaky-seen\"; if [ -e \"$f\" ]; then exit 0; fi; touch \"$f\"; exit 1\n").unwrap();
        git(&r, &["add", "."]);
        git(&r, &["commit", "-qm", "base"]);
        let base = git(&r, &["rev-parse", "HEAD"]);
        (r, base)
    }

    pub fn commit(r: &Path, add: &[&str], rm: &[&str]) -> String {
        for f in add {
            std::fs::write(r.join(f), "x").unwrap();
        }
        for f in rm {
            std::fs::remove_file(r.join(f)).unwrap();
        }
        git(r, &["add", "-A"]);
        git(r, &["commit", "-qm", "change"]);
        git(r, &["rev-parse", "HEAD"])
    }

    pub fn pipeline(shared: &Path) -> Pipeline {
        Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}]\ntiers:\n  fast:\n    - {{ id: a, run: sh check.sh a }}\n    - {{ id: b, run: sh check.sh b }}\n    - {{ id: flaky, run: \"sh flaky.sh x {}\" }}\n",
            crate::pipeline::config::host_platform(),
            shared.display()
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn the_base_is_checked_over_the_same_files_as_the_change() {
        let root = tmp("scope");
        let (r, _) = repo(&root);
        // "notes" is already bad on the base; the change edits it and adds a good file.
        std::fs::write(r.join("notes"), "bad\n").unwrap();
        git(&r, &["add", "."]);
        git(&r, &["commit", "-qm", "base"]);
        let base = git(&r, &["rev-parse", "HEAD"]);
        std::fs::write(r.join("notes"), "bad\nmore\n").unwrap();
        std::fs::write(r.join("fresh"), "good\n").unwrap();
        git(&r, &["add", "."]);
        git(&r, &["commit", "-qm", "change"]);
        let head = git(&r, &["rev-parse", "HEAD"]);
        assert_eq!(changed_files(&r, &base, &head).unwrap(), vec!["fresh", "notes"]);
        let p = Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}]\ntiers: {{ fast: [ {{ id: scoped, run: \"while read f; do if [ -e \\\"$f\\\" ] && grep -q bad \\\"$f\\\"; then echo \\\"bad: $f\\\"; exit 1; fi; done < ${{changed}}\" }} ] }}\n",
            crate::pipeline::config::host_platform()
        ))
        .unwrap();
        let mut h = host(&root);
        h.caches = root.join("caches");
        let db = crate::pipeline::records::tests::db();
        let g = Gate { pipeline: p, from: "t".into(), pinned: true, source: crate::pipeline::config::GateSource::Folder({ let f = root.join("gatefolder"); std::fs::create_dir_all(&f).unwrap(); f }) };
        let t = Target { key: "r", path: &r, pr: Some(3), head: &head, base: &base, candidate: &head };
        let (_, v) = run_tier(&db, &t, &g, Tier::Fast, &h, &|_| {}).await.unwrap();
        assert_eq!(v.outcomes[0].1, verdict::Outcome::PreExisting, "the base fails over the same files: {:?}", v.reasons);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_tier_runs_reruns_and_compares_with_the_base_before_deciding() {
        let root = tmp("gate");
        let (r, base0) = repo(&root);
        // The base already breaks b; the change breaks a.
        let base = commit(&r, &["broken-b"], &[]);
        let head = commit(&r, &["broken-a"], &[]);
        let _ = base0;
        let shared = root.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        let mut h = host(&root);
        h.caches = shared.clone();
        let p = pipeline(&shared);
        let db = crate::pipeline::records::tests::db();
        let t = Target { key: "r", path: &r, pr: Some(7), head: &head, base: &base, candidate: &head };
        let g = Gate { pipeline: p, from: "test.yaml".into(), pinned: false, source: crate::pipeline::config::GateSource::Folder({ let f = root.join("gatefolder"); std::fs::create_dir_all(&f).unwrap(); f }) };
        let (_, v) = run_tier(&db, &t, &g, Tier::Fast, &h, &|_| {}).await.unwrap();
        assert!(!v.pinned && report(&v).contains("unpinned config"));
        let o: std::collections::HashMap<_, _> = v.outcomes.iter().cloned().collect();
        assert_eq!(o["a"], verdict::Outcome::Introduced);
        assert_eq!(o["b"], verdict::Outcome::PreExisting);
        assert_eq!(o["flaky"], verdict::Outcome::Flaky);
        assert_eq!(v.state, "fail");
        // Each result names its SHA: the rerun is on the head, the comparison on the base.
        let res = records::results(&db, "r", Some(7), "fast", &head).unwrap();
        assert!(res.iter().any(|(c, s, x)| c == "a" && *s == Side::Base && x.sha == base && x.status == Status::Pass));
        assert!(res.iter().any(|(c, s, x)| c == "a" && *s == Side::Rerun && x.sha == head));
        assert!(!res.iter().any(|(c, s, _)| c == "flaky" && *s == Side::Base), "a flake is not compared with the base");
        assert!(report(&v).contains("❌ a: introduced"));
        std::fs::remove_dir_all(&root).ok();
    }
}
