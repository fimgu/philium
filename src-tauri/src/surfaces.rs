//! Embedded App Surfaces & Zero-Interference Host Guard.
//!
//! Two responsibilities:
//! 1. Zero-Interference Shims (`<data_dir>/bin/open` and `xdg-open`): placed first on every agent's
//!    `PATH` and exported as `BROWSER`. Stops shell commands (`open https://...`, `vite --open`,
//!    `gh auth`, `open -a KiCad`) from ever popping open host Firefox or stealing macOS desktop focus.
//! 2. Isolated App Surfaces (KiCad, DBeaver, Chromium, Wireshark, FreeCAD, custom containers):
//!    Detects the best available single Linux container runtime (OrbStack > Colima > Docker > Lima),
//!    launches single-window Xpra/noVNC/Selkies containers bound strictly to `127.0.0.1` + the active
//!    workspace/worktree + a persistent per-app config volume, and registers every container with
//!    Philium's Reaper so teardown is guaranteed.
//!
//! rpc (`surfaces.*`):
//!   status {} -> {runtime, engines, shim_active, shim_path, pilot, running}
//!   catalog {} -> [recipe]
//!   list {repo_id?, agent_id?} -> [surface]
//!   add_recipe {id, name, category, image, command?, description?, internal_port?, protocol?, headless_cli?} -> recipe
//!   remove_recipe {id} -> null
//!   launch {recipe_id, repo_id?, agent_id?, workdir?} -> surface
//!   probe {id} -> {id, ready, port?, state}
//!   stop {id} -> null
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::{json, Value};

use crate::ctx::{arg_opt_str, arg_str, Ctx, LockExt};
use crate::db::{new_id, now};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_surface_recipes (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  category TEXT NOT NULL,
  description TEXT NOT NULL,
  image TEXT NOT NULL,
  command TEXT NOT NULL DEFAULT '',
  internal_port INTEGER NOT NULL DEFAULT 10000,
  protocol TEXT NOT NULL DEFAULT 'xpra',
  headless_cli TEXT NOT NULL DEFAULT '',
  builtin INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_surfaces (
  id TEXT PRIMARY KEY,
  recipe_id TEXT NOT NULL,
  name TEXT NOT NULL,
  repo_id TEXT,
  agent_id TEXT,
  workdir TEXT,
  runtime TEXT NOT NULL,
  container_name TEXT NOT NULL,
  container_id TEXT,
  host_port INTEGER NOT NULL,
  url TEXT NOT NULL,
  state TEXT NOT NULL,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
";

const OPEN_SHIM: &str = r#"#!/bin/sh
# Philium Zero-Interference Guard: prevents agent shell commands from opening
# host Firefox, Safari, or macOS desktop apps on the user's screen.
set -eu
agent="${PHILIUM_AGENT:-agent}"
first=""
for arg in "$@"; do
  case "$arg" in
    -*) ;;
    *) first="$arg"; break ;;
  esac
done
case "$first" in
  http://*|https://*)
    printf "Philium Zero-Interference: blocked opening %s in host browser (agent: %s).\nUse your isolated Philium browser tool (browser_open) instead so you never touch the user's Firefox or desktop.\n" "$first" "$agent" >&2
    exit 0
    ;;
  *)
    printf "Philium Zero-Interference: blocked launching host GUI app/file '%s' (agent: %s).\nRun tools headlessly or open an isolated Philium App Surface (e.g. KiCad, DBeaver) instead.\n" "${first:-$*}" "$agent" >&2
    exit 1
    ;;
esac
"#;

const EXEC_MAX_BYTES: usize = 64 * 1024;
const SCREENSHOT_MAX_BYTES: usize = 20 * 1024 * 1024;
const SCREENSHOT_TIMEOUT_SECS: u64 = 15;
const RECONCILE_TTL: Duration = Duration::from_secs(4);
const RECONCILE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

struct LiveCache {
    fetched_at: Instant,
    containers: Option<HashSet<String>>,
}

fn live_cache() -> &'static Mutex<Option<LiveCache>> {
    static CACHE: OnceLock<Mutex<Option<LiveCache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn invalidate_live_cache() {
    *live_cache().lk() = None;
}

pub fn shim_bin_dir(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("bin")
}

pub fn open_shim_path(ctx: &Ctx) -> PathBuf {
    shim_bin_dir(ctx).join("open")
}

/// Write executable `open` and `xdg-open` zero-interference shims into `<data_dir>/bin`.
const GH_SHIM: &str = r#"#!/bin/sh
# Philium gh shim: uses the repository's configured GitHub account (PHILIUM_GH_USER)
# without exposing tokens in the shell environment or requiring `gh auth switch`.
SELF_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REAL_GH=""
OLD_IFS="$IFS"
IFS=:
for d in $PATH; do
  [ -z "$d" ] && continue
  [ "$d" = "$SELF_DIR" ] && continue
  if [ -x "$d/gh" ] && [ ! -d "$d/gh" ]; then
    REAL_GH="$d/gh"
    break
  fi
done
IFS="$OLD_IFS"
if [ -z "$REAL_GH" ]; then
  echo "gh: command not found" >&2
  exit 127
fi
if [ -n "${PHILIUM_GH_USER:-}" ] && [ -z "${GH_TOKEN:-}" ] && [ -z "${GITHUB_TOKEN:-}" ] && [ "${1:-}" != "auth" ]; then
  TOK="$("$REAL_GH" auth token --hostname github.com --user "$PHILIUM_GH_USER" 2>/dev/null || true)"
  if [ -n "$TOK" ]; then
    GH_TOKEN="$TOK" exec "$REAL_GH" "$@"
  fi
fi
exec "$REAL_GH" "$@"
"#;

