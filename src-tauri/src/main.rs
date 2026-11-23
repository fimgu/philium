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

mod access;
mod access_fs;
mod agents;
mod attention;
mod agentshell;
mod board;
mod checks;
mod release_intake;
mod release_verify;
mod release_http;
mod release_scope;
mod release_sql;
mod release_members;
mod release_composition;
mod release_command;
mod review_run;
mod release_document;
mod release_stage;
mod environments;
#[cfg(unix)] mod managed;
mod browser;
#[cfg(test)]
mod browser_permission_tests;
mod ctx;
mod db;
mod gateway;
mod codeindex;
mod graph;
mod guard;
mod hunks;
mod hal;
mod library;
mod dream;
mod local_models;
mod memory_consolidation;
mod memory_ledger;
mod memory_skills;
mod mcp;
mod mcp_servers;
mod oauth;
mod skills;
mod pipeline;
mod providers;
mod reaper;
mod sandbox;
mod secrets;
mod drafts;
mod capture;
mod selfdev;
mod pins;
mod pilot;
mod stats;
mod storage;
mod surfaces;
mod terminals;
mod workday;
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

/// Session body for plain chat: every action denied, whatever the model asks.
fn chat_session_body(ws: &std::path::Path, title: &str, model: Option<&str>) -> Value {
    let rules: Vec<Value> = ["*", "**", "/**"]
        .iter()
        .map(|r| json!({ "action": "*", "resource": r, "effect": "deny" }))
        .collect();
    let mut body = json!({
        "workspace": ws,
        "title": title,
        "permission_mode": "auto_deny",
        "permission_rules": rules,
    });
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        body["model"] = json!(m);
    }
    body
}

/// Start (or restart if its config changed) the desk HAL that backs the
/// plain chat view and subscription model discovery.
pub fn refresh_desk(ctx: &Arc<Ctx>) {
    let dir = ctx.data_dir.join("desk");
    let mut setup = providers::hal_setup(ctx, &dir.join("hal"), &providers::ConfigOpts::default());
    setup.config["prompt"] = json!({ "extra": agents::desk_instructions() });
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
        wrap: crate::sandbox::trusted_wrap().map(|w| Box::new(move |b: &std::path::Path| w(b)) as Box<dyn Fn(&std::path::Path) -> (std::path::PathBuf, Vec<String>)>),
    })
    .map(Arc::new);
    if let Err(e) = &r {
        eprintln!("philium: desk HAL: {e}");
    }
    *ctx.desk.wr() = r;
    ctx.emit("app:desk", json!({}));
}

/// Proxy one request to the desk HAL. `path` must be an absolute API path.
#[tauri::command]
async fn hal(app: State<'_, App>, method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    let h = app.ctx.desk.rd().clone().map_err(|e| format!("HAL failed to start: {e}"))?;
    hal::validate_path(&path)?;
    if method.eq_ignore_ascii_case("PATCH") && path.starts_with("/sessions/") {
        if let Some(eff) = body.as_ref().and_then(|b| b.get("effort")).and_then(Value::as_str).map(str::to_string) {
            app.ctx.gateway.set_effort(None, Some(&eff));
            if let Ok(v) = h.call(hal::parse_method(&method)?, &path, body).await {
                return Ok(v);
            }
            return Ok(json!({ "effort": eff }));
        }
    }
    h.call(hal::parse_method(&method)?, &path, body).await
}

/// One entry point for every feature module: `rpc("terminals.open", {...})`.
#[tauri::command]
async fn rpc(app: State<'_, App>, method: String, args: Option<Value>) -> Result<Value, Value> {
    use futures_util::FutureExt;
    // A bug in one handler must not take the IPC call (or app) down with it:
    // turn panics into an error the UI can show.
    let label = method.clone();
    let scope = workday::scope(args.as_ref());
    let result = match std::panic::AssertUnwindSafe(rpc_inner(&app, method, args)).catch_unwind().await {
        Ok(r) => r,
        Err(p) => {
            let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            app.ctx.log_event("rpc_panic", None, &json!({ "method": label, "panic": msg }));
            Err(format!("Internal error in {label}: {msg}"))
        }
    };
    // The person's own actions, by name and outcome only, for the workday ledger.
    workday::record(&app.ctx, &label, &scope, result.is_ok());
    result.map_err(ctx::rpc_error)
}

async fn rpc_inner(app: &App, method: String, args: Option<Value>) -> Result<Value, String> {
    let ctx = app.ctx.clone();
    let args = args.unwrap_or(Value::Null);
    let (module, m) = method.split_once('.').ok_or("method must be module.name")?;
    if pilot::active() && ((module == "selfdev" && !matches!(m, "status" | "list")) || (module == "reaper" && !matches!(m, "ledger" | "proposals"))) {
        return Err("Host cleanup and self-update are disabled in the side-by-side pilot".into());
    }
    match module {
        "app" => app_call(app, m, args).await,
        "activity" => match m {
            "list" => Ok(json!(ctx.gateway.recent())),
            "quotas" => Ok(json!(ctx.gateway.quotas())),
            "detail" => Ok(json!(ctx.gateway.detail(args["id"].as_u64().ok_or("id")?))),
            "clear" => {
                ctx.gateway.clear();
                Ok(Value::Null)
            }
            _ => Err(format!("unknown activity.{m}")),
        },
        "access" => access::call(&ctx, m, args).await,
        "providers" => providers::call(&ctx, m, args).await,
        "terminals" => terminals::call(&ctx, m, args).await,
        "workspace" => workspace::call(&ctx, m, args).await,
        "agents" => agents::call(&ctx, m, args).await,
        "board" => board::call(&ctx, m, args).await,
        "checks" => checks::call(&ctx, m, args).await,
        "release_intake" if m=="verify_url" => release_verify::run(&ctx,&ctx::arg_str(&args,"repo_id")?,&ctx::arg_str(&args,"url")?).await,
        "release_intake" => release_intake::call(&ctx,m,args).await,
        "environments" => environments::call(m, args).await,
        #[cfg(unix)] "managed" => managed::call(&ctx, m, args).await,
        "browser" => browser::call(&ctx, m, args).await,
        "graph" => graph::call(&ctx, m, args).await,
        "codeindex" => codeindex::call(&ctx, m, args).await,
        "sandbox" => sandbox::call(&ctx, m, args).await,
        "stats" => stats::call(&ctx, m, args).await,
        "storage" => storage::call(&ctx, m, args).await,
        "reaper" => reaper::call(&ctx, m, args).await,
        "library" => library::call(&ctx, m, args).await,
        "skills" => skills::call(&ctx, m, args).await,
        "surfaces" => surfaces::call(&ctx, m, args).await,
        "selfdev" => selfdev::call(&ctx, m, args).await,
        "mcp" if m.starts_with("servers_") => mcp_servers::call(&ctx, m, args).await,
        "mcp" => mcp::call(&ctx, m, args).await,
        "workday" => workday::call(&ctx, m, args).await,
        "pipeline" => pipeline::app::call(&ctx, m, args).await,
        _ => Err(format!("unknown module {module}")),
    }
}

