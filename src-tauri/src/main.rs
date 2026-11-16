//! Filium: desktop shell around the HAL harness with a local model gateway.
//!
//! Three pieces live here:
//! - the gateway (`gateway.rs`): HAL's only route to any model provider,
//!   which injects the real key, guards outbound secrets and logs activity;
//! - provider settings (`providers.rs`): metadata on disk, keys in the OS
//!   keychain, never handed to HAL;
//! - the bundled `hal-desktop-server` child, launched with a generated config
//!   that points every model at the gateway and a scrubbed environment so it
//!   cannot pick up credentials on its own.
//!
//! The web UI never sees a token or key; it reaches HAL only through the
//! `hal` command, which pins requests to an allowlist of loopback routes.

mod gateway;
mod guard;
mod providers;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};

use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use tauri::{Emitter, Manager, RunEvent, State};

use gateway::{Gateway, Route};
use providers::{Provider, Store};

struct Hal {
    base: String,
    token: String,
    client: reqwest::Client,
    // Holding stdin open keeps HAL alive; dropping it (EOF) shuts HAL down.
    proc: Mutex<Option<(Child, ChildStdin)>>,
    config: Value,
}

enum Backend {
    Ready(Hal),
    Failed(String),
}

struct App {
    gateway: Arc<Gateway>,
    gateway_url: String,
    data_dir: PathBuf,
    store: Mutex<Store>,
    hal: RwLock<Arc<Backend>>,
}

fn random_token() -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(48).map(char::from).collect()
}

fn server_path() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("exe dir");
    let name = if cfg!(windows) { "hal-desktop-server.exe" } else { "hal-desktop-server" };
    let path = dir.join(name);
    // Test binaries live one level down (target/debug/deps).
    if cfg!(debug_assertions) && !path.exists() {
        if let Some(up) = dir.parent() {
            return up.join(name);
        }
    }
    path
}

/// Environment variables HAL (or tools it runs) could use to reach a provider
/// without going through the gateway.
fn is_credential_var(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const PREFIXES: [&str; 12] = [
        "OPENAI_", "ANTHROPIC_", "HAL_", "AZURE_OPENAI", "GEMINI_", "GOOGLE_API", "GROQ_",
        "MISTRAL_", "DEEPSEEK_", "OPENROUTER_", "MOONSHOT_", "DASHSCOPE_",
    ];
    const WORDS: [&str; 5] = ["API_KEY", "APIKEY", "SECRET", "TOKEN", "PASSWORD"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || WORDS.iter().any(|w| n.contains(w))
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(path)?.write_all(bytes)
}

fn start_hal(app: &App, store: &Store) -> Result<Hal, String> {
    let token = random_token();
    let hal_dir = app.data_dir.join("hal");
    // Empty login dirs: HAL's Codex / Claude / pi auto-detection finds nothing.
    let no_login = app.data_dir.join("no-login");
    std::fs::create_dir_all(&no_login).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&hal_dir).map_err(|e| e.to_string())?;
    let config = providers::hal_config(store, &app.gateway_url, &app.gateway.token, &hal_dir);
    let config_path = app.data_dir.join("hal.yaml");
    // JSON is valid YAML. File holds only the per-launch gateway token.
    write_private(&config_path, serde_json::to_string_pretty(&config).unwrap().as_bytes())
        .map_err(|e| format!("writing HAL config: {e}"))?;

    let path = server_path();
    let mut cmd = Command::new(&path);
    for (k, _) in std::env::vars_os() {
        if is_credential_var(&k.to_string_lossy()) {
            cmd.env_remove(&k);
        }
    }
    cmd.env("HAL_DESKTOP_TOKEN", &token)
        .env("HAL_CONFIG", &config_path)
        .env("CODEX_HOME", &no_login)
        .env("CLAUDE_CONFIG_DIR", &no_login)
        .env("PI_CODING_AGENT_DIR", &no_login)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().map_err(|e| format!("failed to start {}: {e}", path.display()))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .map_err(|e| format!("reading HAL handshake: {e}"))?;
    let handshake: Value = serde_json::from_str(line.trim()).map_err(|e| {
        let _ = child.kill();
        format!("bad HAL handshake {line:?}: {e}")
    })?;
    let base = handshake["address"].as_str().ok_or("handshake missing address")?.to_string();
    if !base.starts_with("http://127.0.0.1:") {
        let _ = child.kill();
        return Err(format!("refusing non-loopback HAL address {base}"));
    }
    Ok(Hal { base, token, client: reqwest::Client::new(), proc: Mutex::new(Some((child, stdin))), config })
}

