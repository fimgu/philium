//! What Philium keeps on disk, and the rules that keep it small.
//!
//! Every folder Philium writes has an owner and an end:
//! - a check run's folder ends with the run; a run whose process is gone leaves
//!   nothing behind after the next start;
//! - an agent's folder ends with the agent, and its temp folder is emptied while
//!   the agent is not working (every agent is stopped when Philium starts);
//! - Go build caches Philium owns are shared by every agent and check, and held to
//!   a budget, oldest files first, at most once a day and only while nothing builds;
//! - the database keeps records and drops bulk (`codeindex::tidy`, `Ctx::log_event`).
//!
//! Nothing outside Philium's own folders is measured or removed: in the main app the
//! home folder's caches are the person's, so only the side-by-side Pilot, whose home
//! is private, trims them.
//!
//! rpc `storage.*`:
//! - `status {}` -> `{parts:[{key,label,bytes,partial,rule}], disk:{free,total}, budget_gb, last}`
//! - `clean {}` -> `{text, freed, steps}`
//! - `set_budget {gb}` -> `{budget_gb}`
use crate::ctx::Ctx;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Check runs refuse to start below this much free disk, so a full disk is never read
/// as a failing check.
pub const MIN_FREE_FOR_CHECKS: u64 = 10 << 30;
const DEFAULT_BUDGET_GB: u64 = 10;
const DAY_MS: i64 = 86_400_000;
/// A walk stops after this many entries and says its total is partial.
const WALK_LIMIT: u32 = 2_000_000;

/// Bytes free on the disk holding `dir`, when the system says.
#[cfg(unix)]
pub fn disk_free(dir: &Path) -> Option<u64> {
    disk(dir).map(|d| d.0)
}

#[cfg(not(unix))]
pub fn disk_free(_dir: &Path) -> Option<u64> {
    None
}

/// (free, total) bytes on the disk holding `dir`.
#[cfg(unix)]
fn disk(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let mut d = dir.to_path_buf();
    while !d.exists() {
        d = d.parent()?.to_path_buf();
    }
    let p = std::ffi::CString::new(d.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(p.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64 * s.f_frsize as u64, s.f_blocks as u64 * s.f_frsize as u64))
}

#[cfg(not(unix))]
fn disk(_dir: &Path) -> Option<(u64, u64)> {
    None
}

pub fn gb(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// Disk space a file really takes (like `du`), not its length.
fn used(md: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        md.len()
    }
}

/// Size of a file or folder, never following links. `left` bounds the walk.
fn size(p: &Path, left: &mut u32) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(p) else { return 0 };
    if !md.is_dir() {
        return used(&md);
    }
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    let mut n = used(&md);
    for e in rd.flatten() {
        if *left == 0 {
            break;
        }
        *left -= 1;
        n += size(&e.path(), left);
    }
    n
}

/// Where the repo checks keep their runs, caches and mirrors (the account's home, so
/// Docker can mount run folders).
pub fn checks_root() -> PathBuf {
    crate::pilot::real_home().or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("/var/empty")).join("Library/Caches/philium-pipeline")
}

/// The Pilot's private home, whose caches belong to Philium. None in the main app.
fn private_home() -> Option<PathBuf> {
    crate::pilot::active().then(|| std::env::var_os("HOME").map(PathBuf::from)).flatten()
}

/// The checks' folder and the private home this context cleans. Tests get folders of
/// their own, so they never touch the machine's.
fn places(ctx: &Ctx) -> (PathBuf, Option<PathBuf>) {
    if cfg!(test) {
        return (ctx.data_dir.join("checks"), Some(ctx.data_dir.join("home")));
    }
    let _ = ctx;
    (checks_root(), private_home())
}

struct Part {
    key: &'static str,
    label: &'static str,
    paths: Vec<PathBuf>,
    rule: &'static str,
}