pub fn ensure_shims(ctx: &Ctx) -> Result<PathBuf, String> {
    let dir = shim_bin_dir(ctx);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create shim dir: {e}"))?;
    for (name, body) in [("open", OPEN_SHIM), ("xdg-open", OPEN_SHIM), ("gh", GH_SHIM)] {
        let p = dir.join(name);
        let needs_write = std::fs::read_to_string(&p).map(|s| s != body).unwrap_or(true);
        if needs_write {
            std::fs::write(&p, body).map_err(|e| format!("write {name} shim: {e}"))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(dir.join("open"))
}

fn seed_builtin_recipes(ctx: &Ctx) -> Result<(), String> {
    let builtins: &[(&str, &str, &str, &str, &str, &str, i64, &str, &str)] = &[
        ("kicad", "KiCad", "hardware", "PCB & schematics", "lscr.io/linuxserver/kicad:latest", "", 3000, "selkies", "kicad-cli"),
        ("dbeaver", "DBeaver", "database", "SQL & databases", "lscr.io/linuxserver/dbeaver:latest", "", 3000, "selkies", ""),
        ("chromium", "Chromium", "browser", "Sandboxed browser", "lscr.io/linuxserver/chromium:latest", "", 3000, "selkies", ""),
        ("wireshark", "Wireshark", "network", "Packet capture", "lscr.io/linuxserver/wireshark:latest", "", 3000, "selkies", "tshark"),
        ("freecad", "FreeCAD", "hardware", "3D CAD", "lscr.io/linuxserver/freecad:latest", "", 3000, "selkies", "freecadcmd"),
    ];
    let ts = now();
    for (id, name, cat, desc, image, cmd, port, proto, cli) in builtins {
        ctx.db.exec(
            "INSERT INTO app_surface_recipes(id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)
             ON CONFLICT(id) DO NOTHING",
            params![id, name, cat, desc, image, cmd, port, proto, cli, ts],
        )?;
    }
    Ok(())
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    seed_builtin_recipes(ctx)?;
    let _ = ensure_shims(ctx);
    let has_running = ctx
        .db
        .query("SELECT 1 FROM app_surfaces WHERE state='running' LIMIT 1", [])
        .map(|r| !r.is_empty())
        .unwrap_or(false);
    if has_running && !crate::pilot::active() {
        let weak = Arc::downgrade(ctx);
        let _ = std::thread::Builder::new()
            .name("philium-surface-reconcile".into())
            .spawn(move || {
                let live = list_live_containers();
                *live_cache().lk() = Some(LiveCache {
                    fetched_at: Instant::now(),
                    containers: live.clone(),
                });
                if let (Some(live), Some(c)) = (live, weak.upgrade()) {
                    let _ = reconcile_running_rows(&c, &live);
                }
            });
    }
    Ok(())
}

fn find_bin(names: &[&str]) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.insert(0, home.join(".orbstack").join("bin"));
    }
    for name in names {
        for dir in &candidates {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        if let Ok(path) = std::env::var("PATH") {
            for dir in path.split(':') {
                let p = Path::new(dir).join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn detect_engines() -> Value {
    let orb = find_bin(&["orb", "orbctl"]).is_some();
    let colima = find_bin(&["colima"]).is_some();
    let docker = find_bin(&["docker"]).is_some();
    let lima = find_bin(&["limactl"]).is_some();
    let recommended = if orb {
        "orbstack"
    } else if colima {
        "colima"
    } else if docker {
        "docker"
    } else if lima {
        "lima"
    } else {
        "none"
    };
    json!({
        "orbstack": orb,
        "colima": colima,
        "docker": docker,
        "lima": lima,
        "recommended": recommended,
        "docker_ready": docker,
    })
}

fn pick_free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("allocate port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok(port)
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn safe_path_segment(s: &str) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if clean.trim_matches('_').is_empty() {
        "unknown".into()
    } else {
        clean
    }
}

fn is_forbidden_host_mount(path: &Path) -> bool {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    const FORBIDDEN: &[&str] = &[
        "/",
        "/Users",
        "/home",
        "/etc",
        "/private/etc",
        "/var",
        "/private/var",
        "/private",
        "/System",
        "/Library",
        "/Applications",
        "/Volumes",
        "/usr",
        "/bin",
        "/sbin",
        "/opt",
        "/root",
        "/tmp",
        "/private/tmp",
    ];
    if FORBIDDEN.iter().any(|p| canon == Path::new(p)) {
        return true;
    }
    if let Some(home) = crate::pilot::real_home().or_else(|| std::env::var_os("HOME").map(PathBuf::from)) {
        let canon_home = home.canonicalize().unwrap_or(home);
        if canon == canon_home {
            return true;
        }
    }
    false
}

fn build_docker_run_args(
    container_name: &str,
    surface_id: &str,
    agent_id: Option<&str>,
    host_port: u16,
    internal_port: i64,
    workdir: Option<&str>,
    config_dir: &Path,
    image: &str,
    command: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "--label".into(),
        format!("philium.surface={surface_id}"),
        "--label".into(),
        format!("philium.agent={}", agent_id.unwrap_or("workspace")),
        "-p".into(),
        format!("127.0.0.1:{host_port}:{internal_port}"),
        "-e".into(),
        "PUID=1000".into(),
        "-e".into(),
        "PGID=1000".into(),
        "-e".into(),
        "TZ=UTC".into(),
        "-v".into(),
        format!("{}:/config", config_dir.display()),
    ];
    if let Some(wd) = workdir.filter(|w| !w.trim().is_empty()) {
        args.push("-v".into());
        args.push(format!("{wd}:/workspace"));
    }
    args.push(image.into());
    if !command.trim().is_empty() {
        args.push("/bin/sh".into());
        args.push("-c".into());
        args.push(command.trim().into());
    }
    args
}

fn resolve_workdir(ctx: &Arc<Ctx>, repo_id: Option<&str>, agent_id: Option<&str>, explicit: Option<&str>) -> Option<String> {
    if let Some(w) = explicit.filter(|s| !s.trim().is_empty()) {
        return Some(w.to_string());
    }
    if let Some(aid) = agent_id.filter(|s| !s.is_empty()) {
        if let Some(rt) = crate::agents::runtime(aid) {
            return Some(rt.cwd.to_string_lossy().into_owned());
        }
        if let Ok(rows) = ctx.db.query("SELECT path FROM worktrees WHERE agent_id=?1 AND state='active' ORDER BY created_at DESC LIMIT 1", [aid]) {
            if let Some(p) = rows.first().and_then(|r| r["path"].as_str()) {
                return Some(p.to_string());
            }
        }
    }
    if let Some(rid) = repo_id.filter(|s| !s.is_empty()) {
        if let Some(r) = crate::workspace::repo(ctx, rid) {
            if let Some(p) = r["path"].as_str() {
                return Some(p.to_string());
            }
        }
    }
    None
}

fn agent_allowed_roots(ctx: &Ctx, agent_id: &str, agent_name: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push_unique = |p: PathBuf| {
        if !roots.iter().any(|r| r == &p) {
            roots.push(p);
        }
    };
    if let Some(rt) = crate::agents::runtime(agent_id) {
        push_unique(rt.cwd.clone());
        if let Some(policy) = &rt.policy {
            push_unique(policy.workdir.clone());
            push_unique(policy.scratch.join("scratch"));
        }
    }
    if let Ok(rows) = ctx.db.query(
        "SELECT path FROM worktrees WHERE agent_id=?1 AND state='active' ORDER BY created_at DESC LIMIT 1",
        [agent_id],
    ) {
        if let Some(p) = rows.first().and_then(|r| r["path"].as_str()) {
            push_unique(PathBuf::from(p));
        }
    }
    let resolved_name = crate::agents::get_row(ctx, agent_id)
        .and_then(|r| r["name"].as_str().map(str::to_string))
        .filter(|n| !n.is_empty())
        .or_else(|| (!agent_name.trim().is_empty()).then(|| agent_name.trim().to_string()));
    if let Some(name) = resolved_name {
        push_unique(ctx.agent_dir(&name).join("scratch"));
    }
    roots
}

/// Resolve and validate `workdir` for MCP `surface_launch`.
/// Strict agents may ONLY mount their own worktree or scratch directory (or a subdirectory inside it),
/// and host-wide mounts are forbidden for every caller.
fn resolve_mcp_workdir(
    ctx: &Arc<Ctx>,
    agent_id: &str,
    agent_name: &str,
    repo_id: Option<&str>,
    strict: bool,
    explicit: Option<&str>,
) -> Result<Option<String>, String> {
    let explicit = explicit.map(str::trim).filter(|s| !s.is_empty());
    if strict {
        let roots = agent_allowed_roots(ctx, agent_id, agent_name);
        if roots.is_empty() {
            return Err("Strict agent has no worktree or scratch directory available to mount".into());
        }
        let mut canon_roots: Vec<PathBuf> = roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .filter(|r| r.is_dir() && !is_forbidden_host_mount(r))
            .collect();
        if canon_roots.is_empty() {
            // Create the agent's own scratch directory if no worktree/scratch exists on disk yet.
            if let Some(fallback) = roots.last() {
                std::fs::create_dir_all(fallback).map_err(|e| format!("create agent scratch dir: {e}"))?;
                let c = fallback.canonicalize().map_err(|e| format!("resolve scratch dir: {e}"))?;
                if is_forbidden_host_mount(&c) {
                    return Err("Refusing to mount a host-wide directory".into());
                }
                canon_roots.push(c);
            }
        }
        let primary = canon_roots
            .first()
            .cloned()
            .ok_or_else(|| "Strict agent has no valid worktree or scratch directory".to_string())?;
        let Some(req) = explicit else {
            return Ok(Some(primary.to_string_lossy().into_owned()));
        };
        let candidate = if Path::new(req).is_absolute() {
            PathBuf::from(req)
        } else {
            primary.join(req)
        };
        if is_forbidden_host_mount(&candidate) {
            return Err("Strict agents cannot mount host-wide directories".into());
        }
        let canon = candidate
            .canonicalize()
            .map_err(|_| "Strict agents can only mount an existing directory inside their own worktree or scratch directory".to_string())?;
        if !canon.is_dir() {
            return Err("workdir must be an existing directory".into());
        }
        if is_forbidden_host_mount(&canon) {
            return Err("Strict agents cannot mount host-wide directories".into());
        }
        if !canon_roots.iter().any(|root| canon.starts_with(root)) {
            return Err("Strict agents can only mount their own worktree or scratch directory".into());
        }
        return Ok(Some(canon.to_string_lossy().into_owned()));
    }

    if let Some(req) = explicit {
        let p = Path::new(req);
        if is_forbidden_host_mount(p) {
            return Err("Refusing to mount a host-wide directory".into());
        }
        return Ok(Some(req.to_string()));
    }
    let wd = resolve_workdir(ctx, repo_id, Some(agent_id), None);
    if let Some(ref w) = wd {
        if is_forbidden_host_mount(Path::new(w)) {
            return Err("Refusing to mount a host-wide directory".into());
        }
    }
    Ok(wd)
}

#[derive(Debug, PartialEq)]
struct BoundedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: i32,
    truncated: bool,
}

fn read_bounded<R: Read>(mut reader: R, max_bytes: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let room = max_bytes.saturating_sub(out.len());
                if room > 0 {
                    out.extend_from_slice(&buf[..room.min(n)]);
                }
                if n > room {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (out, truncated)
}

fn run_bounded_command(
    program: &Path,
    args: &[String],
    timeout: Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Result<BoundedOutput, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", program.display()))?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        stdout_pipe
            .map(|r| read_bounded(r, max_stdout))
            .unwrap_or_default()
    });
    let stderr_handle = std::thread::spawn(move || {
        stderr_pipe
            .map(|r| read_bounded(r, max_stderr))
            .unwrap_or_default()
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!("command timed out after {}s", timeout.as_secs()));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!("wait failed: {e}"));
            }
        }
    };

    let (stdout, out_trunc) = stdout_handle.join().unwrap_or_default();
    let (stderr, err_trunc) = stderr_handle.join().unwrap_or_default();
    Ok(BoundedOutput {
        stdout,
        stderr,
        exit_code: status.code().unwrap_or(-1),
        truncated: out_trunc || err_trunc,
    })
}

