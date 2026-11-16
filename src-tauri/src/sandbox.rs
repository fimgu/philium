//! Strict mode: OS sandbox for agents on untrusted providers + egress proxy.
//!
//! rpc methods (`sandbox.*`):
//! - `status {}` -> `{available, error?, platform, egress_url, allow_hosts, denied_total, allowed_total}`
//! - `egress {agent?, limit?}` -> rows `{id, ts, agent, host, port, allowed, bytes}` newest first
//! - `allowlist_get {agent?}` -> `{hosts, custom}` (global list, or the agent override if set)
//! - `allowlist_set {hosts, agent?}` -> `{hosts}` (global key `strict.allow_hosts`)
//! - `test_profile {}` -> `{ok, probes:[{name, ok, detail}]}` real sandbox-exec probes (macOS)
//!
//! Events: `sandbox:egress` `{agent, host, port, allowed, bytes, ts}` per connection.
//!
//! Rust API for the agents lane: `Policy`, `profile`, `wrap`, `env`, `available`,
//! `scratch_dir(ctx, agent)` (use it as `Policy.scratch`), `revoke(agent)`.
//! Sandbox backends are pluggable through `Backend`; unsupported OS => `available()`
//! returns Err and strict agents must refuse to start (never run unsandboxed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::ctx::{arg_opt_str, Ctx};

pub const DEFAULT_ALLOW: &[&str] = &[
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
];

const ALLOW_KEY: &str = "strict.allow_hosts";

#[derive(Clone, Debug)]
pub struct Policy {
    pub workdir: PathBuf,
    pub scratch: PathBuf,
    pub hal_dir: PathBuf,
    pub read_only: Vec<PathBuf>,
    /// Informational: enforcement is done by the egress proxy (the kernel only
    /// permits loopback). The proxy reads the live list from settings.
    pub allow_hosts: Vec<String>,
}

/// Pluggable OS sandbox backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Seatbelt,
    /// Linux Landlock + netns + seccomp. Not implemented yet.
    Landlock,
    Unsupported,
}

pub fn backend() -> Backend {
    if cfg!(target_os = "macos") {
        Backend::Seatbelt
    } else if cfg!(target_os = "linux") {
        Backend::Landlock
    } else {
        Backend::Unsupported
    }
}

