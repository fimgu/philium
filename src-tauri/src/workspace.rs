//! Workspace RPC: repos_list, repo_add {path}, repo_clone {url,name?}, repo_remove {id},
//! repo_set {id,integration_branch?,default_branch?}, branches {repo_id}, worktrees {repo_id?},
//! worktree_status {id}, worktree_create {repo_id,name}, worktree_remove {id,force?},
//! diff {worktree_id}, log {repo_id|worktree_id,limit?}, open_in {path,app},
//! relocate {repo_id,path} (moved/renamed repo; must share a root commit or origin).
//! branches rows: {name, commit, upstream, current, date (ms), subject, remote}.
//! repos_list rows add: branch, dirty, remote_url, worktree_count, missing?, error?.
//! All Git mutations share one queue. Events: workspace:changed, plus the workspace change signal.
use crate::{
    ctx::{arg_opt_str, arg_str, Ctx},
    db::{new_id, now},
};
use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

static GIT_QUEUE: Mutex<()> = Mutex::new(());
const DIFF_CAP: usize = 256 * 1024;
#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub id: String,
    pub repo_id: String,
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    for dir in [ctx.root.join("repos"), ctx.root.join("worktrees")] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Root commits identify a repo after it is moved (see `relocate`).
    if let Err(e) = ctx.db.exec("ALTER TABLE repos ADD COLUMN root_commits TEXT", []) {
        if !e.contains("duplicate column") {
            return Err(e);
        }
    }
    Ok(())
}
pub(crate) fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string())
}
fn changed(ctx: &Arc<Ctx>) {
    ctx.emit("workspace:changed", json!({}));
    ctx.notify("workspace");
}
fn safe_name(name: &str) -> Result<&str, String> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        Err("Name must use 1-80 letters, digits, hyphens or underscores".into())
    } else {
        Ok(name)
    }
}
pub fn repo(ctx: &Arc<Ctx>, id: &str) -> Option<Value> {
    ctx.db
        .query("SELECT * FROM repos WHERE id=?1", [id])
        .ok()?
        .into_iter()
        .next()
}
/// The repo's folder, or a clear error when it was deleted, renamed or moved.
pub fn repo_dir(r: &Value) -> Result<PathBuf, String> {
    let p = PathBuf::from(text(r, "path")?);
    if p.is_dir() {
        Ok(p)
    } else {
        Err(format!("Repo folder {} is missing. Remove it or pick its new location", p.display()))
    }
}
/// Identity of a repository: its root commit(s) and origin URL.
fn repo_identity(path: &Path) -> (Vec<String>, Option<String>) {
    let roots = git(path, &["rev-list", "--max-parents=0", "HEAD"])
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    (roots, git(path, &["remote", "get-url", "origin"]).ok())
}
/// Point a moved/renamed repo at its new folder. Only accepted when it is the
/// same repository: a shared root commit, or the same origin remote.
fn relocate(ctx: &Arc<Ctx>, id: &str, path: &str) -> Result<Value, String> {
    let r = repo(ctx, id).ok_or("Unknown repository")?;
    let wanted = expand_home(path);
    let dir = std::fs::canonicalize(&wanted).map_err(|_| format!("No folder at {}", wanted.display()))?;
    let top = git(&dir, &["rev-parse", "--show-toplevel"]).map_err(|_| format!("{} is not inside a Git repository", dir.display()))?;
    let dir = std::fs::canonicalize(top).map_err(|e| e.to_string())?;
    let (roots, remote) = repo_identity(&dir);
    let old_remote = r["remote_url"].as_str().filter(|s| !s.is_empty());
    let same_remote = old_remote.is_some() && old_remote == remote.as_deref();
    let old_roots: Vec<String> = r["root_commits"].as_str().unwrap_or("").split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect();
    let old_path = PathBuf::from(text(&r, "path")?);
    // If the old folder still exists, compare against it directly.
    let old_roots = if old_roots.is_empty() && old_path.is_dir() { repo_identity(&old_path).0 } else { old_roots };
    let same_root = roots.iter().any(|c| old_roots.contains(c));
    if !same_remote && !same_root {
        return Err(format!("{} is a different repository (no shared first commit or origin remote)", dir.display()));
    }
    let ds = dir.to_string_lossy().to_string();
    if ctx.db.query("SELECT id FROM repos WHERE path=?1 AND id!=?2", params![ds, id])?.first().is_some() {
        return Err("Another repo already uses that folder".into());
    }
    ctx.db.exec(
        "UPDATE repos SET path=?2, remote_url=COALESCE(?3, remote_url), root_commits=?4 WHERE id=?1",
        params![id, ds, remote, roots.join(",")],
    )?;
    ctx.log_event("repo_relocated", None, &json!({"id":id,"from":old_path,"to":ds}));
    ctx.emit("workspace:changed", json!({"repo_id":id}));
    ctx.notify("workspace");
    repo(ctx, id).ok_or_else(|| "Unknown repository".into())
}
fn worktree(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    ctx.db.query("SELECT w.*,a.name AS agent_name FROM worktrees w LEFT JOIN agents a ON a.id=w.agent_id WHERE w.id=?1", [id])?.into_iter().next().ok_or_else(|| "Unknown worktree".into())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("Missing {key}"))
}
/// Expand `~` / `~/...` to the home folder; trim stray whitespace and quotes.
pub(crate) fn expand_home(path: &str) -> std::path::PathBuf {
    let p = path.trim().trim_matches(|c| c == '"' || c == '\'');
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match (p, home) {
        ("~", Some(h)) => h,
        (p, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        (p, _) => std::path::PathBuf::from(p),
    }
}
fn add_repo(ctx: &Arc<Ctx>, path: &str) -> Result<Value, String> {
    let wanted = expand_home(path);
    let path = std::fs::canonicalize(&wanted).map_err(|_| format!("No folder at {}", wanted.display()))?;
    let top = git(&path, &["rev-parse", "--show-toplevel"])
        .map_err(|_| format!("{} is not inside a Git repository", path.display()))?;
    let path = std::fs::canonicalize(top).map_err(|e| e.to_string())?;
    let path_str = path.to_string_lossy().to_string();
    if let Some(row) = ctx
        .db
        .query("SELECT * FROM repos WHERE path=?1", [&path_str])?
        .into_iter()
        .next()
    {
        return Ok(row);
    }
    let branch = git(
        &path,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .map(|s| s.trim_start_matches("origin/").to_owned())
    .or_else(|| git(&path, &["symbolic-ref", "--short", "HEAD"]).ok())
    .unwrap_or_else(|| "main".into());
    let remote = git(&path, &["remote", "get-url", "origin"]).ok();
    let id = new_id();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,remote_url,added_at) VALUES(?1,?2,?3,?4,?5,?6)", params![id,name,path_str,branch,remote,now()])?;
    let _ = ctx.db.exec("UPDATE repos SET root_commits=?2 WHERE id=?1", params![id, repo_identity(&path).0.join(",")]);
    changed(ctx);
    repo(ctx, &id).ok_or_else(|| "Repository registration failed".into())
}
pub fn create_worktree(
    ctx: &Arc<Ctx>,
    repo_id: &str,
    agent_id: Option<&str>,
    agent_name: &str,
    base: Option<&str>,
) -> Result<Worktree, String> {
    let _queue = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
    safe_name(agent_name)?;
    if let Some(agent) = agent_id {
        if ctx
            .db
            .query("SELECT id FROM agents WHERE id=?1", [agent])?
            .is_empty()
        {
            return Err("Unknown agent".into());
        }
    }
    let r = repo(ctx, repo_id).ok_or("Unknown repository")?;
    let repo_dir = repo_dir(&r)?;
    let repo_path = repo_dir.as_path();
    let base = base
        .or(r["integration_branch"].as_str())
        .unwrap_or(text(&r, "default_branch")?);
    if base.starts_with('-') {
        return Err("Invalid base reference".into());
    }
    let base_commit = git(
        repo_path,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )?;
    let id = new_id();
    let short = &id[..8];
    let branch = format!("philium/{agent_name}/{short}");
    let folder: String = text(&r, "name")?
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let parent = ctx
        .root
        .join("worktrees")
        .join(if folder.is_empty() { repo_id } else { &folder });
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let path = parent.join(format!("{agent_name}-{short}"));
    let handle = path.to_string_lossy().to_string();
    // Register intent before Git touches disk. Failed attempts remain observable in the ledger.
    let resource = crate::reaper::register(
        ctx,
        agent_id,
        "worktree",
        &handle,
        json!({"worktree_id":id,"repo_id":repo_id,"branch":branch,"creating":true}),
    );
    if let Err(e) = git(
        repo_path,
        &["worktree", "add", "-b", &branch, &handle, &base_commit],
    ) {
        let branch_exists = git(
            repo_path,
            &["show-ref", "--verify", &format!("refs/heads/{branch}")],
        )
        .is_ok();
        if path.exists() || branch_exists {
            let _=ctx.db.exec("INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,'failed',?7)",params![id,repo_id,agent_id,handle,branch,base_commit,now()]);
            let _ = ctx.db.exec(
                "UPDATE resources SET state='leaked' WHERE id=?1",
                [&resource],
            );
            ctx.log_event(
                "workspace:partial_create",
                agent_id,
                &json!({"id":id,"resource_id":resource,"error":e}),
            );
            changed(ctx);
        } else {
            crate::reaper::release(ctx, &resource);
        }
        return Err(e);
    }
    if let Err(e) = ctx.db.exec("INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![id,repo_id,agent_id,handle,branch,base_commit,now()]) {
        let removed = git(repo_path, &["worktree","remove","--force",&handle]);
        if removed.is_ok() { let _ = git(repo_path, &["branch","-D",&branch]); crate::reaper::release(ctx,&resource); }
        else { let _ = ctx.db.exec("UPDATE resources SET state='leaked' WHERE id=?1", [&resource]); }
        return Err(e);
    }
    ctx.db.exec(
        "UPDATE resources SET meta=?1 WHERE id=?2",
        params![
            json!({"worktree_id":id,"repo_id":repo_id,"branch":branch}).to_string(),
            resource
        ],
    )?;
    changed(ctx);
    ctx.log_event(
        "workspace:create",
        agent_id,
        &json!({"id":id,"path":path,"branch":branch}),
    );
    Ok(Worktree {
        id,
        repo_id: repo_id.into(),
        path,
        branch,
        base_commit,
    })
}
pub fn worktree_status(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let w = worktree(ctx, id)?;
    let path = Path::new(text(&w, "path")?);
    let dirty = git(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let upstream = git(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
    .ok();
    let r = repo(ctx, text(&w, "repo_id")?).ok_or("Unknown repository")?;
    let target = r["integration_branch"]
        .as_str()
        .or(r["default_branch"].as_str())
        .unwrap_or("main");
    let comparison = upstream.as_deref().unwrap_or(target);
    let counts = git(
        path,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{comparison}...HEAD"),
        ],
    )?;
    let mut n = counts
        .split_whitespace()
        .filter_map(|v| v.parse::<u64>().ok());
    let behind = n.next().unwrap_or(0);
    let ahead = n.next().unwrap_or(0);
    let unpushed = git(
        path,
        &[
            "rev-list",
            "--count",
            "HEAD",
            &format!("^{}", text(&w, "base_commit")?),
            "--not",
            "--remotes",
        ],
    )?
    .parse::<u64>()
    .unwrap_or(0);
    let unmerged = git(path, &["rev-list", "--count", &format!("{target}..HEAD")])?
        .parse::<u64>()
        .unwrap_or(0);
    let last = git(path, &["log", "-1", "--format=%H%n%s%n%an%n%ct"])?;
    let mut last = last.lines();
    Ok(
        json!({"id":id,"dirty":!dirty.is_empty(),"dirty_files":dirty.split('\0').filter(|s| !s.is_empty()).collect::<Vec<_>>(),"unpushed_commits":unpushed,"unmerged_commits":unmerged,"ahead":ahead,"behind":behind,"upstream":upstream,"unsaved":!dirty.is_empty() || unpushed>0 || unmerged>0,"last_commit":{"hash":last.next(),"subject":last.next(),"author":last.next(),"timestamp":last.next()}}),
    )
}
pub fn remove_worktree(ctx: &Arc<Ctx>, id: &str, force: bool) -> Result<(), String> {
    let _queue = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
    let w = worktree(ctx, id)?;
    let path = Path::new(text(&w, "path")?);
    let root = std::fs::canonicalize(ctx.root.join("worktrees")).map_err(|e| e.to_string())?;
    if !path.starts_with(ctx.root.join("worktrees")) {
        return Err("Worktree registry path is outside Philium".into());
    }
    if std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err("Refusing to remove a symlink worktree".into());
    }
    if path.exists()
        && !std::fs::canonicalize(path)
            .map_err(|e| e.to_string())?
            .starts_with(&root)
    {
        return Err("Refusing to remove a worktree outside Philium".into());
    }
    let r = repo(ctx, text(&w, "repo_id")?).ok_or("Unknown repository")?;
    let repo_dir = repo_dir(&r)?;
    let repo_path = repo_dir.as_path();
    if path.exists() {
        if !force && worktree_status(ctx, id)?["unsaved"] == true {
            return Err("Worktree has unsaved work (dirty, unpushed or unmerged). Explicit confirmation required".into());
        }
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(text(&w, "path")?);
        git(repo_path, &args)?;
    } else {
        if !force {
            return Err(
                "Worktree is missing. Confirm removal to clean its branch and registry".into(),
            );
        }
        let registered = git(repo_path, &["worktree", "list", "--porcelain"])?;
        if registered
            .lines()
            .any(|l| l == format!("worktree {}", path.display()))
        {
            git(
                repo_path,
                &["worktree", "remove", "--force", text(&w, "path")?],
            )?;
        }
    }
    let branch = text(&w, "branch")?;
    if branch.starts_with("philium/")
        && git(
            repo_path,
            &["show-ref", "--verify", &format!("refs/heads/{branch}")],
        )
        .is_ok()
    {
        // The safety check above uses the configured integration target, not the main checkout's HEAD.
        if let Err(e) = git(repo_path, &["branch", "-D", branch]) {
            let _ = ctx.db.exec(
                "UPDATE resources SET state='leaked' WHERE kind='worktree' AND handle=?1",
                [text(&w, "path")?],
            );
            return Err(e);
        }
    }
    ctx.db.exec("UPDATE resources SET state='released',released_at=?1 WHERE kind='worktree' AND handle=?2 AND state!='released'",params![now(),text(&w,"path")?])?;
    ctx.db.exec("DELETE FROM worktrees WHERE id=?1", [id])?;
    ctx.log_event(
        "workspace:remove",
        w["agent_id"].as_str(),
        &json!({"id":id,"force":force}),
    );
    changed(ctx);
    Ok(())
}
fn list_worktrees(ctx: &Arc<Ctx>, repo_id: Option<&str>) -> Result<Value, String> {
    let mut rows = ctx.db.query("SELECT w.*,a.name AS agent_name FROM worktrees w LEFT JOIN agents a ON w.agent_id=a.id WHERE (?1 IS NULL OR w.repo_id=?1) ORDER BY w.created_at DESC",params![repo_id])?;
    for row in &mut rows {
        row["status"] = worktree_status(ctx, text(row, "id")?)
            .unwrap_or_else(|e| json!({"error":e,"unsaved":true}));
    }
    Ok(json!(rows))
}
/// Reconcile a lost ledger row only when Git proves the checkout belongs to a registered repo.
pub(crate) fn recover_worktree(ctx: &Arc<Ctx>, path: &Path) -> Result<String, String> {
    let _q = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
    if let Some(row) = ctx
        .db
        .query(
            "SELECT id FROM worktrees WHERE path=?1",
            [path.to_string_lossy().as_ref()],
        )?
        .first()
    {
        return Ok(text(row, "id")?.into());
    }
    let root = std::fs::canonicalize(ctx.root.join("worktrees")).map_err(|e| e.to_string())?;
    let canonical = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if canonical == root || !canonical.starts_with(&root) || !path.join(".git").is_file() {
        return Err("Not a managed Git worktree".into());
    }
    let branch = git(path, &["symbolic-ref", "--short", "HEAD"])?;
    if !branch.starts_with("philium/") {
        return Err("Orphan branch is not owned by Philium".into());
    }
    let common = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = std::fs::canonicalize(common).map_err(|e| e.to_string())?;
    let mut matched = None;
    for r in ctx.db.query("SELECT * FROM repos", [])? {
        let p = Path::new(text(&r, "path")?);
        if let Ok(c) = git(
            p,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ) {
            if std::fs::canonicalize(c).ok().as_ref() == Some(&common) {
                matched = Some(r);
                break;
            }
        }
    }
    let r = matched.ok_or("Orphan does not belong to a registered repository")?;
    let base = r["integration_branch"]
        .as_str()
        .or(r["default_branch"].as_str())
        .ok_or("Repository has no base")?;
    let base = git(path, &["merge-base", base, "HEAD"])?;
    let id = new_id();
    let owner = branch.split('/').nth(1).unwrap_or("");
    let agent = ctx
        .db
        .query("SELECT id FROM agents WHERE name=?1", [owner])?
        .first()
        .and_then(|r| r["id"].as_str())
        .map(str::to_owned);
    ctx.db.exec("INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,text(&r,"id")?,agent,path.to_string_lossy(),branch,base,now()])?;
    ctx.log_event(
        "workspace:recovered",
        agent.as_deref(),
        &json!({"id":id,"path":path}),
    );
    changed(ctx);
    Ok(id)
}
fn cap(mut s: String) -> (String, bool) {
    let truncated = s.len() > DIFF_CAP;
    if truncated {
        let mut n = DIFF_CAP;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
    (s, truncated)
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    // Blocking Git never occupies an async runtime worker.
    let ctx = ctx.clone();
    let method = method.to_owned();
    tokio::task::spawn_blocking(move || dispatch(&ctx, &method, args))
        .await
        .map_err(|e| e.to_string())?
}
/// Rows from `for-each-ref` (see `branches`). Skips symbolic `<remote>/HEAD`.
fn parse_branches(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.splitn(7, '\t').collect();
            let full = *p.first()?;
            let remote = full.starts_with("refs/remotes/");
            if remote && full.ends_with("/HEAD") {
                return None;
            }
            Some(json!({
                "name": p.get(1),
                "commit": p.get(2),
                "upstream": p.get(3).filter(|s| !s.is_empty()),
                "current": p.get(4) == Some(&"*"),
                "date": p.get(5).and_then(|s| s.parse::<i64>().ok()).map(|s| s * 1000),
                "subject": p.get(6).copied().unwrap_or(""),
                "remote": remote,
            }))
        })
        .collect()
}
fn dispatch(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "repos_list" => {
            let mut rows=ctx.db.query("SELECT r.*,(SELECT count(*) FROM worktrees w WHERE w.repo_id=r.id) AS worktree_count FROM repos r ORDER BY name",[])?;
            for r in &mut rows {
                let p = match repo_dir(r) {
                    Ok(p) => p,
                    Err(e) => {
                        r["missing"] = json!(true);
                        r["dirty"] = json!(false);
                        r["error"] = json!(e);
                        continue;
                    }
                };
                let p = p.as_path();
                if let Ok(u) = git(p, &["remote", "get-url", "origin"]) {
                    r["remote_url"] = json!(u);
                }
                if r["root_commits"].is_null() {
                    let roots = repo_identity(p).0.join(",");
                    if !roots.is_empty() {
                        let _ = ctx.db.exec("UPDATE repos SET root_commits=?2 WHERE id=?1", params![text(r, "id")?, roots]);
                    }
                }
                let branch = git(p, &["branch", "--show-current"]);
                let dirty = git(p, &["status", "--porcelain", "-uno"]);
                r["branch"] = json!(branch.unwrap_or_default());
                // -uno: skip the untracked-file walk; cheap even on big repos.
                r["dirty"] = json!(dirty.as_ref().map(|s| !s.is_empty()).unwrap_or(true));
                if let Err(e) = dirty {
                    r["error"] = json!(e);
                }
            }
            Ok(json!(rows))
        }
        "repo_add" => {
            let _q = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
            add_repo(ctx, &arg_str(&args, "path")?)
        }
        "repo_clone" => {
            let _q = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
            let url = arg_str(&args, "url")?;
            if url.starts_with('-') || url.trim().is_empty() || url.starts_with("ext::") {
                return Err("Invalid clone URL".into());
            }
            let inferred = url
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .unwrap_or("repo")
                .trim_end_matches(".git");
            let name = arg_opt_str(&args, "name").unwrap_or_else(|| inferred.into());
            safe_name(&name)?;
            let path = ctx.root.join("repos").join(&name);
            if path.exists() {
                return Err("Clone destination already exists".into());
            }
            std::fs::create_dir_all(ctx.root.join("repos")).map_err(|e| e.to_string())?;
            git(
                &ctx.root.join("repos"),
                &["clone", "--", &url, &path.to_string_lossy()],
            )?;
            add_repo(ctx, &path.to_string_lossy())
        }
        "repo_remove" => {
            let _q = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
            let id = arg_str(&args, "id")?;
            if !ctx
                .db
                .query("SELECT id FROM worktrees WHERE repo_id=?1", [&id])?
                .is_empty()
            {
                return Err("Remove managed worktrees before unregistering this repository".into());
            }
            ctx.db.exec("DELETE FROM repos WHERE id=?1", [id])?;
            changed(ctx);
            Ok(Value::Null)
        }
        "repo_set" => {
            let _q = GIT_QUEUE.lock().map_err(|e| e.to_string())?;
            let id = arg_str(&args, "id")?;
            let r = repo(ctx, &id).ok_or("Unknown repository")?;
            for key in ["default_branch", "integration_branch"] {
                if let Some(v) = args.get(key) {
                    if !v.is_null() || key == "default_branch" {
                        let name = v.as_str().ok_or("Branch must be text")?;
                        git(
                            Path::new(text(&r, "path")?),
                            &["check-ref-format", "--branch", name],
                        )?;
                        git(
                            Path::new(text(&r, "path")?),
                            &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
                        )?;
                    }
                    ctx.db.exec(
                        &format!("UPDATE repos SET {key}=?1 WHERE id=?2"),
                        params![v.as_str(), id],
                    )?;
                }
            }
            changed(ctx);
            Ok(repo(ctx, &id).unwrap())
        }
        "branches" => {
            let r = repo(ctx, &arg_str(&args, "repo_id")?).ok_or("Unknown repository")?;
            let s = git(
                &repo_dir(&r)?,
                &[
                    "for-each-ref",
                    "--sort=-committerdate",
                    "--format=%(refname)%09%(refname:short)%09%(objectname)%09%(upstream:short)%09%(HEAD)%09%(committerdate:unix)%09%(contents:subject)",
                    "refs/heads",
                    "refs/remotes",
                ],
            )?;
            Ok(json!(parse_branches(&s)))
        }
        "relocate" => relocate(ctx, &arg_str(&args, "repo_id")?, &arg_str(&args, "path")?),
        "worktrees" => list_worktrees(ctx, args["repo_id"].as_str()),
        "worktree_status" => worktree_status(ctx, &arg_str(&args, "id")?),
        "worktree_create" => Ok(json!(create_worktree(
            ctx,
            &arg_str(&args, "repo_id")?,
            None,
            &arg_str(&args, "name")?,
            None
        )?)),
        "worktree_remove" => {
            remove_worktree(
                ctx,
                &arg_str(&args, "id")?,
                args["force"].as_bool().unwrap_or(false),
            )?;
            Ok(Value::Null)
        }
        "diff" => {
            let w = worktree(ctx, &arg_str(&args, "worktree_id")?)?;
            let path = Path::new(text(&w, "path")?);
            let base = text(&w, "base_commit")?;
            let (stat, s) = cap(git(path, &["diff", "--no-ext-diff", "--stat", base, "--"])?);
            let (patch, p) = cap(git(
                path,
                &["diff", "--no-ext-diff", "--no-textconv", base, "--"],
            )?);
            Ok(
                json!({"stat":stat,"patch":patch,"truncated":s||p,"note":"Untracked files are listed in status, not included in the patch"}),
            )
        }
        "log" => {
            let row = if let Some(id) = args["worktree_id"].as_str() {
                worktree(ctx, id)?
            } else {
                repo(ctx, &arg_str(&args, "repo_id")?).ok_or("Unknown repository")?
            };
            let limit = args["limit"]
                .as_u64()
                .unwrap_or(50)
                .clamp(1, 200)
                .to_string();
            let s = git(
                Path::new(text(&row, "path")?),
                &["log", "-n", &limit, "--format=%H%x09%an%x09%ct%x09%s"],
            )?;
            Ok(json!(s.lines().map(|l|{let p=l.splitn(4,'\t').collect::<Vec<_>>();json!({"hash":p.first(),"author":p.get(1),"timestamp":p.get(2),"subject":p.get(3)})}).collect::<Vec<_>>()))
        }
        "open_in" => {
            let path = std::fs::canonicalize(arg_str(&args, "path")?).map_err(|e| e.to_string())?;
            let app = arg_str(&args, "app")?;
            let mut c = Command::new("open");
            match app.as_str() {
                "finder" => {}
                "terminal" => {
                    c.args(["-a", "Terminal"]);
                }
                "editor" => {
                    c.args(["-a", "Visual Studio Code"]);
                }
                _ => return Err("Unknown application".into()),
            };
            if !c.arg(path).status().map_err(|e| e.to_string())?.success() {
                return Err("Application could not be opened".into());
            }
            Ok(Value::Null)
        }
        "landscape" => repo_landscape(
            ctx,
            &arg_str(&args, "repo_id")?,
            args["include_prs"].as_bool().unwrap_or(true),
        ),
        _ => Err(format!("workspace.{method}: unknown method")),
    }
}

