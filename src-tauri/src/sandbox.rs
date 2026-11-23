//! Strict mode: OS sandbox for agents on untrusted providers + egress proxy.
//!
//! rpc methods (`sandbox.*`):
//! - `status {}` -> `{available, error?, platform, egress_url, allow_hosts, denied_total, allowed_total}`
//! - `egress {agent?, limit?}` -> rows `{id, ts, agent, host, port, allowed, bytes}` newest first
//! - `allowlist_get {agent?}` -> `{hosts, custom}` (global list, or the agent override if set)
//! - `allowlist_set {hosts, agent?}` -> `{hosts}` (global key `strict.allow_hosts`)
//! - `test_profile {}` -> `{ok, probes:[{name, ok, detail}]}` real sandbox-exec probes (macOS)
//! - `grants_get {repo_id}` -> `{grants:[{path, write}], private:[home-relative]}`
//! - `grants_set {repo_id, grants:[{path, write?}]}` -> `{grants}` extra folders strict agents on that repo may use
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

use crate::ctx::{arg_opt_str, arg_str, Ctx};

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
const GRANTS_KEY: &str = "setting.strict.grants";

fn expand(raw: &str) -> PathBuf {
    let t = raw.trim();
    match t.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if t == "~" => home(),
        None => PathBuf::from(t),
    }
}

/// The private entry a grant would expose: a grant may sit inside a private
/// folder (e.g. ~/Documents/kicad-libs) but may not contain a secret store
/// (e.g. ~ or ~/.ssh itself) or point inside one of the credential stores.
fn private_hit(p: &Path) -> Option<&'static str> {
    let h = home();
    let personal = ["Documents", "Desktop", "Downloads", "Developer", "Pictures", "Movies", "Music", "Philium"];
    for d in PRIVATE {
        let priv_path = h.join(d);
        if priv_path.starts_with(p) {
            return Some(d);
        }
        if !personal.contains(d) && p.starts_with(&priv_path) {
            return Some(d);
        }
    }
    None
}

/// Repo grants as stored: `[{path, write}]`.
pub fn grants(ctx: &Ctx, repo_id: &str) -> Vec<Value> {
    ctx.db
        .kv_get(&format!("{GRANTS_KEY}.{repo_id}"))
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
}

/// (read_only, read_write) paths granted to agents working on `repo_id`.
pub fn grant_paths(ctx: &Ctx, repo_id: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut ro, mut rw) = (Vec::new(), Vec::new());
    for g in grants(ctx, repo_id) {
        let Some(p) = g.get("path").and_then(Value::as_str).map(expand) else { continue };
        if private_hit(&p).is_some() {
            continue;
        }
        if g.get("write").and_then(Value::as_bool).unwrap_or(false) { rw.push(p) } else { ro.push(p) }
    }
    (ro, rw)
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub workdir: PathBuf,
    pub scratch: PathBuf,
    pub hal_dir: PathBuf,
    pub read_only: Vec<PathBuf>,
    /// Extra read+write folders (the repo's git dir, writable repo grants).
    pub read_write: Vec<PathBuf>,
    /// Extra hosts allowed for this agent in the egress proxy alongside global/per-agent settings.
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