pub fn available() -> Result<(), String> {
    match backend() {
        Backend::Seatbelt => {
            if Path::new("/usr/bin/sandbox-exec").exists() {
                Ok(())
            } else {
                Err("/usr/bin/sandbox-exec not found".into())
            }
        }
        Backend::Landlock => Err("Strict mode on Linux (Landlock) is not implemented yet; refusing to run unsandboxed".into()),
        Backend::Unsupported => Err("Strict mode is not supported on this OS; refusing to run unsandboxed".into()),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/empty"))
}

fn esc(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

/// Real path (macOS: /var -> /private/var) plus the original, deduped.
fn variants(p: &Path) -> Vec<PathBuf> {
    let mut v = vec![p.to_path_buf()];
    if let Ok(c) = std::fs::canonicalize(p) {
        if c != p {
            v.push(c);
        }
    }
    v
}

fn subpaths(rule: &str, paths: &[PathBuf]) -> String {
    let mut s = String::new();
    for p in paths {
        for v in variants(p) {
            s.push_str(&format!("(allow {rule} (subpath \"{}\"))\n", esc(&v)));
        }
    }
    s
}

/// Seatbelt (SBPL) profile. Later rules win, so denies come after the broad
/// read allows and before the explicit workdir/scratch/hal_dir allows.
pub fn profile(p: &Policy) -> String {
    let h = home();
    let mut s = String::new();
    s.push_str("(version 1)\n(deny default)\n");
    s.push_str("(allow process-exec)\n(allow process-fork)\n(allow signal (target self))\n");
    s.push_str("(allow sysctl-read)\n(allow mach-lookup)\n(allow ipc-posix-shm)\n(allow file-read-metadata)\n");
    // System paths needed to run binaries and toolchains.
    for sys in [
        "/usr", "/bin", "/sbin", "/System", "/Library/Frameworks", "/Library/Developer", "/private/etc", "/etc",
        "/opt/homebrew", "/private/var/db", "/Applications", "/var/db/timezone",
    ] {
        s.push_str(&format!("(allow file-read* (subpath \"{sys}\"))\n"));
    }
    // Xcode command line tools: the license check and xcrun's cache. Without these
    // /usr/bin/python3, git and clang refuse to run inside the sandbox.
    s.push_str("(allow file-read* (literal \"/Library/Preferences/com.apple.dt.Xcode.plist\"))\n");
    s.push_str("(allow file-read* file-write* (regex #\"^/private/var/folders/[^/]+/[^/]+/(T|C)/xcrun_db\"))\n");
    s.push_str("(allow file-read* (literal \"/\") (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/random\") (literal \"/dev/urandom\"))\n");
    s.push_str("(allow file-read* file-write* (regex #\"^/dev/(tty|pty|ttys)\"))\n");
    s.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\"))\n");
    s.push_str(&subpaths("file-read*", &p.read_only));
    // Secrets and the rest of the home dir stay out of reach.
    for d in [".ssh", ".aws", ".kube", ".gnupg", ".config/gh", ".docker", ".netrc", "Library", "Documents", "Desktop", "Downloads", "Developer"] {
        s.push_str(&format!("(deny file-read* file-write* (subpath \"{}\"))\n", esc(&h.join(d))));
    }
    s.push_str("(deny file-read* file-write* (subpath \"/Library/Keychains\"))\n(deny file-read* file-write* (subpath \"/private/var/db/dslocal\"))\n");
    // Toolchains (read + exec) and package caches (read/write), re-allowed after the denies.
    let tool_ro: Vec<PathBuf> = [".cargo/bin", ".rustup", ".nvm", ".volta", ".pyenv", ".local/bin", ".bun/bin", ".gitconfig", ".config/git"]
        .iter()
        .map(|d| h.join(d))
        .collect();
    s.push_str(&subpaths("file-read*", &tool_ro));
    let caches: Vec<PathBuf> = [".npm", ".cargo/registry", ".cargo/git", ".cache", "Library/Caches"].iter().map(|d| h.join(d)).collect();
    s.push_str(&subpaths("file-read* file-write*", &caches));
    // The only places the agent may read and write.
    s.push_str(&subpaths("file-read* file-write*", &[p.workdir.clone(), p.scratch.clone(), p.hal_dir.clone()]));
    // Network: loopback only (gateway, egress proxy, MCP).
    s.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    s.push_str("(allow network-bind (local ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n");
    s
}

type WrapFn = Box<dyn Fn(&Path) -> (PathBuf, Vec<String>) + Send + Sync>;

pub fn wrap(p: &Policy) -> WrapFn {
    let base = profile(p);
    Box::new(move |hal: &Path| {
        let mut prof = base.clone();
        prof.push_str(&format!("(allow file-read* (literal \"{}\"))\n", esc(hal)));
        if let Ok(c) = std::fs::canonicalize(hal) {
            prof.push_str(&format!("(allow file-read* (literal \"{}\"))\n", esc(&c)));
        }
        (PathBuf::from("/usr/bin/sandbox-exec"), vec!["-p".into(), prof, hal.to_string_lossy().into_owned()])
    })
}

pub fn scratch_dir(ctx: &Ctx, agent: &str) -> PathBuf {
    ctx.agent_dir(agent).join("scratch")
}

fn tokens() -> &'static Mutex<HashMap<String, String>> {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

fn agent_token(agent: &str) -> String {
    tokens().lock().unwrap().entry(agent.to_string()).or_insert_with(crate::hal::random_token).clone()
}

/// Drop the agent's proxy credential (kill switch / teardown).
pub fn revoke(agent: &str) {
    tokens().lock().unwrap().remove(agent);
}

/// Env for a strict agent. Proxy credential is per agent.
pub fn env(ctx: &Arc<Ctx>, agent: &str) -> Vec<(String, String)> {
    let scratch = scratch_dir(ctx, agent);
    let home = scratch.join("home");
    let _ = std::fs::create_dir_all(&home);
    let mut v = vec![
        ("TMPDIR".to_string(), scratch.to_string_lossy().into_owned()),
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
    ];
    if let Some(url) = ctx.egress_url.get() {
        let auth = url.replacen("http://", &format!("http://{agent}:{}@", agent_token(agent)), 1);
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            v.push((k.to_string(), auth.clone()));
        }
        v.push(("NO_PROXY".into(), "127.0.0.1,localhost".into()));
        v.push(("no_proxy".into(), "127.0.0.1,localhost".into()));
    }
    v
}

