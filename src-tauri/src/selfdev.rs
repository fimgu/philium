//! Improve Philium from inside Philium (SPEC 12.1).
//!
//! Flow: request -> an agent works in a worktree of the Philium source ->
//! test (exact commit) -> preview (isolated data dir, keychain disabled) ->
//! accept (merge, keep previous HEAD) -> rebuild -> restart. Rollback is a
//! revert commit, never a hard reset. Changes to security-sensitive files
//! need explicit approval, even in Auto mode.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::ctx::{arg_opt_str, arg_str, Ctx};
use crate::db::{new_id, now};
use crate::workspace::git;

/// Files whose changes always need explicit approval.
pub const SECURITY_FILES: &[&str] = &[
    "src-tauri/src/gateway.rs",
    "src-tauri/src/guard.rs",
    "src-tauri/src/providers.rs",
    "src-tauri/src/sandbox.rs",
    "src-tauri/src/mcp.rs",
    "src-tauri/src/agents.rs",
    "src-tauri/src/selfdev.rs",
    "src-tauri/tauri.conf.json",
    "src-tauri/capabilities/",
    "src-tauri/Cargo.toml",
    "package.json",
];

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.exec(
        "CREATE TABLE IF NOT EXISTS selfdev (
           id TEXT PRIMARY KEY, request TEXT NOT NULL, context TEXT,
           agent_id TEXT, task_id TEXT, state TEXT NOT NULL,
           test_terminal TEXT, tested_commit TEXT,
           preview_terminal TEXT, build_terminal TEXT,
           prev_head TEXT, merge_commit TEXT, error TEXT,
           created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
        [],
    )?;
    // Remembered so Retry and Discard still find the worktree after its agent is gone.
    let _ = ctx.db.exec("ALTER TABLE selfdev ADD COLUMN worktree_id TEXT", []);
    reconcile_all(ctx);
    Ok(())
}

/// The Philium source checkout. Setting `selfdev.source`, else the tree this
/// binary was built from (dev builds), else ~/Philium/repos/philium.
pub fn source_dir(ctx: &Ctx) -> Option<PathBuf> {
    let set = ctx.setting("selfdev.source", "");
    let candidates = [
        (!set.is_empty()).then(|| PathBuf::from(&set)),
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf),
        Some(ctx.root.join("repos").join("philium")),
    ];
    candidates.into_iter().flatten().find(|p| p.join(".git").exists() && p.join("src-tauri").exists())
}

fn source_repo(ctx: &Arc<Ctx>) -> Result<(String, PathBuf), String> {
    let dir = source_dir(ctx).ok_or("Philium source not found. Set its folder in Settings (selfdev.source).")?;
    let p = dir.to_string_lossy().to_string();
    if let Some(r) = ctx.db.query("SELECT id FROM repos WHERE path=?1", [&p])?.first() {
        return Ok((r["id"].as_str().unwrap_or_default().into(), dir));
    }
    // Register so the repo shows up everywhere (same path as workspace.repo_add).
    let r = futures_lite_block(crate::workspace::call(ctx, "repo_add", json!({ "path": p })))?;
    Ok((r["id"].as_str().ok_or("repo_add returned no id")?.into(), dir))
}

// Runs an async module call from blocking code (we are always inside
// spawn_blocking here, so a runtime handle is available).
fn futures_lite_block<F: std::future::Future<Output = Result<Value, String>>>(f: F) -> Result<Value, String> {
    tokio::runtime::Handle::current().block_on(f)
}

fn row(ctx: &Ctx, id: &str) -> Result<Value, String> {
    ctx.db
        .query("SELECT * FROM selfdev WHERE id=?1", [id])?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no improvement {id}"))
}

fn set(ctx: &Ctx, id: &str, col: &str, val: &str) -> Result<(), String> {
    // col is always a literal from this file.
    ctx.db.exec(
        &format!("UPDATE selfdev SET {col}=?2, updated_at=?3 WHERE id=?1"),
        rusqlite::params![id, val, now()],
    )?;
    ctx.emit("selfdev:changed", json!({ "id": id }));
    Ok(())
}

fn worktree(ctx: &Ctx, agent_id: &str) -> Result<(PathBuf, String), String> {
    let w = ctx
        .db
        .query("SELECT path, branch FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [agent_id])?
        .into_iter()
        .next()
        .ok_or("the agent has no active worktree")?;
    Ok((
        PathBuf::from(w["path"].as_str().unwrap_or_default()),
        w["branch"].as_str().unwrap_or_default().to_string(),
    ))
}

/// Which changed files (vs the source HEAD) are security sensitive.
pub fn flagged(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|f| SECURITY_FILES.iter().any(|s| if s.ends_with('/') { f.starts_with(s) } else { f == s }))
        .cloned()
        .collect()
}

fn changed_files(src: &Path, branch: &str) -> Result<Vec<String>, String> {
    let out = git(src, &["diff", "--name-only", &format!("HEAD...{branch}")])?;
    Ok(out.lines().map(str::to_string).filter(|l| !l.is_empty()).collect())
}

fn redact(s: &str) -> String {
    let mut t = s.to_string();
    let mut f = crate::guard::Findings::new();
    crate::guard::scan_text(&mut t, true, &mut f);
    t
}

fn sh(ctx: &Arc<Ctx>, title: &str, cwd: &Path, cmd: &str, env: Vec<(String, String)>, agent: Option<&str>) -> Result<String, String> {
    crate::terminals::open(
        ctx,
        crate::terminals::Opts {
            title: title.into(),
            command: Some(vec!["/bin/sh".into(), "-lc".into(), cmd.into()]),
            cwd: cwd.to_path_buf(),
            env,
            agent_id: agent.map(str::to_string),
        },
    )
}

fn term_exit(ctx: &Ctx, id: &str) -> Option<i64> {
    ctx.db
        .query("SELECT exit_code, state FROM terminals WHERE id=?1", [id])
        .ok()?
        .first()
        .filter(|r| r["state"] == "exited")
        .and_then(|r| r["exit_code"].as_i64())
}

/// States in which an improvement still depends on its agent.
const LIVE: [&str; 2] = ["building", "preview"];

