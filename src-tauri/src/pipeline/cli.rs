//! `philium run <tier>`, `philium replay`, `philium simulate`, `philium land`.
//! Runs without the window, so a self-hosted runner or a person in a terminal
//! gets the same gate as the app.
use super::config::{self, Tier};
use super::{gate, land, records, replay, runner, simulate, verdict};
use crate::db::Db;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const USAGE: &str = "Usage:
  philium run <fast|full|release> --repo PATH --head SHA --base SHA [--candidate SHA] [--pr N] [--config FILE] [--data DIR] [--json]
  philium replay --repo PATH [--at SHA] [--config FILE] [--replay FILE] [--data DIR] [--json]
  philium simulate --prs N --fail-rate P [--batch-max N] [--seed N] [--json]
  philium land --repo PATH --remote NAME --base SHA --candidate SHA [--pr N] [--tier full] [--config FILE] [--data DIR] [--push]
  philium enroll --repo PATH --environment NAME --by PERSON [--data DIR]
  philium lend --repo PATH --name NAME --path FOLDER --by PERSON [--data DIR]";

/// Some(exit code) when the arguments are a pipeline command.
pub fn dispatch() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first()?.clone();
    if !matches!(cmd.as_str(), "run" | "replay" | "simulate" | "land" | "enroll" | "lend") {
        return None;
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().ok()?;
    Some(match rt.block_on(main(&cmd, &args[1..])) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("philium {cmd}: {e}\n\n{USAGE}");
            3
        }
    })
}

struct Args {
    pos: Vec<String>,
    opts: HashMap<String, String>,
    flags: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { pos: vec![], opts: HashMap::new(), flags: vec![] };
    let mut i = 0;
    while i < args.len() {
        let s = &args[i];
        if let Some(k) = s.strip_prefix("--") {
            if matches!(k, "json" | "push") {
                a.flags.push(k.into());
            } else {
                let v = args.get(i + 1).ok_or_else(|| format!("--{k} needs a value"))?;
                a.opts.insert(k.into(), v.clone());
                i += 1;
            }
        } else {
            a.pos.push(s.clone());
        }
        i += 1;
    }
    Ok(a)
}

impl Args {
    fn get(&self, k: &str) -> Result<&str, String> {
        self.opts.get(k).map(String::as_str).ok_or_else(|| format!("--{k} is required"))
    }
    fn path(&self, k: &str) -> Option<PathBuf> {
        self.opts.get(k).map(PathBuf::from)
    }
    fn json(&self) -> bool {
        self.flags.iter().any(|f| f == "json")
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let o = std::process::Command::new("git").arg("-C").arg(repo).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A commit-ish to the full SHA of a commit that exists in the repo.
fn sha(repo: &Path, r: &str) -> Result<String, String> {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]).map_err(|_| format!("{r} is not a commit in {}", repo.display()))
}

/// How records name the repo: its remote's owner/name, else its folder name.
pub fn repo_key(repo: &Path) -> String {
    let url = git(repo, &["config", "--get", "remote.origin.url"]).unwrap_or_default();
    let tail = url.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = tail.rsplit(['/', ':']).take(2).collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return format!("{}/{}", parts[1], parts[0]);
    }
    repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into())
}

fn data(a: &Args) -> Result<Db, String> {
    let dir = a.path("data").or_else(|| std::env::var_os("PHILIUM_DATA").map(PathBuf::from)).or_else(crate::pilot::data_dir).ok_or("--data names where the records go")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let db = Db::open(&dir.join("pipeline.db"))?;
    records::init(&db)?;
    Ok(db)
}

pub fn host(db: &'static Db, repo: &str) -> Result<runner::Host, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    // Under the home folder so Docker (Colima shares home) can mount run folders;
    // the sandbox still hides the rest of home from checks.
    let root = home.join("Library/Caches/philium-pipeline");
    let scratch = root.join("runs").join(format!("{}", std::process::id()));
    let caches = root.join("caches").join(repo.replace('/', "-"));
    let (r1, r2) = (repo.to_string(), repo.to_string());
    Ok(runner::Host {
        scratch,
        caches,
        tools_home: home.clone(),
        home,
        secret: Box::new(move |n| crate::secrets::get(&format!("pipeline.{r1}.{n}"))),
        enrolled: Box::new(move |e| records::enrolled(db, &r2, e)),
        lent: {
            let r3 = repo.to_string();
            Box::new(move |l| records::lent(db, &r3, l))
        },
        platform: config::host_platform(),
        min_free: crate::storage::MIN_FREE_FOR_CHECKS,
    })
}

fn say(json: bool) -> impl Fn(String) + Sync {
    move |s: String| {
        if !json {
            eprintln!("  {s}");
        }
    }
}