fn stop_hal(backend: &Backend) {
    if let Backend::Ready(h) = backend {
        if let Some((mut child, stdin)) = h.proc.lock().unwrap().take() {
            drop(stdin);
            for _ in 0..30 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = child.kill();
        }
    }
}

impl App {
    /// Push current providers + keychain keys to the gateway. Restart HAL only
    /// if the model list it sees changed (keys/URLs/guard never need a restart).
    fn apply(&self) -> Result<bool, String> {
        let store = self.store.lock().unwrap().clone();
        self.gateway.set_routes(
            store
                .providers
                .iter()
                .map(|p| Route { provider: p.clone(), api_key: providers::get_key(&p.id) })
                .collect(),
        );
        let wanted = providers::hal_config(
            &store,
            &self.gateway_url,
            &self.gateway.token,
            &self.data_dir.join("hal"),
        );
        let current = self.hal.read().unwrap().clone();
        if let Backend::Ready(h) = &*current {
            if h.config == wanted {
                return Ok(false);
            }
        }
        stop_hal(&current);
        let next = match start_hal(self, &store) {
            Ok(h) => Backend::Ready(h),
            Err(e) => {
                eprintln!("filium: {e}");
                Backend::Failed(e)
            }
        };
        *self.hal.write().unwrap() = Arc::new(next);
        Ok(true)
    }
}

/// Only API paths the UI needs; maintenance/admin routes stay unreachable.
fn validate_path(path: &str) -> Result<(), String> {
    let route = path.split('?').next().unwrap_or("");
    let ok_chars = path.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.?=&%".contains(c));
    if !path.starts_with('/')
        || path.starts_with("//")
        || !ok_chars
        || route.split('/').any(|seg| seg == ".." || seg == ".")
        || path.to_ascii_lowercase().contains("%2e")
        || path.to_ascii_lowercase().contains("%2f")
    {
        return Err("invalid path".into());
    }
    const ALLOWED: [&str; 3] = ["/health", "/models", "/sessions"];
    if !ALLOWED.iter().any(|p| route == *p || route.starts_with(&format!("{p}/"))) {
        return Err(format!("route not allowed: {route}"));
    }
    Ok(())
}

/// Proxy one request to HAL. `path` must be an absolute API path.
#[tauri::command]
async fn hal(
    app: State<'_, App>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    let backend = app.hal.read().unwrap().clone();
    let state = match &*backend {
        Backend::Ready(h) => h,
        Backend::Failed(e) => return Err(format!("HAL failed to start: {e}")),
    };
    validate_path(&path)?;
    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => return Err("method not allowed".into()),
    };
    let mut req = state.client.request(method, format!("{}{}", state.base, path)).bearer_auth(&state.token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    if status.is_success() { Ok(value) } else { Err(format!("HAL {status}: {value}")) }
}

// ---------- providers ----------

#[tauri::command]
fn providers_list(app: State<'_, App>) -> Value {
    let store = app.store.lock().unwrap().clone();
    let list: Vec<Value> = store
        .providers
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap();
            v["has_key"] = json!(providers::get_key(&p.id).is_some());
            v
        })
        .collect();
    json!({ "providers": list, "default": store.default, "gateway": app.gateway_url })
}

