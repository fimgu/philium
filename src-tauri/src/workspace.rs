//! Workspace RPC: repos_list, repo_add {path}, repo_clone {url,name?}, repo_remove {id},
//! repo_set {id,integration_branch?,default_branch?}, branches {repo_id}, worktrees {repo_id?},
//! worktree_status {id}, worktree_create {repo_id,name}, worktree_remove {id,force?},
//! diff {worktree_id}, log {repo_id|worktree_id,limit?}, open_in {path,app}.
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
fn worktree(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    ctx.db.query("SELECT w.*,a.name AS agent_name FROM worktrees w LEFT JOIN agents a ON a.id=w.agent_id WHERE w.id=?1", [id])?.into_iter().next().ok_or_else(|| "Unknown worktree".into())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("Missing {key}"))
}
fn add_repo(ctx: &Arc<Ctx>, path: &str) -> Result<Value, String> {
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let top = git(&path, &["rev-parse", "--show-toplevel"])?;
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
    let r = repo(ctx, repo_id).ok_or("Unknown repository")?;
    let repo_path = Path::new(text(&r, "path")?);
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
    // Repo IDs, rather than display names, are stable, collision-free folder names.
    let parent = ctx.root.join("worktrees").join(repo_id);
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
        crate::reaper::release(ctx, &resource);
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
    let comparison = upstream.as_deref().unwrap_or(text(&w, "base_commit")?);
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
    let unpushed = git(path, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?
        .parse::<u64>()
        .unwrap_or(0);
    // Without an upstream, the starting commit itself is not agent work.
    let unpushed = if upstream.is_none() {
        unpushed.min(ahead)
    } else {
        unpushed
    };
    let r = repo(ctx, text(&w, "repo_id")?).ok_or("Unknown repository")?;
    let target = r["integration_branch"]
        .as_str()
        .or(r["default_branch"].as_str())
        .unwrap_or("main");
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
    let repo_path = Path::new(text(&r, "path")?);
    if path.exists() {
        let status = worktree_status(ctx, id)?;
        if !force && status["unsaved"] == true {
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
fn dispatch(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "repos_list" => {
            let mut rows=ctx.db.query("SELECT r.*,(SELECT count(*) FROM worktrees w WHERE w.repo_id=r.id) AS worktree_count FROM repos r ORDER BY name",[])?;
            for r in &mut rows {
                let p = Path::new(text(r, "path")?);
                let branch = git(p, &["branch", "--show-current"]);
                let dirty = git(p, &["status", "--porcelain"]);
                r["branch"] = json!(branch.unwrap_or_default());
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
                Path::new(text(&r, "path")?),
                &[
                    "for-each-ref",
                    "--format=%(refname:short)%09%(objectname)%09%(upstream:short)%09%(HEAD)",
                    "refs/heads",
                    "refs/remotes",
                ],
            )?;
            Ok(json!(s.lines().map(|l|{let p=l.split('\t').collect::<Vec<_>>();json!({"name":p.first(),"commit":p.get(1),"upstream":p.get(2),"current":p.get(3)==Some(&"*")})}).collect::<Vec<_>>()))
        }
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
        _ => Err(format!("workspace.{method}: unknown method")),
    }
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
}
