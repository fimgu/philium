//! Side-by-side trial. Selected by the compiled bundle identifier, not user settings.
use crate::{board, ctx::Ctx, db::Db, gateway::Gateway, hal, workspace};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const ID: &str = "dev.gunnarsson.philium.pilot";
static BASE: OnceLock<PathBuf> = OnceLock::new();
pub fn active() -> bool {
    BASE.get().is_some()
}

// Only the human-operated Environments explorer uses this fixed host directory.
// Do not restore the real HOME to children or expose an arbitrary host path.
pub(crate) fn colima_dir() -> Option<PathBuf> {
    BASE.get().and_then(|base| base.parent().map(|p| p.join(".colima")))
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".colima")))
}

// Refuse links rather than accidentally following a pilot path into live data.
fn child_dir(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let p = parent.join(name);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(&p) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.to_string()),
    }
    if !fs::symlink_metadata(&p)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err(format!("Not a private directory: {}", p.display()));
    }
    Ok(p)
}

pub fn prepare() -> Result<(), String> {
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME missing")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let base = child_dir(&home, "Philium Pilot")?;
    let data = child_dir(&base, "data")?;
    let root = child_dir(&base, "workspace")?;
    let private_home = child_dir(&base, "home")?;
    // Before the desktop/runtime threads start. Never inherit preview overrides from a parent app.
    env::set_var("PHILIUM_DATA_DIR", data);
    env::set_var("PHILIUM_ROOT", root);
    env::set_var("PHILIUM_PREVIEW", "1");
    env::set_var("HOME", &private_home);
    for (key, dir) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("CODEX_HOME", "codex"),
        ("CLAUDE_CONFIG_DIR", "claude"),
    ] {
        env::set_var(key, private_home.join(dir));
    }
    BASE.set(base)
        .map_err(|_| "Pilot already initialized".into())
}

