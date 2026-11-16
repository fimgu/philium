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
mod codeindex;
mod graph;
mod guard;
mod hal;
mod mcp;
mod oauth;
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
        "codeindex" => codeindex::call(&ctx, m, args).await,
        "sandbox" => sandbox::call(&ctx, m, args).await,
        "stats" => stats::call(&ctx, m, args).await,
        "reaper" => reaper::call(&ctx, m, args).await,
        "selfdev" => selfdev::call(&ctx, m, args).await,
        "mcp" => mcp::call(&ctx, m, args).await,
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
        // Plain chat on the desk HAL. It has no sandbox, so chat sessions get
        // no tools at all: every action is denied, whatever the model.
        "chat_create" => {
            let h = ctx.desk().ok_or("HAL is not running")?;
            let ws = ctx.data_dir.join("desk").join("chat");
            std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
            let body = chat_session_body(&ws, args["title"].as_str().unwrap_or("Chat"), args["model"].as_str());
            h.call(reqwest::Method::POST, "/sessions", Some(body)).await
        }
        // Disk used by Philium, by area. Bounded walk so it stays fast.
        "disk" => {
            let (root, data) = (ctx.root.clone(), ctx.data_dir.clone());
            tokio::task::spawn_blocking(move || {
                fn size(p: &std::path::Path, budget: &mut u32) -> u64 {
                    let Ok(md) = std::fs::symlink_metadata(p) else { return 0 };
                    if !md.is_dir() {
                        return md.len();
                    }
                    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
                    let mut n = 0;
                    for e in rd.flatten() {
                        if *budget == 0 {
                            break;
                        }
                        *budget -= 1;
                        n += size(&e.path(), budget);
                    }
                    n
                }
                let mut out = serde_json::Map::new();
                for (k, p) in [
                    ("worktrees", root.join("worktrees")),
                    ("scratch", data.join("agents")),
                    ("previews", data.join("previews")),
                    ("data", data.join("philium.db")),
                    ("logs", data.join("activity.jsonl")),
                ] {
                    let mut budget = 200_000;
                    out.insert(k.into(), json!({ "bytes": size(&p, &mut budget), "partial": budget == 0 }));
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
            let inits: [(&str, fn(&Arc<Ctx>) -> Result<(), String>); 13] = [
                ("providers", providers::init),
                ("sandbox", sandbox::init),
                ("mcp", mcp::init),
                ("terminals", terminals::init),
                ("workspace", workspace::init),
                ("agents", agents::init),
                ("board", board::init),
                ("browser", browser::init),
                ("graph", graph::init),
                ("codeindex", codeindex::init),
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
                    let first = { let mut s = s2.lock().unwrap(); s.push((auth, body.clone())); s.len() == 1 };
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

        let seen_now = seen.lock().unwrap().clone();
        assert!(!seen_now.is_empty(), "upstream got nothing");
        for (auth, body) in &seen_now {
            assert_eq!(auth, "Bearer REAL-KEY-ONLY-IN-GATEWAY");
            assert!(!body.contains("AKIAABCDEFGHIJKLMNOP") && !body.contains("hunter2hunter2"), "secret left: {body}");
            assert!(!body.contains(&gw.token), "gateway token forwarded");
        }
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
        let before = seen.lock().unwrap().len();
        let s = h.call(reqwest::Method::POST, "/sessions", Some(chat_session_body(&ws, "e2e2", None))).await.unwrap();
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).unwrap().to_string();
        h.call(reqwest::Method::POST, &format!("/sessions/{sid}/messages"), Some(json!({"text": "key AKIAABCDEFGHIJKLMNOP"}))).await.unwrap();
        let evs = wait_turn(&h, &sid).await;
        let failed = evs.as_array().unwrap().iter().find(|e| e["type"] == "turn.failed").cloned().expect("turn failed");
        assert!(failed.to_string().contains("Philium guard blocked"), "{failed}");
        assert_eq!(seen.lock().unwrap().len(), before, "blocked request reached upstream");

        h.stop();
        let _ = std::fs::remove_dir_all(&dir);
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
                    s2.lock().unwrap().push((key, v));
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
            None => panic!("desk HAL failed: {:?}", ctx.desk.read().unwrap().as_ref().err()),
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
        let s = seen.lock().unwrap().clone();
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