/// Every home whose private folders an agent must not see: the process HOME and, in the
/// Pilot, the account home it was started from (with its /private/var style real path).
fn private_homes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in std::iter::once(home()).chain(crate::pilot::real_home()) {
        for v in variants(&root) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

fn esc(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
}

fn regex_esc(p: &Path) -> String {
    let mut o = String::new();
    for c in p.to_string_lossy().chars() {
        if "\\.^$|?*+()[]{}\"".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
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

/// Home-relative paths an agent on an untrusted provider must never see:
/// credentials, personal data, browsers, shell history and personal folders.
/// Anything an agent reads can end up in a provider request.
pub const PRIVATE: &[&str] = &[
    // credentials
    ".ssh", ".aws", ".azure", ".kube", ".gnupg", ".docker", ".netrc", ".git-credentials", ".npmrc", ".pypirc",
    ".config/gh", ".config/gcloud", ".config/hub", ".password-store", ".terraform.d", ".vault-token",
    ".jcode", ".claude", ".codex", ".config/op",
    // shell history
    ".zsh_history", ".bash_history", ".python_history", ".node_repl_history", ".lesshst", ".zsh_sessions",
    // personal data and app data that holds sessions or messages
    "Library/Keychains", "Library/Mail", "Library/Messages", "Library/Safari", "Library/Cookies",
    "Library/Containers", "Library/Group Containers", "Library/Mobile Documents", "Library/Accounts",
    "Library/Calendars", "Library/Application Support/AddressBook", "Library/Application Support/CallHistoryDB",
    "Library/Application Support/com.apple.TCC", "Library/Application Support/Google",
    "Library/Application Support/Firefox", "Library/Application Support/BraveSoftware",
    "Library/Application Support/Arc", "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Slack", "Library/Application Support/discord",
    crate::app_id::MAC_DATA_REL,
    // personal folders (share parts of them with repo grants)
    "Documents", "Desktop", "Downloads", "Developer", "Pictures", "Movies", "Music", "Philium",
];

/// Seatbelt (SBPL) profile. Open by default: the agent can read and run
/// everything on the machine except the `PRIVATE` list, so real tooling
/// (KiCad, Xcode CLT, Homebrew, app libraries) just works. Writes are limited
/// to its worktree, scratch, caches, the repo's git dir and granted folders.
/// Later rules win, so the order is: broad allow, private deny, grants.
pub fn profile(p: &Policy) -> String {
    let h = home();
    let mut s = String::new();
    s.push_str("(version 1)\n(deny default)\n");
    s.push_str("(allow process-exec)\n(allow process-fork)\n(allow signal (target self))\n");
    s.push_str("(allow sysctl-read)\n(allow mach-lookup)\n(allow ipc-posix-shm)\n(allow file-read-metadata)\n");
    s.push_str("(allow iokit-open)\n(allow user-preference-read)\n(allow file-ioctl)\n");
    s.push_str("(allow file-read*)\n");
    s.push_str("(allow file-read* file-write* (regex #\"^/dev/(tty|pty|ttys|fd/)\"))\n");
    s.push_str("(allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\"))\n");
    // Shared temp and the per-user temp/cache folders (xcrun, clang, kicad).
    s.push_str("(allow file-read* file-write* (subpath \"/private/tmp\") (subpath \"/private/var/folders\"))\n");
    // Private: never readable or writable. The Pilot points HOME at a private folder, so the
    // account home it started from is listed too; otherwise its keys and logins stay readable.
    let homes = private_homes();
    for root in &homes {
        for d in PRIVATE {
            s.push_str(&format!("(deny file-read* file-write* (subpath \"{}\"))\n", esc(&root.join(d))));
        }
    }
    // The Pilot's database, logs and other agents' sessions. Own work areas are re-allowed below.
    if let Some(store) = crate::pilot::data_dir() {
        for v in variants(&store) {
            s.push_str(&format!("(deny file-read* file-write* (subpath \"{}\"))\n", esc(&v)));
        }
    }
    s.push_str("(deny file-read* file-write* (subpath \"/Library/Keychains\"))\n(deny file-read* file-write* (subpath \"/private/var/db/dslocal\"))\n");
    // Package caches and tool state (read/write).
    let caches: Vec<PathBuf> = [".npm", ".cargo/registry", ".cargo/git", ".cache", "Library/Caches", ".rustup", ".bun/install"]
        .iter()
        .map(|d| h.join(d))
        .collect();
    s.push_str(&subpaths("file-read* file-write*", &caches));
    // Grants: the source repo (read, plus its .git so commits work) and repo grants.
    s.push_str(&subpaths("file-read*", &p.read_only));
    s.push_str(&subpaths("file-read* file-write*", &p.read_write));
    // Own work areas, last so nothing above can take them away.
    s.push_str(&subpaths("file-read* file-write*", &[p.workdir.clone(), p.scratch.clone(), p.hal_dir.clone()]));
    // .env files under home stay private even inside grants and the worktree.
    for root in &homes {
        s.push_str(&format!("(deny file-read* file-write* (regex #\"^{}/.*/\\.env(\\.[^/]*)?$\"))\n", regex_esc(root)));
    }
    // Network: loopback only (gateway, egress proxy, MCP).
    s.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    s.push_str("(allow network-bind (local ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n");
    s.push_str(&deny_secrets());
    s
}

/// Seatbelt rule hiding Philium's secret vault folder (`secrets.rs`).
fn deny_secrets() -> String {
    // Both spellings: the sandbox matches the real path, and the folder may be reached
    // through a symlink (macOS /var, a home on another volume).
    match crate::secrets::dir() {
        Some(d) => variants(d)
            .iter()
            .map(|v| format!("(deny file-read* file-write* (subpath \"{}\"))\n", esc(v)))
            .collect(),
        None => String::new(),
    }
}

/// Profile for trusted (non-Strict) agents: everything allowed except the
/// secret vault. `None` when no sandbox is available on this OS.
pub fn trusted_profile() -> Option<String> {
    if !matches!(backend(), Backend::Seatbelt) || !Path::new("/usr/bin/sandbox-exec").exists() {
        return None;
    }
    Some(format!("(version 1)\n(allow default)\n{}", deny_secrets()))
}

/// Prefix `argv` with sandbox-exec: the Strict profile when there is a
/// policy, otherwise the trusted profile (vault hidden).
pub fn wrap_argv(policy: Option<&Policy>, mut argv: Vec<String>) -> Vec<String> {
    let prof = match policy {
        Some(p) => Some(profile(p)),
        None => trusted_profile(),
    };
    match prof {
        Some(prof) => {
            let mut w = vec!["/usr/bin/sandbox-exec".to_string(), "-p".into(), prof];
            w.append(&mut argv);
            w
        }
        None => argv,
    }
}

type WrapFn = Box<dyn Fn(&Path) -> (PathBuf, Vec<String>) + Send + Sync>;

/// Wrapper for trusted HALs: vault hidden, everything else allowed.
pub fn trusted_wrap() -> Option<WrapFn> {
    let prof = trusted_profile()?;
    Some(Box::new(move |hal: &Path| (PathBuf::from("/usr/bin/sandbox-exec"), vec!["-p".into(), prof.clone(), hal.to_string_lossy().into_owned()])))
}

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
    tokens().lk().entry(agent.to_string()).or_insert_with(crate::hal::random_token).clone()
}

fn policy_hosts() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static H: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Register `Policy.allow_hosts` for `agent` so the egress proxy honors them
/// alongside global `strict.allow_hosts` and per-agent allowlist overrides.
pub fn register_policy(agent: &str, policy: &Policy) {
    let hosts: Vec<String> = policy.allow_hosts.iter().flat_map(|s| parse_hosts(s)).collect();
    if hosts.is_empty() {
        policy_hosts().lk().remove(agent);
    } else {
        policy_hosts().lk().insert(agent.to_string(), hosts);
    }
}

/// Drop the agent's proxy credential and policy host grants (kill switch / teardown).
pub fn revoke(agent: &str) {
    tokens().lk().remove(agent);
    policy_hosts().lk().remove(agent);
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

type AllowCache = HashMap<(PathBuf, Option<String>), (Vec<String>, bool)>;

fn allowlist_cache() -> &'static std::sync::RwLock<AllowCache> {
    static C: OnceLock<std::sync::RwLock<AllowCache>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn invalidate_allowlist_cache() {
    allowlist_cache().wr().clear();
}

fn parse_hosts(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(|h| h.trim().trim_start_matches("*.").to_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

pub fn allowlist(ctx: &Ctx, agent: Option<&str>) -> (Vec<String>, bool) {
    let key = (ctx.data_dir.clone(), agent.map(str::to_string));
    if let Some(hit) = allowlist_cache().rd().get(&key).cloned() {
        return hit;
    }
    let val = 'compute: {
        if let Some(a) = agent {
            if let Some(v) = ctx.db.kv_get(&format!("setting.{ALLOW_KEY}.{a}")) {
                break 'compute (parse_hosts(&v), true);
            }
        }
        match ctx.db.kv_get(&format!("setting.{ALLOW_KEY}")) {
            Some(v) => (parse_hosts(&v), false),
            None => (DEFAULT_ALLOW.iter().map(|s| s.to_string()).collect(), false),
        }
    };
    allowlist_cache().wr().insert(key, val.clone());
    val
}

/// Effective host allowlist for an agent: settings allowlist (per-agent override or global)
/// unioned with any hosts registered from its `Policy.allow_hosts`.
pub fn effective_allowlist(ctx: &Ctx, agent: &str) -> Vec<String> {
    let (mut list, _) = allowlist(ctx, Some(agent));
    if let Some(extra) = policy_hosts().lk().get(agent) {
        for h in extra {
            if !list.contains(h) {
                list.push(h.clone());
            }
        }
    }
    list
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
    let ok = tokens().lk().get(user).map(|t| t == pass).unwrap_or(false);
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
    let list = effective_allowlist(&ctx, &agent);
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
    let pol = Policy { workdir: work.clone(), scratch, hal_dir: hal, read_only: vec![], read_write: vec![], allow_hosts: vec![] };
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
    invalidate_allowlist_cache();
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
    invalidate_allowlist_cache();
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
            invalidate_allowlist_cache();
            Ok(json!({"hosts": hosts}))
        }
        "test_profile" => test_profile(ctx).await,
        "grants_get" => Ok(json!({"grants": grants(ctx, &arg_str(&args, "repo_id")?), "private": PRIVATE})),
        "grants_set" => {
            let repo_id = arg_str(&args, "repo_id")?;
            let list = args.get("grants").and_then(Value::as_array).ok_or("missing `grants`")?;
            let mut out = Vec::new();
            for g in list {
                let raw = g.get("path").and_then(Value::as_str).ok_or("grant needs a path")?;
                let path = expand(raw);
                if !path.is_absolute() {
                    return Err(format!("Use a full path: {raw}"));
                }
                if !path.exists() {
                    return Err(format!("No folder at {}", path.display()));
                }
                if let Some(p) = private_hit(&path) {
                    return Err(format!("{} holds private data ({p}) and cannot be shared", path.display()));
                }
                out.push(json!({"path": path.to_string_lossy(), "write": g.get("write").and_then(Value::as_bool).unwrap_or(false)}));
            }
            ctx.db.kv_set(&format!("{GRANTS_KEY}.{repo_id}"), &Value::Array(out.clone()).to_string())?;
            Ok(json!({"grants": out}))
        }
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
            read_write: vec![],
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
    async fn allowlist_cache_invalidates_on_set() {
        let ctx = Ctx::test();
        let (hosts, custom) = allowlist(&ctx, Some("lion"));
        assert!(!custom);
        assert!(hosts.contains(&"github.com".to_string()));
        call(&ctx, "allowlist_set", json!({"hosts": "custom.example.org", "agent": "lion"}))
            .await
            .unwrap();
        let (hosts2, custom2) = allowlist(&ctx, Some("lion"));
        assert!(custom2);
        assert_eq!(hosts2, vec!["custom.example.org".to_string()]);
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
        invalidate_allowlist_cache();
        let r = go(req("127.0.0.1", up_port, true)).await;
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("hi"), "{r}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let rows = ctx.db.query("SELECT allowed, bytes FROM sandbox_egress WHERE agent='lion' ORDER BY ts", []).unwrap();
        assert!(rows.iter().any(|r| r["allowed"] == 0));
        assert!(rows.iter().any(|r| r["allowed"] == 1 && r["bytes"].as_i64().unwrap() > 0));
        // CONNECT denied for a non-listed host
        let r = go(format!("CONNECT evil.example:443 HTTP/1.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n")).await;
        assert!(r.starts_with("HTTP/1.1 403"));
        revoke("lion");
    }

    #[tokio::test]
    async fn policy_allow_hosts_honored_by_egress_proxy() {
        let ctx = Ctx::test();
        setup_db(&ctx).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _ = ctx.egress_url.set(format!("http://{}", l.local_addr().unwrap()));
        tokio::spawn(serve(ctx.clone(), l));
        let agent = "policy-host-agent";
        let _ = env(&ctx, agent);
        let proxy_addr = ctx.egress_url.get().unwrap().trim_start_matches("http://").to_string();
        let tok = agent_token(agent);
        let cred = base64::engine::general_purpose::STANDARD.encode(format!("{agent}:{tok}"));

        let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = up.accept().await {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
            }
        });
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
        let req = format!("GET http://127.0.0.1:{up_port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n");
        // Global allowlist does not include 127.0.0.1: denied before Policy.allow_hosts is registered.
        assert!(go(req.clone()).await.starts_with("HTTP/1.1 403"));
        let mut p = pol(Path::new("/tmp/policy-test"));
        p.allow_hosts = vec!["127.0.0.1".into(), "*.policy.example".into()];
        register_policy(agent, &p);
        let eff = effective_allowlist(&ctx, agent);
        assert!(host_allowed(&eff, "127.0.0.1"));
        assert!(host_allowed(&eff, "api.policy.example"));
        // Proxy now allows 127.0.0.1 via Policy.allow_hosts while still denying unlisted hosts.
        let ok = go(req).await;
        assert!(ok.starts_with("HTTP/1.1 200") && ok.ends_with("ok"), "{ok}");
        let denied = go(format!("CONNECT evil.example:443 HTTP/1.1\r\nProxy-Authorization: Basic {cred}\r\n\r\n")).await;
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
        revoke(agent);
        assert!(!host_allowed(&effective_allowlist(&ctx, agent), "api.policy.example"));
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
        // A granted folder inside Documents (like a KiCad library) and a .env inside it.
        let grant = home().join("Documents").join(format!(".philium-grant-probe-{}", std::process::id()));
        std::fs::create_dir_all(&grant).unwrap();
        std::fs::write(grant.join("lib.kicad_sym"), "ok").unwrap();
        std::fs::write(grant.join(".env"), "SECRET=1").unwrap();
        let p = Policy { workdir: dir.clone(), scratch: dir.clone(), hal_dir: dir.clone(), read_only: vec![grant.clone()], read_write: vec![], allow_hosts: vec![] };
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
        for secret in [".ssh", "Documents", "Library/Keychains", "Library/Messages", ".zsh_history", crate::app_id::MAC_DATA_REL] {
            let path = home.join(secret);
            let cmd = if path.is_dir() { format!("ls '{}'", path.display()) } else { format!("cat '{}' >/dev/null", path.display()) };
            let o = run(&cmd);
            assert!(!o.status.success(), "{secret} must stay unreadable");
        }
        // Open by default: app support of tools (not on the private list) and /Applications read fine.
        assert!(run("ls /Applications >/dev/null").status.success());
        // Grants: readable, read-only, .env still hidden.
        assert!(run(&format!("cat '{}'", grant.join("lib.kicad_sym").display())).status.success(), "grant readable");
        assert!(!run(&format!("touch '{}'", grant.join("new").display())).status.success(), "read-only grant not writable");
        assert!(!run(&format!("cat '{}'", grant.join(".env").display())).status.success(), ".env hidden");
        // Writes outside work areas fail; inside succeed.
        assert!(!run(&format!("touch '{}'", home.join(".philium-probe-write").display())).status.success());
        assert!(run(&format!("touch '{}'", dir.join("x").display())).status.success());
        let _ = std::fs::remove_dir_all(&grant);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grant_validation() {
        let h = home();
        assert!(private_hit(&h).is_some(), "whole home exposes secrets");
        assert!(private_hit(&h.join(".ssh/keys")).is_some());
        assert!(private_hit(&h.join("Library")).is_some());
        assert!(private_hit(&h.join("Documents/kicad-libs")).is_none());
        assert!(private_hit(&h.join("Documents")).is_some());
        assert!(private_hit(Path::new("/opt/kicad")).is_none());
        assert_eq!(expand("~/x"), h.join("x"));
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