/// Parse `git worktree list --porcelain` on `repo_path` into `(path, branch, head_sha)`.
/// Discovers all existing worktrees on disk (main checkout, Philium-managed, and external checkouts).
pub fn git_worktrees(repo_path: &Path) -> Vec<(PathBuf, String, String)> {
    let Ok(out) = git(repo_path, &["worktree", "list", "--porcelain"]) else {
        return vec![];
    };
    let mut res = vec![];
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_head = String::new();
    let mut cur_branch = String::new();
    let flush = |res: &mut Vec<(PathBuf, String, String)>, p: Option<PathBuf>, b: &str, h: &str| {
        if let Some(p) = p {
            if p.is_dir() {
                let branch = if b.is_empty() {
                    format!("detached@{}", &h[..h.len().min(7)])
                } else {
                    b.to_string()
                };
                res.push((p, branch, h.to_string()));
            }
        }
    };
    for line in out.lines() {
        if line.is_empty() {
            flush(&mut res, cur_path.take(), &cur_branch, &cur_head);
            cur_head.clear();
            cur_branch.clear();
        } else if let Some(w) = line.strip_prefix("worktree ") {
            cur_path = Some(PathBuf::from(w));
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            cur_head = h.trim().to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            cur_branch = b.trim().trim_start_matches("refs/heads/").to_string();
        }
    }
    flush(&mut res, cur_path.take(), &cur_branch, &cur_head);
    res
}