fn parse_live_names(stdout: &[u8]) -> HashSet<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Probe the available local container runtime for live `philium.surface` containers.
/// Returns `Some(names)` when a runtime CLI succeeded, or `None` when no runtime responded.
fn list_live_containers() -> Option<HashSet<String>> {
    if let Some(docker) = find_bin(&["docker"]) {
        let args: Vec<String> = vec![
            "ps".into(),
            "--filter".into(),
            "label=philium.surface".into(),
            "--format".into(),
            "{{.Names}}".into(),
        ];
        if let Ok(out) = run_bounded_command(&docker, &args, RECONCILE_PROBE_TIMEOUT, 256 * 1024, 16 * 1024) {
            if out.exit_code == 0 {
                return Some(parse_live_names(&out.stdout));
            }
        }
    }
    if let Some(colima) = find_bin(&["colima"]) {
        let args: Vec<String> = vec![
            "nerdctl".into(),
            "--".into(),
            "ps".into(),
            "--filter".into(),
            "label=philium.surface".into(),
            "--format".into(),
            "{{.Names}}".into(),
        ];
        if let Ok(out) = run_bounded_command(&colima, &args, RECONCILE_PROBE_TIMEOUT, 256 * 1024, 16 * 1024) {
            if out.exit_code == 0 {
                return Some(parse_live_names(&out.stdout));
            }
        }
    }
    if let Some(limactl) = find_bin(&["limactl"]) {
        let args: Vec<String> = vec![
            "shell".into(),
            "default".into(),
            "nerdctl".into(),
            "ps".into(),
            "--filter".into(),
            "label=philium.surface".into(),
            "--format".into(),
            "{{.Names}}".into(),
        ];
        if let Ok(out) = run_bounded_command(&limactl, &args, RECONCILE_PROBE_TIMEOUT, 256 * 1024, 16 * 1024) {
            if out.exit_code == 0 {
                return Some(parse_live_names(&out.stdout));
            }
        }
    }
    None
}

fn release_reaper_handles(ctx: &Arc<Ctx>, container_name: &str) {
    if container_name.is_empty() {
        return;
    }
    if let Ok(res_rows) = ctx.db.query(
        "SELECT id FROM resources WHERE kind='container' AND handle=?1 AND state='live'",
        [container_name],
    ) {
        for r in res_rows {
            if let Some(rid) = r["id"].as_str() {
                crate::reaper::release(ctx, rid);
            }
        }
    }
}

/// Mark any `'running'` `app_surfaces` rows whose container is not in `live_containers`
/// as `'stopped'` and release their stale reaper entries.
fn reconcile_running_rows(ctx: &Arc<Ctx>, live_containers: &HashSet<String>) -> Result<Vec<String>, String> {
    let rows = ctx.db.query(
        "SELECT id, container_name, container_id FROM app_surfaces WHERE state='running'",
        [],
    )?;
    let mut stopped = Vec::new();
    let ts = now();
    for row in rows {
        let id = row["id"].as_str().unwrap_or("").to_string();
        let cname = row["container_name"].as_str().unwrap_or("");
        let cid = row["container_id"].as_str().unwrap_or("");
        let alive = (!cname.is_empty() && live_containers.contains(cname))
            || (!cid.is_empty() && live_containers.contains(cid));
        if !alive && !id.is_empty() {
            ctx.db.exec(
                "UPDATE app_surfaces SET state='stopped', updated_at=?2 WHERE id=?1 AND state='running'",
                params![id, ts],
            )?;
            release_reaper_handles(ctx, cname);
            ctx.emit("surfaces:changed", json!({ "id": id, "state": "stopped" }));
            stopped.push(id);
        }
    }
    Ok(stopped)
}