async fn app_call(app: &App, m: &str, args: Value) -> Result<Value, String> {
    let ctx = &app.ctx;
    match m {
        "status" => {
            let desk = ctx.desk.rd().clone();
            Ok(json!({
                "desk": match desk { Ok(h) => json!({"ok": h.alive(), "pid": h.pid}), Err(e) => json!({"ok": false, "error": e}) },
                "gateway": ctx.gateway_url,
                "root": ctx.root,
                "data_dir": ctx.data_dir,
                "version": env!("CARGO_PKG_VERSION"),
                "pilot": pilot::active(),
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
        // Plain chat on the desk HAL. It has no sandbox, so chat sessions get
        // no tools at all: every action is denied, whatever the model.
        "chat_create" => {
            let h = ctx.desk().ok_or("HAL is not running")?;
            let ws = ctx.data_dir.join("desk").join("chat");
            std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
            let body = chat_session_body(&ws, args["title"].as_str().unwrap_or("Chat"), args["model"].as_str());
            h.call(reqwest::Method::POST, "/sessions", Some(body)).await
        }
        // Native folder picker (macOS). Returns {path} or {path: null} if cancelled.
        "pick_folder" => {
            let prompt = args["prompt"].as_str().unwrap_or("Choose a folder").replace('"', "'");
            tokio::task::spawn_blocking(move || {
                if !cfg!(target_os = "macos") {
                    return Err("Folder picker is only available on macOS for now; type the path instead.".to_string());
                }
                let script = format!("POSIX path of (choose folder with prompt \"{prompt}\")");
                let out = std::process::Command::new("osascript").args(["-e", &script]).output().map_err(|e| e.to_string())?;
                let p = String::from_utf8_lossy(&out.stdout).trim().trim_end_matches('/').to_string();
                Ok(json!({ "path": if out.status.success() && !p.is_empty() { Some(p) } else { None } }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        // Disk used by Philium, by area, with the same measure as Settings, Storage.
        "disk" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let st = storage::status(&c);
                let mut out = serde_json::Map::new();
                for p in st["parts"].as_array().cloned().unwrap_or_default() {
                    if p["bytes"].as_u64().unwrap_or(0) > 0 {
                        out.insert(p["label"].as_str().unwrap_or("").to_string(), json!({ "bytes": p["bytes"], "partial": p["partial"] }));
                    }
                }
                Ok(Value::Object(out))
            })
            .await
            .map_err(|e| e.to_string())?
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
    // `philium run|replay|simulate|land|enroll`: the pipeline, without the window.
    if let Some(code) = pipeline::cli::dispatch() {
        std::process::exit(code);
    }
    if std::env::args().nth(1).as_deref()==Some("--observe-sql-contract") {std::process::exit(if release_sql::worker().is_some(){0}else{2});}
    let mut config = tauri::generate_context!();
    let first_arg=std::env::args().nth(1);
    let scripted=first_arg.as_deref()==Some("--pilot-ui-script");
    let ui_script=if scripted{std::env::args().nth(2)}else{None};
    if scripted&&ui_script.is_none(){eprintln!("Usage: --pilot-ui-script SCRIPT [SECONDS]");std::process::exit(2);}
    let offscreen=scripted||first_arg.as_deref()==Some("--pilot-check-review-form");
    if offscreen{attention::HIDDEN.store(true,std::sync::atomic::Ordering::Relaxed);}
    if offscreen {let home=std::env::var_os("HOME").map(std::path::PathBuf::from);if !cfg!(target_os="macos")||config.config().identifier!=pilot::ID||!home.is_some_and(|p|std::fs::read_to_string(p.join(".philium-pilot-fixture")).ok().as_deref()==Some("disposable\n")){eprintln!("Disposable Pilot home required");std::process::exit(2);}for w in &mut config.config_mut().app.windows{w.visible=false;w.focus=false;w.focusable=false;w.skip_taskbar=true;w.incognito=true;}}
    let checking = std::env::args().nth(1).as_deref() == Some("--pilot-check");
    if checking && config.config().identifier != pilot::ID {
        eprintln!("Not a pilot bundle"); std::process::exit(2);
    }
    if config.config().identifier == pilot::ID {
        if let Err(e) = pilot::prepare() { eprintln!("Could not isolate pilot: {e}"); std::process::exit(1); }
        if matches!(std::env::args().nth(1).as_deref(),Some("--review-url"|"--pin-review-driver")){match tauri::async_runtime::block_on(release_verify::cli()){Ok(v)=>println!("{}",v),Err(e)=>{eprintln!("Philium: {e}");std::process::exit(2);}}return;}
        if checking {
            if let Err(e) = tauri::async_runtime::block_on(pilot::check()) { eprintln!("Pilot check failed: {e}"); std::process::exit(1); }
            return;
        }
    } else if !offscreen {
        // Dock launches get a bare PATH; Improve Philium needs cargo/npm from the login shell.
        if let Some(home) = std::env::var_os("HOME") { pilot::adopt_login_path(std::path::Path::new(&home)); }
    }
    let builder=tauri::Builder::default();
    #[cfg(target_os="macos")] let builder=if offscreen{builder.activate_ignoring_other_apps(false)}else{builder};
    let mut tauri_app = builder
        .setup(|app| {
            // PHILIUM_DATA_DIR isolates Improve-Philium previews from real data.
            let data_dir = match std::env::var("PHILIUM_DATA_DIR") {
                Ok(d) if !d.is_empty() => std::path::PathBuf::from(d),
                _ => app.path().app_data_dir().expect("app data dir"),
            };
            std::fs::create_dir_all(&data_dir)?;
            secrets::init(&data_dir);

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
            oauth::init_claude_version(&ctx);
            attention::refresh(&ctx);
            let c_learn = ctx.clone();
            gateway.set_learned_efforts_cb(Arc::new(move |pid, mid, levels| {
                providers::record_learned_efforts(&c_learn, pid, mid, levels);
            }));

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

            // Live stream progress -> conversation activity indicator.
            let mut prx = gateway.progress.subscribe();
            let c3 = ctx.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match prx.recv().await {
                        Ok(p) => c3.emit("gateway:progress", p),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            // Keep philium.db-wal from growing without bound between restarts.
            let c_wal = ctx.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    let c = c_wal.clone();
                    let _ = tokio::task::spawn_blocking(move || c.db.with(|x| x.execute_batch("PRAGMA wal_checkpoint(PASSIVE);"))).await;
                }
            });

            // Module init. Failures are logged, never fatal: the UI shows them.
            let inits: [(&str, fn(&Arc<Ctx>) -> Result<(), String>); 19] = [
                ("workday", workday::init),
                ("access", access::init),
                ("library", library::init),
                ("surfaces", surfaces::init),
                ("providers", providers::init),
                ("sandbox", sandbox::init),
                ("mcp", mcp::init),
                ("terminals", terminals::init),
                ("workspace", workspace::init),
                ("agents", agents::init),
                // After agents: every agent is stopped now, so nothing builds while caches are trimmed.
                ("storage", storage::init),
                ("board", board::init),
                ("browser", browser::init),
                ("graph", graph::init),
                ("codeindex", codeindex::init),
                ("stats", stats::init),
                ("reaper", reaper::init),
                ("selfdev", selfdev::init),
                // Last: check runs Philium quit during continue, with every module ready.
                ("pipeline", pipeline::app::init),
            ];
            for (name, f) in inits {
                if let Err(e) = f(&ctx) {
                    eprintln!("philium: {name} init: {e}");
                    ctx.log_event("init_error", None, &json!({ "module": name, "error": e }));
                }
            }

            // Memory's local embedder and reranker: fetched once in the background, then offline.
            if !pilot::active() {
                local_models::start(&ctx);
                dream::start(&ctx);
            }
            #[cfg(unix)] managed::start_watcher(&ctx);
            app.manage(App { ctx: ctx.clone() });
            let c_desk = ctx.clone();
            tauri::async_runtime::spawn_blocking(move || refresh_desk(&c_desk));

            // Desk HAL watcher: surface a crash and restart it with backoff.
            let c4 = ctx.clone();
            tauri::async_runtime::spawn(async move {
                let mut backoff = 2u64;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    let dead = c4.desk().and_then(|h| h.exit_reason());
                    let Some(reason) = dead else {
                        backoff = 2;
                        continue;
                    };
                    c4.log_event("desk_hal_exited", None, &json!({ "reason": reason }));
                    *c4.desk.wr() = Err(format!("{reason}. Restarting."));
                    c4.emit("app:desk", json!({ "error": reason }));
                    let c = c4.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || refresh_desk(&c)).await;
                    backoff = (backoff * 2).min(60);
                }
            });

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
        .invoke_handler(tauri::generate_handler![hal, rpc, drafts::drafts_load, drafts::drafts_save, drafts::drafts_put_blob, drafts::drafts_blob, capture::capture_screenshot, capture::open_screen_settings, capture::agent_attachment])
        .build(config)
        .expect("error while building Philium");
    #[cfg(target_os="macos")] if offscreen{tauri_app.set_activation_policy(tauri::ActivationPolicy::Prohibited);}
    tauri_app.run(move |handle, event| {
        if offscreen && matches!(&event,RunEvent::Ready){match &ui_script{Some(path)=>pilot::ui_script(handle.clone(),path.clone()),None=>pilot::review_form(handle.clone())}}
        if let RunEvent::Exit = event {
            if let Some(app) = handle.try_state::<App>() {
                // Drop UI emits first so the shutdown thread can never queue
                // into tao while the main thread waits for it.
                app.ctx.begin_exit();
                // Run shutdown work on a dedicated OS thread and join with a
                // bounded timeout.
                //
                // Root cause: RunEvent::Exit fires on the main thread while tao's
                // event-dispatch loop is active and holds its internal event-queue
                // mutex.  The shutdown helpers call ctx.emit() (set_state →
                // "agents:changed", reaper::release → "reaper:changed", log_event →
                // "event"), which routes through AppHandle::emit →
                // tao::AppState::queue_event → pthread_mutex_lock — the same mutex
                // the event-dispatch loop already holds.  The main thread deadlocks
                // on itself.  Moving the work off the main thread breaks that cycle.
                //
                // Additional benefit: Hal::stop sleeps up to 3 s and
                // reaper::stop_process sleeps up to 5 s per process; keeping those
                // sleeps off the main thread prevents the UI from stalling visibly
                // while waiting for child processes.
                let ctx = app.ctx.clone();
                let handle = std::thread::Builder::new()
                    .name("philium-shutdown".into())
                    .spawn(move || {
                        agents::shutdown(&ctx);
                        terminals::shutdown(&ctx);
                        let _ = reaper::shutdown(&ctx);
                        if let Some(h) = ctx.desk() {
                            h.stop();
                        }
                    });
                if let Ok(thread) = handle {
                    // Bounded wait: HAL stop takes up to 3 s, reaper stop up to
                    // 5 s. Past the deadline we exit anyway; child processes are
                    // in Philium's process group and the reaper ledger is
                    // reconciled on next launch.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !thread.is_finished() && std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    if thread.is_finished() {
                        let _ = thread.join();
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod e2e {
    //! Real HAL (bundled sidecar) + real gateway + fake upstream, with a
    //! hostile parent environment. Proves the desk HAL reaches the model only
    //! through the gateway, never sees the real key, secrets are redacted, and
    //! Chat sessions cannot run tools even when the model asks.
    use super::*;
    use crate::gateway::Route;
    use crate::guard::GuardMode;
    use crate::providers::{Kind, Provider};
    use std::sync::Mutex as M;
    use std::time::Duration;

    type Seen = Arc<M<Vec<(String, String)>>>;

    async fn upstream(marker: std::path::PathBuf) -> (String, Seen) {
        let seen: Seen = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/v1/{*rest}",
            axum::routing::any(move |h: axum::http::HeaderMap, body: String| {
                let s2 = s2.clone();
                let marker = marker.clone();
                async move {
                    let auth = h.get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
                    let first = { let mut s = s2.lk(); s.push((auth, body.clone())); s.len() == 1 };
                    if !body.contains("\"stream\":true") {
                        return ([("content-type", "application/json")], r#"{"data":[{"id":"up-model"}]}"#.to_string());
                    }
                    // First turn: try to run a shell command. Later: plain text.
                    let delta = if first && body.contains("\"tools\"") {
                        let args = serde_json::to_string(&json!({"command": format!("touch {}", marker.display())})).unwrap();
                        json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"shell","arguments":args}}]})
                    } else {
                        json!({"role":"assistant","content":"done"})
                    };
                    let fin = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                    let sse = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":fin}],"usage":{"prompt_tokens":21,"completion_tokens":2}}),
                    );
                    ([("content-type", "text/event-stream")], sse)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (url, seen)
    }

    async fn wait_turn(h: &Hal, sid: &str) -> Value {
        for _ in 0..150 {
            let evs = h.call(reqwest::Method::GET, &format!("/sessions/{sid}/events?after=0&limit=1000"), None).await.unwrap();
            if evs.as_array().unwrap().iter().any(|e| e["type"] == "assistant.completed" || e["type"] == "turn.failed") {
                return evs;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("turn never finished");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn desk_hal_goes_only_through_gateway() {
        if !hal::server_path().exists() {
            eprintln!("skipping: no sidecar at {}", hal::server_path().display());
            return;
        }
        // Hostile env: if HAL used any of these, the upstream would see them.
        std::env::set_var("OPENAI_API_KEY", "sk-LEAKED-FROM-ENV-000000000000000000");
        std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:9/v1");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-LEAKED-FROM-ENV-0000000000");

        let dir = std::env::temp_dir().join(format!("philium-e2e-{}", hal::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("PWNED");
        let (up_url, seen) = upstream(marker.clone()).await;

        let gw = Gateway::new(hal::random_token(), Some(dir.join("activity.jsonl")));
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_url = format!("http://{}", gl.local_addr().unwrap());
        let r = gw.router();
        tokio::spawn(async move { axum::serve(gl, r).await.unwrap() });

        let ctx = Ctx::new(db::Db::memory(), dir.join("data"), dir.join("root"), gw.clone(), gateway_url);
        let p = Provider {
            id: "cn".into(),
            name: "CN".into(),
            kind: Kind::OpenaiChat,
            base_url: up_url,
            trusted: false,
            guard: GuardMode::Redact,
            enabled: true,
            default_model: Some("up-model".into()),
        };
        providers::upsert(&ctx, &p).unwrap();
        // Same as sync_routes(), with a test key instead of the keychain.
        gw.set_routes(vec![Route { provider: p.clone(), api_key: Some("REAL-KEY-ONLY-IN-GATEWAY".into()) }]);
        let c2 = ctx.clone();
        tokio::task::spawn_blocking(move || refresh_desk(&c2)).await.unwrap();
        let h = ctx.desk().expect("desk HAL starts");

        let cfg = std::fs::read_to_string(dir.join("data/desk/hal.yaml")).unwrap();
        assert!(!cfg.contains("REAL-KEY") && !cfg.contains("LEAKED") && !cfg.contains("/v1\""), "{cfg}");

        let models = h.call(reqwest::Method::GET, "/models", None).await.unwrap().to_string();
        assert!(models.contains("\"cn\""), "{models}");
        assert!(!models.contains("LEAKED") && !models.contains("127.0.0.1:9/v1"), "env model leaked in: {models}");

        // A Chat session: the model asks for a shell command, which must be denied.
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let s = h.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "e2e", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).expect("session id").to_string();
        // Exercise the same session PATCH used by the effort controls against
        // the real bundled HAL, then observe its outbound gateway request.
        h.call(reqwest::Method::PATCH, &format!("/sessions/{sid}"), Some(json!({"effort":"low"}))).await.unwrap();
        let saved = h.call(reqwest::Method::GET, &format!("/sessions/{sid}"), None).await.unwrap();
        assert_eq!(saved["effort"], "low", "effort PATCH did not persist: {saved}");
        assert!(h.call(reqwest::Method::PATCH, &format!("/sessions/{sid}"), Some(json!({"effort":"unsupported-effort"}))).await.is_err());
        h.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"),
            Some(json!({"text": "deploy using AKIAABCDEFGHIJKLMNOP and DB_PASSWORD=hunter2hunter2"}))).await.unwrap();
        let evs = wait_turn(&h, &sid).await;
        let text = evs.to_string();
        assert!(!text.contains("\"turn.failed\""), "{text}");
        assert!(!marker.exists(), "Chat ran a shell command");
        assert!(!text.contains("permission.requested"), "Chat asked instead of denying: {text}");
        let denied = evs.as_array().unwrap().iter().find(|e| e["type"] == "tool.finished" && e["tool"] == "shell").expect("model tried shell");
        assert_eq!(denied["ok"], false);
        assert!(denied["result"].as_str().unwrap().starts_with("Permission denied"), "{denied}");

        let seen_now = seen.lk().clone();
        assert!(!seen_now.is_empty(), "upstream got nothing");
        for (auth, body) in &seen_now {
            assert_eq!(auth, "Bearer REAL-KEY-ONLY-IN-GATEWAY");
            assert!(!body.contains("AKIAABCDEFGHIJKLMNOP") && !body.contains("hunter2hunter2"), "secret left: {body}");
            assert!(!body.contains(&gw.token), "gateway token forwarded");
            let request: Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["reasoning_effort"], "low", "saved effort was not forwarded");
        }
        h.call(reqwest::Method::PATCH, &format!("/sessions/{sid}"), Some(json!({"effort":"default"}))).await.unwrap();
        let reset = h.call(reqwest::Method::GET, &format!("/sessions/{sid}"), None).await.unwrap();
        assert!(reset["effort"].is_null(), "default did not clear session effort: {reset}");
        println!("real HAL effort: low persisted, invalid rejected, gateway forwarded low, default cleared");
        assert!(seen_now.iter().any(|(_, b)| b.contains("[REDACTED:aws_access_key]")));

        tokio::time::sleep(Duration::from_millis(200)).await;
        let acts = gw.recent();
        let chat = acts.iter().find(|a| a.path == "/chat/completions").expect("logged");
        assert_eq!((chat.guard, chat.status), ("redacted", Some(200)));
        let disk = std::fs::read_to_string(dir.join("activity.jsonl")).unwrap();
        assert!(disk.contains("\"redacted\"") && !disk.contains("AKIA") && !disk.contains("REAL-KEY"));

        // Block mode, hot-switched (no HAL restart): nothing reaches upstream.
        let mut blocking = p.clone();
        blocking.guard = GuardMode::Block;
        gw.set_routes(vec![Route { provider: blocking, api_key: Some("REAL-KEY-ONLY-IN-GATEWAY".into()) }]);
        let before = seen.lk().len();
        let s = h.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "e2e2", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).unwrap().to_string();
        h.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"), Some(json!({"text": "key AKIAABCDEFGHIJKLMNOP"}))).await.unwrap();
        let evs = wait_turn(&h, &sid).await;
        let failed = evs.as_array().unwrap().iter().find(|e| e["type"] == "turn.failed").cloned().expect("turn failed");
        assert!(failed.to_string().contains("Philium guard blocked"), "{failed}");
        assert_eq!(seen.lk().len(), before, "blocked request reached upstream");

        h.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Images through the real bundled HAL: the gateway forwards image content
    /// upstream, HAL stores the blob, and Philium's scoped transcript reader
    /// returns exactly those bytes for the user message and nothing else.
    #[tokio::test(flavor = "multi_thread")]
    async fn sent_image_reaches_model_and_renders_from_hal_history() {
        if !hal::server_path().exists() {
            eprintln!("skipping: no sidecar at {}", hal::server_path().display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("philium-e2e-img-{}", hal::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let (up_url, seen) = upstream(dir.join("unused")).await;
        let gw = Gateway::new(hal::random_token(), None);
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_url = format!("http://{}", gl.local_addr().unwrap());
        let r = gw.router();
        tokio::spawn(async move { axum::serve(gl, r).await.unwrap() });
        let ctx = Ctx::new(db::Db::memory(), dir.join("data"), dir.join("root"), gw.clone(), gateway_url);
        let p = Provider { id: "cn".into(), name: "CN".into(), kind: Kind::OpenaiChat, base_url: up_url, trusted: false, guard: GuardMode::Redact, enabled: true, default_model: Some("up-model".into()) };
        providers::upsert(&ctx, &p).unwrap();
        gw.set_routes(vec![Route { provider: p.clone(), api_key: Some("KEY".into()) }]);
        let c2 = ctx.clone();
        tokio::task::spawn_blocking(move || refresh_desk(&c2)).await.unwrap();
        let h = ctx.desk().expect("desk HAL starts");
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let s = h.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "img", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).unwrap().to_string();
        // 1x1 PNG, the same shape the composer's sendableImages() produces.
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let png = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png_b64).unwrap();
        let sha: String = { use sha2::Digest; sha2::Sha256::digest(&png).iter().map(|b| format!("{b:02x}")).collect() };
        h.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"),
            Some(agents::message_body("what colour is this", json!([{ "data": png_b64, "media_type": "image/png", "name": "dot.png" }])))).await.unwrap();
        let evs = wait_turn(&h, &sid).await;
        let created = evs.as_array().unwrap().iter().find(|e| e["type"] == "message.created" && e["role"] == "user").cloned().expect("user message event");
        assert_eq!(created["attachments"][0]["sha256"], sha, "event lacks attachment ref the UI renders: {created}");
        assert!(seen.lk().iter().any(|(_, b)| b.contains(png_b64) || b.contains("image_url")), "image never reached upstream");
        let db_path = std::fs::read_dir(dir.join("data/desk")).unwrap().flatten()
            .map(|e| e.path().join("hal.db")).find(|p| p.exists()).expect("desk hal.db");
        let (media, bytes) = agents::history::attachment_at(&db_path, &sid, &sha).expect("scoped read");
        assert_eq!((media.as_str(), bytes), ("image/png", png));
        assert!(agents::history::attachment_at(&db_path, "other-session", &sha).is_err(), "peer session could read image");
        println!("real HAL image: forwarded upstream, stored as {sha}, scoped read ok, peer denied");
        h.stop();

        // Exercise the production rules builder: standing instructions travel, archived memory does not.
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        crate::library::init(&ctx).unwrap();
        crate::library::memory_save(&ctx, "Filip-pref-marker-7731 keep replies short", "preference", None, "test").unwrap();
        let adir = dir.join("agent-hal");
        let mut setup = providers::hal_setup(&ctx, &adir.join("hal"), &providers::ConfigOpts::default());
        setup.config["prompt"] = json!({ "extra": crate::agents::prompt_rules(&ctx, false) });
        let ah = Hal::start(Launch { dir: &adir, config: setup.config, cwd: None, env: setup.env, allow_logins: false, wrap: None }).expect("agent-style HAL starts");
        let s = ah.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "rules", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).unwrap().to_string();
        let before = seen.lk().len();
        ah.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"), Some(json!({"text":"hi"}))).await.unwrap();
        wait_turn(&ah, &sid).await;
        let bodies: Vec<String> = seen.lk()[before..].iter().map(|(_, b)| b.clone()).collect();
        assert!(bodies.iter().any(|b| b.contains("standing rules") && b.contains("skill_get review-pr")));
        assert!(bodies.iter().all(|b| !b.contains("Filip-pref-marker-7731")));
        println!("real HAL rules reached the recording recipient; archived personal memory did not");
        ah.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn helper_keeps_workspace_and_session_across_resume() {
        struct Scratch(std::path::PathBuf);
        impl Scratch { fn path(&self) -> &std::path::Path { &self.0 } }
        impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let dir = Scratch(std::env::temp_dir().join(format!("philium-helper-{}", hal::random_token())));
        std::fs::create_dir(&dir.0).unwrap();
        let (url, seen) = upstream(dir.path().join("marker")).await;
        let gw = Gateway::new(hal::random_token(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let router = gw.router();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let ctx = Ctx::new(db::Db::memory(), dir.path().join("data"), dir.path().join("root"), gw.clone(), addr);
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        library::init(&ctx).unwrap(); codeindex::init(&ctx).unwrap();
        let p = Provider { id:"record".into(), name:"Loopback".into(), kind:Kind::OpenaiChat, base_url:url, trusted:true, guard:GuardMode::Redact, enabled:true, default_model:Some("up-model".into()) };
        providers::upsert(&ctx, &p).unwrap();
        gw.set_routes(vec![Route { provider:p, api_key:Some("fixture".into()) }]);
        for (id, symbol) in [("r-a", "visible_workspace_fn"), ("r-b", "sibling_private_fn")] {
            let path = dir.path().join(id).join("same"); std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("lib.rs"),format!("pub fn {symbol}() {{}}\n")).unwrap();
            for args in [&["init","-b","main"][..], &["add","lib.rs"], &["-c","user.name=Test","-c","user.email=fixture@example.invalid","commit","-m","fixture"]] { workspace::git(&path,args).unwrap(); }
            ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES(?1,'same',?2,'main',0)", [id,path.to_str().unwrap()]).unwrap();
        }
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES('parent-fixture','parent-fixture','record/up-model','r-a',0,0)", []).unwrap();
        let child = mcp::call_tool(&ctx,"parent-fixture","agent_spawn",json!({"name":"scoped-fixture","brief":"Reply OK"})).await.unwrap();
        let id = child["id"].as_str().unwrap();
        let ready = || async {
            for _ in 0..200 {
                if let Some(rt) = agents::runtime(id) { return rt; }
                if let Some(reason) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) { panic!("helper failed: {reason}"); }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("helper did not start");
        };
        let rt = ready().await;
        wait_turn(&rt.hal, &rt.session).await;
        let sid = rt.session.clone();
        assert!(seen.lk().iter().any(|(_,b)| b.contains("visible_workspace_fn")));
        assert!(seen.lk().iter().all(|(_,b)| !b.contains("sibling_private_fn")));
        agents::stop(&ctx,id,"stopped");
        agents::call(&ctx,"resume",json!({"id":id})).await.unwrap();
        let resumed = ready().await;
        assert_eq!(resumed.session,sid);
        assert_eq!(resumed.cwd,rt.cwd);
        agents::stop(&ctx,id,"stopped"); server.abort();
    }

    /// What a real agent is given, through the bundled HAL, the gateway and Philium's MCP:
    /// a worker's system prompt holds the ranked codebase map and no worktree landscape,
    /// an orchestrator's holds both, `code_find` answers through the real tool path, and
    /// once two agents change the same function each hears of it on its next tool result.
    #[tokio::test(flavor = "multi_thread")]
    async fn agents_start_with_the_map_and_hear_of_clashes() {
        if !hal::server_path().exists() {
            eprintln!("skipping: no sidecar at {}", hal::server_path().display());
            return;
        }
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let dir = Scratch(std::env::temp_dir().join(format!("philium-ctx-e2e-{}", hal::random_token())));
        std::fs::create_dir(&dir.0).unwrap();
        // The model: each agent looks a concept up, then outlines a file, then stops. Every
        // request is kept so the test can read what the agent was told.
        // The second tool call waits until the test has made the clash, so the call that
        // ends after the scan is known.
        let seen: Arc<M<Vec<Value>>> = Default::default();
        let s2 = seen.clone();
        let (go, gate) = tokio::sync::watch::channel(false);
        let app = axum::Router::new().route(
            "/v1/{*rest}",
            axum::routing::any(move |body: String| {
                let s2 = s2.clone();
                let mut gate = gate.clone();
                async move {
                    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    if v["stream"] != json!(true) {
                        return ([("content-type", "application/json")], r#"{"data":[{"id":"up-model"}]}"#.to_string());
                    }
                    s2.lk().push(v.clone());
                    let results = v["messages"].as_array().map(|m| m.iter().filter(|m| m["role"] == "tool").count()).unwrap_or(0);
                    if results == 1 {
                        let _ = gate.wait_for(|open| *open).await;
                    }
                    let call = |name: &str, args: Value| json!({"tool_calls":[{"index":0,"id":format!("call_{results}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]});
                    let delta = match results {
                        0 => call("mcp_philium_code_find", json!({"query": "retry backoff"})),
                        1 => call("mcp_philium_code_outline", json!({"path": "src/net.rs"})),
                        _ => json!({"role":"assistant","content":"done"}),
                    };
                    let fin = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                    let sse = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":fin}],"usage":{"prompt_tokens":21,"completion_tokens":2}}),
                    );
                    ([("content-type", "text/event-stream")], sse)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", l.local_addr().unwrap());
        let upstream = tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let gw = Gateway::new(hal::random_token(), None);
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", gl.local_addr().unwrap());
        let router = gw.router();
        let gateway = tokio::spawn(async move { axum::serve(gl, router).await.unwrap() });
        let ctx = Ctx::new(db::Db::memory(), dir.0.join("data"), dir.0.join("root"), gw.clone(), addr);
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        library::init(&ctx).unwrap();
        codeindex::init(&ctx).unwrap();
        let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ctx.mcp_url.set(format!("http://{}/mcp", ml.local_addr().unwrap())).unwrap();
        let mr = mcp::router(ctx.clone());
        let mcp_server = tokio::spawn(async move { axum::serve(ml, mr).await.unwrap() });
        let p = Provider { id: "record".into(), name: "Loopback".into(), kind: Kind::OpenaiChat, base_url: url, trusted: true, guard: GuardMode::Redact, enabled: true, default_model: Some("up-model".into()) };
        providers::upsert(&ctx, &p).unwrap();
        gw.set_routes(vec![Route { provider: p, api_key: Some("fixture".into()) }]);

        // A checkout where `Client` is used across files and `retry_backoff` is documented.
        let repo = dir.0.join("app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/net.rs"), "pub struct Client { pub tries: u32 }\n\n/// Wait longer after each failed try.\npub fn retry_backoff(c: &Client) -> u64 {\n    1u64 << c.tries\n}\n").unwrap();
        for f in ["a", "b", "c"] {
            std::fs::write(repo.join(format!("src/{f}.rs")), format!("use crate::net::Client;\npub fn use_{f}(c: &Client) -> u32 {{ c.tries }}\n")).unwrap();
        }
        std::fs::write(repo.join("src/net_test.rs"), "fn helper_for_tests() {}\n").unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["add", "."], &["-c", "user.name=Test", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"]] {
            workspace::git(&repo, args).unwrap();
        }
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r1','app',?1,'main',0)", [repo.to_str().unwrap()]).unwrap();

        let start = |name: &str, role: &str| {
            agents::spawn(&ctx, agents::SpawnReq {
                name: Some(name.into()), role: Some(role.into()), model: Some("record/up-model".into()), repo_id: Some("r1".into()),
                task_id: None, brief: Some("Look around.".into()), parent_id: None, strict: false, base: None,
            }).unwrap()["id"].as_str().unwrap().to_string()
        };
        let (wid, lid) = (start("wren", "worker"), start("lead", "orchestrator"));
        let wt_of = |id: &str| -> std::path::PathBuf {
            let r = ctx.db.query("SELECT path FROM worktrees WHERE agent_id=?1 AND state='active'", [id]).unwrap();
            r.first().and_then(|x| x["path"].as_str()).map(Into::into).unwrap_or_default()
        };
        let who = |r: &Value| r["messages"][0]["content"].as_str().unwrap_or_default().contains("Your agent name is \"wren\"");
        let after = |r: &Value, n: usize| r["messages"].as_array().map(|m| m.iter().filter(|m| m["role"] == "tool").count()).unwrap_or(0) == n;
        let waited = |pred: &dyn Fn(&Value) -> bool| {
            let ok = seen.lk().iter().any(pred);
            ok
        };
        // Both agents ask the model after their first tool result. Their worktrees then
        // change the same function, a clash scan runs, and only then is the second call made.
        for _ in 0..300 {
            if waited(&|r| who(r) && after(r, 1)) && waited(&|r| !who(r) && after(r, 1)) { break; }
            for id in [&wid, &lid] {
                if let Some(e) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) { panic!("agent failed: {e}"); }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(waited(&|r| who(r) && after(r, 1)) && waited(&|r| !who(r) && after(r, 1)), "both agents reached their first tool result");
        let (ww, lw) = (wt_of(&wid), wt_of(&lid));
        for (wt, body) in [(&ww, "1u64 << (c.tries + 1)"), (&lw, "2u64 << c.tries")] {
            std::fs::write(wt.join("src/net.rs"), format!("pub struct Client {{ pub tries: u32 }}\n\n/// Wait longer after each failed try.\npub fn retry_backoff(c: &Client) -> u64 {{\n    {body}\n}}\n")).unwrap();
        }
        let scan = codeindex::call(&ctx, "clashes", json!({"repo_id": "r1"})).await.unwrap();
        assert!(scan["text"].as_str().unwrap_or_default().contains("retry_backoff"), "{scan}");
        go.send(true).unwrap();
        for _ in 0..300 {
            if waited(&|r| who(r) && after(r, 2)) && waited(&|r| !who(r) && after(r, 2)) { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let reqs = seen.lk().clone();
        let system = |worker: bool| reqs.iter().find(|r| who(r) == worker).map(|r| r["messages"][0]["content"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
        let (ws, ls) = (system(true), system(false));
        let tool_text = |worker: bool, n: usize| -> String {
            reqs.iter().filter(|r| who(r) == worker && after(r, n + 1)).flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
                .filter(|m| m["role"] == "tool").nth(n).map(|m| m["content"].to_string()).unwrap_or_default()
        };

        // The map, ranked: `Client` is used by three other files, the test file names nothing.
        let map = ws.split("\n## Codebase map\n").nth(1).unwrap_or_default();
        assert!(map.contains("src/net.rs  Client 1") && !map.contains("helper_for_tests"), "worker prompt map:\n{ws}");
        assert!(!ws.contains("## Worktrees and branches") && ls.contains("## Worktrees and branches") && ls.contains("\n## Codebase map\n"), "landscape only for the orchestrator:\n{ls}");
        // The map sits before the cache break, in the part that stays the same every turn.
        for s in [&ws, &ls] {
            let (shared, _) = s.split_once("<!-- PHILIUM_CACHE_BREAK -->").expect("cache break marker");
            assert!(shared.contains("\n## Codebase map\n"), "map after the cache break:\n{s}");
        }
        // code_find through HAL and MCP finds the function by the words in its doc comment.
        let found = tool_text(true, 0);
        assert!(found.contains("src/net.rs") && found.contains("retry_backoff"), "code_find result: {found}");
        // Each agent hears of the clash once, on the tool result after the scan.
        let w2 = tool_text(true, 1);
        let l2 = tool_text(false, 1);
        assert!(w2.contains("your work now overlaps with") && w2.contains("retry_backoff"), "worker second result: {w2}");
        assert!(l2.contains("your work now overlaps with"), "orchestrator second result: {l2}");
        assert!(!tool_text(true, 0).contains("overlaps"), "no clash before the scan");
        println!("worker map {} chars, orchestrator prompt adds the landscape, code_find and clash notes reached both agents", map.len());

        for id in [&wid, &lid] { agents::stop(&ctx, id, "stopped"); }
        for h in [upstream, gateway, mcp_server] { h.abort(); }
    }

    /// Anthropic provider through the real HAL: HAL validates its config, speaks
    /// chat.completions, the gateway translates to Messages and back, and a tool
    /// call round-trips (denied in Chat, result returned to the model).
    #[tokio::test(flavor = "multi_thread")]
    async fn anthropic_provider_through_hal_and_gateway() {
        if !hal::server_path().exists() {
            eprintln!("skipping: no sidecar at {}", hal::server_path().display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("philium-e2e-ant-{}", hal::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let seen: Arc<M<Vec<(String, Value)>>> = Default::default();
        let s2 = seen.clone();
        let app = axum::Router::new().route(
            "/v1/{*rest}",
            axum::routing::any(move |h: axum::http::HeaderMap, body: String| {
                let s2 = s2.clone();
                async move {
                    let key = h.get("x-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let has_result = body.contains("tool_result");
                    s2.lk().push((key, v));
                    let ev = |t: &str, d: Value| format!("event: {t}\ndata: {d}\n\n");
                    let mut sse = ev("message_start", json!({"type":"message_start","message":{"id":"m1","model":"claude-e2e","usage":{"input_tokens":30,"output_tokens":1}}}));
                    if has_result {
                        sse += &ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
                        sse += &ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"all done"}}));
                        sse += &ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}));
                    } else {
                        sse += &ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"shell","input":{}}}));
                        sse += &ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}));
                        sse += &ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"echo hi\"}"}}));
                        sse += &ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}));
                    }
                    sse += &ev("message_stop", json!({"type":"message_stop"}));
                    ([("content-type", "text/event-stream")], sse)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_url = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let gw = Gateway::new(hal::random_token(), None);
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_url = format!("http://{}", gl.local_addr().unwrap());
        let r = gw.router();
        tokio::spawn(async move { axum::serve(gl, r).await.unwrap() });
        let ctx = Ctx::new(db::Db::memory(), dir.join("data"), dir.join("root"), gw.clone(), gateway_url);
        let p = Provider {
            id: "claude".into(), name: "Claude".into(), kind: Kind::Anthropic, base_url: up_url,
            trusted: false, guard: GuardMode::Redact, enabled: true, default_model: Some("claude-e2e".into()),
        };
        providers::upsert(&ctx, &p).unwrap();
        gw.set_routes(vec![Route { provider: p.clone(), api_key: Some("sk-ant-REAL".into()) }]);
        let c2 = ctx.clone();
        tokio::task::spawn_blocking(move || refresh_desk(&c2)).await.unwrap();
        let h = match ctx.desk() {
            Some(h) => h,
            None => panic!("desk HAL failed: {:?}", ctx.desk.rd().as_ref().err()),
        };
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let s = h.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "ant", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).expect("session id").to_string();
        h.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"), Some(json!({"text": "hello"}))).await.unwrap();
        let evs = wait_turn(&h, &sid).await;
        let text = evs.to_string();
        assert!(!text.contains("\"turn.failed\""), "{text}");
        assert!(text.contains("all done"), "{text}");
        assert!(evs.as_array().unwrap().iter().any(|e| e["type"] == "tool.finished" && e["tool"] == "shell"), "{text}");
        let s = seen.lk().clone();
        assert!(s.len() >= 2);
        assert!(s.iter().all(|(k, _)| k == "sk-ant-REAL"));
        assert!(s[0].1["tools"][0]["input_schema"].is_object(), "{}", s[0].1);
        assert!(s.last().unwrap().1.to_string().contains("tool_result"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let a = gw.recent().into_iter().find(|a| a.path == "/chat/completions").expect("logged");
        assert_eq!((a.input_tokens, a.state), (Some(30), "done"));
        h.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