fn path_status(path: &Path, target: &str) -> Value {
    let dirty = git(path, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]).unwrap_or_default();
    let dirty_files: Vec<&str> = dirty.split('\0').filter(|s| !s.is_empty()).collect();
    let counts = git(path, &["rev-list", "--left-right", "--count", &format!("{target}...HEAD")]).unwrap_or_default();
    let mut n = counts.split_whitespace().filter_map(|v| v.parse::<u64>().ok());
    let behind = n.next().unwrap_or(0);
    let ahead = n.next().unwrap_or(0);
    let last = git(path, &["log", "-1", "--format=%H%n%s%n%an%n%ct"]).unwrap_or_default();
    let mut l = last.lines();
    json!({
        "dirty": !dirty_files.is_empty(),
        "dirty_count": dirty_files.len(),
        "dirty_files": dirty_files.into_iter().take(40).collect::<Vec<_>>(),
        "ahead": ahead,
        "behind": behind,
        "last_commit": {
            "hash": l.next().unwrap_or(""),
            "subject": l.next().unwrap_or(""),
            "author": l.next().unwrap_or(""),
            "timestamp": l.next().unwrap_or(""),
        }
    })
}

/// Discover all worktrees of `repo_id` (main checkout + Philium-managed + pre-existing external git worktrees).
pub fn discover_worktrees(ctx: &Ctx, repo_id: &str) -> Result<Vec<Value>, String> {
    let r = ctx.db.query("SELECT * FROM repos WHERE id=?1", [repo_id])?.into_iter().next().ok_or("Unknown repository")?;
    let repo_path = repo_dir(&r)?;
    let target = r["integration_branch"]
        .as_str()
        .or(r["default_branch"].as_str())
        .unwrap_or("main");
    let main_canon = std::fs::canonicalize(&repo_path).unwrap_or_else(|_| repo_path.clone());
    let managed = ctx.db.query(
        "SELECT w.*, a.name AS agent_name FROM worktrees w LEFT JOIN agents a ON a.id=w.agent_id WHERE w.repo_id=?1",
        [repo_id],
    )?;
    let mut by_canon: std::collections::HashMap<PathBuf, Value> = std::collections::HashMap::new();
    for m in managed {
        if let Some(ps) = m["path"].as_str() {
            let p = PathBuf::from(ps);
            let canon = std::fs::canonicalize(&p).unwrap_or(p);
            by_canon.insert(canon, m);
        }
    }
    let mut out = vec![];
    for (p, branch, head) in git_worktrees(&repo_path) {
        let canon = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        let st = path_status(&p, target);
        if let Some(m) = by_canon.remove(&canon) {
            out.push(json!({
                "id": m["id"],
                "repo_id": repo_id,
                "path": p.to_string_lossy(),
                "branch": branch,
                "head": head,
                "kind": "philium",
                "agent_id": m["agent_id"],
                "agent_name": m["agent_name"],
                "status": st,
            }));
        } else if canon == main_canon {
            out.push(json!({
                "id": format!("local:{repo_id}"),
                "repo_id": repo_id,
                "path": p.to_string_lossy(),
                "branch": branch,
                "head": head,
                "kind": "main",
                "agent_id": Value::Null,
                "agent_name": "Main checkout",
                "status": st,
            }));
        } else {
            let dir_name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "external".into());
            out.push(json!({
                "id": format!("ext:{repo_id}:{dir_name}"),
                "repo_id": repo_id,
                "path": p.to_string_lossy(),
                "branch": branch,
                "head": head,
                "kind": "external",
                "agent_id": Value::Null,
                "agent_name": format!("External ({dir_name})"),
                "status": st,
            }));
        }
    }
    Ok(out)
}

