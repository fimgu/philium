//! Filium: desktop shell around the HAL harness.
//!
//! The Rust side owns the bundled `hal-desktop-server` child process and its
//! private bearer token. The web UI never sees the token; it can only reach
//! HAL through the `hal` command below, which pins requests to loopback.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use rand::{distributions::Alphanumeric, Rng};
use serde_json::Value;
use tauri::{Manager, RunEvent, State};

struct Hal {
    base: String,
    token: String,
    client: reqwest::Client,
    // Holding stdin open keeps HAL alive; dropping it (EOF) shuts HAL down.
    proc: Mutex<Option<(Child, ChildStdin)>>,
}

fn server_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let dir = exe.parent().expect("exe dir");
    let name = if cfg!(windows) { "hal-desktop-server.exe" } else { "hal-desktop-server" };
    dir.join(name)
}

fn start_hal() -> Result<Hal, String> {
    let token: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    let path = server_path();
    let mut child = Command::new(&path)
        .env("HAL_DESKTOP_TOKEN", &token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", path.display()))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .map_err(|e| format!("reading HAL handshake: {e}"))?;
    let handshake: Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("bad HAL handshake {line:?}: {e}"))?;
    let base = handshake["address"]
        .as_str()
        .ok_or("handshake missing address")?
        .to_string();
    if !base.starts_with("http://127.0.0.1:") {
        return Err(format!("refusing non-loopback HAL address {base}"));
    }
    Ok(Hal {
        base,
        token,
        client: reqwest::Client::new(),
        proc: Mutex::new(Some((child, stdin))),
    })
}

/// Only API paths the UI needs; maintenance/admin routes stay unreachable.
fn validate_path(path: &str) -> Result<(), String> {
    let route = path.split('?').next().unwrap_or("");
    let ok_chars = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/-_.?=&%".contains(c));
    if !path.starts_with('/')
        || path.starts_with("//")
        || !ok_chars
        || route.split('/').any(|seg| seg == ".." || seg == ".")
        || path.to_ascii_lowercase().contains("%2e")
        || path.to_ascii_lowercase().contains("%2f")
    {
        return Err("invalid path".into());
    }
    const ALLOWED: [&str; 4] = ["/health", "/models", "/sessions", "/providers"];
    if !ALLOWED
        .iter()
        .any(|p| route == *p || route.starts_with(&format!("{p}/")))
    {
        return Err(format!("route not allowed: {route}"));
    }
    Ok(())
}

/// Proxy one request to HAL. `path` must be an absolute API path.
#[tauri::command]
async fn hal(
    state: State<'_, Hal>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    validate_path(&path)?;
    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => return Err("method not allowed".into()),
    };
    let mut req = state
        .client
        .request(method, format!("{}{}", state.base, path))
        .bearer_auth(&state.token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    if status.is_success() {
        Ok(value)
    } else {
        Err(format!("HAL {status}: {value}"))
    }
}

fn shutdown(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Hal>() {
        if let Some((mut child, stdin)) = state.proc.lock().unwrap().take() {
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

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let hal = start_hal().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            app.manage(hal);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![hal])
        .build(tauri::generate_context!())
        .expect("error while building Filium");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            shutdown(handle);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::validate_path;

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
            "/sessions@evil",
            "/sessions\n/x",
            "/sessionsX",
        ] {
            assert!(validate_path(p).is_err(), "{p:?} should be rejected");
        }
    }
}