async fn reconcile_if_needed(ctx: &Arc<Ctx>) {
    if crate::pilot::active() {
        return;
    }
    let has_running = ctx
        .db
        .query("SELECT 1 FROM app_surfaces WHERE state='running' LIMIT 1", [])
        .map(|r| !r.is_empty())
        .unwrap_or(false);
    if !has_running {
        return;
    }
    let cached = {
        let guard = live_cache().lk();
        guard
            .as_ref()
            .filter(|c| c.fetched_at.elapsed() < RECONCILE_TTL)
            .map(|c| c.containers.clone())
    };
    let live = match cached {
        Some(v) => v,
        None => {
            let probed = tokio::task::spawn_blocking(list_live_containers)
                .await
                .ok()
                .flatten();
            *live_cache().lk() = Some(LiveCache {
                fetched_at: Instant::now(),
                containers: probed.clone(),
            });
            probed
        }
    };
    if let Some(live) = live {
        let _ = reconcile_running_rows(ctx, &live);
    }
}

async fn launch_surface(
    ctx: &Arc<Ctx>,
    recipe_id: &str,
    repo_id: Option<String>,
    agent_id: Option<String>,
    workdir: Option<String>,
    strict: bool,
) -> Result<Value, String> {
    if crate::pilot::active() {
        return Err("Container launches are disabled in the side-by-side pilot".into());
    }
    if let Some(ref wd) = workdir {
        if is_forbidden_host_mount(Path::new(wd)) {
            return Err("Refusing to mount a host-wide directory".into());
        }
    }
    let recipe = ctx
        .db
        .query(
            "SELECT id,name,image,command,internal_port FROM app_surface_recipes WHERE id=?1",
            [recipe_id],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| format!("unknown app surface recipe: {recipe_id}"))?;
    let docker = find_bin(&["docker"]).ok_or_else(|| {
        "No container runtime found. Install OrbStack (recommended for 16 GiB Macs) or Colima to run isolated Linux app surfaces inside Philium.".to_string()
    })?;
    let engines = detect_engines();
    let runtime = engines["recommended"].as_str().unwrap_or("docker").to_string();

    let id = format!("surf_{}", new_id());
    let short = id.chars().take(12).collect::<String>();
    let container_name = format!("philium-surface-{recipe_id}-{short}");
    let host_port = pick_free_loopback_port()?;
    let internal_port = recipe["internal_port"].as_i64().unwrap_or(3000);
    let url = format!("http://127.0.0.1:{host_port}/");
    let config_dir = if strict {
        let owner = safe_path_segment(agent_id.as_deref().unwrap_or("strict"));
        ctx.data_dir.join("surface-configs").join("strict").join(owner).join(recipe_id)
    } else {
        ctx.data_dir.join("surface-configs").join(recipe_id)
    };
    std::fs::create_dir_all(&config_dir).map_err(|e| format!("create surface config dir: {e}"))?;

    let run_args = build_docker_run_args(
        &container_name,
        &id,
        agent_id.as_deref(),
        host_port,
        internal_port,
        workdir.as_deref(),
        &config_dir,
        recipe["image"].as_str().unwrap_or(""),
        recipe["command"].as_str().unwrap_or(""),
    );

    let docker_bin = docker.clone();
    let out = tokio::task::spawn_blocking(move || Command::new(docker_bin).args(&run_args).stdin(Stdio::null()).output())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("launch container: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "Container runtime failed to start surface. Ensure OrbStack or Colima is running.".into()
        } else {
            err
        });
    }
    let container_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let ts = now();
    let name = recipe["name"].as_str().unwrap_or(recipe_id).to_string();
    ctx.db.exec(
        "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'running',?12,?12)",
        params![id, recipe_id, name, repo_id, agent_id, workdir, runtime, container_name, container_id, host_port as i64, url, ts],
    )?;
    invalidate_live_cache();
    let _ = crate::reaper::register(
        ctx,
        agent_id.as_deref(),
        "container",
        &container_name,
        json!({ "surface_id": id, "recipe_id": recipe_id, "port": host_port }),
    );
    ctx.emit("surfaces:changed", json!({ "id": id, "state": "running" }));
    Ok(json!({
        "id": id,
        "recipe_id": recipe_id,
        "name": name,
        "url": url,
        "host_port": host_port,
        "container_name": container_name,
        "workdir": workdir,
        "runtime": runtime,
        "state": "running",
    }))
}

async fn stop_surface_by_row(ctx: &Arc<Ctx>, id: &str, cname: &str) -> Result<(), String> {
    if crate::pilot::active() {
        return Err("Container teardown is disabled in the side-by-side pilot".into());
    }
    if let Some(docker) = find_bin(&["docker"]) {
        if !cname.is_empty() {
            let c = cname.to_string();
            let _ = tokio::task::spawn_blocking(move || Command::new(docker).args(["rm", "-f", &c]).stdin(Stdio::null()).output()).await;
        }
    }
    ctx.db.exec(
        "UPDATE app_surfaces SET state='stopped', updated_at=?2 WHERE id=?1",
        params![id, now()],
    )?;
    invalidate_live_cache();
    release_reaper_handles(ctx, cname);
    ctx.emit("surfaces:changed", json!({ "id": id, "state": "stopped" }));
    Ok(())
}

/// Stop and clean up all non-stopped app surfaces belonging to `agent_id`.
/// Marks DB rows stopped, releases reaper entries, emits `surfaces:changed`, and removes
/// containers off-thread so it never blocks Tokio and never panics in sync tests.
pub fn forget_agent(ctx: &Arc<Ctx>, agent_id: &str) {
    let Ok(rows) = ctx.db.query(
        "SELECT id, container_name FROM app_surfaces WHERE agent_id=?1 AND state != 'stopped'",
        [agent_id],
    ) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let ts = now();
    let _ = ctx.db.exec(
        "UPDATE app_surfaces SET state='stopped', updated_at=?2 WHERE agent_id=?1 AND state != 'stopped'",
        params![agent_id, ts],
    );
    invalidate_live_cache();
    let mut containers = Vec::new();
    for row in rows {
        let sid = row["id"].as_str().unwrap_or("").to_string();
        let cname = row["container_name"].as_str().unwrap_or("").to_string();
        if !cname.is_empty() {
            release_reaper_handles(ctx, &cname);
            containers.push(cname);
        }
        if !sid.is_empty() {
            ctx.emit("surfaces:changed", json!({ "id": sid, "state": "stopped" }));
        }
    }
    if !crate::pilot::active() && !containers.is_empty() {
        if let Some(docker) = find_bin(&["docker"]) {
            let _ = std::thread::Builder::new()
                .name("philium-surface-cleanup".into())
                .spawn(move || {
                    for cname in containers {
                        let _ = Command::new(&docker)
                            .args(["rm", "-f", &cname])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .output();
                    }
                });
        }
    }
}

// ---------- MCP tools ----------

fn mcp_tool(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        }
    })
}

