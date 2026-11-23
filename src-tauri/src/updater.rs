//! In-app updates. Releases on GitHub carry a signed update bundle and `latest.json`
//! (endpoint and public key live in tauri.conf.json). Philium checks on launch and every
//! few hours, the person installs with one click, and the app restarts into the new version.
//! An update is installed only if its signature matches the bundled public key.

use crate::ctx::Ctx;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri_plugin_updater::UpdaterExt;

/// Current state, shown in the UI. `phase`: idle, checking, available, downloading, ready, error.
static STATE: Mutex<Option<Value>> = Mutex::new(None);
const TOPIC: &str = "app:update";
const RECHECK: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

fn set(ctx: &Ctx, v: Value) {
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = Some(v.clone());
    ctx.emit(TOPIC, v);
}

pub fn state() -> Value {
    STATE.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_else(|| json!({ "phase": "idle" }))
}

fn busy() -> bool {
    matches!(state()["phase"].as_str(), Some("checking" | "downloading" | "ready"))
}

/// Ask GitHub for the newest release. Quiet checks do not surface network errors,
/// so an offline launch does not nag.
pub async fn check(ctx: &Arc<Ctx>, quiet: bool) -> Result<Value, String> {
    if busy() { return Ok(state()); }
    let handle = ctx.handle().ok_or("App is starting")?.clone();
    let current = handle.package_info().version.to_string();
    set(ctx, json!({ "phase": "checking", "current": current }));
    let result = async { handle.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string()) }.await;
    let v = match result {
        Ok(Some(u)) => json!({ "phase": "available", "current": current, "version": u.version, "notes": u.body, "date": u.date.map(|d| d.to_string()) }),
        Ok(None) => json!({ "phase": "idle", "current": current, "latest": true, "checked": now() }),
        Err(e) if quiet => { eprintln!("philium: update check: {e}"); json!({ "phase": "idle", "current": current }) }
        Err(e) => json!({ "phase": "error", "current": current, "error": format!("Could not check for updates: {e}") }),
    };
    set(ctx, v.clone());
    Ok(v)
}

/// Download, verify and install the newest release. The app restarts with `restart`.
pub async fn install(ctx: &Arc<Ctx>) -> Result<Value, String> {
    if matches!(state()["phase"].as_str(), Some("downloading" | "ready")) { return Ok(state()); }
    let handle = ctx.handle().ok_or("App is starting")?.clone();
    let current = handle.package_info().version.to_string();
    let fail = |e: String| { let v = json!({ "phase": "error", "current": current, "error": e }); set(ctx, v.clone()); v };
    let update = match handle.updater().map_err(|e| e.to_string())?.check().await {
        Ok(Some(u)) => u,
        Ok(None) => { let v = json!({ "phase": "idle", "current": current, "latest": true, "checked": now() }); set(ctx, v.clone()); return Ok(v); }
        Err(e) => return Ok(fail(format!("Could not check for updates: {e}"))),
    };
    let version = update.version.clone();
    set(ctx, json!({ "phase": "downloading", "current": current, "version": version, "downloaded": 0, "total": null }));
    let got = Arc::new(Mutex::new(0u64));
    let (c, v2, cur2, g) = (ctx.clone(), version.clone(), current.clone(), got.clone());
    let mut last = std::time::Instant::now();
    let r = update
        .download_and_install(
            move |chunk, total| {
                let mut n = g.lock().unwrap_or_else(|p| p.into_inner());
                *n += chunk as u64;
                // Throttle UI events to a few per second.
                if last.elapsed().as_millis() >= 200 {
                    last = std::time::Instant::now();
                    set(&c, json!({ "phase": "downloading", "current": cur2, "version": v2, "downloaded": *n, "total": total }));
                }
            },
            || {},
        )
        .await;
    match r {
        Ok(()) => { let v = json!({ "phase": "ready", "current": current, "version": version }); set(ctx, v.clone()); Ok(v) }
        Err(e) => Ok(fail(format!("Update failed: {e}"))),
    }
}

/// Restart into the installed version. Only after `install` reached "ready".
pub fn restart(ctx: &Arc<Ctx>) -> Result<Value, String> {
    if state()["phase"] != "ready" { return Err("No update is ready to install".into()); }
    ctx.handle().ok_or("App is starting")?.restart();
}

/// Check shortly after launch, then every few hours. Preview and pilot builds never update.
pub fn spawn_background(ctx: Arc<Ctx>) {
    if cfg!(debug_assertions) || std::env::var_os("PHILIUM_PREVIEW").is_some() || crate::pilot::active() { return; }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        loop {
            let _ = check(&ctx, true).await;
            tokio::time::sleep(RECHECK).await;
        }
    });
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