/// Why an in-progress improvement can no longer progress, if it cannot: its agent was removed,
/// stopped or failed, or its board task was cancelled. `None` while the agent is alive.
fn lost_reason(ctx: &Ctx, r: &Value) -> Option<(&'static str, String)> {
    if !LIVE.contains(&r["state"].as_str().unwrap_or_default()) {
        return None;
    }
    let agent = r["agent_id"].as_str().unwrap_or_default();
    let a = ctx.db.query("SELECT name, state FROM agents WHERE id=?1", [agent]).ok()?.into_iter().next();
    let task = r["task_id"].as_str().and_then(|t| ctx.db.query("SELECT status FROM tasks WHERE id=?1", [t]).ok()?.into_iter().next());
    if task.as_ref().is_some_and(|t| t["status"] == "cancelled") {
        return Some(("stopped", "Its board task was cancelled.".into()));
    }
    let Some(a) = a else {
        return Some(("failed", "Its agent was removed before it finished. Retry starts a new agent in the same worktree, if it is still there.".into()));
    };
    let name = a["name"].as_str().unwrap_or("The agent");
    match a["state"].as_str() {
        // The agent stops after building; only a 'building' improvement needs it working.
        Some("failed") if r["state"] == "building" => {
            let err = ctx.db.kv_get(&format!("agent_error.{agent}")).filter(|e| !e.is_empty());
            Some(("failed", format!("{name} failed: {}", err.unwrap_or_else(|| "no error recorded".into()))))
        }
        Some("stopped") if r["state"] == "building" => {
            let err = ctx.db.kv_get(&format!("agent_error.{agent}")).filter(|e| !e.is_empty());
            Some(("stopped", match err {
                Some(e) => format!("{name} stopped: {e}"),
                None => format!("{name} was stopped before it finished."),
            }))
        }
        _ => None,
    }
}

/// Bring an improvement's state in line with its agent and task. Never leaves 'building'
/// pointing at an agent that is gone.
fn reconcile(ctx: &Ctx, id: &str) -> Result<Value, String> {
    let mut r = row(ctx, id)?;
    if r["worktree_id"].is_null() {
        let agent = r["agent_id"].as_str().unwrap_or_default();
        if let Some(w) = ctx.db.query("SELECT id FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [agent])?.first().and_then(|w| w["id"].as_str().map(str::to_string)) {
            ctx.db.exec("UPDATE selfdev SET worktree_id=?2 WHERE id=?1", [id, &w])?;
            r["worktree_id"] = json!(w);
        }
    }
    if let Some((state, reason)) = lost_reason(ctx, &r) {
        ctx.db.exec(
            "UPDATE selfdev SET state=?2, error=?3, updated_at=?4 WHERE id=?1",
            rusqlite::params![id, state, reason, now()],
        )?;
        ctx.log_event("selfdev_lost", r["agent_id"].as_str(), &json!({ "id": id, "state": state, "reason": reason }));
        ctx.emit("selfdev:changed", json!({ "id": id }));
        return row(ctx, id);
    }
    Ok(r)
}

/// Reconcile every in-progress improvement. Cheap: a few indexed lookups per row.
pub fn reconcile_all(ctx: &Ctx) {
    let Ok(rows) = ctx.db.query("SELECT id FROM selfdev WHERE state IN ('building','preview')", []) else { return };
    for r in rows {
        if let Some(id) = r["id"].as_str() {
            let _ = reconcile(ctx, id);
        }
    }
}

/// The improvement's worktree, whether its agent still exists or not.
fn own_worktree(ctx: &Ctx, r: &Value) -> Option<Value> {
    let agent = r["agent_id"].as_str().unwrap_or_default();
    if let Some(w) = ctx.db.query("SELECT * FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [agent]).ok()?.into_iter().next() {
        return Some(w);
    }
    let id = r["worktree_id"].as_str()?;
    ctx.db.query("SELECT * FROM worktrees WHERE id=?1 AND state='active'", [id]).ok()?.into_iter().next()
}

fn status(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let mut r = reconcile(ctx, id)?;
    let agent = r["agent_id"].as_str().unwrap_or_default().to_string();
    if let Some(t) = r["preview_terminal"].as_str() {
        let terminal = ctx.db.query("SELECT state, exit_code FROM terminals WHERE id=?1", [t])?;
        let terminal = terminal.first();
        r["preview"] = json!(preview_state(
            terminal.and_then(|t| t["state"].as_str()),
            terminal.and_then(|t| t["exit_code"].as_i64()),
        ));
        r["preview_exit_code"] = json!(terminal.and_then(|t| t["exit_code"].as_i64()));
    }
    if let Some(t) = r["test_terminal"].as_str() {
        r["tests"] = match term_exit(ctx, t) {
            None => json!("running"),
            Some(0) => json!("passed"),
            Some(_) => json!("failed"),
        };
    }
    if let Some(t) = r["build_terminal"].as_str() {
        r["build"] = match term_exit(ctx, t) {
            None => json!("running"),
            Some(0) => json!("passed"),
            Some(_) => json!("failed"),
        };
    }
    if let (Ok((_, src)), Ok((wt, branch))) = (source_repo(ctx), worktree(ctx, &agent)) {
        let files = changed_files(&src, &branch).unwrap_or_default();
        r["flagged"] = json!(flagged(&files));
        r["files"] = json!(files);
        r["head"] = json!(git(&wt, &["rev-parse", "HEAD"]).ok());
        r["dirty"] = json!(git(&wt, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false));
    }
    if let Some(a) = ctx.db.query("SELECT name, state FROM agents WHERE id=?1", [&agent])?.first() {
        r["agent_name"] = a["name"].clone();
        r["agent_state"] = a["state"].clone();
    }
    r["agent_exists"] = json!(r["agent_name"].is_string());
    r["worktree"] = json!(own_worktree(ctx, &r).and_then(|w| w["path"].as_str().map(str::to_string)));
    Ok(r)
}

fn request(ctx: &Arc<Ctx>, text: &str, context: &Value, model: Option<String>) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Err("Describe what you want to change.".into());
    }
    let (repo_id, _) = source_repo(ctx)?;
    let text = redact(text.trim());
    let context = redact(&context.to_string());
    let title: String = text.lines().next().unwrap_or("Improve Philium").chars().take(80).collect();
    let task = crate::board::create(
        ctx,
        "you",
        &format!("Philium: {title}"),
        &format!("{text}\n\nWhere: {context}"),
        Some("normal"),
        Some(&repo_id),
        &[],
        None,
    )?;
    let task_id = task["id"].as_str().unwrap_or_default().to_string();
    let brief = improver_brief(&text, &context);
    let agent = spawn_improver(ctx, &repo_id, &task_id, brief, model, None)?;
    let id = new_id();
    ctx.db.exec(
        "INSERT INTO selfdev(id,request,context,agent_id,task_id,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'building',?6,?6)",
        rusqlite::params![id, text, context, agent["id"].as_str(), task_id, now()],
    )?;
    ctx.log_event("selfdev_requested", agent["id"].as_str(), &json!({ "id": id, "title": title }));
    ctx.emit("selfdev:changed", json!({ "id": id }));
    Ok(json!({ "id": id, "agent": agent }))
}