fn parts(ctx: &Ctx) -> Vec<Part> {
    let d = &ctx.data_dir;
    let (checks, home) = places(ctx);
    let mut v = vec![
        Part {
            key: "database",
            label: "Records",
            paths: ["philium.db", "philium.db-wal", "pipeline.db", "pipeline.db-wal"].iter().map(|f| d.join(f)).collect(),
            rule: "Agents, reviews, checks and the code index. Space is given back at start when much of it is free.",
        },
        Part { key: "worktrees", label: "Agent worktrees", paths: vec![ctx.root.join("worktrees")], rule: "One per agent, removed with the agent (Agents, Remove finished)." },
        Part { key: "agents", label: "Agent folders", paths: vec![d.join("agents")], rule: "Each agent's own folder. Its temp folder is emptied when Philium starts; the folder goes with the agent." },
        Part { key: "check_runs", label: "Check runs", paths: vec![checks.join("runs")], rule: "Each run's own copy of the repo, deleted when the run ends." },
        Part { key: "check_caches", label: "Check caches", paths: vec![checks.join("caches")], rule: "Build caches for repo checks. Go's are kept under the budget, oldest files first." },
        Part { key: "mirrors", label: "Repo mirrors", paths: vec![checks.join("mirrors")], rule: "One copy of each repo's history for checks, fetched as needed." },
        Part { key: "previews", label: "Previews", paths: vec![d.join("previews")], rule: "Screens agents captured." },
    ];
    if let Some(h) = home {
        v.push(Part { key: "agent_caches", label: "Agent build caches", paths: vec![h.join("Library/Caches"), h.join(".npm"), h.join(".cache")], rule: "Shared by every agent. Go's are kept under the budget, oldest files first." });
    }
    v
}

fn budget(ctx: &Ctx) -> u64 {
    ctx.setting("storage.budget_gb", &DEFAULT_BUDGET_GB.to_string()).parse().ok().filter(|g: &u64| *g >= 1).unwrap_or(DEFAULT_BUDGET_GB)
}

pub fn status(ctx: &Ctx) -> Value {
    let parts: Vec<Value> = parts(ctx)
        .into_iter()
        .map(|p| {
            let mut left = WALK_LIMIT;
            let bytes: u64 = p.paths.iter().map(|x| size(x, &mut left)).sum();
            json!({ "key": p.key, "label": p.label, "bytes": bytes, "partial": left == 0, "rule": p.rule })
        })
        .collect();
    let (free, total) = disk(&ctx.data_dir).unwrap_or((0, 0));
    let last = ctx.db.kv_get("storage.last").and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    json!({ "parts": parts, "disk": { "free": free, "total": total, "min_free_for_checks": MIN_FREE_FOR_CHECKS }, "budget_gb": budget(ctx), "last": last })
}

/// Is a process with this id alive (or alive but not ours to signal)?
fn alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn remove(p: &Path, freed: &mut u64) -> Result<(), String> {
    let mut left = WALK_LIMIT;
    let n = size(p, &mut left);
    let r = if std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false) { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    r.map_err(|e| format!("{}: {e}", p.display()))?;
    *freed += n;
    Ok(())
}

/// Folders of check runs whose process is gone: `<pid>` from the command line,
/// `app-<pid>` from the app.
fn leftover_runs(runs: &Path, freed: &mut u64, steps: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(runs) else { return };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pid) = name.strip_prefix("app-").unwrap_or(&name).parse::<i32>().ok() else { continue };
        if alive(pid) {
            continue;
        }
        match remove(&e.path(), freed) {
            Ok(()) => n += 1,
            Err(e) => steps.push(format!("Could not remove a check run's folder: {e}")),
        }
    }
    if n > 0 {
        steps.push(format!("Removed {n} folder{} left by check runs that ended without cleaning up", if n == 1 { "" } else { "s" }));
    }
}

