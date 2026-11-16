//! Philium: a secure, lightweight agent workstation around the HAL harness.
//!
//! Layout (see docs/SPEC.md):
//! - `gateway` + `guard`: the only route from any HAL to a model provider.
//!   Injects the real key, guards outbound secrets, records every request.
//! - `providers`: provider accounts, keys in the OS keychain, live models.
//! - `hal`: launches one HAL process (desk chat, or one per agent).
//! - `db`/`ctx`: SQLite store and the shared module context.
//! - feature modules reached from the UI via `rpc("module.method", args)`.
//!
//! The web UI never sees a token or key.

mod agents;
mod board;
mod browser;
mod ctx;
mod db;
mod gateway;
mod graph;
mod guard;
mod hal;
mod mcp;
mod providers;
mod reaper;
mod sandbox;
mod selfdev;
mod stats;
mod terminals;
mod workspace;

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{Manager, RunEvent, State};

use ctx::Ctx;
use gateway::Gateway;
use hal::{Hal, Launch};

struct App {
    ctx: Arc<Ctx>,
}

/// Start (or restart if its config changed) the desk HAL that backs the
/// plain chat view and subscription model discovery.
pub fn refresh_desk(ctx: &Arc<Ctx>) {
    let dir = ctx.data_dir.join("desk");
    let setup = providers::hal_setup(ctx, &dir.join("hal"), &providers::ConfigOpts::default());
    if let Some(h) = ctx.desk() {
        if h.config == setup.config && h.alive() {
            return;
        }
        h.stop();
    }
    let r = Hal::start(Launch {
        dir: &dir,
        config: setup.config,
        cwd: None,
        env: setup.env,
        allow_logins: setup.allow_logins,
        wrap: None,
    })
    .map(Arc::new);
    if let Err(e) = &r {
        eprintln!("philium: desk HAL: {e}");
    }
    *ctx.desk.write().unwrap() = r;
    ctx.emit("app:desk", json!({}));
}

/// Proxy one request to the desk HAL. `path` must be an absolute API path.
#[tauri::command]
async fn hal(app: State<'_, App>, method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    let h = app.ctx.desk.read().unwrap().clone().map_err(|e| format!("HAL failed to start: {e}"))?;
    hal::validate_path(&path)?;
    h.call(hal::parse_method(&method)?, &path, body).await
}

/// One entry point for every feature module: `rpc("terminals.open", {...})`.
#[tauri::command]
async fn rpc(app: State<'_, App>, method: String, args: Option<Value>) -> Result<Value, String> {
    let ctx = app.ctx.clone();
    let args = args.unwrap_or(Value::Null);
    let (module, m) = method.split_once('.').ok_or("method must be module.name")?;
    match module {
        "app" => app_call(&app, m, args).await,
        "activity" => match m {
            "list" => Ok(json!(ctx.gateway.recent())),
            "detail" => Ok(json!(ctx.gateway.detail(args["id"].as_u64().ok_or("id")?))),
            "clear" => {
                ctx.gateway.clear();
                Ok(Value::Null)
            }
            _ => Err(format!("unknown activity.{m}")),
        },
        "providers" => providers::call(&ctx, m, args).await,
        "terminals" => terminals::call(&ctx, m, args).await,
        "workspace" => workspace::call(&ctx, m, args).await,
        "agents" => agents::call(&ctx, m, args).await,
        "board" => board::call(&ctx, m, args).await,
        "browser" => browser::call(&ctx, m, args).await,
        "graph" => graph::call(&ctx, m, args).await,
        "sandbox" => sandbox::call(&ctx, m, args).await,
        "stats" => stats::call(&ctx, m, args).await,
        "reaper" => reaper::call(&ctx, m, args).await,
        "selfdev" => selfdev::call(&ctx, m, args).await,
        _ => Err(format!("unknown module {module}")),
    }
}