// ---------------------------------------------------------------- allowlist

fn parse_hosts(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(|h| h.trim().trim_start_matches("*.").to_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

pub fn allowlist(ctx: &Ctx, agent: Option<&str>) -> (Vec<String>, bool) {
    if let Some(a) = agent {
        if let Some(v) = ctx.db.kv_get(&format!("setting.{ALLOW_KEY}.{a}")) {
            return (parse_hosts(&v), true);
        }
    }
    match ctx.db.kv_get(&format!("setting.{ALLOW_KEY}")) {
        Some(v) => (parse_hosts(&v), false),
        None => (DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect(), false),
    }
}

/// Exact host or any subdomain of an entry.
pub fn host_allowed(list: &[String], host: &str) -> bool {
    let h = host.trim_end_matches('.').to_lowercase();
    list.iter().any(|e| h == *e || h.ends_with(&format!(".{e}")))
}

// ---------------------------------------------------------------- proxy

fn log_conn(ctx: &Arc<Ctx>, agent: &str, host: &str, port: u16, allowed: bool, bytes: u64) {
    let ts = crate::db::now();
    let _ = ctx.db.exec(
        "INSERT INTO sandbox_egress(id,ts,agent,host,port,allowed,bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![crate::db::new_id(), ts, agent, host, port as i64, allowed as i64, bytes as i64],
    );
    ctx.emit("sandbox:egress", json!({"agent": agent, "host": host, "port": port, "allowed": allowed, "bytes": bytes, "ts": ts}));
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    rest: Vec<u8>,
}

async fn read_head(s: &mut TcpStream) -> Option<Head> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let end = loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    };
    let text = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    Some(Head { method, target, headers, rest: buf[end + 4..].to_vec() })
}

fn auth_agent(h: &Head) -> Option<String> {
    let v = h.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("proxy-authorization"))?.1.clone();
    let b = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic "))?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?;
    let raw = String::from_utf8(raw).ok()?;
    let (user, pass) = raw.split_once(':')?;
    let ok = tokens().lock().unwrap().get(user).map(|t| t == pass).unwrap_or(false);
    ok.then(|| user.to_string())
}

fn split_hostport(t: &str, default: u16) -> (String, u16) {
    if let Some(rest) = t.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once("]:") {
            return (h.to_string(), p.parse().unwrap_or(default));
        }
        return (rest.trim_end_matches(']').to_string(), default);
    }
    match t.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap()),
        _ => (t.to_string(), default),
    }
}