/// Folders of agents that no longer exist, and the temp folders of agents not working.
fn agent_folders(ctx: &Ctx, freed: &mut u64, steps: &mut Vec<String>) {
    let dir = ctx.data_dir.join("agents");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let rows = ctx.db.query("SELECT name, state FROM agents", []).unwrap_or_default();
    let state = |name: &str| rows.iter().find(|r| r["name"].as_str() == Some(name)).map(|r| r["state"].as_str().unwrap_or("").to_string());
    let (mut gone, mut emptied) = (0, 0);
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        match state(&name) {
            None => match remove(&e.path(), freed) {
                Ok(()) => gone += 1,
                Err(e) => steps.push(format!("Could not remove a removed agent's folder: {e}")),
            },
            Some(s) if matches!(s.as_str(), "stopped" | "failed") => {
                let tmp = e.path().join("tmp");
                let Ok(inner) = std::fs::read_dir(&tmp) else { continue };
                let before = *freed;
                for f in inner.flatten() {
                    if let Err(e) = remove(&f.path(), freed) {
                        steps.push(format!("Could not empty an agent's temp folder: {e}"));
                    }
                }
                if *freed > before {
                    emptied += 1;
                }
            }
            Some(_) => {}
        }
    }
    if gone > 0 {
        steps.push(format!("Removed {gone} folder{} of agents that no longer exist", if gone == 1 { "" } else { "s" }));
    }
    if emptied > 0 {
        steps.push(format!("Emptied the temp folder{} of {emptied} stopped agent{}", if emptied == 1 { "" } else { "s" }, if emptied == 1 { "" } else { "s" }));
    }
}

/// Go marks its build cache with this README. Caches are found by what they are,
/// not by their folder's name.
const GO_CACHE_README: &str = "This directory holds cached build artifacts from the Go build system.";

fn go_caches(root: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    if std::fs::read_to_string(root.join("README")).is_ok_and(|s| s.starts_with(GO_CACHE_README)) {
        out.push(root.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            go_caches(&e.path(), depth - 1, out);
        }
    }
}

/// Hold Philium's Go build caches to the budget, oldest files first, down to 80% of it.
/// Go treats a missing entry as a miss and rebuilds it, so this is safe while nothing
/// builds; callers make sure of that.
fn trim_go_caches(roots: &[PathBuf], budget: u64, freed: &mut u64, steps: &mut Vec<String>) {
    let mut caches = vec![];
    for r in roots {
        go_caches(r, 3, &mut caches);
    }
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = vec![];
    for c in &caches {
        let Ok(rd) = std::fs::read_dir(c) else { continue };
        for sub in rd.flatten() {
            let Ok(inner) = std::fs::read_dir(sub.path()) else { continue };
            for f in inner.flatten() {
                let Ok(md) = f.metadata() else { continue };
                if md.is_file() {
                    files.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), used(&md), f.path()));
                }
            }
        }
    }
    let total: u64 = files.iter().map(|f| f.1).sum();
    if total <= budget {
        return;
    }
    files.sort_by_key(|f| f.0);
    let goal = budget / 10 * 8;
    let (mut now, mut n) = (total, 0u64);
    for (_, bytes, path) in files {
        if now <= goal {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            now -= bytes;
            *freed += bytes;
            n += bytes;
        }
    }
    steps.push(format!("Trimmed Go build caches from {} to {} (budget {})", gb(total), gb(total - n), gb(budget)));
}

fn cache_roots(ctx: &Ctx) -> Vec<PathBuf> {
    let (checks, home) = places(ctx);
    let mut v = vec![checks.join("caches")];
    if let Some(h) = home {
        v.push(h.join("Library/Caches"));
        v.push(h.join(".cache"));
    }
    v
}

/// Anything building right now: an agent at work or a check run in progress.
fn building(ctx: &Ctx) -> bool {
    let working = ctx.db.query("SELECT id FROM agents WHERE state IN ('provisioning','running','waiting') LIMIT 1", []).map(|r| !r.is_empty()).unwrap_or(true);
    working || crate::pipeline::app::running()
}