async fn app_call(app: &App, m: &str, args: Value) -> Result<Value, String> {
    let ctx = &app.ctx;
    match m {
        "status" => {
            let desk = ctx.desk.read().unwrap().clone();
            Ok(json!({
                "desk": match desk { Ok(h) => json!({"ok": h.alive(), "pid": h.pid}), Err(e) => json!({"ok": false, "error": e}) },
                "gateway": ctx.gateway_url,
                "root": ctx.root,
                "data_dir": ctx.data_dir,
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }
        "settings_get" => {
            let rows = ctx.db.query("SELECT k, v FROM kv WHERE k LIKE 'setting.%'", [])?;
            let mut out = serde_json::Map::new();
            for r in rows {
                let k = r["k"].as_str().unwrap_or("").trim_start_matches("setting.").to_string();
                out.insert(k, r["v"].clone());
            }
            Ok(Value::Object(out))
        }
        "settings_set" => {
            let k = ctx::arg_str(&args, "key")?;
            if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
                return Err("bad key".into());
            }
            let v = match &args["value"] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            ctx.db.kv_set(&format!("setting.{k}"), &v)?;
            ctx.notify("settings");
            Ok(Value::Null)
        }
        "events" => {
            let limit = args["limit"].as_i64().unwrap_or(100).clamp(1, 1000);
            Ok(json!(ctx.db.query(
                "SELECT * FROM events ORDER BY id DESC LIMIT ?1",
                [limit]
            )?))
        }
        _ => Err(format!("unknown app.{m}")),
    }
}

fn philium_root() -> std::path::PathBuf {
    if let Ok(r) = std::env::var("PHILIUM_ROOT") {
        return r.into();
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
    std::path::PathBuf::from(home).join("Philium")
}

fn main() {
    let tauri_app = tauri::Builder::default()
        .setup(|app| {
            // PHILIUM_DATA_DIR isolates Improve-Philium previews from real data.
            let data_dir = match std::env::var("PHILIUM_DATA_DIR") {
                Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
                _ => app.path().app_data_dir().expect("app data dir"),
            };
            std::fs::create_dir_all(&data_dir)?;

            // Gateway on an ephemeral loopback port, inside this process.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let gateway_url = format!("http://{}", listener.local_addr()?);
            let gateway = Gateway::new(hal::random_token(), Some(data_dir.join("activity.jsonl")));
            let router = gateway.router();
            tauri::async_runtime::spawn(async move {
                let l = tokio::net::TcpListener::from_std(listener).expect("gateway listener");
                if let Err(e) = axum::serve(l, router).await {
                    eprintln!("philium: gateway stopped: {e}");
                }
            });

            let db = db::Db::open(&data_dir.join("philium.db")).map_err(|e| format!("database: {e}"))?;
            let ctx = Ctx::new(db, data_dir, philium_root(), gateway.clone(), gateway_url);
            ctx.set_handle(app.handle().clone());

            // Live activity -> UI and stats.
            let mut rx = gateway.events.subscribe();
            let c2 = ctx.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(a) => {
                            stats::record(&c2, &a);
                            c2.emit("activity", a);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            // Module init. Failures are logged, never fatal: the UI shows them.
            let inits: [(&str, fn(&Arc<Ctx>) -> Result<(), String>); 12] = [
                ("providers", providers::init),
                ("sandbox", sandbox::init),
                ("mcp", mcp::init),
                ("terminals", terminals::init),
                ("workspace", workspace::init),
                ("agents", agents::init),
                ("board", board::init),
                ("browser", browser::init),
                ("graph", graph::init),
                ("stats", stats::init),
                ("reaper", reaper::init),
                ("selfdev", selfdev::init),
            ];
            for (name, f) in inits {
                if let Err(e) = f(&ctx) {
                    eprintln!("philium: {name} init: {e}");
                    ctx.log_event("init_error", None, &json!({ "module": name, "error": e }));
                }
            }

            refresh_desk(&ctx);
            app.manage(App { ctx: ctx.clone() });

            // Provider changes -> restart desk HAL if its model list changed.
            let mut changes = ctx.changes.subscribe();
            let c3 = ctx.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(what) = changes.recv().await {
                    if what == "providers" {
                        let c = c3.clone();
                        let _ = tauri::async_runtime::spawn_blocking(move || refresh_desk(&c)).await;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![hal, rpc])
        .build(tauri::generate_context!())
        .expect("error while building Philium");
    tauri_app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(app) = handle.try_state::<App>() {
                agents::shutdown(&app.ctx);
                terminals::shutdown(&app.ctx);
                let _ = reaper::shutdown(&app.ctx);
                if let Some(h) = app.ctx.desk() {
                    h.stop();
                }
            }
        }
    });
}