fn run_gh_bounded(repo_path: &Path, args: &[&str], timeout_ms: u64) -> Option<Value> {
    let mut child = Command::new("gh")
        .args(args)
        .current_dir(repo_path)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let out = child.wait_with_output().ok()?;
                return serde_json::from_slice(&out.stdout).ok();
            }
            Ok(None) => {
                if start.elapsed() > std::time::Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// Cached open + recently merged GitHub PRs via `gh pr list` (60s TTL, 3.5s timeout).
pub fn github_prs(repo_path: &Path) -> Value {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, Value)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((t, v)) = g.get(repo_path) {
            if t.elapsed() < std::time::Duration::from_secs(60) {
                return v.clone();
            }
        }
    }
    let open = run_gh_bounded(
        repo_path,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "15",
            "--json",
            "number,title,headRefName,baseRefName,author,isDraft,reviewDecision,updatedAt,url",
        ],
        3500,
    )
    .unwrap_or_else(|| json!([]));
    let merged = run_gh_bounded(
        repo_path,
        &[
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "5",
            "--json",
            "number,title,headRefName,baseRefName,author,mergedAt,url",
        ],
        2500,
    )
    .unwrap_or_else(|| json!([]));
    let val = json!({ "open": open, "recently_merged": merged });
    if let Ok(mut g) = cache.lock() {
        g.insert(repo_path.to_path_buf(), (std::time::Instant::now(), val.clone()));
    }
    val
}