/// Apply every rule. `daily` limits the cache trim to once a day (at start).
pub fn clean(ctx: &Ctx, daily: bool) -> Value {
    let (mut freed, mut steps) = (0u64, vec![]);
    leftover_runs(&places(ctx).0.join("runs"), &mut freed, &mut steps);
    agent_folders(ctx, &mut freed, &mut steps);
    let last_trim = ctx.db.kv_get("storage.trimmed_at").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    if daily && crate::db::now() - last_trim < DAY_MS {
        // Trimmed today already.
    } else if building(ctx) {
        steps.push("Build caches were left as they are: something is building. They are trimmed when nothing is.".into());
    } else {
        trim_go_caches(&cache_roots(ctx), budget(ctx) << 30, &mut freed, &mut steps);
        let _ = ctx.db.kv_set("storage.trimmed_at", &crate::db::now().to_string());
    }
    let text = if freed == 0 && steps.is_empty() { "Nothing to clean up".to_string() } else { format!("Freed {}", gb(freed)) };
    let out = json!({ "at": crate::db::now(), "text": text, "freed": freed, "steps": steps });
    let _ = ctx.db.kv_set("storage.last", &out.to_string());
    out
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    let out = clean(ctx, true);
    if out["freed"].as_u64().unwrap_or(0) > 0 || out["steps"].as_array().is_some_and(|s| !s.is_empty()) {
        ctx.log_event("storage_cleaned", None, &out);
    }
    Ok(())
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || status(&c)).await.map_err(|e| e.to_string())
        }
        "clean" => {
            let c = ctx.clone();
            let mut out = tokio::task::spawn_blocking(move || clean(&c, false)).await.map_err(|e| e.to_string())?;
            // The records too: give back space the database no longer uses.
            let c = ctx.clone();
            if let Ok(Ok(Some(said))) = tokio::task::spawn_blocking(move || crate::codeindex::tidy(&c)).await {
                if let Some(s) = out["steps"].as_array_mut() {
                    s.push(json!(said));
                }
            }
            ctx.log_event("storage_cleaned", None, &out);
            Ok(out)
        }
        "set_budget" => {
            let g = args["gb"].as_u64().filter(|g| (1..=1000).contains(g)).ok_or("budget_gb is a whole number of GB from 1 to 1000")?;
            ctx.db.kv_set("setting.storage.budget_gb", &g.to_string())?;
            Ok(json!({ "budget_gb": g }))
        }
        _ => Err(format!("unknown storage.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![b'x'; bytes]).unwrap();
    }

    fn set_mtime(p: &Path, secs_ago: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn every_folder_has_an_owner_and_an_end() {
        let ctx = Ctx::test();
        let runs = ctx.data_dir.join("runs");
        // A run whose process is gone leaves its folder; a live one keeps it.
        let dead = {
            let mut c = std::process::Command::new("true").spawn().unwrap();
            let id = c.id() as i32;
            c.wait().unwrap();
            id
        };
        write(&runs.join(format!("app-{dead}/philium-x/src/file")), 10_000);
        write(&runs.join(format!("{}/philium-y/src/file", std::process::id())), 10_000);
        write(&runs.join("not-a-run/file"), 10);
        let (mut freed, mut steps) = (0, vec![]);
        leftover_runs(&runs, &mut freed, &mut steps);
        assert!(!runs.join(format!("app-{dead}")).exists(), "{steps:?}");
        assert!(runs.join(std::process::id().to_string()).exists(), "a live run keeps its folder");
        assert!(runs.join("not-a-run").exists(), "only folders named for a process are run folders");
        assert!(freed >= 10_000);

        // Agents: a removed agent's folder goes; a stopped agent's temp folder empties;
        // a working agent's stays as it is.
        for (id, name, st) in [("a1", "stopper", "stopped"), ("a2", "worker", "running")] {
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES(?1,?2,'m',?3,0,0)", [id, name, st]).unwrap();
        }
        let agents = ctx.data_dir.join("agents");
        write(&agents.join("stopper/tmp/fakehome/big"), 50_000);
        write(&agents.join("stopper/scratch/keep"), 10);
        write(&agents.join("worker/tmp/in-use"), 10);
        write(&agents.join("long-gone/tmp/x"), 10);
        let (mut freed, mut steps) = (0, vec![]);
        agent_folders(&ctx, &mut freed, &mut steps);
        assert!(!agents.join("long-gone").exists(), "{steps:?}");
        assert!(agents.join("stopper/tmp").exists() && std::fs::read_dir(agents.join("stopper/tmp")).unwrap().next().is_none(), "the temp folder stays, empty");
        assert!(agents.join("stopper/scratch/keep").exists(), "only temp is emptied");
        assert!(agents.join("worker/tmp/in-use").exists(), "a working agent's temp is left alone");
        assert_eq!(steps.len(), 2, "{steps:?}");
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }

    #[test]
    fn go_build_caches_are_held_to_the_budget_oldest_first_and_nothing_else_is_touched() {
        let ctx = Ctx::test();
        let root = ctx.data_dir.join("caches");
        let go = root.join("repo/go-cache");
        write(&go.join("README"), 0);
        std::fs::write(go.join("README"), format!("{GO_CACHE_README}\nSee 'go help cache' for more information.\n")).unwrap();
        // Ten 1 MB entries, the first five old.
        for i in 0..10 {
            let f = go.join(format!("{:02x}/entry{i}-d", i));
            write(&f, 1 << 20);
            set_mtime(&f, if i < 5 { 86_400 * 10 } else { 60 });
        }
        // Something else in the same cache root that is not a Go cache.
        let npm = root.join("npm/_cacache/content/x");
        write(&npm, 3 << 20);
        set_mtime(&npm, 86_400 * 30);
        let (mut freed, mut steps) = (0, vec![]);
        // A budget of 6 MB: down to 80% of it, so the oldest files go first.
        trim_go_caches(&[root.clone()], 6 << 20, &mut freed, &mut steps);
        let left: Vec<usize> = (0..10).filter(|i| go.join(format!("{:02x}/entry{i}-d", i)).exists()).collect();
        assert!(left.iter().all(|i| *i >= 4), "oldest first: {left:?}");
        assert!(left.len() <= 4 && left.len() >= 3, "{left:?}");
        assert!(go.join("README").exists(), "Go's own marker stays");
        assert!(npm.exists(), "a cache that is not Go's is never trimmed");
        assert!(steps[0].contains("budget"), "{steps:?}");
        // Under budget: nothing happens.
        let (mut freed2, mut steps2) = (0, vec![]);
        trim_go_caches(&[root.clone()], 100 << 20, &mut freed2, &mut steps2);
        assert_eq!((freed2, steps2.len()), (0, 0));
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }

    #[test]
    fn clean_reports_what_it_did_and_trims_at_most_daily() {
        let ctx = Ctx::test();
        let first = clean(&ctx, true);
        assert!(first["text"].is_string());
        assert!(ctx.db.kv_get("storage.trimmed_at").is_some());
        let st = status(&ctx);
        assert!(st["parts"].as_array().unwrap().iter().any(|p| p["key"] == "database"));
        assert!(st["disk"]["total"].as_u64().unwrap() > 0);
        assert_eq!(st["budget_gb"], 10);
        assert_eq!(st["last"]["text"], first["text"]);
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }
}

/// Trim a copy of a real Go build cache to see how long it takes:
/// `PHILIUM_TRIM_COPY=/path/to/copy cargo test trim_a_copy -- --ignored --nocapture`.
#[cfg(test)]
mod trim_copy {
    #[test]
    #[ignore]
    fn trim_a_copy() {
        let Some(p) = std::env::var_os("PHILIUM_TRIM_COPY").map(std::path::PathBuf::from) else { return };
        let t = std::time::Instant::now();
        let (mut freed, mut steps) = (0, vec![]);
        super::trim_go_caches(&[p], super::DEFAULT_BUDGET_GB << 30, &mut freed, &mut steps);
        println!("trim: {steps:?}, freed {} in {:.1} s", super::gb(freed), t.elapsed().as_secs_f64());
    }
}