/// Big requests take hours: the improver is a full agent with no turn or time limit of its own,
/// and the board renews its task lease for as long as its runtime is alive.
fn improver_brief(text: &str, context: &str) -> String {
    format!(
        "You are improving Philium itself, in a worktree of its source. Never touch the running app or its data.\n\
         Request from the user:\n{text}\n\nWhere they were: {context}\n\n\
         Rules:\n\
         - Large requests are expected. Plan first (write a checklist), then work through it for as long as it takes. Commit after each working step so nothing is lost if you are stopped.\n\
         - Keep the existing design system (src/ui/kit.jsx, src/styles). No em dashes in UI text.\n\
         - Run `cargo test <module>::` for modules you touched in src-tauri and `npx vite build` before you say you are done.\n\
         - Commit on your branch with a plain message. No Co-authored-by or agent signatures.\n\
         - Do not change gateway, guard, providers, sandbox, mcp or agents code unless the request needs it; if you do, explain why in your final message.\n\
         - When finished, say DONE and summarize what changed and how to try it."
    )
}

/// Same role as a normal working agent, so it gets the same prompt, limits and lease renewal.
fn spawn_improver(ctx: &Arc<Ctx>, repo_id: &str, task_id: &str, brief: String, model: Option<String>, worktree_id: Option<&str>) -> Result<Value, String> {
    crate::agents::spawn_adopting(
        ctx,
        crate::agents::SpawnReq {
            name: None,
            role: Some("worker".into()),
            model,
            repo_id: Some(repo_id.to_string()),
            task_id: Some(task_id.to_string()),
            brief: Some(brief),
            parent_id: None,
            strict: false,
            base: None,
            route_reason: None,
        },
        worktree_id,
    )
}

/// A new agent on the same request, continuing in the same worktree when it still exists.
fn retry(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = reconcile(ctx, id)?;
    if r["state"] == "building" {
        return Err("This improvement is still being built.".into());
    }
    if matches!(r["state"].as_str(), Some("accepted")) {
        return Err("This improvement was already accepted.".into());
    }
    let (repo_id, _) = source_repo(ctx)?;
    let wt = own_worktree(ctx, &r);
    // The old agent hands over its worktree; stop it first so two agents never share one.
    if let Some(old) = r["agent_id"].as_str().filter(|a| crate::agents::get_row(ctx, a).is_some()) {
        crate::agents::stop(ctx, old, "stopped");
    }
    let request = r["request"].as_str().unwrap_or_default().to_string();
    let context = r["context"].as_str().unwrap_or_default().to_string();
    let title: String = request.lines().next().unwrap_or("Improve Philium").chars().take(80).collect();
    let task_open = r["task_id"].as_str().and_then(|t| ctx.db.query("SELECT status FROM tasks WHERE id=?1", [t]).ok()?.into_iter().next()).is_some_and(|t| !matches!(t["status"].as_str(), Some("done" | "cancelled")));
    let task_id = match r["task_id"].as_str().filter(|_| task_open) {
        Some(t) => t.to_string(),
        None => crate::board::create(ctx, "you", &format!("Philium: {title}"), &format!("{request}\n\nWhere: {context}"), Some("normal"), Some(&repo_id), &[], None)?["id"].as_str().unwrap_or_default().to_string(),
    };
    let mut brief = improver_brief(&request, &context);
    if wt.is_some() {
        brief.push_str("\n\nAn earlier agent started on this in your worktree and was stopped. Check `git log` and `git status` first and continue from there.");
    }
    let wt_id = wt.as_ref().and_then(|w| w["id"].as_str().map(str::to_string));
    let agent = spawn_improver(ctx, &repo_id, &task_id, brief, None, wt_id.as_deref())?;
    ctx.db.exec(
        "UPDATE selfdev SET agent_id=?2, task_id=?3, state='building', error=NULL, test_terminal=NULL, tested_commit=NULL, worktree_id=COALESCE(?4, worktree_id), updated_at=?5 WHERE id=?1",
        rusqlite::params![id, agent["id"].as_str(), task_id, wt_id, now()],
    )?;
    ctx.log_event("selfdev_retried", agent["id"].as_str(), &json!({ "id": id, "same_worktree": wt_id.is_some() }));
    ctx.emit("selfdev:changed", json!({ "id": id }));
    Ok(json!({ "id": id, "agent": agent, "same_worktree": wt_id.is_some() }))
}

fn test(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = row(ctx, id)?;
    let (wt, branch) = worktree(ctx, r["agent_id"].as_str().unwrap_or_default())?;
    if !git(&wt, &["status", "--porcelain"])?.is_empty() {
        return Err("The agent has uncommitted changes. Ask it to commit first.".into());
    }
    let head = git(&wt, &["rev-parse", "HEAD"])?;
    let (_, src) = source_repo(ctx)?;
    let files = changed_files(&src, &branch)?;
    prepare_worktree(ctx, &wt)?;
    let t = sh(
        ctx,
        "Improve: tests",
        &wt,
        &gate_command(&files),
        shared_target(ctx),
        r["agent_id"].as_str(),
    )?;
    set(ctx, id, "test_terminal", &t)?;
    set(ctx, id, "tested_commit", &head)?;
    Ok(json!({ "terminal": t }))
}

/// Fast test gate: type-check everything (including tests), then run only the
/// Rust test modules the diff touched, plus the frontend build and node tests
/// when the frontend changed. The full suite takes far too long per change.
pub fn gate_command(files: &[String]) -> String {
    let mut mods: Vec<String> = files
        .iter()
        .filter_map(|f| f.strip_prefix("src-tauri/src/"))
        .filter_map(|f| f.strip_suffix(".rs"))
        .filter_map(|f| {
            let m = f.split('/').next().unwrap_or(f);
            (!matches!(m, "main" | "lib") && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| format!("{m}::"))
        })
        .collect();
    mods.sort();
    mods.dedup();
    let mut cmd = String::from("set -e; (cd src-tauri && cargo check --tests");
    if !mods.is_empty() {
        cmd += &format!(" && cargo test -- {}", mods.join(" "));
    }
    cmd += ")";
    let front = files.iter().any(|f| f.starts_with("src/") || matches!(f.as_str(), "package.json" | "package-lock.json" | "vite.config.js" | "index.html"));
    if front {
        cmd += "; npm install --no-audit --no-fund; npx vite build";
        if files.iter().any(|f| f.starts_with("src/")) {
            cmd += "; node --test $(find src -name '*.test.js' -not -path '*/node_modules/*')";
        }
    }
    cmd
}

// A live CLI process is not proof that the preview window is ready.
fn preview_state(state: Option<&str>, code: Option<i64>) -> &'static str {
    match (state, code) {
        (Some("running"), _) => "active",
        (Some("exited"), Some(0)) => "stopped",
        (Some("exited"), Some(_)) => "failed",
        _ => "unavailable",
    }
}