/// Packaged, non-graphical regression. Only accepts an explicitly marked disposable HOME.
/// The test harness starts an independent process for each phase. No imported real workspaces.
pub async fn check() -> Result<(), String> {
    let base = BASE.get().ok_or("Not a pilot binary")?;
    let marker = base
        .parent()
        .ok_or("No fixture home")?
        .join(".philium-pilot-fixture");
    if fs::read_to_string(marker).ok().as_deref() != Some("disposable\n") {
        return Err("A disposable test home is required".into());
    }
    let phase = env::args().nth(2).ok_or("Missing seed or recover")?;
    if !matches!(phase.as_str(), "seed" | "recover") {
        return Err("Unknown phase".into());
    }
    let dir = base.join("data");
    let db = dir.join("philium.db");
    if (phase == "seed") == db.exists() {
        return Err("Unexpected profile state".into());
    }
    let ctx = Ctx::new(
        Db::open(&db)?,
        dir,
        base.join("workspace"),
        Gateway::new(hal::random_token(), None),
        "http://127.0.0.1:9".into(),
    );
    workspace::init(&ctx)?;
    board::init(&ctx)?;
    crate::library::init(&ctx)?;
    crate::reaper::init(&ctx)?;
    crate::refresh_desk(&ctx);
    let desk = ctx.desk().ok_or("Bundled HAL did not start")?;
    let result = async {
        let app = crate::App { ctx: ctx.clone() };
        for method in ["selfdev.apply", "reaper.sweep"] {
            if !crate::rpc_inner(&app, method.into(), None)
                .await
                .unwrap_err()
                .contains("disabled in the side-by-side pilot")
            {
                return Err(format!("Missing pilot guard: {method}"));
            }
        }
        for name in ["alpha", "beta"] {
            let path = base.join(name).join("same");
            if phase == "seed" {
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                workspace::git(&path, &["init", "-b", "main"])?;
                workspace::git(
                    &path,
                    &[
                        "-c",
                        "user.name=Pilot",
                        "-c",
                        "user.email=pilot@example.invalid",
                        "commit",
                        "--allow-empty",
                        "-m",
                        name,
                    ],
                )?;
                let r = crate::rpc_inner(
                    &app,
                    "workspace.repo_add".into(),
                    Some(json!({"path":path})),
                )
                .await?;
                crate::rpc_inner(
                    &app,
                    "board.create".into(),
                    Some(json!({"repo_id":r["id"],"title":name})),
                )
                .await?;
                ctx.db.kv_set(
                    &format!("fixture.{name}"),
                    r["id"].as_str().ok_or("Missing ID")?,
                )?;
            }
            let id = ctx
                .db
                .kv_get(&format!("fixture.{name}"))
                .ok_or("Missing saved repo")?;
            if workspace::repo(&ctx, &id)
                .as_ref()
                .and_then(|r| r["path"].as_str())
                != path.to_str()
            {
                return Err("Checkout not preserved".to_owned());
            }
            let tasks =
                crate::rpc_inner(&app, "board.list".into(), Some(json!({"repo_id":id}))).await?;
            let rows = tasks.as_array().ok_or("Missing board")?;
            if rows.len() != 1 || rows[0]["title"] != name {
                return Err("Lost task or peer task visible".into());
            }
        }
        let alpha = ctx.db.kv_get("fixture.alpha").ok_or("Missing alpha checkout")?;
        let beta = ctx.db.kv_get("fixture.beta").ok_or("Missing beta checkout")?;
        let tasks = crate::rpc_inner(&app,"board.list".into(),Some(json!({"repo_id":alpha}))).await?;
        let task = tasks[0]["id"].as_str().ok_or("Missing alpha task")?;
        let denied = crate::rpc_inner(&app,"agents.spawn".into(),Some(json!({"repo_id":beta,"task_id":task}))).await;
        if !matches!(denied, Err(ref msg) if msg.contains("Choose a task from the agent's workspace")) { return Err("Sibling task was not rejected before provisioning".into()); }
        for (rid, label) in [(&alpha,"alpha"),(&beta,"beta")] {
            if phase == "seed" {
                ctx.db.exec("INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES(?1,?1,'fixture',?2,1,1)", rusqlite::params![label,rid])?;
                crate::reaper::register(&ctx,Some(label),"kind_cluster",label,json!({}));
                let tid=crate::rpc_inner(&app,"terminals.open".into(),Some(json!({"repo_id":rid,"cwd":base,"title":label,"command":["/bin/pwd"]}))).await?;
                let tid=tid.as_str().ok_or("Missing terminal ID")?;
                let expected=base.join(label).join("same");
                let mut correct=false;
                for _ in 0..100 {
                    let bytes=crate::terminals::since(&ctx,tid,0).unwrap_or_default();
                    if String::from_utf8_lossy(&bytes).trim()==expected.to_string_lossy() { correct=true; break; }
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
                if !correct {return Err("Terminal did not start in its registered checkout".into());}
            }
            let terms=crate::rpc_inner(&app,"terminals.list".into(),None).await?;
            let scoped: Vec<_>=terms.as_array().ok_or("Invalid terminal list")?.iter().filter(|t|t["workspace_id"].as_str()==Some(rid.as_str())).collect();
            if scoped.len()!=1 || scoped[0]["title"]!=label {return Err("Terminal checkout identity not preserved".into());}
            let d = crate::rpc_inner(&app,"workspace.overview".into(),Some(json!({"repo_id":rid}))).await?;
            for (items,field) in [("tasks","title"),("agents","name"),("resources","handle")] {
                if d[items].as_array().map(Vec::len)!=Some(1) || d[items][0][field]!=label {return Err(format!("{items} escaped checkout scope"));}
            }
        }
        for method in ["agents.kind_create","agents.kind_delete"] {
            let result=crate::rpc_inner(&app,method.into(),Some(json!({"id":"alpha","cluster":"alpha"}))).await;
            if !matches!(result,Err(ref e) if e.contains("disabled in the side-by-side pilot")) {return Err(format!("Missing pilot guard for {method}"));}
        }
        if phase == "seed" {
            let session = desk
                .call(
                    reqwest::Method::POST,
                    "/sessions",
                    Some(crate::chat_session_body(
                        &base.join("workspace"),
                        "pilot-recovery",
                        None,
                    )),
                )
                .await?;
            ctx.db.kv_set(
                "fixture.session",
                session["id"].as_str().ok_or("No HAL session")?,
            )?;
        }
        let id = ctx
            .db
            .kv_get("fixture.session")
            .ok_or("Session not saved")?;
        let session = desk
            .call(reqwest::Method::GET, &format!("/sessions/{id}"), None)
            .await?;
        if session["id"] != id {
            return Err("Wrong HAL session".into());
        }
        if ctx.db.kv_get("library.imported_at").is_some()
            || crate::reaper::shutdown(&ctx)?["skipped"] != "pilot"
        {
            return Err("Pilot startup or shutdown isolation failed".into());
        }
        Ok(())
    }
    .await;
    crate::terminals::shutdown(&ctx);
    desk.stop();
    result?;
    println!("Pilot {phase}: selected workspace tasks isolated, both IDs preserved, bundled HAL session recovered.");
    Ok(())
}