/// `api_key`: `None` keeps the stored key, `Some("")` removes it.
#[tauri::command]
async fn provider_save(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    provider: Provider,
    api_key: Option<String>,
    make_default: bool,
) -> Result<bool, String> {
    providers::validate(&provider)?;
    let id = provider.id.clone();
    match api_key.as_deref().map(str::trim) {
        Some("") => providers::delete_key(&id),
        Some(k) => providers::set_key(&id, k)?,
        None => {}
    }
    {
        let mut s = app.store.lock().unwrap();
        s.upsert(provider);
        if make_default {
            s.default = Some(id);
        }
        s.save(&app.data_dir)?;
    }
    reapply(handle).await
}

/// HAL restarts block briefly (handshake), so run off the async runtime.
async fn reapply(handle: tauri::AppHandle) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || handle.state::<App>().apply())
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn provider_delete(
    handle: tauri::AppHandle,
    app: State<'_, App>,
    id: String,
) -> Result<bool, String> {
    {
        let mut s = app.store.lock().unwrap();
        s.remove(&id);
        s.save(&app.data_dir)?;
    }
    providers::delete_key(&id);
    reapply(handle).await
}

/// Minimal live request through the gateway, so it shows up in Activity.
#[tauri::command]
async fn provider_test(app: State<'_, App>, id: String) -> Result<String, String> {
    let p = app
        .store
        .lock()
        .unwrap()
        .providers
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .ok_or("unknown provider")?;
    let base = format!("{}/p/{}", app.gateway_url, p.id);
    let c = reqwest::Client::new();
    let req = match p.kind {
        providers::Kind::OpenaiChat => c.post(format!("{base}/chat/completions")).json(&json!({
            "model": p.model, "max_tokens": 8,
            "messages": [{"role": "user", "content": "Reply with the single word OK."}]
        })),
        providers::Kind::OpenaiResponses => c.post(format!("{base}/responses")).json(&json!({
            "model": p.model, "max_output_tokens": 16, "input": "Reply with the single word OK."
        })),
        providers::Kind::Anthropic => c
            .post(format!("{base}/messages"))
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": p.model, "max_tokens": 8,
                "messages": [{"role": "user", "content": "Reply with the single word OK."}]
            })),
    };
    let resp = req.bearer_auth(&app.gateway.token).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(format!("{status}"))
    } else {
        Err(format!("{status}: {}", text.chars().take(400).collect::<String>()))
    }
}

// ---------- activity ----------

#[tauri::command]
fn activity_list(app: State<'_, App>) -> Value {
    json!(app.gateway.recent())
}

#[tauri::command]
fn activity_detail(app: State<'_, App>, id: u64) -> Option<Value> {
    app.gateway.detail(id)
}

#[tauri::command]
fn activity_clear(app: State<'_, App>) {
    app.gateway.clear();
}

