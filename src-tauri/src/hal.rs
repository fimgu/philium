//! Launch and talk to one `hal-desktop-server` process.
//!
//! Philium runs one HAL per agent (SPEC 9.3) plus one "desk" HAL for the
//! plain chat view. Every HAL gets: a generated 0600 config pointing all
//! models at the gateway, a scrubbed environment (no provider credentials),
//! empty login dirs, and an optional sandbox wrapper (`sandbox.rs`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use serde_json::Value;

pub struct Hal {
    pub base: String,
    pub token: String,
    pub client: reqwest::Client,
    pub pid: u32,
    // Holding stdin open keeps HAL alive; dropping it (EOF) shuts HAL down.
    proc: Mutex<Option<(Child, ChildStdin)>>,
    pub config: Value,
}

/// How to launch one HAL.
pub struct Launch<'a> {
    /// Private dir for this HAL: config, data, empty login dirs.
    pub dir: &'a Path,
    pub config: Value,
    pub cwd: Option<&'a Path>,
    /// Extra env (TMPDIR, KUBECONFIG, PHILIUM_* ...). Applied after scrubbing.
    pub env: Vec<(String, String)>,
    /// Keep real login dirs (Codex/Claude) for trusted subscription models.
    pub allow_logins: bool,
    /// Optional wrapper: given the HAL binary, returns program + args to run
    /// instead (used for Seatbelt: `sandbox-exec -p <profile> <hal>`).
    pub wrap: Option<Box<dyn Fn(&Path) -> (PathBuf, Vec<String>) + 'a>>,
}

pub fn random_token() -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng().sample_iter(&Alphanumeric).take(48).map(char::from).collect()
}

pub fn server_path() -> PathBuf {
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
pub fn is_credential_var(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const PREFIXES: [&str; 12] = [
        "OPENAI_", "ANTHROPIC_", "HAL_", "AZURE_OPENAI", "GEMINI_", "GOOGLE_API", "GROQ_",
        "MISTRAL_", "DEEPSEEK_", "OPENROUTER_", "MOONSHOT_", "DASHSCOPE_",
    ];
    const WORDS: [&str; 5] = ["API_KEY", "APIKEY", "SECRET", "TOKEN", "PASSWORD"];
    PREFIXES.iter().any(|p| n.starts_with(p)) || WORDS.iter().any(|w| n.contains(w))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

impl Hal {
    pub fn start(l: Launch) -> Result<Hal, String> {
        let token = random_token();
        let no_login = l.dir.join("no-login");
        std::fs::create_dir_all(&no_login).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(l.dir.join("hal")).map_err(|e| e.to_string())?;
        let config_path = l.dir.join("hal.yaml");
        // JSON is valid YAML. File holds only the per-launch gateway token.
        write_private(&config_path, serde_json::to_string_pretty(&l.config).unwrap().as_bytes())
            .map_err(|e| format!("writing HAL config: {e}"))?;

        let bin = server_path();
        let (prog, args) = match &l.wrap {
            Some(w) => w(&bin),
            None => (bin.clone(), vec![]),
        };
        let mut cmd = Command::new(&prog);
        cmd.args(&args);
        for (k, _) in std::env::vars_os() {
            if is_credential_var(&k.to_string_lossy()) {
                cmd.env_remove(&k);
            }
        }
        cmd.env("HAL_DESKTOP_TOKEN", &token).env("HAL_CONFIG", &config_path);
        if !l.allow_logins {
            cmd.env("CODEX_HOME", &no_login)
                .env("CLAUDE_CONFIG_DIR", &no_login)
                .env("PI_CODING_AGENT_DIR", &no_login);
        }
        for (k, v) in &l.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = l.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| format!("failed to start {}: {e}", prog.display()))?;
        let pid = child.id();
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
        Ok(Hal {
            base,
            token,
            client: reqwest::Client::new(),
            pid,
            proc: Mutex::new(Some((child, stdin))),
            config: l.config,
        })
    }

    pub fn alive(&self) -> bool {
        match self.proc.lock().unwrap().as_mut() {
            Some((c, _)) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn stop(&self) {
        if let Some((mut child, stdin)) = self.proc.lock().unwrap().take() {
            drop(stdin);
            for _ in 0..30 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// One JSON request to this HAL's API.
    pub async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut req = self.client.request(method, format!("{}{}", self.base, path)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if status.is_success() { Ok(value) } else { Err(format!("HAL {status}: {value}")) }
    }
}

impl Drop for Hal {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Only API paths the UI needs; maintenance/admin routes stay unreachable.
pub fn validate_path(path: &str) -> Result<(), String> {
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
    const ALLOWED: [&str; 4] = ["/health", "/models", "/sessions", "/stats"];
    if !ALLOWED.iter().any(|p| route == *p || route.starts_with(&format!("{p}/"))) {
        return Err(format!("route not allowed: {route}"));
    }
    Ok(())
}

pub fn parse_method(m: &str) -> Result<reqwest::Method, String> {
    Ok(match m {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        _ => return Err("method not allowed".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for p in ["/health", "/models", "/sessions", "/sessions/abc-123/events?after=5&limit=500",
                  "/sessions/abc/permissions/r1/approve"] {
            assert!(validate_path(p).is_ok(), "{p}");
        }
    }

    #[test]
    fn rejects_escape_and_admin_routes() {
        for p in ["", "health", "//evil.example/x", "http://evil.example/sessions",
                  "/sessions/../maintenance/shutdown", "/sessions/%2e%2e/maintenance/shutdown",
                  "/sessions%2f..%2fmaintenance", "/maintenance/shutdown", "/maintenance/freeze",
                  "/desktop/models", "/providers", "/sessions@evil", "/sessions\n/x", "/sessionsX"] {
            assert!(validate_path(p).is_err(), "{p:?} should be rejected");
        }
    }
}