async fn main(cmd: &str, rest: &[String]) -> Result<i32, String> {
    let a = parse(rest)?;
    if cmd == "simulate" {
        let s = simulate::Sim {
            prs: a.get("prs")?.parse().map_err(|_| "--prs is a number")?,
            fail_rate: a.get("fail-rate")?.parse().map_err(|_| "--fail-rate is a fraction such as 0.1")?,
            batch_max: a.opts.get("batch-max").map(|v| v.parse()).transpose().map_err(|_| "--batch-max is a number")?.unwrap_or(4),
            seed: a.opts.get("seed").map(|v| v.parse()).transpose().map_err(|_| "--seed is a number")?.unwrap_or(1),
        };
        let r = simulate::simulate(&s);
        if a.json() {
            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        } else {
            println!("{} changes, {} landed, {} sent back, {} gate runs ({:.2} per change), mean batch {:.1}; wrongly landed {}, wrongly sent back {}", r.prs, r.landed, r.ejected, r.runs, r.runs_per_pr, r.mean_batch, r.wrongly_landed, r.wrongly_ejected);
        }
        return Ok(if r.wrongly_landed + r.wrongly_ejected == 0 { 0 } else { 1 });
    }
    let repo = PathBuf::from(a.get("repo")?);
    let key = repo_key(&repo);
    let db: &'static Db = Box::leak(Box::new(data(&a)?));
    if cmd == "lend" {
        let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
        let raw = a.get("path")?;
        let path = match raw.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(raw),
        };
        records::lend(db, &key, a.get("name")?, &path, a.get("by")?, &home)?;
        println!("{} is lent read-only to {key}'s checks", a.get("name")?);
        return Ok(0);
    }
    if cmd == "enroll" {
        records::enroll(db, &key, a.get("environment")?, a.get("by")?)?;
        println!("{} is enrolled read-only for {key}", a.get("environment")?);
        return Ok(0);
    }
    let host = host(db, &key)?;
    let out = match cmd {
        "run" => {
            let tier = Tier::parse(a.pos.first().ok_or("name the tier: fast, full or release")?)?;
            let head = sha(&repo, a.get("head")?)?;
            let base = sha(&repo, a.get("base")?)?;
            let candidate = match a.opts.get("candidate") {
                Some(c) => sha(&repo, c)?,
                None => head.clone(),
            };
            let pr = a.opts.get("pr").map(|v| v.parse::<i64>()).transpose().map_err(|_| "--pr is a number")?;
            let g = config::load(&repo, &base, a.path("config").as_deref())?;
            eprintln!("{key}: tier {} on {} (base {}), gate from {}{}", tier.as_str(), &candidate[..12], &base[..12], g.from, if g.pinned { "" } else { " (unpinned config)" });
            let t = gate::Target { key: &key, path: &repo, pr, head: &head, base: &base, candidate: &candidate };
            let (run, v) = gate::run_tier(db, &t, &g, tier, &host, &say(a.json())).await?;
            if a.json() {
                println!("{}", serde_json::json!({"run": run, "verdict": v}));
            } else {
                print!("{}", gate::report(&v));
            }
            code(&v)
        }
        "replay" => {
            let at = sha(&repo, a.opts.get("at").map(String::as_str).unwrap_or("HEAD"))?;
            let g = config::load(&repo, &at, a.path("config").as_deref())?;
            let (p, from) = (&g.pipeline, &g.from);
            let (set, text) = replay::load(&repo, &at, a.path("replay").as_deref(), p)?;
            let cfg_text = serde_yaml::to_string(p).unwrap_or_default();
            let digest = replay::digest(&[&cfg_text, &text]);
            eprintln!("{key}: replaying {} known bugs against the gate from {from}", set.bugs.len());
            let gate_dir = g.materialize(&host.scratch.join("gate"))?;
            let r = replay::run(db, &key, &repo, p, &set, &digest, &gate_dir, &host, &say(a.json())).await?;
            if a.json() {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            } else {
                for b in &r.bugs {
                    println!("{} {}: {} on the bug{}{}", if b.caught { "✅" } else { "❌" }, b.id, b.buggy.as_str(), b.fixed.map(|f| format!(", {} on the control", f.as_str())).unwrap_or_default(), b.reason.as_deref().map(|x| format!(" ({x})")).unwrap_or_default());
                }
                println!("catch rate: {}/{} ({:.0}%)", r.caught, r.total, r.rate * 100.0);
                if let Some(b) = r.rejected_below {
                    println!("rejected: the previous gate caught {:.0}%", b * 100.0);
                }
            }
            if r.rejected_below.is_some() {
                1
            } else {
                0
            }
        }
        "land" => {
            let base = sha(&repo, a.get("base")?)?;
            let candidate = sha(&repo, a.get("candidate")?)?;
            let pr = a.opts.get("pr").map(|v| v.parse::<i64>()).transpose().map_err(|_| "--pr is a number")?;
            let g = config::load(&repo, &base, a.path("config").as_deref())?;
            let p = &g.pipeline;
            let tier = Tier::parse(a.opts.get("tier").map(String::as_str).unwrap_or("full"))?;
            let v = verdict::for_candidate(db, &key, pr, tier.as_str(), p.tier(tier).iter().map(|c| c.id.clone()).collect(), &base, &candidate)?;
            let plan = land::land(db, &key, &repo, a.get("remote")?, &p.branch, &base, &candidate, &v, a.flags.iter().any(|f| f == "push"))?;
            println!("{}", if plan.pushed { format!("landed: {} is now {}", plan.branch, plan.to) } else { format!("would run: {}", plan.argv.join(" ")) });
            0
        }
        _ => 3,
    };
    std::fs::remove_dir_all(&host.scratch).ok();
    Ok(out)
}

fn code(v: &verdict::Verdict) -> i32 {
    match v.state {
        "pass" => 0,
        "fail" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repo_key_comes_from_the_remote_not_from_code() {
        let root = crate::pipeline::runner::tests::tmp("key");
        let g = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&root).args(args).output().unwrap();
        g(&["init", "-q"]);
        assert_eq!(repo_key(&root), root.file_name().unwrap().to_string_lossy());
        g(&["remote", "add", "origin", "git@github.com:some-org/some-repo.git"]);
        assert_eq!(repo_key(&root), "some-org/some-repo");
        g(&["remote", "set-url", "origin", "https://github.com/other/thing"]);
        assert_eq!(repo_key(&root), "other/thing");
        let a = parse(&["fast".into(), "--repo".into(), "/x".into(), "--json".into()]).unwrap();
        assert_eq!((a.pos[0].as_str(), a.get("repo").unwrap(), a.json()), ("fast", "/x", true));
        assert!(parse(&["--repo".into()]).is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