async fn reply(s: &mut TcpStream, status: &str, extra: &str) {
    let _ = s.write_all(format!("HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
}

async fn handle(ctx: Arc<Ctx>, mut c: TcpStream) {
    let Some(head) = read_head(&mut c).await else { return };
    let Some(agent) = auth_agent(&head) else {
        log_conn(&ctx, "?", &head.target, 0, false, 0);
        return reply(&mut c, "407 Proxy Authentication Required", "Proxy-Authenticate: Basic realm=\"philium\"\r\n").await;
    };
    let (list, _) = allowlist(&ctx, Some(&agent));
    let is_connect = head.method.eq_ignore_ascii_case("CONNECT");
    let (host, port, path) = if is_connect {
        let (h, p) = split_hostport(&head.target, 443);
        (h, p, String::new())
    } else if let Some(rest) = head.target.strip_prefix("http://") {
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (h, p) = split_hostport(auth, 80);
        (h, p, path)
    } else {
        log_conn(&ctx, &agent, &head.target, 0, false, 0);
        return reply(&mut c, "400 Bad Request", "").await;
    };
    if !host_allowed(&list, &host) {
        log_conn(&ctx, &agent, &host, port, false, 0);
        return reply(&mut c, "403 Forbidden", "X-Philium: host not on allowlist\r\n").await;
    }
    let mut up = match TcpStream::connect((host.as_str(), port)).await {
        Ok(u) => u,
        Err(_) => {
            log_conn(&ctx, &agent, &host, port, true, 0);
            return reply(&mut c, "502 Bad Gateway", "").await;
        }
    };
    if is_connect {
        if c.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.is_err() {
            return;
        }
        if !head.rest.is_empty() {
            let _ = up.write_all(&head.rest).await;
        }
    } else {
        let mut req = format!("{} {} HTTP/1.1\r\n", head.method, path);
        for (k, v) in &head.headers {
            let kl = k.to_lowercase();
            if kl.starts_with("proxy-") || kl == "connection" {
                continue;
            }
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        let mut out = req.into_bytes();
        out.extend_from_slice(&head.rest);
        if up.write_all(&out).await.is_err() {
            return;
        }
    }
    let bytes = match tokio::io::copy_bidirectional(&mut c, &mut up).await {
        Ok((a, b)) => a + b,
        Err(_) => 0,
    };
    log_conn(&ctx, &agent, &host, port, true, bytes);
}

async fn serve(ctx: Arc<Ctx>, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((s, _)) => {
                tokio::spawn(handle(ctx.clone(), s));
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sandbox_egress(
    id TEXT PRIMARY KEY, ts INTEGER NOT NULL, agent TEXT NOT NULL, host TEXT NOT NULL,
    port INTEGER NOT NULL, allowed INTEGER NOT NULL, bytes INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX IF NOT EXISTS sandbox_egress_ts ON sandbox_egress(ts);";

fn setup_db(ctx: &Ctx) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    setup_db(ctx)?;
    let std_l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    std_l.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = std_l.local_addr().map_err(|e| e.to_string())?.port();
    let _ = ctx.egress_url.set(format!("http://127.0.0.1:{port}"));
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        if let Ok(l) = TcpListener::from_std(std_l) {
            serve(c, l).await;
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- rpc

async fn run_probe(prof: &str, script: &str, env: &[(String, String)]) -> (bool, String) {
    let mut cmd = tokio::process::Command::new("/usr/bin/sandbox-exec");
    cmd.args(["-p", prof, "/bin/sh", "-c", script]).env_clear().env("PATH", "/usr/bin:/bin");
    for (k, v) in env {
        cmd.env(k, v);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await {
        Ok(Ok(o)) => (
            o.status.success(),
            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().chars().take(300).collect(),
        ),
        Ok(Err(e)) => (false, e.to_string()),
        Err(_) => (false, "timeout".into()),
    }
}

async fn test_profile(ctx: &Arc<Ctx>) -> Result<Value, String> {
    available()?;
    let id = crate::db::new_id();
    let probe = ctx.data_dir.join("sandbox-probe").join(&id);
    let work = probe.join("work");
    let scratch = probe.join("scratch");
    let hal = probe.join("hal");
    for d in [&work, &scratch, &hal] {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let pol = Policy { workdir: work.clone(), scratch, hal_dir: hal, read_only: vec![], allow_hosts: vec![] };
    let prof = profile(&pol);
    // Local upstream so the "allowed host via proxy" probe works offline.
    let up = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let up_port = up.local_addr().map_err(|e| e.to_string())?.port();
    let srv = tokio::spawn(async move {
        while let Ok((mut s, _)) = up.accept().await {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
        }
    });
    let agent = "__probe__";
    ctx.db.kv_set(&format!("setting.{ALLOW_KEY}.{agent}"), "127.0.0.1")?;
    let mut penv = env(ctx, agent);
    penv.retain(|(k, _)| k.to_lowercase().ends_with("_proxy") && k != "NO_PROXY");
    let h = home();
    let outside = h.join(format!(".philium-probe-{id}"));
    let probes: Vec<(&str, String, bool)> = vec![
        ("cannot read ~/.ssh", format!("ls '{}'", h.join(".ssh").display()), false),
        ("cannot write outside workdir", format!("echo x > '{}'", outside.display()), false),
        ("can write inside workdir", format!("echo x > '{}/f'", work.display()), true),
        ("cannot reach internet directly", "/usr/bin/curl -sS --noproxy '*' -m 4 http://1.1.1.1/".into(), false),
        (
            "can reach allowed host via proxy",
            format!("/usr/bin/curl -sS -m 6 -x \"$HTTP_PROXY\" http://127.0.0.1:{up_port}/"),
            true,
        ),
    ];
    let mut out = vec![];
    let mut all = true;
    for (name, script, expect) in probes {
        let (ok, detail) = run_probe(&prof, &script, &penv).await;
        let pass = ok == expect;
        all &= pass;
        out.push(json!({"name": name, "ok": pass, "detail": detail}));
    }
    srv.abort();
    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&probe);
    let _ = ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("setting.{ALLOW_KEY}.{agent}")]);
    revoke(agent);
    Ok(json!({"ok": all, "probes": out}))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let a = available();
            let q = |allowed: i64| {
                ctx.db
                    .query("SELECT COUNT(*) AS n FROM sandbox_egress WHERE allowed=?1", [allowed])
                    .ok()
                    .and_then(|r| r.first().and_then(|x| x["n"].as_i64()))
                    .unwrap_or(0)
            };
            Ok(json!({
                "available": a.is_ok(), "error": a.err(), "platform": std::env::consts::OS,
                "egress_url": ctx.egress_url.get(), "allow_hosts": allowlist(ctx, None).0,
                "denied_total": q(0), "allowed_total": q(1),
            }))
        }
        "egress" => {
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(200).clamp(1, 5000);
            let rows = match arg_opt_str(&args, "agent") {
                Some(a) => ctx.db.query("SELECT * FROM sandbox_egress WHERE agent=?1 ORDER BY ts DESC LIMIT ?2", rusqlite::params![a, limit])?,
                None => ctx.db.query("SELECT * FROM sandbox_egress ORDER BY ts DESC LIMIT ?1", [limit])?,
            };
            Ok(Value::Array(rows))
        }
        "allowlist_get" => {
            let (hosts, custom) = allowlist(ctx, arg_opt_str(&args, "agent").as_deref());
            Ok(json!({"hosts": hosts, "custom": custom}))
        }
        "allowlist_set" => {
            let hosts: Vec<String> = match args.get("hosts") {
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).flat_map(parse_hosts).collect(),
                Some(Value::String(s)) => parse_hosts(s),
                _ => return Err("missing `hosts`".into()),
            };
            let key = match arg_opt_str(&args, "agent") {
                Some(a) => format!("setting.{ALLOW_KEY}.{a}"),
                None => format!("setting.{ALLOW_KEY}"),
            };
            ctx.db.kv_set(&key, &hosts.join(","))?;
            Ok(json!({"hosts": hosts}))
        }
        "test_profile" => test_profile(ctx).await,
        _ => Err(format!("sandbox.{method}: unknown method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol(base: &Path) -> Policy {
        Policy {
            workdir: base.join("work"),
            scratch: base.join("scratch"),
            hal_dir: base.join("hal"),
            read_only: vec![PathBuf::from("/opt/tools")],
            allow_hosts: vec![],
        }
    }

    #[test]
    fn profile_rules() {
        let p = profile(&pol(Path::new("/tmp/x")));
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow file-read* file-write* (subpath \"/tmp/x/work\"))"));
        assert!(p.contains("(allow file-read* (subpath \"/opt/tools\"))"));
        assert!(!p.contains("file-write* (subpath \"/opt/tools\")"));
        assert!(p.contains(".ssh") && p.contains(".aws") && p.contains("/Library/Keychains"));
        assert!(p.contains("(remote ip \"localhost:*\")"));
        assert!(!p.contains("(allow network*)") && !p.contains("(allow network-outbound)\n"));
        // denies come before the workdir allow so the workdir can live under a denied tree
        assert!(p.find("(deny file-read*").unwrap() < p.find("(subpath \"/tmp/x/work\")").unwrap());
    }

    #[test]
    fn profile_escapes_quotes() {
        let p = profile(&pol(Path::new("/tmp/a\"b")));
        assert!(p.contains("/tmp/a\\\"b/work"));
    }

    #[test]
    fn wrap_builds_sandbox_exec() {
        let (prog, args) = wrap(&pol(Path::new("/tmp/x")))(Path::new("/bin/hal"));
        assert_eq!(prog, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(args[0], "-p");
        assert_eq!(args.last().unwrap(), "/bin/hal");
        assert!(args[1].contains("(literal \"/bin/hal\")"));
    }

    #[test]
    fn allowlist_matching() {
        let l = parse_hosts("github.com, *.Example.org\npypi.org");
        assert!(host_allowed(&l, "github.com"));
        assert!(host_allowed(&l, "api.github.com"));
        assert!(host_allowed(&l, "a.example.org"));
        assert!(!host_allowed(&l, "evilgithub.com"));
        assert!(!host_allowed(&l, "github.com.evil.io"));
    }

    #[tokio::test]
    async fn env_and_proxy_allow_deny() {
        let ctx = Ctx::test();
        setup_db(&ctx).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _ = ctx.egress_url.set(format!("http://{}", l.local_addr().unwrap()));
        tokio::spawn(serve(ctx.clone(), l));
        let e = env(&ctx, "lion");
        assert!(e.iter().any(|(k, v)| k == "HTTPS_PROXY" && v.contains("lion:")));
        assert!(e.iter().any(|(k, _)| k == "TMPDIR"));
        let proxy = ctx.egress_url.get().unwrap().clone();
        let proxy_addr = proxy.trim_start_matches("http://").to_string();
        let tok = agent_token("lion");
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("lion:{tok}"));

        let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = up.accept().await {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi").await;
            }
        });
        let req = |host: &str, port: u16, auth: bool| {
            format!(
                "GET http://{host}:{port}/ HTTP/1.1\r\nHost: {host}\r\n{}\r\n",
                if auth { format!("Proxy-Authorization: Basic {cred}\r\n") } else { String::new() }
            )
        };
        let go = |r: String| {
            let a = proxy_addr.clone();
            async move {
                let mut s = TcpStream::connect(a).await.unwrap();
                s.write_all(r.as_bytes()).await.unwrap();
                let mut o = String::new();
                let _ = s.read_to_string(&mut o).await;
                o
            }
        };
        // denied: not on the allowlist
        let r = go(req("127.0.0.1", up_port, true)).await;
        assert!(r.starts_with("HTTP/1.1 403"), "{r}");
        // no credential
        assert!(go(req("127.0.0.1", up_port, false)).await.starts_with("HTTP/1.1 407"));
        // allowed
        ctx.db.kv_set(&format!("setting.{ALLOW_KEY}"), "127.0.0.1").unwrap();
        let r = go(req("127.0.0.1", up_port, true)).await;
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("hi"), "{r}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let rows = ctx.db.query("SELECT allowed, bytes FROM sandbox_egress WHERE agent='lion' ORDER BY ts", []).unwrap();
        assert!(rows.iter().any(|r| r["allowed"] == 0));
        assert!(rows.iter().any(|r| r["allowed"] == 1 && r["bytes"].as_i64().unwrap() > 0));
        // CONNECT denied for a non-listed host
        let r = go(format!("CONNECT evil.example:443 HTTP/1.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n")).await;
        assert!(r.starts_with("HTTP/1.1 403"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn real_sandbox_probes() {
        let ctx = Ctx::test();
        setup_db(&ctx).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _ = ctx.egress_url.set(format!("http://{}", l.local_addr().unwrap()));
        tokio::spawn(serve(ctx.clone(), l));
        let r = test_profile(&ctx).await.unwrap();
        let bad: Vec<_> = r["probes"].as_array().unwrap().iter().filter(|p| p["ok"] != true).collect();
        assert!(bad.is_empty(), "{bad:?}");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod toolchain_probe {
    use super::*;
    #[test]
    fn toolchains_run_but_home_stays_private() {
        let dir = std::env::temp_dir().join(format!("sbx-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = Policy { workdir: dir.clone(), scratch: dir.clone(), hal_dir: dir.clone(), read_only: vec![], allow_hosts: vec![] };
        let run = |cmd: &str| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile(&p), "/bin/sh", "-c", cmd])
                .env("TMPDIR", &dir)
                .output()
                .unwrap()
        };
        assert!(run("/usr/bin/python3 -V").status.success(), "python3 should run");
        if std::path::Path::new("/Applications/KiCad/KiCad.app").exists() {
            let o = run("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli version");
            assert!(o.status.success(), "kicad-cli: {}", String::from_utf8_lossy(&o.stderr));
        }
        let home = home();
        for secret in [".ssh", "Documents", "Library/Keychains"] {
            let o = run(&format!("ls '{}'", home.join(secret).display()));
            assert!(!o.status.success(), "{secret} must stay unreadable");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