fn main() {
    let tauri_app = tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("app data dir");
            std::fs::create_dir_all(&data_dir)?;

            // Gateway on an ephemeral loopback port, inside this process.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.set_nonblocking(true)?;
            let gateway_url = format!("http://{}", listener.local_addr()?);
            let gateway = Gateway::new(random_token(), Some(data_dir.join("activity.jsonl")));
            let router = gateway.router();
            tauri::async_runtime::spawn(async move {
                let l = tokio::net::TcpListener::from_std(listener).expect("gateway listener");
                if let Err(e) = axum::serve(l, router).await {
                    eprintln!("filium: gateway stopped: {e}");
                }
            });
            // Live activity -> UI.
            let mut rx = gateway.events.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(a) => {
                            let _ = handle.emit("activity", a);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });

            let state = App {
                gateway,
                gateway_url,
                store: Mutex::new(Store::load(&data_dir)),
                data_dir,
                hal: RwLock::new(Arc::new(Backend::Failed("starting".into()))),
            };
            // Never abort: a missing/broken sidecar must show in the UI.
            let _ = state.apply();
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hal,
            providers_list,
            provider_save,
            provider_delete,
            provider_test,
            activity_list,
            activity_detail,
            activity_clear
        ])
        .build(tauri::generate_context!())
        .expect("error while building Filium");
    tauri_app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(app) = handle.try_state::<App>() {
                stop_hal(&app.hal.read().unwrap());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{is_credential_var, validate_path};

    /// Real HAL (bundled sidecar) + real gateway + fake upstream, with a
    /// hostile parent environment. Proves HAL can only reach the model through
    /// the gateway, never sees the real key, and secrets are redacted.
    #[test]
    fn real_hal_goes_only_through_gateway() {
        use super::*;
        use std::sync::Mutex as M;
        if !server_path().exists() {
            eprintln!("skipping: no sidecar at {}", server_path().display());
            return;
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _g = rt.enter();
        // Hostile env: if HAL used any of these, the upstream would see them.
        std::env::set_var("OPENAI_API_KEY", "sk-LEAKED-FROM-ENV-000000000000000000");
        std::env::set_var("OPENAI_BASE_URL", "http://127.0.0.1:9/v1");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-LEAKED-FROM-ENV-0000000000");

        let seen: Arc<M<Vec<(String, String)>>> = Default::default();
        let s2 = seen.clone();
        let upstream = axum::Router::new().route(
            "/v1/{*rest}",
            axum::routing::any(move |h: axum::http::HeaderMap, body: String| {
                let s2 = s2.clone();
                async move {
                    let auth = h.get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
                    s2.lock().unwrap().push((auth, body.clone()));
                    let streaming = body.contains("\"stream\":true");
                    if streaming {
                        let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":null}]}\n\n\
                                   data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":2}}\n\n\
                                   data: [DONE]\n\n";
                        ([("content-type", "text/event-stream")], sse.to_string())
                    } else {
                        ([("content-type", "application/json")], r#"{"data":[{"id":"up-model"}],"choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]}"#.to_string())
                    }
                }
            }),
        );
        let ul = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let up_url = format!("http://{}/v1", ul.local_addr().unwrap());
        rt.spawn(async move { axum::serve(ul, upstream).await.unwrap() });

        let dir = std::env::temp_dir().join(format!("filium-e2e-{}", random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let gl = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let gateway_url = format!("http://{}", gl.local_addr().unwrap());
        let gateway = Gateway::new(random_token(), Some(dir.join("activity.jsonl")));
        let r = gateway.router();
        rt.spawn(async move { axum::serve(gl, r).await.unwrap() });

        let mut store = Store::default();
        store.upsert(Provider {
            id: "cn".into(),
            name: "CN".into(),
            kind: providers::Kind::OpenaiChat,
            base_url: up_url,
            model: "up-model".into(),
            context_window: 64000,
            guard: guard::GuardMode::Redact,
        });
        let app = App {
            gateway: gateway.clone(),
            gateway_url,
            data_dir: dir.clone(),
            store: Mutex::new(store.clone()),
            hal: RwLock::new(Arc::new(Backend::Failed("init".into()))),
        };
        // Same as apply(), but with a test key instead of the keychain.
        gateway.set_routes(vec![Route { provider: store.providers[0].clone(), api_key: Some("REAL-KEY-ONLY-IN-GATEWAY".into()) }]);
        let hal = start_hal(&app, &store).expect("HAL starts");
        let cfg_text = std::fs::read_to_string(dir.join("hal.yaml")).unwrap();
        assert!(!cfg_text.contains("REAL-KEY") && !cfg_text.contains("LEAKED"), "{cfg_text}");

        let c = reqwest::Client::new();
        let call = |m: reqwest::Method, p: &str, b: Option<Value>| {
            let mut r = c.request(m, format!("{}{}", hal.base, p)).bearer_auth(&hal.token);
            if let Some(b) = b {
                r = r.json(&b);
            }
            async move { r.send().await.unwrap().json::<Value>().await.unwrap() }
        };
        let models = rt.block_on(call(reqwest::Method::GET, "/models", None));
        let names = models.to_string();
        assert!(names.contains("\"cn\""), "{names}");
        assert!(!names.contains("LEAKED") && !names.contains("127.0.0.1:9"), "env model leaked in: {names}");

        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let s = rt.block_on(call(reqwest::Method::POST, "/sessions", Some(json!({"workspace": ws, "permission_mode": "auto_deny"}))));
        let sid = s["id"].as_str().or(s["session"]["id"].as_str()).expect("session id").to_string();
        rt.block_on(call(
            reqwest::Method::POST,
            &format!("/sessions/{sid}/messages"),
            Some(json!({"text": "deploy using AKIAABCDEFGHIJKLMNOP and DB_PASSWORD=hunter2hunter2"})),
        ));
        let mut done = false;
        for _ in 0..100 {
            let evs = rt.block_on(call(reqwest::Method::GET, &format!("/sessions/{sid}/events?after=0&limit=500"), None));
            if evs.as_array().unwrap().iter().any(|e| e["type"] == "assistant.completed" || e["type"] == "turn.failed") {
                done = true;
                assert!(!evs.to_string().contains("turn.failed"), "{evs}");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(done, "turn never completed");

        let seen = seen.lock().unwrap().clone();
        assert!(!seen.is_empty(), "upstream got nothing");
        for (auth, body) in &seen {
            assert_eq!(auth, "Bearer REAL-KEY-ONLY-IN-GATEWAY");
            assert!(!body.contains("AKIAABCDEFGHIJKLMNOP") && !body.contains("hunter2hunter2"), "secret left: {body}");
            assert!(!body.contains(&gateway.token), "gateway token forwarded");
        }
        assert!(seen.iter().any(|(_, b)| b.contains("[REDACTED:aws_access_key]")));

        std::thread::sleep(std::time::Duration::from_millis(200));
        let acts = gateway.recent();
        let chat = acts.iter().find(|a| a.path == "/chat/completions").expect("logged");
        assert_eq!((chat.state, chat.guard, chat.status), ("done", "redacted", Some(200)));
        assert_eq!(chat.input_tokens, Some(21));
        let disk = std::fs::read_to_string(dir.join("activity.jsonl")).unwrap();
        assert!(disk.contains("\"redacted\"") && !disk.contains("AKIA") && !disk.contains("REAL-KEY"));
        let hal_log_dir = std::fs::read_dir(dir.join("hal")).map(|d| d.count()).unwrap_or(0);
        eprintln!("ok: {} upstream calls, {} activity rows, hal data entries {hal_log_dir}", seen.len(), acts.len());

        stop_hal(&Backend::Ready(hal));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scrubs_provider_credentials_from_hal_env() {
        for v in ["OPENAI_API_KEY", "OPENAI_BASE_URL", "ANTHROPIC_API_KEY", "HAL_MODEL", "HAL_TOKEN",
                  "DEEPSEEK_API_KEY", "GITHUB_TOKEN", "MY_SECRET", "openai_api_key"] {
            assert!(is_credential_var(v), "{v}");
        }
        for v in ["PATH", "HOME", "LANG", "TMPDIR", "SHELL", "USER", "CARGO_HOME"] {
            assert!(!is_credential_var(v), "{v}");
        }
    }

    #[test]
    fn allows_ui_routes() {
        for p in [
            "/health",
            "/models",
            "/sessions",
            "/sessions/abc-123/events?after=5&limit=500",
            "/sessions/abc/permissions/r1/approve",
        ] {
            assert!(validate_path(p).is_ok(), "{p}");
        }
    }

    #[test]
    fn rejects_escape_and_admin_routes() {
        for p in [
            "",
            "health",
            "//evil.example/x",
            "http://evil.example/sessions",
            "/sessions/../maintenance/shutdown",
            "/sessions/%2e%2e/maintenance/shutdown",
            "/sessions%2f..%2fmaintenance",
            "/maintenance/shutdown",
            "/maintenance/freeze",
            "/desktop/models",
            "/providers",
            "/sessions@evil",
            "/sessions\n/x",
            "/sessionsX",
        ] {
            assert!(validate_path(p).is_err(), "{p:?} should be rejected");
        }
    }
}
