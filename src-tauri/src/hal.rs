//! Launch and talk to one `hal-desktop-server` process.
//!
//! Philium runs one HAL per agent (SPEC 9.3) plus one "desk" HAL for the
//! plain chat view. Every HAL gets: a generated 0600 config pointing all
//! models at the gateway, a scrubbed environment (no provider credentials),
//! empty login dirs, and an optional sandbox wrapper (`sandbox.rs`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub struct Hal {
    pub base: String,
    pub token: String,
    pub client: reqwest::Client,
    pub pid: u32,
    // Holding stdin open keeps HAL alive; dropping it (EOF) shuts HAL down.
    proc: Mutex<Option<(Child, ChildStdin)>>,
    pub config: Value,
    /// Last stderr lines, kept for crash reports.
    tail: Arc<Mutex<Vec<String>>>,
    /// Exit status once the process is known to have exited.
    exited: Mutex<Option<std::process::ExitStatus>>,
}

/// How long a HAL may take to print its handshake before we give up.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// Most useful line of HAL's stderr: the first mentioning an error, else the first.
pub fn first_error_line(lines: &[String]) -> Option<String> {
    let l = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
    let pick = l.clone().find(|l| l.to_lowercase().contains("error")).or_else(|| l.clone().next())?;
    Some(pick.chars().take(300).collect())
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
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("failed to start {}: {e}", prog.display()))?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        // Keep the stderr tail so a failed start can explain itself, and keep
        // echoing it to our own stderr.
        let tail: Arc<Mutex<Vec<String>>> = Arc::default();
        let reader = child.stderr.take().map(|err| {
            let tail = tail.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    eprintln!("hal: {line}");
                    let mut t = tail.lk();
                    t.push(line);
                    if t.len() > 20 {
                        t.remove(0);
                    }
                }
            })
        });
        // Read the handshake on a helper thread so a wedged HAL cannot hang us.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let r = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = tx.send(r);
        });
        let (read, line) = match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(Ok(l)) if l.is_empty() => (Err(std::io::Error::other("HAL closed stdout before the handshake")), l),
            Ok(Ok(l)) => (Ok(()), l),
            Ok(Err(e)) => (Err(e), String::new()),
            Err(_) => (Err(std::io::Error::other("HAL did not start within 30s")), String::new()),
        };
        let explain = |child: &mut Child, what: String| {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(r) = reader {
                let _ = r.join();
            }
            match first_error_line(&tail.lk()) {
                Some(l) => format!("HAL exited: {l}"),
                None => what,
            }
        };
        if let Err(e) = read {
            return Err(explain(&mut child, format!("reading HAL handshake: {e}")));
        }
        let handshake: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => return Err(explain(&mut child, format!("bad HAL handshake {line:?}: {e}"))),
        };
        let Some(base) = handshake["address"].as_str().map(str::to_string) else {
            return Err(explain(&mut child, "HAL handshake missing address".into()));
        };
        if !base.starts_with("http://127.0.0.1:") {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("refusing non-loopback HAL address {base}"));
        }
        Ok(Hal::attach(child, stdin, tail, base, token, pid, l.config))
    }

    fn attach(child: Child, stdin: ChildStdin, tail: Arc<Mutex<Vec<String>>>, base: String, token: String, pid: u32, config: Value) -> Hal {
        Hal {
            base,
            token,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            pid,
            proc: Mutex::new(Some((child, stdin))),
            config,
            tail,
            exited: Mutex::new(None),
        }
    }

    pub fn alive(&self) -> bool {
        match self.proc.lk().as_mut() {
            Some((c, _)) => match c.try_wait() {
                Ok(None) => true,
                Ok(Some(st)) => {
                    *self.exited.lk() = Some(st);
                    false
                }
                Err(_) => false,
            },
            None => false,
        }
    }

    /// Human-readable reason for an unexpected exit, or None while running.
    pub fn exit_reason(&self) -> Option<String> {
        if self.alive() {
            return None;
        }
        let status = match *self.exited.lk() {
            Some(st) => describe_status(st),
            None => "stopped".to_string(),
        };
        Some(match first_error_line(&self.tail.lk()) {
            Some(l) => format!("HAL {status}: {l}"),
            None => format!("HAL {status}"),
        })
    }

    pub fn stop(&self) {
        if let Some((mut child, stdin)) = self.proc.lk().take() {
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
fn describe_status(st: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = st.signal() {
            let name = match sig {
                9 => " (SIGKILL)",
                15 => " (SIGTERM)",
                11 => " (SIGSEGV)",
                6 => " (SIGABRT)",
                _ => "",
            };
            return format!("was killed by signal {sig}{name}");
        }
    }
    match st.code() {
        Some(c) => format!("exited with code {c}"),
        None => "exited".into(),
    }
}

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
    fn picks_error_line() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(first_error_line(&v(&["", "boot", "config error: bad"])).unwrap(), "config error: bad");
        assert_eq!(first_error_line(&v(&["panic"])).unwrap(), "panic");
        assert!(first_error_line(&[]).is_none());
    }

    #[test]
    fn allows_ui_routes() {
        for p in ["/health", "/models", "/sessions", "/sessions/abc-123/events?after=5&limit=500",
                  "/sessions/abc/permissions/r1/approve"] {
            assert!(validate_path(p).is_ok(), "{p}");
        }
    }

    fn fake(script: &str) -> Hal {
        let mut c = Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let tail: Arc<Mutex<Vec<String>>> = Arc::default();
        let err = c.stderr.take().unwrap();
        let t2 = tail.clone();
        let rd = std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                t2.lk().push(l);
            }
        });
        let stdin = c.stdin.take().unwrap();
        let pid = c.id();
        let h = Hal::attach(c, stdin, tail, "http://127.0.0.1:9".into(), "t".into(), pid, Value::Null);
        if !script.contains("sleep") {
            let _ = rd.join();
        }
        h
    }

    #[test]
    fn crash_is_detected_with_reason() {
        let h = fake("echo 'error: model config broken' >&2; exit 3");
        for _ in 0..100 {
            if !h.alive() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let r = h.exit_reason().expect("exited");
        assert!(r.contains("code 3") && r.contains("model config broken"), "{r}");
    }

    #[test]
    fn killed_hal_reports_signal_and_stop_is_idempotent() {
        let h = fake("sleep 30");
        assert!(h.alive() && h.exit_reason().is_none());
        unsafe { libc::kill(h.pid as i32, libc::SIGKILL) };
        for _ in 0..100 {
            if !h.alive() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(h.exit_reason().unwrap().contains("SIGKILL"));
        h.stop();
        h.stop();
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

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