// Worktrees omit ignored binaries. Reuse the source checkout's staged sidecar,
// never build HAL implicitly or overwrite a worktree's existing binary.
fn sidecar_directory(wt: &Path) -> Result<(Option<std::fs::File>, PathBuf), String> {
    let wt = wt.canonicalize().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let mut handle = std::fs::File::open(&wt).map_err(|e| e.to_string())?;
    let mut directory = wt;
    for component in ["src-tauri", "binaries"] {
        directory.push(component);
        #[cfg(unix)] {
            use std::os::fd::{AsRawFd, FromRawFd};
            let name = std::ffi::CString::new(component).unwrap();
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            let mut fd = unsafe { libc::openat(handle.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                if unsafe { libc::mkdirat(handle.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                fd = unsafe { libc::openat(handle.as_raw_fd(), name.as_ptr(), flags) };
            }
            if fd < 0 { return Err(format!("Unsafe sidecar directory {}: {}", directory.display(), std::io::Error::last_os_error())); }
            handle = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        #[cfg(not(unix))] {
            match std::fs::symlink_metadata(&directory) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {},
                Ok(_) => return Err(format!("Unsafe sidecar directory: {}", directory.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&directory).map_err(|e| e.to_string())?,
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    #[cfg(unix)]
    return Ok((Some(handle), directory));
    #[cfg(not(unix))]
    Ok((None, directory))
}

fn stage_preview_sidecar(wt: &Path, src: &Path, target: &str) -> Result<(), String> {
    if target.is_empty() || !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Could not determine the Rust host target for the preview sidecar.".into());
    }
    let (directory_handle, directory) = sidecar_directory(wt)?;
    #[cfg(unix)]
    let directory_handle = directory_handle.ok_or("Missing sidecar directory handle")?;
    let ext = if target.contains("windows") { ".exe" } else { "" };
    let relative = format!("src-tauri/binaries/hal-desktop-server-{target}{ext}");
    let dest = directory.join(format!("hal-desktop-server-{target}{ext}"));
    #[cfg(unix)]
    let name = std::ffi::CString::new(format!("hal-desktop-server-{target}{ext}")).unwrap();
    #[cfg(unix)] {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe { libc::openat(directory_handle.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if fd >= 0 {
            let existing = unsafe { std::fs::File::from_raw_fd(fd) };
            if existing.metadata().map_err(|e| e.to_string())?.is_file() { return Ok(()); }
            return Err(format!("Unsafe sidecar destination: {}", dest.display()));
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return Err(format!("Unsafe sidecar destination: {}", dest.display()));
        }
    }
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(&dest) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => return Ok(()),
        Ok(_) => return Err(format!("Unsafe sidecar destination: {}", dest.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e.to_string()),
    }
    let source = src.join(&relative);
    let mut input = std::fs::File::open(&source).map_err(|e| format!(
        "Preview sidecar missing: {} ({e}). Stage it in the source checkout with npm run sidecar, then retry.", source.display()
    ))?;
    #[cfg(unix)]
    let mut output = {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe { libc::openat(directory_handle.as_raw_fd(), name.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
        if fd < 0 { return Err(std::io::Error::last_os_error().to_string()); }
        unsafe { std::fs::File::from_raw_fd(fd) }
    };
    #[cfg(not(unix))]
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| e.to_string())?;
    let result = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.set_permissions(input.metadata()?.permissions()));
    if let Err(e) = result {
        drop(output);
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            unsafe { libc::unlinkat(directory_handle.as_raw_fd(), name.as_ptr(), 0); }
        }
        #[cfg(not(unix))]
        let _ = std::fs::remove_file(&dest);
        return Err(format!("Could not stage preview sidecar: {e}"));
    }
    Ok(())
}

fn prepare_worktree(ctx: &Ctx, wt: &Path) -> Result<(), String> {
    let src = source_dir(ctx).ok_or("Philium source not found.")?;
    stage_preview_sidecar(wt, &src, env!("PHILIUM_TARGET_TRIPLE"))
}

// Every fresh worktree would otherwise compile all crates cold (many minutes).
// Reuse the source checkout's debug target dir so only Philium itself rebuilds.
fn shared_target(ctx: &Ctx) -> Vec<(String, String)> {
    source_dir(ctx)
        .map(|s| vec![("CARGO_TARGET_DIR".into(), s.join("src-tauri").join("target").to_string_lossy().into())])
        .unwrap_or_default()
}

/// Run the changed app side by side: own data dir, own root, keychain off.
fn preview(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = status(ctx, id)?;
    if r["preview"] == "active" {
        return Err("A preview process is already active. Stop it in its terminal before opening another.".into());
    }
    let (wt, _) = worktree(ctx, r["agent_id"].as_str().unwrap_or_default())?;
    prepare_worktree(ctx, &wt)?;
    let base = ctx.data_dir.join("previews").join(id);
    std::fs::create_dir_all(base.join("data")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(base.join("root")).map_err(|e| e.to_string())?;
    let port = 1420 + (crate::hal::random_token().as_bytes()[0] as u16 % 200) + 1;
    let mut env = shared_target(ctx);
    env.extend([
        ("PHILIUM_PREVIEW".into(), "1".into()),
        ("PHILIUM_DATA_DIR".into(), base.join("data").to_string_lossy().into()),
        ("PHILIUM_ROOT".into(), base.join("root").to_string_lossy().into()),
        ("PHILIUM_DEV_PORT".into(), port.to_string()),
        ("CODEX_HOME".into(), base.join("empty-codex").to_string_lossy().into()),
        ("CLAUDE_CONFIG_DIR".into(), base.join("empty-claude").to_string_lossy().into()),
    ]);
    let cmd = format!(
        "npm install --no-audit --no-fund && npx tauri dev --config '{{\"identifier\":\"{preview_id}\",\"build\":{{\"devUrl\":\"http://localhost:{port}\",\"beforeDevCommand\":\"npx vite --port {port} --strictPort\"}},\"app\":{{\"windows\":[{{\"title\":\"Philium preview\",\"width\":1200,\"height\":800}}]}}}}'",
        preview_id = crate::app_id::PREVIEW
    );
    let t = sh(ctx, "Improve: preview", &wt, &cmd, env, r["agent_id"].as_str())?;
    set(ctx, id, "preview_terminal", &t)?;
    set(ctx, id, "state", "preview")?;
    Ok(json!({ "terminal": t }))
}

fn accept(ctx: &Arc<Ctx>, id: &str, approve_security: bool) -> Result<Value, String> {
    let st = status(ctx, id)?;
    if st["state"] == "accepted" {
        return Err("Already accepted.".into());
    }
    if st["dirty"] == true {
        return Err("The agent has uncommitted changes. Ask it to commit first.".into());
    }
    if st["tests"] != "passed" {
        return Err("Run the tests first. They must pass before accepting.".into());
    }
    if st["tested_commit"] != st["head"] {
        return Err("The agent changed things after the tests ran. Run the tests again.".into());
    }
    let flagged = st["flagged"].as_array().cloned().unwrap_or_default();
    if !flagged.is_empty() && !approve_security {
        return Ok(json!({ "needs_security_approval": true, "flagged": flagged }));
    }
    let (_, src) = source_repo(ctx)?;
    if !git(&src, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("The Philium source has uncommitted changes. Commit or stash them first.".into());
    }
    let (_, branch) = worktree(ctx, st["agent_id"].as_str().unwrap_or_default())?;
    let prev = git(&src, &["rev-parse", "HEAD"])?;
    let msg = format!("Improve Philium: {}", st["request"].as_str().unwrap_or("").lines().next().unwrap_or(""));
    git(&src, &["merge", "--no-ff", "-m", &msg, &branch])?;
    let merge = git(&src, &["rev-parse", "HEAD"])?;
    set(ctx, id, "prev_head", &prev)?;
    set(ctx, id, "merge_commit", &merge)?;
    set(ctx, id, "state", "accepted")?;
    ctx.log_event("selfdev_accepted", st["agent_id"].as_str(), &json!({ "id": id, "prev": prev, "merge": merge, "flagged": flagged }));
    if let Some(t) = st["task_id"].as_str() {
        let _ = crate::board::update(ctx, "you", t, &json!({ "status": "done" }));
    }
    let b = rebuild(ctx, id, &src)?;
    Ok(json!({ "merged": merge, "previous": prev, "build_terminal": b }))
}

fn rebuild(ctx: &Arc<Ctx>, id: &str, src: &Path) -> Result<String, String> {
    let t = sh(
        ctx,
        "Improve: rebuild Philium",
        src,
        "set -e; npm install --no-audit --no-fund; npx tauri build --bundles app",
        vec![],
        None,
    )?;
    set(ctx, id, "build_terminal", &t)?;
    Ok(t)
}

fn rollback(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = row(ctx, id)?;
    let merge = r["merge_commit"].as_str().ok_or("Nothing to roll back: this change was never accepted.")?.to_string();
    let (_, src) = source_repo(ctx)?;
    if !git(&src, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err("The Philium source has uncommitted changes. Commit or stash them first.".into());
    }
    git(&src, &["revert", "--no-edit", "-m", "1", &merge])?;
    set(ctx, id, "state", "rolled_back")?;
    ctx.log_event("selfdev_rolled_back", None, &json!({ "id": id, "merge": merge }));
    let b = rebuild(ctx, id, &src)?;
    Ok(json!({ "build_terminal": b }))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "source" => Ok(json!(source_dir(ctx).map(|p| p.to_string_lossy().to_string()))),
        "list" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || reconcile_all(&c)).await.map_err(|e| e.to_string())?;
            Ok(json!(ctx.db.query("SELECT * FROM selfdev ORDER BY created_at DESC LIMIT 200", [])?))
        }
        "retry" => {
            let c = ctx.clone();
            let id = arg_str(&args, "id")?;
            tokio::task::spawn_blocking(move || retry(&c, &id)).await.map_err(|e| e.to_string())?
        }
        // Throw the change away: its agent (if any), worktree and branch.
        "discard" => {
            let id = arg_str(&args, "id")?;
            let r = reconcile(ctx, &id)?;
            if r["state"] == "accepted" {
                return Err("Accepted changes are rolled back, not discarded.".into());
            }
            let force = args["force"].as_bool().unwrap_or(false);
            let wt = own_worktree(ctx, &r);
            if let Some(a) = r["agent_id"].as_str().filter(|a| crate::agents::get_row(ctx, a).is_some()) {
                let rep = crate::agents::call(ctx, "remove", json!({ "id": a, "force": force })).await?;
                if rep["needs_confirmation"] == true {
                    return Ok(rep);
                }
            }
            if let Some(w) = wt.and_then(|w| w["id"].as_str().map(str::to_string)) {
                if ctx.db.query("SELECT id FROM worktrees WHERE id=?1 AND state='active'", [&w])?.first().is_some() {
                    if !force {
                        let path = ctx.db.query("SELECT path FROM worktrees WHERE id=?1", [&w])?.first().and_then(|x| x["path"].as_str().map(PathBuf::from));
                        if path.is_some_and(|p| p.is_dir() && git(&p, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)) {
                            return Ok(json!({ "needs_confirmation": true, "reason": "The worktree has uncommitted changes." }));
                        }
                    }
                    let c = ctx.clone();
                    tokio::task::spawn_blocking(move || crate::workspace::remove_worktree(&c, &w, true)).await.map_err(|e| e.to_string())??;
                }
            }
            if let Some(t) = r["task_id"].as_str() {
                let _ = ctx.db.exec("UPDATE tasks SET status='cancelled', updated_at=?2 WHERE id=?1 AND status NOT IN ('done','cancelled')", rusqlite::params![t, now()]);
            }
            ctx.db.exec("UPDATE selfdev SET state='discarded', updated_at=?2 WHERE id=?1", rusqlite::params![id, now()])?;
            ctx.emit("selfdev:changed", json!({ "id": id }));
            Ok(json!({ "discarded": true }))
        }
        "get" => {
            let c = ctx.clone();
            let id = arg_str(&args, "id")?;
            tokio::task::spawn_blocking(move || status(&c, &id)).await.map_err(|e| e.to_string())?
        }
        "request" => {
            let c = ctx.clone();
            let (text, context, model) = (arg_str(&args, "text")?, args["context"].clone(), arg_opt_str(&args, "model"));
            tokio::task::spawn_blocking(move || request(&c, &text, &context, model)).await.map_err(|e| e.to_string())?
        }
        "test" => {
            let c = ctx.clone();
            let id = arg_str(&args, "id")?;
            tokio::task::spawn_blocking(move || test(&c, &id)).await.map_err(|e| e.to_string())?
        }
        // What the agent changed versus the source checkout, for review before accepting.
        "diff" => {
            let r = row(ctx, &arg_str(&args, "id")?)?;
            crate::agents::call(ctx, "diff", json!({ "id": r["agent_id"] })).await
        }
        "preview" => {
            let c = ctx.clone();
            let id = arg_str(&args, "id")?;
            tokio::task::spawn_blocking(move || preview(&c, &id)).await.map_err(|e| e.to_string())?
        }
        "accept" => {
            let c = ctx.clone();
            let (id, ok) = (arg_str(&args, "id")?, args["approve_security"].as_bool().unwrap_or(false));
            tokio::task::spawn_blocking(move || accept(&c, &id, ok)).await.map_err(|e| e.to_string())?
        }
        "rollback" => {
            let c = ctx.clone();
            let id = arg_str(&args, "id")?;
            tokio::task::spawn_blocking(move || rollback(&c, &id)).await.map_err(|e| e.to_string())?
        }
        "reject" => {
            let id = arg_str(&args, "id")?;
            let r = row(ctx, &id)?;
            let rep = crate::agents::call(
                ctx,
                "remove",
                json!({ "id": r["agent_id"], "force": args["force"].as_bool().unwrap_or(false) }),
            )
            .await?;
            if rep["needs_confirmation"] != true {
                set(ctx, &id, "state", "rejected")?;
            }
            Ok(rep)
        }
        "restart" => {
            let h = ctx.handle().ok_or("no app handle")?;
            h.restart();
        }
        _ => Err(format!("unknown selfdev.{method}")),
    }
}