/// Detailed view of a single GitHub PR (`gh pr view <num>`).
pub fn github_pr_view(repo_path: &Path, pr_number: u64) -> Option<Value> {
    let n = pr_number.to_string();
    run_gh_bounded(
        repo_path,
        &[
            "pr",
            "view",
            &n,
            "--json",
            "number,title,state,isDraft,headRefName,baseRefName,author,body,reviewDecision,statusCheckRollup,reviews,comments,files,url",
        ],
        4500,
    )
}

/// Complete local + remote + GitHub landscape for a repository.
pub fn repo_landscape(ctx: &Ctx, repo_id: &str, include_prs: bool) -> Result<Value, String> {
    let r = ctx.db.query("SELECT * FROM repos WHERE id=?1", [repo_id])?.into_iter().next().ok_or("Unknown repository")?;
    let p = repo_dir(&r)?;
    let target = r["integration_branch"]
        .as_str()
        .or(r["default_branch"].as_str())
        .unwrap_or("main");
    let wts = discover_worktrees(ctx, repo_id).unwrap_or_default();
    let raw_branches = git(
        &p,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(refname:short)%09%(objectname)%09%(upstream:short)%09%(HEAD)%09%(committerdate:unix)%09%(contents:subject)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .map(|s| parse_branches(&s))
    .unwrap_or_default();
    let mut branches = vec![];
    for mut b in raw_branches.into_iter().take(35) {
        if let Some(name) = b["name"].as_str() {
            if name != target && name != format!("origin/{target}") {
                if let Ok(c) = git(&p, &["rev-list", "--left-right", "--count", &format!("{target}...{name}")]) {
                    let mut it = c.split_whitespace().filter_map(|x| x.parse::<u64>().ok());
                    b["behind"] = json!(it.next().unwrap_or(0));
                    b["ahead"] = json!(it.next().unwrap_or(0));
                }
            } else {
                b["behind"] = json!(0);
                b["ahead"] = json!(0);
            }
        }
        branches.push(b);
    }
    let prs = if include_prs {
        github_prs(&p)
    } else {
        json!({ "open": [], "recently_merged": [] })
    };
    Ok(json!({
        "repo_id": repo_id,
        "name": r["name"],
        "path": p.to_string_lossy(),
        "default_branch": target,
        "remote_url": r["remote_url"],
        "worktrees": wts,
        "branches": branches,
        "prs": prs,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Arc<Ctx>, String) {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let p = ctx.root.join("fixture");
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-b", "main"]).unwrap();
        git(&p, &["config", "user.name", "Test"]).unwrap();
        git(&p, &["config", "user.email", "test@example.test"]).unwrap();
        std::fs::write(p.join("file"), "one\n").unwrap();
        git(&p, &["add", "."]).unwrap();
        git(&p, &["commit", "-m", "initial"]).unwrap();
        let r = add_repo(&ctx, &p.to_string_lossy()).unwrap();
        (ctx, r["id"].as_str().unwrap().into())
    }
    #[test]
    fn branch_rows_and_origin_head_skipped() {
        let out = "refs/heads/main\tmain\tabc\torigin/main\t*\t1700000000\tfix: a\tb\n\
                   refs/remotes/origin/HEAD\torigin\tabc\t\t \t1700000000\tx\n\
                   refs/remotes/origin/dev\torigin/dev\tdef\t\t \t1600000000\t";
        let b = parse_branches(out);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0]["current"], true);
        assert_eq!(b[0]["date"], 1_700_000_000_000i64);
        assert_eq!(b[0]["subject"], "fix: a\tb");
        assert_eq!((b[1]["remote"].clone(), b[1]["current"].clone()), (json!(true), json!(false)));
        assert!(b[1]["upstream"].is_null());
    }
    #[test]
    fn moved_repo_errors_then_relocates_only_to_same_repo() {
        let (ctx, id) = setup();
        let rows = dispatch(&ctx, "repos_list", json!({})).unwrap();
        assert_eq!(rows[0]["dirty"], false);
        assert_eq!(rows[0]["branch"], "main");
        let b = dispatch(&ctx, "branches", json!({"repo_id":id})).unwrap();
        assert_eq!(b[0]["subject"], "initial");
        assert!(b[0]["date"].as_i64().unwrap() > 0);
        let old = ctx.root.join("fixture");
        let new = ctx.root.join("moved");
        std::fs::rename(&old, &new).unwrap();
        let e = create_worktree(&ctx, &id, None, "lion", None).unwrap_err();
        assert!(e.contains("is missing. Remove it or pick its new location"), "{e}");
        assert!(dispatch(&ctx, "branches", json!({"repo_id":id})).unwrap_err().contains("missing"));
        assert_eq!(dispatch(&ctx, "repos_list", json!({})).unwrap()[0]["missing"], true);
        // A different repo is refused.
        let other = ctx.root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-b", "main"]).unwrap();
        git(&other, &["-c", "user.name=T", "-c", "user.email=t@e.t", "commit", "--allow-empty", "-m", "x"]).unwrap();
        let e = dispatch(&ctx, "relocate", json!({"repo_id":id,"path":other})).unwrap_err();
        assert!(e.contains("different repository"), "{e}");
        let r = dispatch(&ctx, "relocate", json!({"repo_id":id,"path":new})).unwrap();
        assert_eq!(r["path"], json!(std::fs::canonicalize(&new).unwrap()));
        assert!(create_worktree(&ctx, &id, None, "lion", None).is_ok());
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn create_status_remove() {
        let (ctx, id) = setup();
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        assert!(w.path.exists());
        assert!(w.branch.starts_with("philium/lion/"));
        assert_eq!(worktree_status(&ctx, &w.id).unwrap()["unsaved"], false);
        assert_eq!(
            ctx.db
                .query("SELECT * FROM resources WHERE state='live'", [])
                .unwrap()
                .len(),
            1
        );
        remove_worktree(&ctx, &w.id, false).unwrap();
        assert!(!w.path.exists());
        assert!(worktree(&ctx, &w.id).is_err());
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn dirty_and_unpushed_refused() {
        let (ctx, id) = setup();
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        std::fs::write(w.path.join("new"), "unsaved").unwrap();
        assert!(remove_worktree(&ctx, &w.id, false).is_err());
        git(&w.path, &["add", "."]).unwrap();
        git(&w.path, &["commit", "-m", "agent work"]).unwrap();
        let status = worktree_status(&ctx, &w.id).unwrap();
        assert_eq!(status["dirty"], false);
        assert_eq!(status["unpushed_commits"], 1);
        assert!(remove_worktree(&ctx, &w.id, false).is_err());
        remove_worktree(&ctx, &w.id, true).unwrap();
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn names_and_unregister_are_safe() {
        let (ctx, id) = setup();
        assert!(create_worktree(&ctx, &id, None, "../escape", None).is_err());
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        assert!(dispatch(&ctx, "repo_remove", json!({"id":id})).is_err());
        remove_worktree(&ctx, &w.id, false).unwrap();
        let p = repo(&ctx, &id).unwrap()["path"]
            .as_str()
            .unwrap()
            .to_string();
        dispatch(&ctx, "repo_remove", json!({"id":id})).unwrap();
        assert!(Path::new(&p).exists());
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn clone_branches_log_diff_and_integration_divergence() {
        let (ctx, id) = setup();
        let r = repo(&ctx, &id).unwrap();
        let path = Path::new(r["path"].as_str().unwrap());
        let cloned = dispatch(&ctx, "repo_clone", json!({"url":path,"name":"cloned"})).unwrap();
        assert!(Path::new(cloned["path"].as_str().unwrap())
            .join(".git")
            .exists());
        git(path, &["branch", "integration"]).unwrap();
        dispatch(
            &ctx,
            "repo_set",
            json!({"id":id,"integration_branch":"integration"}),
        )
        .unwrap();
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        git(path, &["checkout", "integration"]).unwrap();
        std::fs::write(path.join("main-change"), "two").unwrap();
        git(path, &["add", "."]).unwrap();
        git(path, &["commit", "-m", "integration update"]).unwrap();
        assert_eq!(worktree_status(&ctx, &w.id).unwrap()["behind"], 1);
        std::fs::write(w.path.join("file"), "changed\n").unwrap();
        let diff = dispatch(&ctx, "diff", json!({"worktree_id":w.id})).unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+changed"));
        assert!(
            dispatch(&ctx, "branches", json!({"repo_id":id}))
                .unwrap()
                .as_array()
                .unwrap()
                .len()
                >= 3
        );
        assert_eq!(
            dispatch(&ctx, "log", json!({"worktree_id":w.id,"limit":1}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        remove_worktree(&ctx, &w.id, true).unwrap();
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn pushed_but_unmerged_still_requires_confirmation() {
        let (ctx, id) = setup();
        let r = repo(&ctx, &id).unwrap();
        let path = Path::new(r["path"].as_str().unwrap());
        let remote = ctx.root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare"]).unwrap();
        git(path, &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        git(path, &["push", "origin", "main"]).unwrap();
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        std::fs::write(w.path.join("agent-file"), "work").unwrap();
        git(&w.path, &["add", "."]).unwrap();
        git(&w.path, &["commit", "-m", "work"]).unwrap();
        git(&w.path, &["push", "-u", "origin", &w.branch]).unwrap();
        let status = worktree_status(&ctx, &w.id).unwrap();
        assert_eq!(status["unpushed_commits"], 0);
        assert_eq!(status["unmerged_commits"], 1);
        assert!(remove_worktree(&ctx, &w.id, false).is_err());
        remove_worktree(&ctx, &w.id, true).unwrap();
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn recover_orphan_and_missing_checkout() {
        let (ctx, id) = setup();
        let w = create_worktree(&ctx, &id, None, "lion", None).unwrap();
        ctx.db
            .exec("DELETE FROM worktrees WHERE id=?1", [&w.id])
            .unwrap();
        let recovered = recover_worktree(&ctx, &w.path).unwrap();
        assert_ne!(recovered, w.id);
        std::fs::remove_dir_all(&w.path).unwrap();
        assert!(remove_worktree(&ctx, &recovered, false).is_err());
        remove_worktree(&ctx, &recovered, true).unwrap();
        let r = repo(&ctx, &id).unwrap();
        assert!(git(
            Path::new(r["path"].as_str().unwrap()),
            &["show-ref", "--verify", &format!("refs/heads/{}", w.branch)]
        )
        .is_err());
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn auto_teardown_asks_for_dirty_work_and_isolates_peer() {
        let (ctx, id) = setup();
        for (agent, name) in [("a", "lion"), ("b", "otter")] {
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES(?1,?2,'test/model','done',?3,?3)",params![agent,name,now()]).unwrap();
        }
        ctx.db.kv_set("setting.teardown.mode", "auto").unwrap();
        let w = create_worktree(&ctx, &id, Some("a"), "lion", None).unwrap();
        let peer = create_worktree(&ctx, &id, Some("b"), "otter", None).unwrap();
        std::fs::write(w.path.join("precious"), "unsaved").unwrap();
        let scratch = ctx.agent_dir("lion").join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("tmp"), "throwaway").unwrap();
        crate::reaper::register(
            &ctx,
            Some("a"),
            "scratch_dir",
            scratch.to_str().unwrap(),
            json!({}),
        );
        let plan = crate::reaper::teardown_agent(&ctx, "a", false).unwrap();
        assert_eq!(plan["needs_confirmation"], true);
        assert!(!scratch.exists());
        assert!(w.path.join("precious").exists());
        assert!(peer.path.exists());
        let proposals = plan["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0]["unsaved"], 1);
        let result = crate::reaper::call(&ctx, "approve", json!({"ids":[proposals[0]["id"]]}))
            .await
            .unwrap();
        assert_eq!(result[0]["state"], "released");
        assert!(!w.path.exists());
        assert!(peer.path.exists());
        remove_worktree(&ctx, &peer.id, false).unwrap();
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn capped_unicode_diff_is_valid() {
        let (s, truncated) = cap("🦁".repeat(DIFF_CAP));
        assert!(truncated);
        assert!(s.len() <= DIFF_CAP);
        assert!(s.ends_with('🦁'));
    }
}