pub fn mcp_tools() -> Vec<Value> {
    let s = json!({ "type": "string" });
    let i = json!({ "type": "integer" });
    vec![
        mcp_tool(
            "surface_catalog",
            "List available embedded app surface recipes (KiCad, DBeaver, Chromium, Wireshark, FreeCAD, and custom containers).",
            json!({}),
            &[],
        ),
        mcp_tool(
            "surface_launch",
            "Launch an isolated embedded app surface container bound to 127.0.0.1 and your workspace.",
            json!({ "recipe_id": s, "workdir": s }),
            &["recipe_id"],
        ),
        mcp_tool(
            "surface_exec",
            "Run a headless CLI or shell command inside a running surface container with bounded output and timeout (default 30s, max 120s).",
            json!({
                "id": s,
                "command": {
                    "description": "Command as an array of strings (argv) or a shell command string.",
                    "anyOf": [
                        { "type": "array", "items": { "type": "string" } },
                        { "type": "string" }
                    ]
                },
                "timeout_secs": i
            }),
            &["id", "command"],
        ),
        mcp_tool(
            "surface_screenshot",
            "Capture a PNG frame/snapshot from a running surface container and save it under <data_dir>/surfaces/<id>/.",
            json!({ "id": s }),
            &["id"],
        ),
        mcp_tool(
            "surface_stop",
            "Stop and remove a running surface container.",
            json!({ "id": s }),
            &["id"],
        ),
    ]
}

fn load_scoped_surface(ctx: &Ctx, agent: &str, strict: bool, id: &str, require_running: bool) -> Result<Value, String> {
    let row = ctx
        .db
        .query(
            "SELECT id, recipe_id, name, repo_id, agent_id, workdir, runtime, container_name, container_id, host_port, url, state
             FROM app_surfaces WHERE id=?1",
            [id],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| "surface not found".to_string())?;
    let owner_agent = row["agent_id"].as_str();
    let allowed = if owner_agent == Some(agent) {
        true
    } else if strict {
        false
    } else {
        let surface_repo = row["repo_id"].as_str().filter(|s| !s.is_empty());
        let agent_repo = crate::agents::get_row(ctx, agent)
            .and_then(|r| r["repo_id"].as_str().map(str::to_string))
            .filter(|s| !s.is_empty());
        surface_repo.is_some() && surface_repo == agent_repo.as_deref()
    };
    if !allowed {
        return Err(if strict {
            "Strict agents can only access their own surfaces".into()
        } else {
            "surface belongs to another agent or workspace".into()
        });
    }
    if require_running && row["state"].as_str() != Some("running") {
        return Err("surface is not running".into());
    }
    Ok(row)
}

fn parse_exec_command(val: &Value) -> Result<Vec<String>, String> {
    if let Some(arr) = val.as_array() {
        if arr.is_empty() {
            return Err("`command` array cannot be empty".into());
        }
        let mut argv = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item
                .as_str()
                .ok_or_else(|| "`command` array elements must be strings".to_string())?;
            if s.contains('\0') {
                return Err("`command` cannot contain null bytes".into());
            }
            argv.push(s.to_string());
        }
        if argv[0].trim().is_empty() {
            return Err("`command` executable cannot be empty".into());
        }
        return Ok(argv);
    }
    if let Some(s) = val.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err("`command` cannot be empty".into());
        }
        if trimmed.contains('\0') {
            return Err("`command` cannot contain null bytes".into());
        }
        return Ok(vec!["/bin/sh".into(), "-c".into(), trimmed.into()]);
    }
    Err("missing or invalid `command` (expected string or array of strings)".into())
}

const SCREENSHOT_SCRIPT: &str = r#"
d="${DISPLAY:-}"
if [ -z "$d" ]; then
  for sock in /tmp/.X11-unix/X*; do
    [ -e "$sock" ] && d=":${sock##*/X}" && break
  done
  d="${d:-:1}"
fi
export DISPLAY="$d"
if command -v import >/dev/null 2>&1; then
  exec import -window root png:-
elif command -v scrot >/dev/null 2>&1; then
  tmp="$(mktemp /tmp/philium-shot-XXXXXX.png)"
  trap 'rm -f "$tmp"' EXIT
  scrot -o "$tmp" >/dev/null 2>&1 && cat "$tmp"
elif command -v ffmpeg >/dev/null 2>&1; then
  exec ffmpeg -loglevel error -f x11grab -i "$DISPLAY" -vframes 1 -f image2pipe -vcodec png -
elif command -v grim >/dev/null 2>&1; then
  exec grim -t png -
else
  echo "NO_CAPTURE_TOOL" >&2
  exit 127
fi
"#;