#[cfg(test)]
mod preview_focused_tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("philium-preview-test-{}-{unique}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn terminal_status_never_calls_an_exited_preview_active() {
        assert_eq!(preview_state(Some("running"), None), "active");
        assert_eq!(preview_state(Some("exited"), Some(101)), "failed");
        assert_eq!(preview_state(Some("exited"), Some(0)), "stopped");
        assert_eq!(preview_state(Some("exited"), None), "unavailable");
        assert_eq!(preview_state(None, None), "unavailable");
    }

    #[test]
    fn sidecar_is_staged_without_overwriting_existing_worktree_binary() {
        let f = Fixture::new();
        let src = f.0.join("source");
        let wt = f.0.join("worktree");
        std::fs::create_dir(&wt).unwrap();
        let relative = "src-tauri/binaries/hal-desktop-server-aarch64-apple-darwin";
        std::fs::create_dir_all(src.join("src-tauri/binaries")).unwrap();
        std::fs::write(src.join(relative), b"staged sidecar").unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(src.join(relative), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        stage_preview_sidecar(&wt, &src, "aarch64-apple-darwin").unwrap();
        assert_eq!(std::fs::read(wt.join(relative)).unwrap(), b"staged sidecar");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(wt.join(relative)).unwrap().permissions().mode() & 0o777, 0o755);
        }
        std::fs::write(src.join(relative), b"different source").unwrap();
        stage_preview_sidecar(&wt, &src, "aarch64-apple-darwin").unwrap();
        assert_eq!(std::fs::read(wt.join(relative)).unwrap(), b"staged sidecar");
    }

    #[test]
    fn missing_sidecar_fails_before_launching_or_building() {
        let f = Fixture::new();
        let wt = f.0.join("worktree");
        std::fs::create_dir(&wt).unwrap();
        let error = stage_preview_sidecar(&wt, &f.0.join("source"), "aarch64-apple-darwin").unwrap_err();
        assert!(error.contains("npm run sidecar"), "{error}");
        assert!(!wt.join("src-tauri/binaries/hal-desktop-server-aarch64-apple-darwin").exists());
        assert!(stage_preview_sidecar(&wt, &f.0, "../escape").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn staging_rejects_directory_and_leaf_symlink_escapes() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = f.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        for component in ["src-tauri", "binaries", "leaf"] {
            let wt = f.0.join(component);
            std::fs::create_dir(&wt).unwrap();
            let link = if component == "src-tauri" {
                wt.join("src-tauri")
            } else {
                std::fs::create_dir(wt.join("src-tauri")).unwrap();
                if component == "binaries" { wt.join("src-tauri/binaries") } else {
                    std::fs::create_dir(wt.join("src-tauri/binaries")).unwrap();
                    wt.join("src-tauri/binaries/hal-desktop-server-aarch64-apple-darwin")
                }
            };
            symlink(&outside, &link).unwrap();
            let error = stage_preview_sidecar(&wt, &f.0, "aarch64-apple-darwin").unwrap_err();
            assert!(error.contains("Unsafe sidecar"), "{error}");
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the compiled test binary directly (not under cargo test) so the
    /// actual nested cargo test can use an explicitly chosen build target.
    /// Supply disposable source/worktree checkouts with PHILIUM_SELFDEV_TEST_*.
    #[tokio::test]
    #[ignore = "requires disposable clean worktree, staged source sidecar and real cargo/npm/vite"]
    async fn public_test_stages_sidecar_before_real_cargo() {
        let src = PathBuf::from(std::env::var_os("PHILIUM_SELFDEV_TEST_SOURCE").expect("disposable source"));
        let wt = PathBuf::from(std::env::var_os("PHILIUM_SELFDEV_TEST_WORKTREE").expect("disposable worktree"));
        let head = git(&wt, &["rev-parse", "HEAD"]).unwrap();
        assert!(git(&wt, &["status", "--porcelain"]).unwrap().is_empty());
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let relative = format!("src-tauri/binaries/hal-desktop-server-{}{ext}", env!("PHILIUM_TARGET_TRIPLE"));
        assert!(!wt.join(&relative).exists(), "must start without the ignored sidecar");
        assert!(src.join(&relative).is_file());
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        crate::terminals::init(&ctx).unwrap();
        ctx.db.kv_set("setting.selfdev.source", &src.to_string_lossy()).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('repo','fixture',?1,0)", [src.to_string_lossy().as_ref()]).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('agent','fixture','test',0,0)", []).unwrap();
        ctx.db.exec("INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,created_at) VALUES('wt','repo','agent',?1,'HEAD',?2,0)", rusqlite::params![wt.to_string_lossy(), head]).unwrap();
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,state,created_at,updated_at) VALUES('improvement','fixture','agent','building',0,0)", []).unwrap();
        struct Cleanup(Arc<Ctx>, Option<String>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(id) = &self.1 { let _ = crate::terminals::kill(&self.0, id); }
                if let Some(root) = self.0.data_dir.parent() { let _ = std::fs::remove_dir_all(root); }
            }
        }
        let mut cleanup = Cleanup(ctx.clone(), None);
        let reply = call(&ctx, "test", json!({"id":"improvement"})).await.unwrap();
        let terminal = reply["terminal"].as_str().unwrap().to_string();
        cleanup.1 = Some(terminal.clone());
        assert_eq!(std::fs::read(wt.join(&relative)).unwrap(), std::fs::read(src.join(&relative)).unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        let exit = loop {
            if let Some(exit) = term_exit(&ctx, &terminal) { break exit; }
            assert!(std::time::Instant::now() < deadline, "real selfdev.test timed out");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        let output = crate::terminals::snapshot(&ctx, &terminal, 256 * 1024).unwrap_or_default();
        println!("public selfdev.test worktree HEAD={head} exit={exit}\n{output}");
        assert_eq!(exit, 0, "actual selfdev.test failed, see captured terminal output");
        assert!(output.contains("test result: ok"), "must observe real Rust test completion");
        let status = call(&ctx, "get", json!({"id":"improvement"})).await.unwrap();
        assert_eq!(status["tests"], "passed");
        assert_eq!(status["tested_commit"], head);
        crate::terminals::call(&ctx, "remove", json!({"id":terminal})).await.unwrap();
        cleanup.1 = None;
    }

    /// The whole improver as a user drives it, against the real HAL sidecar, gateway, MCP
    /// and terminals: request -> agent edits a tooltip in its own worktree of a disposable
    /// clone and commits -> diff -> real tests -> accept (merge) -> reject cleans up.
    /// A scripted stand-in model plays the agent. Run via scripts/selfdev-e2e.sh.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "real HAL sidecar, disposable Philium clone and real cargo/npm; run scripts/selfdev-e2e.sh"]
    async fn improver_end_to_end_tooltip_change() {
        use crate::gateway::{Gateway, Route};
        use crate::guard::GuardMode;
        use crate::providers::{Kind, Provider};
        use std::time::{Duration, Instant};
        assert!(crate::hal::server_path().is_file(), "HAL sidecar required");
        let src = PathBuf::from(std::env::var_os("PHILIUM_SELFDEV_TEST_SOURCE").expect("disposable source clone"));
        let scratch = PathBuf::from(std::env::var_os("PHILIUM_SELFDEV_TEST_SCRATCH").expect("scratch dir"));
        let commit = "perl -pi -e 's/title=\"Improve Philium\"/title=\"Improve Philium (ask an agent)\"/' src/main.jsx && git add src/main.jsx && git -c user.name=Agent -c user.email=agent@example.invalid commit -qm 'Clearer Improve tooltip' && echo COMMITTED";
        let app = axum::Router::new().route("/v1/{*rest}", axum::routing::any(move |body: String| async move {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if v["stream"] != json!(true) {
                return ([("content-type", "application/json")], r#"{"data":[{"id":"up-model"}]}"#.to_string());
            }
            let tools = v["messages"].as_array().map(|m| m.iter().filter(|m| m["role"] == "tool").count()).unwrap_or(0);
            let delta = if tools == 0 {
                let args = json!({"command": commit, "timeout_secs": 60}).to_string();
                json!({"tool_calls":[{"index":0,"id":"call_0","type":"function","function":{"name":"mcp_philium_shell_run","arguments":args}}]})
            } else {
                json!({"role":"assistant","content":"DONE. Tooltip on the Improve button now reads Improve Philium (ask an agent)."})
            };
            let fin = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
            ([("content-type", "text/event-stream")], format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":fin}],"usage":{"prompt_tokens":2,"completion_tokens":2}}))
            )
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let gw = Gateway::new(crate::hal::random_token(), None);
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gurl = format!("http://{}", gl.local_addr().unwrap());
        let r = gw.router();
        tokio::spawn(async move { axum::serve(gl, r).await.unwrap() });
        let ctx = Ctx::new(crate::db::Db::memory(), scratch.join("data"), scratch.join("root"), gw.clone(), gurl);
        let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ctx.mcp_url.set(format!("http://{}/mcp", ml.local_addr().unwrap())).unwrap();
        let mr = crate::mcp::router(ctx.clone());
        tokio::spawn(async move { axum::serve(ml, mr).await.unwrap() });
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        for f in [crate::library::init, crate::codeindex::init, crate::terminals::init, crate::workspace::init, crate::board::init, crate::reaper::init, init] {
            f(&ctx).unwrap();
        }
        let p = Provider { id: "stub".into(), name: "Stub".into(), kind: Kind::OpenaiChat, base_url: up, trusted: true, guard: GuardMode::Redact, enabled: true, default_model: Some("up-model".into()) };
        crate::providers::upsert(&ctx, &p).unwrap();
        gw.set_routes(vec![Route { provider: p, api_key: Some("fixture".into()) }]);
        ctx.db.kv_set("setting.selfdev.source", &src.to_string_lossy()).unwrap();
        let base = git(&src, &["rev-parse", "HEAD"]).unwrap();

        // 1. The user asks from the Improve dialog.
        assert_eq!(call(&ctx, "source", json!({})).await.unwrap(), json!(src.to_string_lossy()));
        let req = call(&ctx, "request", json!({"text": "Change the Improve button tooltip to say Improve Philium (ask an agent)", "context": {"screen": "home"}, "model": "stub/up-model"})).await.unwrap();
        let id = req["id"].as_str().unwrap().to_string();
        let agent = req["agent"]["id"].as_str().unwrap().to_string();
        struct Stop(Arc<Ctx>, String);
        impl Drop for Stop { fn drop(&mut self) { crate::agents::stop(&self.0, &self.1, "stopped"); crate::terminals::kill_agent(&self.0, &self.1); } }
        let _stop = Stop(ctx.clone(), agent.clone());

        // 2. The agent works in its own worktree and commits.
        let deadline = Instant::now() + Duration::from_secs(120);
        let st = loop {
            if let Some(e) = ctx.db.kv_get(&format!("agent_error.{agent}")).filter(|e| !e.is_empty()) { panic!("agent failed: {e}"); }
            let st = call(&ctx, "get", json!({"id": id})).await.unwrap();
            if st["files"].as_array().is_some_and(|f| !f.is_empty()) && st["dirty"] == false { break st; }
            assert!(Instant::now() < deadline, "agent never committed: {st}");
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        assert_eq!(st["state"], "building");
        assert_eq!(st["files"], json!(["src/main.jsx"]), "{st}");
        assert_eq!(st["flagged"], json!([]));
        let (wt, _) = worktree(&ctx, &agent).unwrap();
        assert_ne!(wt, src, "agent must work in a separate worktree");
        assert!(std::fs::read_to_string(src.join("src/main.jsx")).unwrap().contains("title=\"Improve Philium\""), "source untouched before accept");

        // 3. The diff the user reviews.
        let diff = call(&ctx, "diff", json!({"id": id})).await.unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+            <button class=\"icon-btn\" onClick={() => improve()} title=\"Improve Philium (ask an agent)\">"), "{diff}");

        // 4. Accept is refused until tests pass on this exact commit.
        assert!(call(&ctx, "accept", json!({"id": id})).await.unwrap_err().contains("Run the tests first"));
        let t = call(&ctx, "test", json!({"id": id})).await.unwrap()["terminal"].as_str().unwrap().to_string();
        let deadline = Instant::now() + Duration::from_secs(1500);
        let exit = loop {
            if let Some(x) = term_exit(&ctx, &t) { break x; }
            assert!(Instant::now() < deadline, "tests timed out");
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let out = crate::terminals::snapshot(&ctx, &t, 64 * 1024).unwrap_or_default();
        assert_eq!(exit, 0, "selfdev tests failed:\n{out}");
        let st = call(&ctx, "get", json!({"id": id})).await.unwrap();
        assert_eq!((st["tests"].clone(), st["tested_commit"].clone()), (json!("passed"), st["head"].clone()), "{st}");

        // 5. Accept merges into the source checkout and starts a rebuild (stopped here).
        let acc = call(&ctx, "accept", json!({"id": id})).await.unwrap();
        crate::terminals::kill(&ctx, acc["build_terminal"].as_str().unwrap()).unwrap();
        assert_eq!(acc["previous"], json!(base));
        assert!(std::fs::read_to_string(src.join("src/main.jsx")).unwrap().contains("title=\"Improve Philium (ask an agent)\""));
        assert_eq!(row(&ctx, &id).unwrap()["state"], "accepted");

        // 6. Roll back adds a revert commit.
        let rb = call(&ctx, "rollback", json!({"id": id})).await.unwrap();
        crate::terminals::kill(&ctx, rb["build_terminal"].as_str().unwrap()).unwrap();
        assert!(std::fs::read_to_string(src.join("src/main.jsx")).unwrap().contains("title=\"Improve Philium\""));

        // 7. Discard: the agent and its worktree go away.
        let mut rep = call(&ctx, "reject", json!({"id": id})).await.unwrap();
        if rep["needs_confirmation"] == true { rep = call(&ctx, "reject", json!({"id": id, "force": true})).await.unwrap(); }
        assert_ne!(rep["needs_confirmation"], true, "{rep}");
        assert_eq!(row(&ctx, &id).unwrap()["state"], "rejected");
        assert!(!wt.exists(), "worktree removed on discard");
        println!("improver e2e: request, agent commit in {}, diff, tests {}, accept, rollback, discard OK", wt.display(), "passed");
    }

    #[test]
    fn gate_runs_only_touched_modules() {
        let f = |v: &[&str]| gate_command(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let c = f(&["src-tauri/src/selfdev.rs", "src-tauri/src/agents/x.rs", "src-tauri/src/main.rs"]);
        assert!(c.contains("cargo check --tests"), "{c}");
        assert!(c.contains("cargo test -- agents:: selfdev::"), "{c}");
        assert!(!c.contains("vite"), "{c}");
        let c = f(&["src/ui/improve.jsx"]);
        assert!(!c.contains("cargo test"), "{c}");
        assert!(c.contains("npx vite build") && c.contains("node --test"), "{c}");
    }

    #[test]
    fn security_files_are_flagged() {
        let f = flagged(&[
            "src/screens/home.jsx".into(),
            "src-tauri/src/gateway.rs".into(),
            "src-tauri/capabilities/default.json".into(),
            "src-tauri/src/guardian.rs".into(),
        ]);
        assert_eq!(f, vec!["src-tauri/src/gateway.rs".to_string(), "src-tauri/capabilities/default.json".to_string()]);
    }

    #[test]
    fn redacts_context() {
        let s = redact("key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA here");
        assert!(!s.contains("AAAAAAAAAAAAAAAAAAAA"), "{s}");
    }

    #[test]
    fn table_and_row_lookup() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        assert!(row(&ctx, "nope").is_err());
        ctx.db
            .exec("INSERT INTO selfdev(id,request,state,created_at,updated_at) VALUES('x','r','building',0,0)", [])
            .unwrap();
        set(&ctx, "x", "state", "preview").unwrap();
        assert_eq!(row(&ctx, "x").unwrap()["state"], "preview");
    }

    // The 'lion' run: its agent was removed while building, and the improvement stayed
    // 'building' for 11 hours. Now the first look at it marks it failed with a reason.
    #[test]
    fn deleted_agent_fails_the_improvement() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES('a','lion','p/m','running',0,0)", []).unwrap();
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,state,created_at,updated_at) VALUES('s','r','a','building',0,0)", []).unwrap();
        assert_eq!(reconcile(&ctx, "s").unwrap()["state"], "building", "a working agent keeps it building");
        ctx.db.exec("DELETE FROM agents WHERE id='a'", []).unwrap();
        let r = reconcile(&ctx, "s").unwrap();
        assert_eq!(r["state"], "failed");
        assert!(r["error"].as_str().unwrap().contains("removed"), "{r}");
        // Startup and list sweeps do the same for rows nobody opened.
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,state,created_at,updated_at) VALUES('t','r','gone','building',0,0)", []).unwrap();
        reconcile_all(&ctx);
        assert_eq!(row(&ctx, "t").unwrap()["state"], "failed");
    }

    #[test]
    fn stopped_agent_or_cancelled_task_stops_the_improvement() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES('a','lion','p/m','stopped',0,0)", []).unwrap();
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,state,created_at,updated_at) VALUES('s','r','a','building',0,0)", []).unwrap();
        let r = reconcile(&ctx, "s").unwrap();
        assert_eq!(r["state"], "stopped");
        assert!(r["error"].as_str().unwrap().contains("lion"), "{r}");
        // A stopped agent after building is normal: preview keeps working.
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,state,created_at,updated_at) VALUES('p','r','a','preview',0,0)", []).unwrap();
        assert_eq!(reconcile(&ctx, "p").unwrap()["state"], "preview");
        ctx.db.exec("INSERT INTO tasks(id,title,status,created_at,updated_at) VALUES('tk','Philium: x','claimed',0,0)", []).unwrap();
        let tid = "tk";
        ctx.db.exec("UPDATE agents SET state='idle' WHERE id='a'", []).unwrap();
        ctx.db.exec("INSERT INTO selfdev(id,request,agent_id,task_id,state,created_at,updated_at) VALUES('c','r','a',?1,'building',0,0)", [tid]).unwrap();
        assert_eq!(reconcile(&ctx, "c").unwrap()["state"], "building");
        ctx.db.exec("UPDATE tasks SET status='cancelled' WHERE id=?1", [tid]).unwrap();
        let r = reconcile(&ctx, "c").unwrap();
        assert_eq!(r["state"], "stopped");
        assert!(r["error"].as_str().unwrap().contains("cancelled"));
    }

    #[test]
    fn improver_is_a_normal_long_running_worker() {
        let b = improver_brief("big", "{}");
        assert!(b.contains("Plan first") && b.contains("as long as it takes"));
    }
}
