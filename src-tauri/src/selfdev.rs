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

fn status(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let mut r = row(ctx, id)?;
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
    let brief = format!(
        "You are improving Philium itself, in a worktree of its source. Never touch the running app or its data.\n\
         Request from the user:\n{text}\n\nWhere they were: {context}\n\n\
         Rules:\n\
         - Keep the existing design system (src/ui/kit.jsx, src/styles). No em dashes in UI text.\n\
         - Run `cargo test` in src-tauri and `npx vite build` before you say you are done.\n\
         - Commit on your branch with a plain message. No Co-authored-by or agent signatures.\n\
         - Do not change gateway, guard, providers, sandbox, mcp or agents code unless the request needs it; if you do, explain why in your final message.\n\
         - When finished, say DONE and summarize what changed and how to try it."
    );
    let agent = crate::agents::spawn(
        ctx,
        crate::agents::SpawnReq {
            name: None,
            role: Some("selfdev".into()),
            model,
            repo_id: Some(repo_id),
            task_id: Some(task_id.clone()),
            brief: Some(brief),
            parent_id: None,
            strict: false,
        },
    )?;
    let id = new_id();
    ctx.db.exec(
        "INSERT INTO selfdev(id,request,context,agent_id,task_id,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'building',?6,?6)",
        rusqlite::params![id, text, context, agent["id"].as_str(), task_id, now()],
    )?;
    ctx.log_event("selfdev_requested", agent["id"].as_str(), &json!({ "id": id, "title": title }));
    ctx.emit("selfdev:changed", json!({ "id": id }));
    Ok(json!({ "id": id, "agent": agent }))
}

fn test(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = row(ctx, id)?;
    let (wt, _) = worktree(ctx, r["agent_id"].as_str().unwrap_or_default())?;
    if !git(&wt, &["status", "--porcelain"])?.is_empty() {
        return Err("The agent has uncommitted changes. Ask it to commit first.".into());
    }
    let head = git(&wt, &["rev-parse", "HEAD"])?;
    let t = sh(
        ctx,
        "Improve: tests",
        &wt,
        "set -e; (cd src-tauri && cargo test); npm install --no-audit --no-fund; npx vite build",
        vec![],
        r["agent_id"].as_str(),
    )?;
    set(ctx, id, "test_terminal", &t)?;
    set(ctx, id, "tested_commit", &head)?;
    Ok(json!({ "terminal": t }))
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
fn stage_preview_sidecar(wt: &Path, src: &Path, target: &str) -> Result<(), String> {
    if target.is_empty() || !target.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Could not determine the Rust host target for the preview sidecar.".into());
    }
    let ext = if target.contains("windows") { ".exe" } else { "" };
    let relative = format!("src-tauri/binaries/hal-desktop-server-{target}{ext}");
    let dest = wt.join(&relative);
    if dest.is_file() { return Ok(()); }
    let source = src.join(&relative);
    let mut input = std::fs::File::open(&source).map_err(|e| format!(
        "Preview sidecar missing: {} ({e}). Stage it in the source checkout with npm run sidecar, then retry.", source.display()
    ))?;
    std::fs::create_dir_all(dest.parent().ok_or("Invalid sidecar path")?).map_err(|e| e.to_string())?;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| e.to_string())?;
    let result = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.set_permissions(input.metadata()?.permissions()));
    if let Err(e) = result {
        drop(output);
        let _ = std::fs::remove_file(&dest);
        return Err(format!("Could not stage preview sidecar: {e}"));
    }
    Ok(())
}

/// Run the changed app side by side: own data dir, own root, keychain off.
fn preview(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let r = status(ctx, id)?;
    if r["preview"] == "active" {
        return Err("A preview process is already active. Stop it in its terminal before opening another.".into());
    }
    let (wt, _) = worktree(ctx, r["agent_id"].as_str().unwrap_or_default())?;
    let src = source_dir(ctx).ok_or("Philium source not found.")?;
    let host = std::process::Command::new("rustc").arg("-vV").output().map_err(|e| e.to_string())?;
    if !host.status.success() { return Err("rustc -vV failed while checking the preview sidecar.".into()); }
    let version = String::from_utf8_lossy(&host.stdout);
    let target = version.lines().find_map(|l| l.strip_prefix("host: ")).ok_or("rustc returned no host target")?;
    stage_preview_sidecar(&wt, &src, target)?;
    let base = ctx.data_dir.join("previews").join(id);
    std::fs::create_dir_all(base.join("data")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(base.join("root")).map_err(|e| e.to_string())?;
    let port = 1420 + (crate::hal::random_token().as_bytes()[0] as u16 % 200) + 1;
    let env = vec![
        ("PHILIUM_PREVIEW".into(), "1".into()),
        ("PHILIUM_DATA_DIR".into(), base.join("data").to_string_lossy().into()),
        ("PHILIUM_ROOT".into(), base.join("root").to_string_lossy().into()),
        ("PHILIUM_DEV_PORT".into(), port.to_string()),
        ("CODEX_HOME".into(), base.join("empty-codex").to_string_lossy().into()),
        ("CLAUDE_CONFIG_DIR".into(), base.join("empty-claude").to_string_lossy().into()),
    ];
    let cmd = format!(
        "npm install --no-audit --no-fund && npx tauri dev --config '{{\"identifier\":\"dev.gunnarsson.philium.preview\",\"build\":{{\"devUrl\":\"http://localhost:{port}\",\"beforeDevCommand\":\"npx vite --port {port} --strictPort\"}},\"app\":{{\"windows\":[{{\"title\":\"Philium preview\",\"width\":1200,\"height\":800}}]}}}}'"
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
        "list" => Ok(json!(ctx.db.query("SELECT * FROM selfdev ORDER BY created_at DESC LIMIT 200", [])?)),
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
            let dir = std::env::temp_dir().join(format!("philium-preview-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
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
    fn missing_sidecar_fails_before_creating_worktree_or_building() {
        let f = Fixture::new();
        let wt = f.0.join("worktree");
        let error = stage_preview_sidecar(&wt, &f.0.join("source"), "aarch64-apple-darwin").unwrap_err();
        assert!(error.contains("npm run sidecar"), "{error}");
        assert!(!wt.exists());
        assert!(stage_preview_sidecar(&wt, &f.0, "../escape").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