pub async fn mcp_call(
    ctx: &Arc<Ctx>,
    agent: &str,
    agent_name: &str,
    strict: bool,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    match name {
        "surface_catalog" => {
            let rows = ctx.db.query(
                "SELECT id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin
                 FROM app_surface_recipes ORDER BY builtin DESC, category, name",
                [],
            )?;
            Ok(json!(rows))
        }
        "surface_launch" => {
            let recipe_id = arg_str(&args, "recipe_id")?;
            let repo_id = crate::agents::get_row(ctx, agent)
                .and_then(|r| r["repo_id"].as_str().map(str::to_string))
                .filter(|s| !s.is_empty());
            let workdir = resolve_mcp_workdir(
                ctx,
                agent,
                agent_name,
                repo_id.as_deref(),
                strict,
                arg_opt_str(&args, "workdir").as_deref(),
            )?;
            launch_surface(ctx, &recipe_id, repo_id, Some(agent.to_string()), workdir, strict).await
        }
        "surface_exec" => {
            if crate::pilot::active() {
                return Err("Container execution is disabled in the side-by-side pilot".into());
            }
            let id = arg_str(&args, "id")?;
            let row = load_scoped_surface(ctx, agent, strict, &id, true)?;
            let cname = row["container_name"]
                .as_str()
                .filter(|s| !s.is_empty() && !s.starts_with('-'))
                .ok_or_else(|| "surface has no valid container name".to_string())?
                .to_string();
            let cmd = parse_exec_command(&args["command"])?;
            let timeout_secs = args["timeout_secs"].as_u64().unwrap_or(30).clamp(1, 120);
            let docker = find_bin(&["docker"]).ok_or_else(|| "No container runtime CLI (`docker`) found".to_string())?;
            let mut exec_args = Vec::with_capacity(cmd.len() + 2);
            exec_args.push("exec".to_string());
            exec_args.push(cname);
            exec_args.extend(cmd);
            let out = tokio::task::spawn_blocking(move || {
                run_bounded_command(
                    &docker,
                    &exec_args,
                    Duration::from_secs(timeout_secs),
                    EXEC_MAX_BYTES,
                    EXEC_MAX_BYTES,
                )
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({
                "stdout": String::from_utf8_lossy(&out.stdout),
                "stderr": String::from_utf8_lossy(&out.stderr),
                "exit_code": out.exit_code,
                "truncated": out.truncated,
            }))
        }
        "surface_screenshot" => {
            if crate::pilot::active() {
                return Err("Container screenshots are disabled in the side-by-side pilot".into());
            }
            let id = arg_str(&args, "id")?;
            let row = load_scoped_surface(ctx, agent, strict, &id, true)?;
            let cname = row["container_name"]
                .as_str()
                .filter(|s| !s.is_empty() && !s.starts_with('-'))
                .ok_or_else(|| "surface has no valid container name".to_string())?
                .to_string();
            let docker = find_bin(&["docker"]).ok_or_else(|| "No container runtime CLI (`docker`) found".to_string())?;
            let exec_args = vec![
                "exec".to_string(),
                cname,
                "/bin/sh".to_string(),
                "-c".to_string(),
                SCREENSHOT_SCRIPT.to_string(),
            ];
            let out = tokio::task::spawn_blocking(move || {
                run_bounded_command(
                    &docker,
                    &exec_args,
                    Duration::from_secs(SCREENSHOT_TIMEOUT_SECS),
                    SCREENSHOT_MAX_BYTES,
                    16 * 1024,
                )
            })
            .await
            .map_err(|e| e.to_string())??;
            let stderr_text = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if out.exit_code == 127 || stderr_text.contains("NO_CAPTURE_TOOL") {
                return Err("Screenshot capture tools (import, scrot, ffmpeg, grim) are not available in this container image".into());
            }
            if out.exit_code != 0 || out.truncated || !out.stdout.starts_with(PNG_MAGIC) {
                let detail = if stderr_text.is_empty() {
                    format!("exit_code {}", out.exit_code)
                } else {
                    stderr_text
                };
                return Err(format!("Failed to capture PNG screenshot from surface container: {detail}"));
            }
            let dir = ctx.data_dir.join("surfaces").join(safe_path_segment(&id));
            let file_name = format!("shot-{}-{}.png", now(), &new_id()[..8]);
            let path = dir.join(file_name);
            let bytes = out.stdout.len();
            let png = out.stdout;
            let write_path = path.clone();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                std::fs::create_dir_all(&dir).map_err(|e| format!("create screenshot dir: {e}"))?;
                std::fs::write(&write_path, &png).map_err(|e| format!("write screenshot: {e}"))
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({
                "id": id,
                "path": path.to_string_lossy(),
                "bytes": bytes,
            }))
        }
        "surface_stop" => {
            let id = arg_str(&args, "id")?;
            let row = load_scoped_surface(ctx, agent, strict, &id, false)?;
            let cname = row["container_name"].as_str().unwrap_or("");
            stop_surface_by_row(ctx, &id, cname).await?;
            Ok(json!({ "id": id, "state": "stopped" }))
        }
        _ => Err(format!("unknown surface tool `{name}`")),
    }
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let engines = detect_engines();
            let shim = open_shim_path(ctx);
            Ok(json!({
                "runtime": engines["recommended"],
                "engines": engines,
                "shim_active": shim.is_file(),
                "shim_path": shim,
                "pilot": crate::pilot::active(),
                "running": ctx.db.query("SELECT count(*) AS n FROM app_surfaces WHERE state='running'", [])?[0]["n"],
            }))
        }
        "catalog" => {
            let rows = ctx.db.query(
                "SELECT id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin
                 FROM app_surface_recipes ORDER BY builtin DESC, category, name",
                [],
            )?;
            Ok(json!(rows))
        }
        "list" => {
            reconcile_if_needed(ctx).await;
            let repo_id = arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty());
            let agent_id = arg_opt_str(&args, "agent_id").filter(|s| !s.is_empty());
            let rows = ctx.db.query(
                "SELECT id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,error,created_at,updated_at
                 FROM app_surfaces
                 WHERE (?1 IS NULL OR repo_id=?1)
                   AND (?2 IS NULL OR agent_id=?2)
                 ORDER BY (state='running') DESC, updated_at DESC",
                params![repo_id, agent_id],
            )?;
            Ok(json!(rows))
        }
        "add_recipe" => {
            let id = arg_str(&args, "id")?.trim().to_lowercase();
            if !valid_slug(&id) {
                return Err("recipe id must use letters, digits, - and _".into());
            }
            let name = arg_str(&args, "name")?.trim().to_string();
            let image = arg_str(&args, "image")?.trim().to_string();
            if image.contains(char::is_whitespace) {
                return Err("container image cannot contain whitespace".into());
            }
            let category = args["category"].as_str().unwrap_or("custom").trim();
            let description = args["description"].as_str().unwrap_or("Custom isolated Linux app surface").trim();
            let command = args["command"].as_str().unwrap_or("").trim();
            let port = args["internal_port"].as_i64().unwrap_or(3000).clamp(1, 65535);
            let proto = args["protocol"].as_str().unwrap_or("selkies").trim();
            let cli = args["headless_cli"].as_str().unwrap_or("").trim();
            ctx.db.exec(
                "INSERT INTO app_surface_recipes(id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, category=excluded.category,
                 description=excluded.description, image=excluded.image, command=excluded.command,
                 internal_port=excluded.internal_port, protocol=excluded.protocol, headless_cli=excluded.headless_cli",
                params![id, name, category, description, image, command, port, proto, cli, now()],
            )?;
            ctx.notify("surfaces");
            ctx.emit("surfaces:changed", json!({ "recipe": id }));
            Ok(json!({ "id": id, "name": name, "image": image }))
        }
        "remove_recipe" => {
            let id = arg_str(&args, "id")?;
            let rows = ctx.db.query("SELECT builtin FROM app_surface_recipes WHERE id=?1", [&id])?;
            if rows.first().and_then(|r| r["builtin"].as_i64()) == Some(1) {
                return Err("built-in surface recipes cannot be deleted".into());
            }
            ctx.db.exec("DELETE FROM app_surface_recipes WHERE id=?1", [&id])?;
            ctx.notify("surfaces");
            ctx.emit("surfaces:changed", json!({ "recipe": id }));
            Ok(Value::Null)
        }
        "launch" => {
            let recipe_id = arg_str(&args, "recipe_id")?;
            let repo_id = arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty());
            let agent_id = arg_opt_str(&args, "agent_id").filter(|s| !s.is_empty());
            let workdir = resolve_workdir(ctx, repo_id.as_deref(), agent_id.as_deref(), arg_opt_str(&args, "workdir").as_deref());
            let strict = agent_id
                .as_deref()
                .and_then(|aid| crate::agents::get_row(ctx, aid))
                .and_then(|r| r["strict"].as_i64())
                == Some(1);
            launch_surface(ctx, &recipe_id, repo_id, agent_id, workdir, strict).await
        }
        "probe" => {
            let id = arg_str(&args, "id")?;
            let row = ctx
                .db
                .query("SELECT host_port, state FROM app_surfaces WHERE id=?1", [&id])?
                .into_iter()
                .next()
                .ok_or_else(|| "surface not found".to_string())?;
            let port = row["host_port"].as_u64().unwrap_or(0) as u16;
            let running = row["state"].as_str() == Some("running");
            if !running || port == 0 {
                return Ok(json!({ "id": id, "ready": false, "state": row["state"] }));
            }
            let ready = tokio::time::timeout(
                Duration::from_millis(250),
                tokio::net::TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
            Ok(json!({ "id": id, "ready": ready, "port": port, "state": "running" }))
        }
        "stop" => {
            let id = arg_str(&args, "id")?;
            let row = ctx
                .db
                .query("SELECT container_name FROM app_surfaces WHERE id=?1", [&id])?
                .into_iter()
                .next()
                .ok_or_else(|| "surface not found".to_string())?;
            let cname = row["container_name"].as_str().unwrap_or("");
            stop_surface_by_row(ctx, &id, cname).await?;
            Ok(Value::Null)
        }
        _ => Err(format!("unknown surfaces.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_executable_zero_interference_shims_and_blocks_host_apps() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let shim = open_shim_path(&ctx);
        assert!(shim.is_file());
        assert!(shim_bin_dir(&ctx).join("xdg-open").is_file());

        // URL call exits 0 with guidance, without launching Firefox.
        let url_out = Command::new(&shim)
            .env("PHILIUM_AGENT", "otter")
            .arg("https://example.com")
            .output()
            .unwrap();
        assert!(url_out.status.success());
        assert!(String::from_utf8_lossy(&url_out.stderr).contains("browser_open"));

        // Host GUI app launch is blocked with exit 1.
        let app_out = Command::new(&shim)
            .env("PHILIUM_AGENT", "otter")
            .args(["-a", "KiCad"])
            .output()
            .unwrap();
        assert!(!app_out.status.success());
        assert!(String::from_utf8_lossy(&app_out.stderr).contains("App Surface"));
    }

    #[test]
    fn seeds_builtin_catalog_and_builds_isolated_loopback_docker_args() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let rows = ctx.db.query("SELECT id FROM app_surface_recipes ORDER BY id", []).unwrap();
        let ids: Vec<_> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert!(ids.contains(&"kicad"));
        assert!(ids.contains(&"dbeaver"));
        assert!(ids.contains(&"chromium"));

        let args = build_docker_run_args(
            "philium-surface-kicad-1",
            "surf_1",
            Some("agent_1"),
            43123,
            3000,
            Some("/tmp/wt"),
            Path::new("/tmp/cfg"),
            "lscr.io/linuxserver/kicad:latest",
            "",
        );
        assert!(args.contains(&"127.0.0.1:43123:3000".to_string()));
        assert!(args.contains(&"philium.surface=surf_1".to_string()));
        assert!(args.contains(&"philium.agent=agent_1".to_string()));
        assert!(args.contains(&"/tmp/wt:/workspace".to_string()));
    }

    #[test]
    fn mcp_tools_expose_five_surface_tools_with_valid_schema() {
        let tools = mcp_tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            vec![
                "surface_catalog",
                "surface_launch",
                "surface_exec",
                "surface_screenshot",
                "surface_stop",
            ]
        );
        for tool in &tools {
            assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["inputSchema"]["properties"].is_object());
            assert!(tool["inputSchema"]["required"].is_array());
        }
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["recipe_id"]));
        assert_eq!(tools[2]["inputSchema"]["required"], json!(["id", "command"]));
        assert_eq!(tools[3]["inputSchema"]["required"], json!(["id"]));
        assert_eq!(tools[4]["inputSchema"]["required"], json!(["id"]));
    }

    #[test]
    fn strict_workdir_enforcement_allows_own_worktree_and_rejects_escapes() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let wt = crate::ctx::test_dir("strict-wt");
        let sub = wt.join("pcb");
        let outside = crate::ctx::test_dir("strict-outside");
        std::fs::create_dir_all(&sub).unwrap();
        let repo_dir = crate::ctx::test_dir("strict-repo");

        ctx.db
            .exec(
                "INSERT INTO repos(id,name,path,added_at) VALUES('r1','repo',?1,0)",
                [repo_dir.to_string_lossy().as_ref()],
            )
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,repo_id,strict,created_at,last_active_at) VALUES('s1','fox','p/m','r1',1,0,0)",
                [],
            )
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,state,created_at) VALUES('w1','r1','s1',?1,'fox','HEAD','active',0)",
                [wt.to_string_lossy().as_ref()],
            )
            .unwrap();

        let canon_wt = wt.canonicalize().unwrap().to_string_lossy().into_owned();
        let canon_sub = sub.canonicalize().unwrap().to_string_lossy().into_owned();

        // Default mounts own worktree.
        assert_eq!(
            resolve_mcp_workdir(&ctx, "s1", "fox", Some("r1"), true, None).unwrap(),
            Some(canon_wt.clone())
        );
        // Subdirectory inside own worktree (relative and absolute) is allowed.
        assert_eq!(
            resolve_mcp_workdir(&ctx, "s1", "fox", Some("r1"), true, Some("pcb")).unwrap(),
            Some(canon_sub.clone())
        );
        assert_eq!(
            resolve_mcp_workdir(&ctx, "s1", "fox", Some("r1"), true, Some(&canon_sub)).unwrap(),
            Some(canon_sub)
        );
        // Outside worktree, parent repo root, traversal, and host-wide mounts are all rejected.
        for bad in [
            outside.to_string_lossy().as_ref(),
            repo_dir.to_string_lossy().as_ref(),
            "/",
            "/etc",
            "../../etc",
        ] {
            assert!(
                resolve_mcp_workdir(&ctx, "s1", "fox", Some("r1"), true, Some(bad)).is_err(),
                "expected strict workdir rejection for {bad}"
            );
        }
        #[cfg(unix)]
        {
            let link = wt.join("escape-link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(
                resolve_mcp_workdir(&ctx, "s1", "fox", Some("r1"), true, Some(link.to_str().unwrap())).is_err(),
                "symlink escape outside worktree must be rejected"
            );
        }
    }

    #[test]
    fn ownership_scoping_isolates_strict_and_cross_repo_agents() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db
            .exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','a','/tmp/a',0),('rb','b','/tmp/b',0)", [])
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,repo_id,strict,created_at,last_active_at)
                 VALUES('a1','owl','p/m','ra',0,0,0),('a2','lynx','p/m','ra',0,0,0),('b1','ibis','p/m','rb',0,0,0),('s1','fox','p/m','ra',1,0,0)",
                [],
            )
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
                 VALUES('surf_a1','kicad','KiCad','ra','a1','/tmp/a','docker','philium-surface-kicad-a1','cid1',40001,'http://127.0.0.1:40001/','running',0,0)",
                [],
            )
            .unwrap();

        // Owner allowed.
        assert!(load_scoped_surface(&ctx, "a1", false, "surf_a1", true).is_ok());
        // Same-repo non-strict peer allowed.
        assert!(load_scoped_surface(&ctx, "a2", false, "surf_a1", true).is_ok());
        // Different-repo peer denied.
        assert!(load_scoped_surface(&ctx, "b1", false, "surf_a1", true).is_err());
        // Strict agent in same repo denied (must own agent_id).
        assert!(load_scoped_surface(&ctx, "s1", true, "surf_a1", true).is_err());
    }

    #[test]
    fn forget_agent_stops_surfaces_and_releases_reaper_entries() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db
            .exec(
                "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
                 VALUES
                 ('surf_1','kicad','KiCad',NULL,'agent_x',NULL,'docker','philium-surface-kicad-x1','cid1',41001,'http://127.0.0.1:41001/','running',0,0),
                 ('surf_2','dbeaver','DBeaver',NULL,'agent_y',NULL,'docker','philium-surface-dbeaver-y1','cid2',41002,'http://127.0.0.1:41002/','running',0,0)",
                [],
            )
            .unwrap();
        let r1 = crate::reaper::register(&ctx, Some("agent_x"), "container", "philium-surface-kicad-x1", json!({}));
        let r2 = crate::reaper::register(&ctx, Some("agent_y"), "container", "philium-surface-dbeaver-y1", json!({}));

        forget_agent(&ctx, "agent_x");

        let s1 = ctx.db.query("SELECT state FROM app_surfaces WHERE id='surf_1'", []).unwrap();
        let s2 = ctx.db.query("SELECT state FROM app_surfaces WHERE id='surf_2'", []).unwrap();
        assert_eq!(s1[0]["state"], "stopped");
        assert_eq!(s2[0]["state"], "running");

        let res1 = ctx.db.query("SELECT state FROM resources WHERE id=?1", [&r1]).unwrap();
        let res2 = ctx.db.query("SELECT state FROM resources WHERE id=?1", [&r2]).unwrap();
        assert_eq!(res1[0]["state"], "released");
        assert_eq!(res2[0]["state"], "live");
    }

    #[test]
    fn reconcile_marks_missing_containers_stopped_and_keeps_live_ones() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db
            .exec(
                "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
                 VALUES
                 ('surf_live','kicad','KiCad',NULL,'a1',NULL,'docker','philium-surface-kicad-live','cid_live',42001,'http://127.0.0.1:42001/','running',0,0),
                 ('surf_dead','dbeaver','DBeaver',NULL,'a1',NULL,'docker','philium-surface-dbeaver-dead','cid_dead',42002,'http://127.0.0.1:42002/','running',0,0)",
                [],
            )
            .unwrap();
        let r_live = crate::reaper::register(&ctx, Some("a1"), "container", "philium-surface-kicad-live", json!({}));
        let r_dead = crate::reaper::register(&ctx, Some("a1"), "container", "philium-surface-dbeaver-dead", json!({}));

        let live = HashSet::from(["philium-surface-kicad-live".to_string()]);
        let stopped = reconcile_running_rows(&ctx, &live).unwrap();
        assert_eq!(stopped, vec!["surf_dead".to_string()]);

        let rows = ctx
            .db
            .query("SELECT id, state FROM app_surfaces ORDER BY id", [])
            .unwrap();
        assert_eq!(rows[0]["id"], "surf_dead");
        assert_eq!(rows[0]["state"], "stopped");
        assert_eq!(rows[1]["id"], "surf_live");
        assert_eq!(rows[1]["state"], "running");

        assert_eq!(
            ctx.db.query("SELECT state FROM resources WHERE id=?1", [&r_live]).unwrap()[0]["state"],
            "live"
        );
        assert_eq!(
            ctx.db.query("SELECT state FROM resources WHERE id=?1", [&r_dead]).unwrap()[0]["state"],
            "released"
        );
    }

    #[test]
    fn bounded_exec_truncates_large_output_and_enforces_timeout() {
        let out = run_bounded_command(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "printf 'hello'; printf 'err' >&2; exit 3".to_string(),
            ],
            Duration::from_secs(5),
            1024,
            1024,
        )
        .unwrap();
        assert_eq!(out.stdout, b"hello");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.exit_code, 3);
        assert!(!out.truncated);

        // Large output exceeding cap is bounded and sets truncated=true without deadlocking.
        let big = run_bounded_command(
            Path::new("/bin/sh"),
            &[
                "-c".to_string(),
                "dd if=/dev/zero bs=1024 count=64 2>/dev/null | tr '\\0' 'x'".to_string(),
            ],
            Duration::from_secs(5),
            256,
            256,
        )
        .unwrap();
        assert_eq!(big.stdout.len(), 256);
        assert_eq!(big.exit_code, 0);
        assert!(big.truncated);

        // Timeout terminates hung commands promptly.
        let timed_out = run_bounded_command(
            Path::new("/bin/sh"),
            &["-c".to_string(), "sleep 10".to_string()],
            Duration::from_millis(200),
            256,
            256,
        );
        assert!(timed_out.is_err());
        assert!(timed_out.unwrap_err().contains("timed out"));

        // Command parsing accepts string or string array and rejects invalid inputs.
        assert_eq!(
            parse_exec_command(&json!(["kicad-cli", "--version"])).unwrap(),
            vec!["kicad-cli", "--version"]
        );
        assert_eq!(
            parse_exec_command(&json!("kicad-cli --version")).unwrap(),
            vec!["/bin/sh", "-c", "kicad-cli --version"]
        );
        assert!(parse_exec_command(&json!([])).is_err());
        assert!(parse_exec_command(&json!("")).is_err());
        assert!(parse_exec_command(&json!(42)).is_err());
    }

    #[tokio::test]
    async fn mcp_call_catalog_returns_builtin_recipes() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let catalog = mcp_call(&ctx, "a1", "owl", false, "surface_catalog", json!({}))
            .await
            .unwrap();
        let ids: Vec<&str> = catalog
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        assert!(ids.contains(&"kicad") && ids.contains(&"dbeaver") && ids.contains(&"freecad"));
    }

    #[tokio::test]
    async fn mcp_call_enforces_scope_on_exec_screenshot_and_stop() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx.db
            .exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','a','/tmp/a',0),('rb','b','/tmp/b',0)", [])
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,repo_id,strict,created_at,last_active_at)
                 VALUES('a1','owl','p/m','ra',0,0,0),('s1','fox','p/m','ra',1,0,0),('b1','ibis','p/m','rb',0,0,0)",
                [],
            )
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
                 VALUES('surf_own','kicad','KiCad','ra','a1','/tmp/a','docker','philium-surface-kicad-own','cid',43001,'http://127.0.0.1:43001/','running',0,0)",
                [],
            )
            .unwrap();

        // Strict peer and cross-repo peer are denied for exec, screenshot, and stop.
        for (agent, name, strict) in [("s1", "fox", true), ("b1", "ibis", false)] {
            assert!(mcp_call(&ctx, agent, name, strict, "surface_exec", json!({"id": "surf_own", "command": "echo hi"})).await.is_err());
            assert!(mcp_call(&ctx, agent, name, strict, "surface_screenshot", json!({"id": "surf_own"})).await.is_err());
            assert!(mcp_call(&ctx, agent, name, strict, "surface_stop", json!({"id": "surf_own"})).await.is_err());
        }
        // Owner can stop its surface.
        let stopped = mcp_call(&ctx, "a1", "owl", false, "surface_stop", json!({"id": "surf_own"}))
            .await
            .unwrap();
        assert_eq!(stopped["state"], "stopped");
        // Stopped surface cannot be exec'd or screenshotted.
        assert!(mcp_call(&ctx, "a1", "owl", false, "surface_exec", json!({"id": "surf_own", "command": "echo hi"})).await.is_err());
        assert!(mcp_call(&ctx, "a1", "owl", false, "surface_screenshot", json!({"id": "surf_own"})).await.is_err());
    }
}
