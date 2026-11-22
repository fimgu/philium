//! Generalized capability-based Access Grant broker for Philium agents.
//!
//! Agents start scoped to their own repository worktree. When an agent needs
//! access to another registered repository or an external folder/file, it calls
//! `request_access(...)`, which blocks until the user approves/denies via
//! `access.decide` or the request times out.
//!
//! Security invariants:
//! - Valid scopes: `read`, `code`, `context`, `communicate`, `write`.
//! - `code`, `context`, `communicate` are ONLY valid for `target_kind == "repo"`.
//! - `write` is ONLY valid for `target_kind` in `("folder", "file")` — NEVER `"repo"`.
//! - Strict agents (`strict == 1`) are hard-denied `context` and `communicate`
//!   at request, approval, grant-creation, and resolution time.
//! - Persistent repo-owned grants (`agent_id IS NULL`) only match a Strict agent
//!   when `strict_allowed == 1`.
//! - Blocked roots (`/`, `$HOME`, `secrets::dir()`, `pilot::data_dir()`, and
//!   credential/app stores in `sandbox::PRIVATE`) can never be requested or granted.
//!
//! RPC surface (`access.*`):
//! - `pending { agent_id? }`
//! - `decide { request_id, decision, target_kind?, target_repo_id?, target_path?, label?, scopes?, lifetime?, strict_allowed?, note? }`
//! - `grants { agent_id?, repo_id?, include_revoked? }`
//! - `grant_create { source_repo_id?, agent_id?, target_kind, target_repo_id?, target_path?, label?, scopes, lifetime?, strict_allowed? }`
//! - `revoke { id }`
//! - `catalog { exclude_repo_id? }`
//!
//! Live events:
//! - `access:requested` `{ request }`
//! - `access:changed` `{ agent_id?, repo_id?, grant_id?, request_id? }`

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::params;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::ctx::{arg_opt_str, arg_str, Ctx, LockExt};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS access_requests (
  id TEXT PRIMARY KEY,
  agent_id TEXT NOT NULL,
  agent_name TEXT NOT NULL,
  agent_repo_id TEXT,
  strict INTEGER NOT NULL DEFAULT 0,
  provider TEXT NOT NULL DEFAULT '',
  target_kind TEXT NOT NULL,
  target_hint TEXT NOT NULL,
  resolved_repo_id TEXT,
  resolved_repo_name TEXT,
  resolved_path TEXT,
  requested_scopes TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  grant_id TEXT,
  decision_note TEXT,
  created_at INTEGER NOT NULL,
  resolved_at INTEGER
);
CREATE INDEX IF NOT EXISTS access_requests_status_created ON access_requests(status, created_at);
CREATE INDEX IF NOT EXISTS access_requests_agent_status ON access_requests(agent_id, status);

CREATE TABLE IF NOT EXISTS access_grants (
  id TEXT PRIMARY KEY,
  agent_id TEXT,
  source_repo_id TEXT,
  target_kind TEXT NOT NULL,
  target_repo_id TEXT,
  target_path TEXT NOT NULL,
  label TEXT NOT NULL,
  scopes TEXT NOT NULL,
  lifetime TEXT NOT NULL,
  uses_remaining INTEGER,
  strict_allowed INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL DEFAULT 'user',
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS access_grants_agent_revoked ON access_grants(agent_id, revoked_at);
CREATE INDEX IF NOT EXISTS access_grants_source_repo_revoked ON access_grants(source_repo_id, revoked_at);
"#;

const VALID_SCOPES: &[&str] = &["read", "code", "context", "communicate", "write"];
const PERSONAL_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Developer",
    "Pictures",
    "Movies",
    "Music",
    "Philium",
];

#[derive(Clone, Debug)]
pub struct ResolvedGrant {
    pub id: String,
    pub target_kind: String,
    pub target_repo_id: Option<String>,
    pub target_path: PathBuf,
    pub label: String,
    pub scopes: Vec<String>,
    pub lifetime: String,
}

#[derive(Clone, Debug)]
struct AgentInfo {
    id: String,
    name: String,
    repo_id: Option<String>,
    strict: bool,
    provider: String,
}

#[derive(Clone, Debug)]
struct RepoInfo {
    id: String,
    name: String,
    path: String,
}

fn pending_waiters() -> &'static Mutex<HashMap<String, oneshot::Sender<Value>>> {
    static WAITERS: OnceLock<Mutex<HashMap<String, oneshot::Sender<Value>>>> = OnceLock::new();
    WAITERS.get_or_init(Default::default)
}

/// Ensures a pending waiter entry is removed if the awaiting future is dropped.
struct WaiterGuard(String);
impl Drop for WaiterGuard {
    fn drop(&mut self) {
        pending_waiters().lk().remove(&self.0);
    }
}

/// Idempotently initialize the `access_requests` and `access_grants` tables
/// and expire any stale pending requests left over from a previous crash.
pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    let now = crate::db::now();
    ctx.db.exec(
        "UPDATE access_requests SET status='expired', resolved_at=?1 WHERE status='pending'",
        [now],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- paths & security

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/empty"))
}

fn expand_tilde_str(raw: &str) -> PathBuf {
    let t = raw.trim();
    match t.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if t == "~" => home_dir(),
        None => PathBuf::from(t),
    }
}

fn expand_tilde_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(s) => expand_tilde_str(s),
        None => path.to_path_buf(),
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            Component::Normal(c) => out.push(c),
        }
    }
    if out.as_os_str().is_empty() && path.is_absolute() {
        out.push(Component::RootDir.as_os_str());
    }
    out
}

fn path_variants(p: &Path) -> Vec<PathBuf> {
    let norm = normalize_path(p);
    let mut out = vec![norm.clone()];
    if let Ok(canon) = std::fs::canonicalize(&norm) {
        if canon != norm {
            out.push(canon);
        }
    }
    out
}

fn private_homes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in std::iter::once(home_dir()).chain(crate::pilot::real_home()) {
        for v in path_variants(&root) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

/// Returns `Some(reason)` if `path` is a forbidden root or sensitive credential/app store.
///
/// Blocks:
/// - filesystem root (`/`)
/// - `$HOME` itself (and Pilot's real account home)
/// - Philium's secret vault (`crate::secrets::dir()`)
/// - Pilot's data directory (`crate::pilot::data_dir()`)
/// - System credential paths (`/Library/Keychains`, `/private/var/db/dslocal`)
/// - Credential/app stores from `crate::sandbox::PRIVATE` (matching `sandbox.rs:57-70`:
///   subfolders inside personal folders like `~/Documents/specs` ARE allowed, while
///   `~/Documents` itself, `$HOME`, `~/.ssh`, `~/.aws`, `~/Library/Keychains`, etc. are blocked).
pub fn is_blocked_root(path: &Path) -> Option<&'static str> {
    let expanded = expand_tilde_path(path);
    let candidates = path_variants(&expanded);
    let homes = private_homes();

    let secrets_variants: Vec<PathBuf> = crate::secrets::dir()
        .map(path_variants)
        .unwrap_or_default();
    let pilot_variants: Vec<PathBuf> = crate::pilot::data_dir()
        .as_deref()
        .map(path_variants)
        .unwrap_or_default();

    for p in &candidates {
        if p == Path::new("/") || (p.is_absolute() && p.parent().is_none()) {
            return Some("filesystem root");
        }
        for h in &homes {
            if p == h {
                return Some("home directory");
            }
        }
        for sv in &secrets_variants {
            if p.starts_with(sv) || sv.starts_with(p) {
                return Some("secret vault");
            }
        }
        for pv in &pilot_variants {
            if p.starts_with(pv) || pv.starts_with(p) {
                return Some("pilot data");
            }
        }
        for sys in ["/Library/Keychains", "/private/var/db/dslocal"] {
            let sp = Path::new(sys);
            if p.starts_with(sp) || sp.starts_with(p) {
                return Some(sys);
            }
        }
        for h in &homes {
            for d in crate::sandbox::PRIVATE {
                let priv_path = h.join(d);
                if priv_path.starts_with(p) {
                    return Some(d);
                }
                if !PERSONAL_FOLDERS.contains(d) && p.starts_with(&priv_path) {
                    return Some(d);
                }
            }
        }
    }
    None
}

fn validate_kind(kind: &str) -> Result<String, String> {
    let k = kind.trim().to_ascii_lowercase();
    match k.as_str() {
        "repo" | "folder" | "file" => Ok(k),
        _ => Err(format!("Invalid target_kind `{kind}`: expected `repo`, `folder`, or `file`")),
    }
}

fn validate_lifetime(lifetime: &str) -> Result<String, String> {
    let l = lifetime.trim().to_ascii_lowercase();
    match l.as_str() {
        "once" | "session" | "persistent" => Ok(l),
        _ => Err(format!("Invalid lifetime `{lifetime}`: expected `once`, `session`, or `persistent`")),
    }
}

fn normalize_scopes(scopes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in scopes {
        let n = s.trim().to_ascii_lowercase();
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn validate_scopes(target_kind: &str, scopes: &[String], strict: bool) -> Result<(), String> {
    if scopes.is_empty() {
        return Err("At least one access scope is required".into());
    }
    for s in scopes {
        if !VALID_SCOPES.contains(&s.as_str()) {
            return Err(format!("Unknown access scope `{s}`"));
        }
        match s.as_str() {
            "code" | "context" | "communicate" => {
                if target_kind != "repo" {
                    return Err(format!("Scope `{s}` is only valid for `repo` targets, not `{target_kind}`"));
                }
            }
            "write" => {
                if target_kind == "repo" {
                    return Err("Scope `write` is never allowed on `repo` targets; use `communicate` to coordinate with the target repo's agent".into());
                }
            }
            "read" => {}
            _ => unreachable!(),
        }
        if strict && matches!(s.as_str(), "context" | "communicate") {
            return Err(format!("Strict agents cannot be granted `{s}` scope"));
        }
    }
    Ok(())
}

fn parse_scopes_json(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn parse_scopes_arg(val: &Value) -> Result<Vec<String>, String> {
    let arr = val.as_array().ok_or("`scopes` must be an array of strings")?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().ok_or("each scope must be a string")?;
        out.push(s.to_string());
    }
    Ok(normalize_scopes(&out))
}

fn canonicalize_checked(raw: &str) -> Result<PathBuf, String> {
    let expanded = expand_tilde_str(raw);
    if !expanded.is_absolute() {
        return Err(format!("Target path must be absolute: `{raw}`"));
    }
    if let Some(why) = is_blocked_root(&expanded) {
        return Err(format!("Path `{}` is blocked ({why})", expanded.display()));
    }
    let canon = std::fs::canonicalize(&expanded)
        .map_err(|e| format!("Target path `{}` is not accessible: {e}", expanded.display()))?;
    if let Some(why) = is_blocked_root(&canon) {
        return Err(format!("Path `{}` is blocked ({why})", canon.display()));
    }
    Ok(canon)
}

fn default_label_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------- DB helpers

fn load_agent(ctx: &Ctx, agent_id: &str) -> Option<AgentInfo> {
    let rows = ctx
        .db
        .query(
            "SELECT id, name, repo_id, strict, model_ref FROM agents WHERE id=?1",
            [agent_id],
        )
        .ok()?;
    let r = rows.first()?;
    let model_ref = r["model_ref"].as_str().unwrap_or("");
    let provider = model_ref
        .split_once('/')
        .map(|(p, _)| p)
        .unwrap_or(model_ref)
        .to_string();
    Some(AgentInfo {
        id: r["id"].as_str()?.to_string(),
        name: r["name"].as_str().unwrap_or("").to_string(),
        repo_id: r["repo_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        strict: r["strict"].as_i64().unwrap_or(0) != 0,
        provider,
    })
}

fn load_repo_by_id(ctx: &Ctx, repo_id: &str) -> Option<RepoInfo> {
    let rows = ctx
        .db
        .query("SELECT id, name, path FROM repos WHERE id=?1", [repo_id])
        .ok()?;
    let r = rows.first()?;
    Some(RepoInfo {
        id: r["id"].as_str()?.to_string(),
        name: r["name"].as_str().unwrap_or("").to_string(),
        path: r["path"].as_str().unwrap_or("").to_string(),
    })
}

fn find_repo_by_hint(ctx: &Ctx, hint: &str) -> Option<RepoInfo> {
    let t = hint.trim();
    if t.is_empty() {
        return None;
    }
    let looks_like_path = t.starts_with('/') || t.starts_with("~/") || t == "~";

    if !looks_like_path {
        if let Some(r) = load_repo_by_id(ctx, t) {
            return Some(r);
        }
        if let Ok(rows) = ctx.db.query(
            "SELECT id, name, path FROM repos WHERE name=?1 OR LOWER(name)=LOWER(?1) ORDER BY CASE WHEN name=?1 THEN 0 ELSE 1 END LIMIT 1",
            [t],
        ) {
            if let Some(r) = rows.first() {
                return Some(RepoInfo {
                    id: r["id"].as_str()?.to_string(),
                    name: r["name"].as_str().unwrap_or("").to_string(),
                    path: r["path"].as_str().unwrap_or("").to_string(),
                });
            }
        }
    }

    // Match by raw or canonical path
    let expanded = expand_tilde_str(t);
    if expanded.is_absolute() {
        let target_variants = path_variants(&expanded);
        if let Ok(rows) = ctx.db.query("SELECT id, name, path FROM repos", []) {
            for r in rows {
                let Some(rp) = r["path"].as_str() else { continue };
                let repo_variants = path_variants(&expand_tilde_str(rp));
                if target_variants.iter().any(|tv| repo_variants.contains(tv)) {
                    return Some(RepoInfo {
                        id: r["id"].as_str()?.to_string(),
                        name: r["name"].as_str().unwrap_or("").to_string(),
                        path: rp.to_string(),
                    });
                }
            }
        }
    }
    None
}

fn format_request_row(mut row: Value) -> Value {
    let scopes = row["requested_scopes"]
        .as_str()
        .map(parse_scopes_json)
        .unwrap_or_default();
    let strict = row["strict"].as_i64().unwrap_or(0) != 0;
    if let Some(obj) = row.as_object_mut() {
        obj.insert("requested_scopes".into(), json!(scopes));
        obj.insert("strict".into(), Value::Bool(strict));
    }
    row
}

fn format_grant_row(mut row: Value, strip_strict_forbidden: bool) -> Value {
    let mut scopes = row["scopes"]
        .as_str()
        .map(parse_scopes_json)
        .unwrap_or_default();
    if strip_strict_forbidden {
        scopes.retain(|s| !matches!(s.as_str(), "context" | "communicate"));
    }
    let strict_allowed = row["strict_allowed"].as_i64().unwrap_or(0) != 0;
    if let Some(obj) = row.as_object_mut() {
        obj.insert("scopes".into(), json!(scopes));
        obj.insert("strict_allowed".into(), Value::Bool(strict_allowed));
        if !obj.contains_key("agent_name") {
            obj.insert("agent_name".into(), Value::Null);
        }
    }
    row
}

fn load_request_json(ctx: &Ctx, request_id: &str) -> Result<Value, String> {
    let rows = ctx.db.query("SELECT * FROM access_requests WHERE id=?1", [request_id])?;
    rows.into_iter()
        .next()
        .map(format_request_row)
        .ok_or_else(|| format!("access request `{request_id}` not found"))
}

fn load_grant_json(ctx: &Ctx, grant_id: &str) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT g.*, a.name AS agent_name
         FROM access_grants g
         LEFT JOIN agents a ON a.id = g.agent_id
         WHERE g.id=?1",
        [grant_id],
    )?;
    rows.into_iter()
        .next()
        .map(|r| format_grant_row(r, false))
        .ok_or_else(|| format!("access grant `{grant_id}` not found"))
}

// ---------------------------------------------------------------- public Rust API

pub fn pending_count(ctx: &Ctx) -> i64 {
    ctx.db
        .query("SELECT COUNT(*) AS n FROM access_requests WHERE status='pending'", [])
        .ok()
        .and_then(|r| r.first().and_then(|row| row["n"].as_i64()))
        .unwrap_or(0)
}

/// Active non-revoked grants matching `agent_id` OR persistent grants for the
/// agent's `repo_id` where `(!strict || strict_allowed == 1)`.
pub fn list_active_grants(ctx: &Ctx, agent_id: &str) -> Result<Vec<Value>, String> {
    let Some(agent) = load_agent(ctx, agent_id) else {
        return Ok(Vec::new());
    };
    let rows = match agent.repo_id.as_deref() {
        Some(repo_id) => {
            if agent.strict {
                ctx.db.query(
                    "SELECT g.*, a.name AS agent_name
                     FROM access_grants g
                     LEFT JOIN agents a ON a.id = g.agent_id
                     WHERE g.revoked_at IS NULL
                       AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
                       AND (g.agent_id = ?1 OR (g.agent_id IS NULL AND g.lifetime = 'persistent' AND g.source_repo_id = ?2 AND g.strict_allowed = 1))
                     ORDER BY g.created_at DESC, g.id ASC",
                    params![agent_id, repo_id],
                )?
            } else {
                ctx.db.query(
                    "SELECT g.*, a.name AS agent_name
                     FROM access_grants g
                     LEFT JOIN agents a ON a.id = g.agent_id
                     WHERE g.revoked_at IS NULL
                       AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
                       AND (g.agent_id = ?1 OR (g.agent_id IS NULL AND g.lifetime = 'persistent' AND g.source_repo_id = ?2))
                     ORDER BY g.created_at DESC, g.id ASC",
                    params![agent_id, repo_id],
                )?
            }
        }
        None => ctx.db.query(
            "SELECT g.*, a.name AS agent_name
             FROM access_grants g
             LEFT JOIN agents a ON a.id = g.agent_id
             WHERE g.revoked_at IS NULL
               AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
               AND g.agent_id = ?1
             ORDER BY g.created_at DESC, g.id ASC",
            [agent_id],
        )?,
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let formatted = format_grant_row(row, agent.strict);
        let has_scopes = formatted["scopes"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if has_scopes {
            out.push(formatted);
        }
    }
    Ok(out)
}

/// Compact bulleted summary placed after `<!-- PHILIUM_CACHE_BREAK -->` listing
/// active `grant_id`s, labels, target kinds, and scopes.
pub fn active_grants_prompt(ctx: &Ctx, agent_id: &str) -> Option<String> {
    let grants = list_active_grants(ctx, agent_id).ok()?;
    if grants.is_empty() {
        return None;
    }
    let mut out = String::from("\n## Active external access grants\n");
    for g in grants {
        let id = g["id"].as_str().unwrap_or("");
        let label = g["label"].as_str().unwrap_or("");
        let kind = g["target_kind"].as_str().unwrap_or("");
        let lifetime = g["lifetime"].as_str().unwrap_or("");
        let scopes: Vec<&str> = g["scopes"]
            .as_array()
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if id.is_empty() || scopes.is_empty() {
            continue;
        }
        let repo_tag = g["target_repo_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|r| format!(" repo_id={r}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- `{id}` ({kind}{repo_tag}): {label} — scopes: [{}], lifetime: {lifetime}\n",
            scopes.join(", ")
        ));
    }
    if out == "\n## Active external access grants\n" {
        return None;
    }
    Some(out)
}

/// Resolve an active grant for `agent_id` matching `grant_or_repo` by:
/// 1. exact grant `id`
/// 2. exact `target_repo_id`
/// 3. case-insensitive `label` or registered repo name
///
/// Enforces Strict constraints and `required_scope`. If `lifetime == "once"`
/// and `consume_once` is true, atomically decrements `uses_remaining`, sets
/// `revoked_at` when it hits 0, and emits `access:changed`.
pub fn resolve_grant(
    ctx: &Ctx,
    agent_id: &str,
    grant_or_repo: &str,
    required_scope: &str,
    consume_once: bool,
) -> Result<ResolvedGrant, String> {
    let scope = required_scope.trim().to_ascii_lowercase();
    if !VALID_SCOPES.contains(&scope.as_str()) {
        return Err(format!("Unknown required scope `{required_scope}`"));
    }
    let key = grant_or_repo.trim();
    if key.is_empty() {
        return Err("Missing grant or repo identifier".into());
    }
    let agent = load_agent(ctx, agent_id)
        .ok_or_else(|| format!("Unknown agent `{agent_id}`"))?;
    if agent.strict && matches!(scope.as_str(), "context" | "communicate") {
        return Err(format!("Strict agents cannot use `{scope}` scope"));
    }

    let rows = match agent.repo_id.as_deref() {
        Some(repo_id) => {
            if agent.strict {
                ctx.db.query(
                    "SELECT g.*, r.name AS target_repo_name
                     FROM access_grants g
                     LEFT JOIN repos r ON r.id = g.target_repo_id
                     WHERE g.revoked_at IS NULL
                       AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
                       AND (g.agent_id = ?1 OR (g.agent_id IS NULL AND g.lifetime = 'persistent' AND g.source_repo_id = ?2 AND g.strict_allowed = 1))
                     ORDER BY CASE WHEN g.lifetime = 'once' THEN 1 ELSE 0 END, g.created_at DESC",
                    params![agent_id, repo_id],
                )?
            } else {
                ctx.db.query(
                    "SELECT g.*, r.name AS target_repo_name
                     FROM access_grants g
                     LEFT JOIN repos r ON r.id = g.target_repo_id
                     WHERE g.revoked_at IS NULL
                       AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
                       AND (g.agent_id = ?1 OR (g.agent_id IS NULL AND g.lifetime = 'persistent' AND g.source_repo_id = ?2))
                     ORDER BY CASE WHEN g.lifetime = 'once' THEN 1 ELSE 0 END, g.created_at DESC",
                    params![agent_id, repo_id],
                )?
            }
        }
        None => ctx.db.query(
            "SELECT g.*, r.name AS target_repo_name
             FROM access_grants g
             LEFT JOIN repos r ON r.id = g.target_repo_id
             WHERE g.revoked_at IS NULL
               AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
               AND g.agent_id = ?1
             ORDER BY CASE WHEN g.lifetime = 'once' THEN 1 ELSE 0 END, g.created_at DESC",
            [agent_id],
        )?,
    };

    // Rank matching candidates: priority 0 = id, 1 = target_repo_id, 2 = label / repo name
    let mut matched: Vec<(u8, Value, Vec<String>)> = Vec::new();
    for row in rows {
        let id = row["id"].as_str().unwrap_or("");
        let target_repo_id = row["target_repo_id"].as_str().unwrap_or("");
        let label = row["label"].as_str().unwrap_or("");
        let repo_name = row["target_repo_name"].as_str().unwrap_or("");

        let prio = if id == key {
            Some(0u8)
        } else if !target_repo_id.is_empty() && target_repo_id == key {
            Some(1u8)
        } else if label.eq_ignore_ascii_case(key)
            || (!repo_name.is_empty() && repo_name.eq_ignore_ascii_case(key))
        {
            Some(2u8)
        } else {
            None
        };

        if let Some(p) = prio {
            let mut scopes = row["scopes"]
                .as_str()
                .map(parse_scopes_json)
                .unwrap_or_default();
            if agent.strict {
                scopes.retain(|s| !matches!(s.as_str(), "context" | "communicate"));
            }
            matched.push((p, row, scopes));
        }
    }

    if matched.is_empty() {
        return Err(format!("No active access grant matching `{key}`"));
    }

    // Sort by match priority (id < target_repo_id < label), keeping non-once ahead of once.
    matched.sort_by_key(|(prio, row, _)| {
        let is_once = u8::from(row["lifetime"].as_str() == Some("once"));
        (*prio, is_once)
    });

    let chosen = matched.iter().find(|(_, row, scopes)| {
        let kind = row["target_kind"].as_str().unwrap_or("");
        scopes.iter().any(|s| s == &scope)
            && validate_scopes(kind, std::slice::from_ref(&scope), agent.strict).is_ok()
    });

    let Some((_, row, scopes)) = chosen else {
        let first_label = matched[0]
            .1["label"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(key);
        return Err(format!(
            "Grant `{first_label}` does not permit `{scope}` scope"
        ));
    };

    let grant_id = row["id"].as_str().unwrap_or("").to_string();
    let target_kind = row["target_kind"].as_str().unwrap_or("").to_string();
    let target_repo_id = row["target_repo_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let target_path = PathBuf::from(row["target_path"].as_str().unwrap_or(""));
    let label = row["label"].as_str().unwrap_or("").to_string();
    let lifetime = row["lifetime"].as_str().unwrap_or("").to_string();
    let source_repo_id = row["source_repo_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some(why) = is_blocked_root(&target_path) {
        return Err(format!("Grant target `{}` is blocked ({why})", target_path.display()));
    }

    if lifetime == "once" && consume_once {
        let now = crate::db::now();
        let updated = ctx.db.with(|c| {
            c.execute(
                "UPDATE access_grants
                 SET uses_remaining = CASE
                       WHEN uses_remaining IS NULL THEN 0
                       ELSE uses_remaining - 1
                     END,
                     revoked_at = CASE
                       WHEN uses_remaining IS NULL OR uses_remaining <= 1 THEN ?1
                       ELSE revoked_at
                     END
                 WHERE id = ?2
                   AND revoked_at IS NULL
                   AND (uses_remaining IS NULL OR uses_remaining > 0)",
                params![now, grant_id],
            )
        })?;
        if updated == 0 {
            return Err(format!("One-time grant `{grant_id}` has already been consumed or revoked"));
        }
        ctx.emit(
            "access:changed",
            json!({
                "agent_id": agent_id,
                "repo_id": source_repo_id,
                "grant_id": grant_id,
            }),
        );
    }

    Ok(ResolvedGrant {
        id: grant_id,
        target_kind,
        target_repo_id,
        target_path,
        label,
        scopes: scopes.clone(),
        lifetime,
    })
}

/// True iff sender is NOT strict AND holds an active grant to `target_repo_id`
/// containing `"communicate"`.
pub fn can_communicate(ctx: &Ctx, sender_agent_id: &str, target_repo_id: &str) -> bool {
    if target_repo_id.trim().is_empty() {
        return false;
    }
    let Some(agent) = load_agent(ctx, sender_agent_id) else {
        return false;
    };
    if agent.strict {
        return false;
    }
    let Ok(grants) = list_active_grants(ctx, sender_agent_id) else {
        return false;
    };
    grants.iter().any(|g| {
        g["target_kind"].as_str() == Some("repo")
            && g["target_repo_id"].as_str() == Some(target_repo_id)
            && g["scopes"]
                .as_array()
                .map(|arr| arr.iter().any(|s| s.as_str() == Some("communicate")))
                .unwrap_or(false)
    })
}

fn find_covering_grant(
    grants: &[Value],
    target_kind: &str,
    resolved_repo_id: Option<&str>,
    resolved_path: Option<&str>,
    requested_scopes: &[String],
) -> Option<Value> {
    if resolved_repo_id.is_none() && resolved_path.is_none() {
        return None;
    }
    // Prefer persistent/session over once so we don't depend on a single-use grant when a durable one exists.
    let mut candidates: Vec<&Value> = grants
        .iter()
        .filter(|g| {
            let g_scopes: Vec<&str> = g["scopes"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !requested_scopes.iter().all(|req| g_scopes.contains(&req.as_str())) {
                return false;
            }
            if let Some(rid) = resolved_repo_id {
                return g["target_kind"].as_str() == Some("repo")
                    && g["target_repo_id"].as_str() == Some(rid);
            }
            if let Some(rp) = resolved_path {
                let Some(gp) = g["target_path"].as_str() else {
                    return false;
                };
                let req_path = Path::new(rp);
                let grant_path = Path::new(gp);
                let g_kind = g["target_kind"].as_str().unwrap_or("");
                if g_kind == "file" {
                    return target_kind == "file" && req_path == grant_path;
                }
                return req_path == grant_path || req_path.starts_with(grant_path);
            }
            false
        })
        .collect();

    candidates.sort_by_key(|g| u8::from(g["lifetime"].as_str() == Some("once")));
    candidates.first().map(|g| (*g).clone())
}

/// Request external access on behalf of an agent. Blocks until approved, denied,
/// cancelled, or timed out (clamped to 5..=600s).
pub async fn request_access(
    ctx: &Arc<Ctx>,
    agent_id: &str,
    target: &str,
    kind: Option<&str>,
    scopes: Vec<String>,
    reason: &str,
    timeout_secs: u64,
) -> Result<Value, String> {
    let agent = load_agent(ctx, agent_id)
        .ok_or_else(|| format!("Unknown agent `{agent_id}`"))?;
    let hint = target.trim();
    if hint.is_empty() {
        return Err("Missing `target`".into());
    }
    let explicit_kind = match kind.map(str::trim).filter(|s| !s.is_empty()) {
        Some(k) => Some(validate_kind(k)?),
        None => None,
    };

    // Resolve target hint: registered repo -> existing absolute/tilde path -> unresolved human hint.
    let target_kind: String;
    let mut resolved_repo_id: Option<String> = None;
    let mut resolved_repo_name: Option<String> = None;
    let mut resolved_path: Option<String> = None;

    let matched_repo = if explicit_kind.as_deref() == Some("file") {
        None
    } else {
        find_repo_by_hint(ctx, hint)
    };

    if let Some(repo) = matched_repo {
        let repo_path = expand_tilde_str(&repo.path);
        if let Some(why) = is_blocked_root(&repo_path) {
            return Err(format!("Repository path `{}` is blocked ({why})", repo_path.display()));
        }
        let canon = std::fs::canonicalize(&repo_path).unwrap_or_else(|_| normalize_path(&repo_path));
        if let Some(why) = is_blocked_root(&canon) {
            return Err(format!("Repository path `{}` is blocked ({why})", canon.display()));
        }
        target_kind = "repo".to_string();
        resolved_repo_id = Some(repo.id);
        resolved_repo_name = Some(repo.name);
        resolved_path = Some(canon.to_string_lossy().into_owned());
    } else {
        let expanded = expand_tilde_str(hint);
        if expanded.is_absolute() {
            if let Some(why) = is_blocked_root(&expanded) {
                return Err(format!("Path `{}` is blocked ({why})", expanded.display()));
            }
            if let Ok(meta) = std::fs::metadata(&expanded) {
                let canon = std::fs::canonicalize(&expanded)
                    .map_err(|e| format!("Cannot resolve `{}`: {e}", expanded.display()))?;
                if let Some(why) = is_blocked_root(&canon) {
                    return Err(format!("Path `{}` is blocked ({why})", canon.display()));
                }
                let disk_kind = if meta.is_file() { "file" } else { "folder" };
                if let Some(ek) = explicit_kind.as_deref() {
                    if ek != "repo" && ek != disk_kind {
                        return Err(format!(
                            "Target `{}` is a `{disk_kind}`, not a `{ek}`",
                            canon.display()
                        ));
                    }
                }
                target_kind = disk_kind.to_string();
                resolved_path = Some(canon.to_string_lossy().into_owned());
            } else {
                // Path does not exist on disk yet; keep unresolved so the human can pick it.
                target_kind = explicit_kind.unwrap_or_else(|| "folder".to_string());
            }
        } else {
            // Human hint like "Q3 specs" — never resolve relative to Philium's process CWD.
            target_kind = explicit_kind.unwrap_or_else(|| "folder".to_string());
        }
    }

    // Normalize & validate scopes.
    let norm_scopes = if scopes.is_empty() {
        if target_kind == "repo" {
            vec!["read".to_string(), "code".to_string()]
        } else {
            vec!["read".to_string()]
        }
    } else {
        normalize_scopes(&scopes)
    };
    validate_scopes(&target_kind, &norm_scopes, agent.strict)?;

    // Fast-path: already covered by an active grant.
    let active = list_active_grants(ctx, agent_id)?;
    if let Some(existing) = find_covering_grant(
        &active,
        &target_kind,
        resolved_repo_id.as_deref(),
        resolved_path.as_deref(),
        &norm_scopes,
    ) {
        return Ok(json!({
            "status": "already_granted",
            "grant": existing,
        }));
    }

    // Insert pending request and register waiter.
    let id = crate::db::new_id();
    let now = crate::db::now();
    let scopes_json = Value::Array(norm_scopes.iter().map(|s| Value::String(s.clone())).collect()).to_string();
    let reason_trimmed = reason.trim().to_string();

    ctx.db.exec(
        "INSERT INTO access_requests(
            id, agent_id, agent_name, agent_repo_id, strict, provider,
            target_kind, target_hint, resolved_repo_id, resolved_repo_name, resolved_path,
            requested_scopes, reason, status, created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'pending',?14)",
        params![
            id,
            agent.id,
            agent.name,
            agent.repo_id,
            i64::from(agent.strict),
            agent.provider,
            target_kind,
            hint,
            resolved_repo_id,
            resolved_repo_name,
            resolved_path,
            scopes_json,
            reason_trimmed,
            now,
        ],
    )?;

    let (tx, mut rx) = oneshot::channel::<Value>();
    pending_waiters().lk().insert(id.clone(), tx);
    let _guard = WaiterGuard(id.clone());

    let req_json = load_request_json(ctx, &id)?;
    ctx.log_event(
        "access_requested",
        Some(&agent.id),
        &json!({
            "request_id": id,
            "target_kind": target_kind,
            "target_hint": hint,
            "resolved_repo_id": resolved_repo_id,
            "resolved_path": resolved_path,
            "scopes": norm_scopes,
        }),
    );
    ctx.emit("access:requested", json!({ "request": req_json }));
    crate::attention::refresh(ctx);

    let wait = Duration::from_secs(timeout_secs.clamp(5, 600));
    match tokio::time::timeout(wait, &mut rx).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(_)) => {
            // Sender dropped without sending: mark cancelled if still pending.
            let _ = ctx.db.exec(
                "UPDATE access_requests SET status='cancelled', resolved_at=?1 WHERE id=?2 AND status='pending'",
                params![crate::db::now(), id],
            );
            ctx.emit(
                "access:changed",
                json!({
                    "agent_id": agent.id,
                    "repo_id": agent.repo_id,
                    "request_id": id,
                }),
            );
            crate::attention::refresh(ctx);
            Ok(json!({ "status": "cancelled", "request_id": id }))
        }
        Err(_) => {
            // Remove waiter first to serialize with any concurrent `decide`.
            let won_race = pending_waiters().lk().remove(&id).is_some();
            if !won_race {
                if let Ok(val) = rx.try_recv() {
                    return Ok(val);
                }
            }
            let _ = ctx.db.exec(
                "UPDATE access_requests SET status='expired', resolved_at=?1 WHERE id=?2 AND status='pending'",
                params![crate::db::now(), id],
            );
            ctx.emit(
                "access:changed",
                json!({
                    "agent_id": agent.id,
                    "repo_id": agent.repo_id,
                    "request_id": id,
                }),
            );
            crate::attention::refresh(ctx);
            Ok(json!({ "status": "expired", "request_id": id }))
        }
    }
}

/// Cancel all pending access requests for `agent_id` and revoke all `session`/`once` grants.
pub fn cleanup_agent(ctx: &Arc<Ctx>, agent_id: &str) {
    let now = crate::db::now();
    let repo_id = load_agent(ctx, agent_id).and_then(|a| a.repo_id);

    let pending_ids: Vec<String> = ctx
        .db
        .query(
            "SELECT id FROM access_requests WHERE agent_id=?1 AND status='pending'",
            [agent_id],
        )
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();

    let _ = ctx.db.exec(
        "UPDATE access_requests SET status='cancelled', resolved_at=?1 WHERE agent_id=?2 AND status='pending'",
        params![now, agent_id],
    );

    if !pending_ids.is_empty() {
        let mut map = pending_waiters().lk();
        for req_id in pending_ids {
            if let Some(tx) = map.remove(&req_id) {
                let _ = tx.send(json!({ "status": "cancelled", "request_id": req_id }));
            }
        }
    }

    let _ = ctx.db.exec(
        "UPDATE access_grants
         SET revoked_at=?1
         WHERE agent_id=?2
           AND revoked_at IS NULL
           AND lifetime IN ('session', 'once')",
        params![now, agent_id],
    );

    ctx.emit(
        "access:changed",
        json!({
            "agent_id": agent_id,
            "repo_id": repo_id,
        }),
    );
    crate::attention::refresh(ctx);
}

// ---------------------------------------------------------------- RPC surface

fn resolve_target_for_grant(
    ctx: &Ctx,
    requested_kind: &str,
    explicit_kind: bool,
    target_repo_id: Option<&str>,
    target_path: Option<&str>,
    label_override: Option<&str>,
) -> Result<(String, Option<String>, Option<String>, PathBuf, String), String> {
    if requested_kind == "repo" {
        let repo = if let Some(rid) = target_repo_id.filter(|s| !s.is_empty()) {
            load_repo_by_id(ctx, rid)
                .ok_or_else(|| format!("Registered repository `{rid}` not found"))?
        } else if let Some(tp) = target_path.filter(|s| !s.is_empty()) {
            find_repo_by_hint(ctx, tp)
                .ok_or_else(|| format!("No registered repository matches `{tp}`"))?
        } else {
            return Err("Please select a registered repository".into());
        };
        let canon = canonicalize_checked(&repo.path)?;
        let label = label_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| repo.name.clone());
        Ok(("repo".into(), Some(repo.id), Some(repo.name), canon, label))
    } else {
        let raw_path = target_path
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("Please select a target folder or file")?;
        let canon = canonicalize_checked(raw_path)?;
        let disk_kind = if canon.is_file() { "file" } else { "folder" };
        let final_kind = if explicit_kind {
            if requested_kind != disk_kind {
                return Err(format!(
                    "Target `{}` is a `{disk_kind}`, not a `{requested_kind}`",
                    canon.display()
                ));
            }
            requested_kind.to_string()
        } else {
            disk_kind.to_string()
        };
        let label = label_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_label_for_path(&canon));
        Ok((final_kind, None, None, canon, label))
    }
}

fn rpc_pending(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let rows = match arg_opt_str(args, "agent_id").filter(|s| !s.is_empty()) {
        Some(aid) => ctx.db.query(
            "SELECT * FROM access_requests WHERE status='pending' AND agent_id=?1 ORDER BY created_at ASC",
            [aid],
        )?,
        None => ctx.db.query(
            "SELECT * FROM access_requests WHERE status='pending' ORDER BY created_at ASC",
            [],
        )?,
    };
    Ok(Value::Array(rows.into_iter().map(format_request_row).collect()))
}

fn rpc_decide(ctx: &Arc<Ctx>, args: &Value) -> Result<Value, String> {
    let request_id = arg_str(args, "request_id")?;
    let decision = arg_str(args, "decision")?.trim().to_ascii_lowercase();
    let note = arg_opt_str(args, "note").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let req_rows = ctx.db.query("SELECT * FROM access_requests WHERE id=?1", [&request_id])?;
    let req_row = req_rows
        .first()
        .ok_or_else(|| format!("Access request `{request_id}` not found"))?;
    let status = req_row["status"].as_str().unwrap_or("");
    if status != "pending" {
        return Err(format!("Access request `{request_id}` is already `{status}`"));
    }

    let req_agent_id = req_row["agent_id"].as_str().unwrap_or("").to_string();
    let req_agent_repo_id = req_row["agent_repo_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let req_strict = req_row["strict"].as_i64().unwrap_or(0) != 0;

    match decision.as_str() {
        "deny" => {
            let now = crate::db::now();
            let updated = ctx.db.exec(
                "UPDATE access_requests SET status='denied', decision_note=?1, resolved_at=?2 WHERE id=?3 AND status='pending'",
                params![note, now, request_id],
            )?;
            if updated == 0 {
                return Err(format!("Access request `{request_id}` is no longer pending"));
            }
            let updated_req = load_request_json(ctx, &request_id)?;
            ctx.log_event(
                "access_denied",
                Some(&req_agent_id),
                &json!({ "request_id": request_id, "note": note }),
            );
            ctx.emit(
                "access:changed",
                json!({
                    "agent_id": req_agent_id,
                    "repo_id": req_agent_repo_id,
                    "request_id": request_id,
                }),
            );
            crate::attention::refresh(ctx);
            if let Some(tx) = pending_waiters().lk().remove(&request_id) {
                let _ = tx.send(json!({
                    "status": "denied",
                    "request_id": request_id,
                    "note": note,
                }));
            }
            Ok(json!({ "request": updated_req }))
        }
        "approve" => {
            let override_kind = arg_opt_str(args, "target_kind").filter(|s| !s.is_empty());
            let explicit_kind = override_kind.is_some();
            let override_repo_id = arg_opt_str(args, "target_repo_id").filter(|s| !s.is_empty());
            let override_path = arg_opt_str(args, "target_path").filter(|s| !s.is_empty());

            let raw_kind = if let Some(ref k) = override_kind {
                k.as_str()
            } else if override_repo_id.is_some() && override_path.is_none() {
                "repo"
            } else if override_path.is_some() && override_repo_id.is_none()
                && req_row["target_kind"].as_str() == Some("repo")
            {
                "folder"
            } else {
                req_row["target_kind"].as_str().unwrap_or("folder")
            };
            let validated_kind = validate_kind(raw_kind)?;

            let target_repo_id_in = override_repo_id.or_else(|| {
                req_row["resolved_repo_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            let target_path_in = override_path.or_else(|| {
                req_row["resolved_path"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            let label_override = arg_opt_str(args, "label").filter(|s| !s.is_empty());

            let (final_kind, final_repo_id, final_repo_name, canon_path, label) =
                resolve_target_for_grant(
                    ctx,
                    &validated_kind,
                    explicit_kind,
                    target_repo_id_in.as_deref(),
                    target_path_in.as_deref(),
                    label_override.as_deref(),
                )?;

            let final_scopes = match args.get("scopes") {
                Some(v) if !v.is_null() => parse_scopes_arg(v)?,
                _ => normalize_scopes(&parse_scopes_json(
                    req_row["requested_scopes"].as_str().unwrap_or("[]"),
                )),
            };
            validate_scopes(&final_kind, &final_scopes, req_strict)?;

            let lifetime = match arg_opt_str(args, "lifetime").filter(|s| !s.is_empty()) {
                Some(l) => validate_lifetime(&l)?,
                None => "session".to_string(),
            };

            let (grant_agent_id, source_repo_id, uses_remaining, strict_allowed) =
                match lifetime.as_str() {
                    "persistent" => {
                        let Some(src_repo) = req_agent_repo_id.clone() else {
                            return Err("Persistent grants require the agent to belong to a repository".into());
                        };
                        let sa = args
                            .get("strict_allowed")
                            .and_then(Value::as_bool)
                            .unwrap_or(req_strict);
                        if req_strict && !sa {
                            return Err("Approving a persistent grant for a Strict agent requires `strict_allowed: true`".into());
                        }
                        (None, Some(src_repo), None, sa)
                    }
                    "once" => {
                        let sa = args
                            .get("strict_allowed")
                            .and_then(Value::as_bool)
                            .unwrap_or(req_strict);
                        (Some(req_agent_id.clone()), req_agent_repo_id.clone(), Some(1i64), sa)
                    }
                    "session" => {
                        let sa = args
                            .get("strict_allowed")
                            .and_then(Value::as_bool)
                            .unwrap_or(req_strict);
                        (Some(req_agent_id.clone()), req_agent_repo_id.clone(), None, sa)
                    }
                    _ => unreachable!(),
                };

            let grant_id = crate::db::new_id();
            let now = crate::db::now();
            let canon_str = canon_path.to_string_lossy().into_owned();
            let scopes_json = Value::Array(
                final_scopes.iter().map(|s| Value::String(s.clone())).collect(),
            )
            .to_string();

            let updated = ctx.db.with(|c| {
                c.execute_batch("BEGIN IMMEDIATE")?;
                let res: rusqlite::Result<usize> = (|| {
                    let n = c.execute(
                        "UPDATE access_requests
                         SET status='approved', grant_id=?1, decision_note=?2, resolved_at=?3,
                             target_kind=?4, resolved_repo_id=?5, resolved_repo_name=?6, resolved_path=?7
                         WHERE id=?8 AND status='pending'",
                        params![
                            grant_id,
                            note,
                            now,
                            final_kind,
                            final_repo_id,
                            final_repo_name,
                            canon_str,
                            request_id,
                        ],
                    )?;
                    if n == 0 {
                        return Ok(0);
                    }
                    c.execute(
                        "INSERT INTO access_grants(
                            id, agent_id, source_repo_id, target_kind, target_repo_id,
                            target_path, label, scopes, lifetime, uses_remaining,
                            strict_allowed, created_by, created_at
                         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'user',?12)",
                        params![
                            grant_id,
                            grant_agent_id,
                            source_repo_id,
                            final_kind,
                            final_repo_id,
                            canon_str,
                            label,
                            scopes_json,
                            lifetime,
                            uses_remaining,
                            i64::from(strict_allowed),
                            now,
                        ],
                    )?;
                    Ok(n)
                })();
                match res {
                    Ok(n) => {
                        c.execute_batch("COMMIT")?;
                        Ok(n)
                    }
                    Err(e) => {
                        let _ = c.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            })?;

            if updated == 0 {
                return Err(format!("Access request `{request_id}` is no longer pending"));
            }

            let grant = load_grant_json(ctx, &grant_id)?;
            let updated_req = load_request_json(ctx, &request_id)?;
            ctx.log_event(
                "access_approved",
                Some(&req_agent_id),
                &json!({
                    "request_id": request_id,
                    "grant_id": grant_id,
                    "target_kind": final_kind,
                    "target_path": canon_str,
                    "scopes": final_scopes,
                    "lifetime": lifetime,
                }),
            );
            ctx.emit(
                "access:changed",
                json!({
                    "agent_id": req_agent_id,
                    "repo_id": source_repo_id,
                    "grant_id": grant_id,
                    "request_id": request_id,
                }),
            );
            crate::attention::refresh(ctx);
            if let Some(tx) = pending_waiters().lk().remove(&request_id) {
                let _ = tx.send(json!({
                    "status": "approved",
                    "grant": grant,
                }));
            }
            Ok(json!({ "request": updated_req, "grant": grant }))
        }
        _ => Err(format!("Invalid decision `{decision}`: expected `approve` or `deny`")),
    }
}

fn rpc_grants(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let agent_id = arg_opt_str(args, "agent_id").filter(|s| !s.is_empty());
    let repo_id = arg_opt_str(args, "repo_id").filter(|s| !s.is_empty());
    let include_revoked = args
        .get("include_revoked")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Some(aid) = agent_id {
        if !include_revoked {
            return Ok(Value::Array(list_active_grants(ctx, &aid)?));
        }
        let agent = load_agent(ctx, &aid);
        let strict = agent.as_ref().map(|a| a.strict).unwrap_or(false);
        let agent_repo = agent.and_then(|a| a.repo_id);
        let rows = match agent_repo.as_deref() {
            Some(rid) => ctx.db.query(
                "SELECT g.*, a.name AS agent_name
                 FROM access_grants g
                 LEFT JOIN agents a ON a.id = g.agent_id
                 WHERE g.agent_id = ?1 OR (g.agent_id IS NULL AND g.source_repo_id = ?2)
                 ORDER BY g.created_at DESC, g.id ASC",
                params![aid, rid],
            )?,
            None => ctx.db.query(
                "SELECT g.*, a.name AS agent_name
                 FROM access_grants g
                 LEFT JOIN agents a ON a.id = g.agent_id
                 WHERE g.agent_id = ?1
                 ORDER BY g.created_at DESC, g.id ASC",
                [aid],
            )?,
        };
        return Ok(Value::Array(
            rows.into_iter().map(|r| format_grant_row(r, strict)).collect(),
        ));
    }

    if let Some(rid) = repo_id {
        let sql = if include_revoked {
            "SELECT g.*, a.name AS agent_name
             FROM access_grants g
             LEFT JOIN agents a ON a.id = g.agent_id
             WHERE g.source_repo_id = ?1
             ORDER BY g.created_at DESC, g.id ASC"
        } else {
            "SELECT g.*, a.name AS agent_name
             FROM access_grants g
             LEFT JOIN agents a ON a.id = g.agent_id
             WHERE g.source_repo_id = ?1
               AND g.revoked_at IS NULL
               AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
             ORDER BY g.created_at DESC, g.id ASC"
        };
        let rows = ctx.db.query(sql, [rid])?;
        return Ok(Value::Array(
            rows.into_iter().map(|r| format_grant_row(r, false)).collect(),
        ));
    }

    let sql = if include_revoked {
        "SELECT g.*, a.name AS agent_name
         FROM access_grants g
         LEFT JOIN agents a ON a.id = g.agent_id
         ORDER BY g.created_at DESC, g.id ASC"
    } else {
        "SELECT g.*, a.name AS agent_name
         FROM access_grants g
         LEFT JOIN agents a ON a.id = g.agent_id
         WHERE g.revoked_at IS NULL
           AND (g.uses_remaining IS NULL OR g.uses_remaining > 0)
         ORDER BY g.created_at DESC, g.id ASC"
    };
    let rows = ctx.db.query(sql, [])?;
    Ok(Value::Array(
        rows.into_iter().map(|r| format_grant_row(r, false)).collect(),
    ))
}

fn rpc_grant_create(ctx: &Arc<Ctx>, args: &Value) -> Result<Value, String> {
    let target_kind_in = validate_kind(&arg_str(args, "target_kind")?)?;
    let agent_id_in = arg_opt_str(args, "agent_id").filter(|s| !s.is_empty());
    let mut source_repo_id = arg_opt_str(args, "source_repo_id").filter(|s| !s.is_empty());

    let mut agent_strict = false;
    if let Some(aid) = agent_id_in.as_deref() {
        let agent = load_agent(ctx, aid)
            .ok_or_else(|| format!("Unknown agent `{aid}`"))?;
        agent_strict = agent.strict;
        if source_repo_id.is_none() {
            source_repo_id = agent.repo_id;
        }
    }

    let lifetime = match arg_opt_str(args, "lifetime").filter(|s| !s.is_empty()) {
        Some(l) => validate_lifetime(&l)?,
        None => {
            if agent_id_in.is_some() {
                "session".to_string()
            } else {
                "persistent".to_string()
            }
        }
    };

    let (grant_agent_id, uses_remaining) = match lifetime.as_str() {
        "persistent" => {
            if source_repo_id.is_none() {
                return Err("Persistent grants require `source_repo_id`".into());
            }
            (None, None)
        }
        "session" => {
            let Some(aid) = agent_id_in.clone() else {
                return Err("Session grants require `agent_id`".into());
            };
            (Some(aid), None)
        }
        "once" => {
            let Some(aid) = agent_id_in.clone() else {
                return Err("One-time grants require `agent_id`".into());
            };
            (Some(aid), Some(1i64))
        }
        _ => unreachable!(),
    };

    let scopes = parse_scopes_arg(args.get("scopes").ok_or("missing `scopes`")?)?;
    let strict_allowed = args
        .get("strict_allowed")
        .and_then(Value::as_bool)
        .unwrap_or(agent_strict);

    let target_repo_id_in = arg_opt_str(args, "target_repo_id").filter(|s| !s.is_empty());
    let target_path_in = arg_opt_str(args, "target_path").filter(|s| !s.is_empty());
    let label_override = arg_opt_str(args, "label").filter(|s| !s.is_empty());

    let (final_kind, final_repo_id, _, canon_path, label) = resolve_target_for_grant(
        ctx,
        &target_kind_in,
        true,
        target_repo_id_in.as_deref(),
        target_path_in.as_deref(),
        label_override.as_deref(),
    )?;

    validate_scopes(&final_kind, &scopes, agent_strict)?;

    let grant_id = crate::db::new_id();
    let now = crate::db::now();
    let canon_str = canon_path.to_string_lossy().into_owned();
    let scopes_json = Value::Array(scopes.iter().map(|s| Value::String(s.clone())).collect()).to_string();

    ctx.db.exec(
        "INSERT INTO access_grants(
            id, agent_id, source_repo_id, target_kind, target_repo_id,
            target_path, label, scopes, lifetime, uses_remaining,
            strict_allowed, created_by, created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'user',?12)",
        params![
            grant_id,
            grant_agent_id,
            source_repo_id,
            final_kind,
            final_repo_id,
            canon_str,
            label,
            scopes_json,
            lifetime,
            uses_remaining,
            i64::from(strict_allowed),
            now,
        ],
    )?;

    let grant = load_grant_json(ctx, &grant_id)?;
    ctx.log_event(
        "access_granted",
        grant_agent_id.as_deref(),
        &json!({
            "grant_id": grant_id,
            "source_repo_id": source_repo_id,
            "target_kind": final_kind,
            "target_path": canon_str,
            "scopes": scopes,
            "lifetime": lifetime,
        }),
    );
    ctx.emit(
        "access:changed",
        json!({
            "agent_id": grant_agent_id,
            "repo_id": source_repo_id,
            "grant_id": grant_id,
        }),
    );
    Ok(grant)
}

fn rpc_revoke(ctx: &Arc<Ctx>, args: &Value) -> Result<Value, String> {
    let id = arg_str(args, "id")?;
    let existing = ctx.db.query(
        "SELECT id, agent_id, source_repo_id FROM access_grants WHERE id=?1",
        [&id],
    )?;
    let row = existing
        .first()
        .ok_or_else(|| format!("Access grant `{id}` not found"))?;
    let agent_id = row["agent_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let source_repo_id = row["source_repo_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let now = crate::db::now();
    ctx.db.exec(
        "UPDATE access_grants SET revoked_at=?1 WHERE id=?2 AND revoked_at IS NULL",
        params![now, id],
    )?;
    ctx.log_event("access_revoked", agent_id.as_deref(), &json!({ "grant_id": id }));
    ctx.emit(
        "access:changed",
        json!({
            "agent_id": agent_id,
            "repo_id": source_repo_id,
            "grant_id": id,
        }),
    );
    Ok(json!({ "id": id, "revoked": true }))
}

fn rpc_catalog(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let rows = match arg_opt_str(args, "exclude_repo_id").filter(|s| !s.is_empty()) {
        Some(ex) => ctx.db.query(
            "SELECT id, name, path, default_branch FROM repos WHERE id != ?1 ORDER BY LOWER(name) ASC, name ASC",
            [ex],
        )?,
        None => ctx.db.query(
            "SELECT id, name, path, default_branch FROM repos ORDER BY LOWER(name) ASC, name ASC",
            [],
        )?,
    };
    Ok(Value::Array(rows))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "pending" => rpc_pending(ctx, &args),
        "decide" => rpc_decide(ctx, &args),
        "grants" => rpc_grants(ctx, &args),
        "grant_create" => rpc_grant_create(ctx, &args),
        "revoke" => rpc_revoke(ctx, &args),
        "catalog" => rpc_catalog(ctx, &args),
        _ => Err(format!("access.{method}: unknown method")),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::test_dir;

    fn setup_ctx() -> Arc<Ctx> {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        ctx
    }

    fn seed_repo(ctx: &Ctx, id: &str, name: &str, path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        ctx.db
            .exec(
                "INSERT INTO repos(id, name, path, default_branch, added_at) VALUES(?1,?2,?3,'main',0)",
                params![id, name, path.to_string_lossy().as_ref()],
            )
            .unwrap();
    }

    fn seed_agent(ctx: &Ctx, id: &str, name: &str, repo_id: Option<&str>, strict: bool, model_ref: &str) {
        ctx.db
            .exec(
                "INSERT INTO agents(id, name, model_ref, repo_id, strict, created_at, last_active_at)
                 VALUES(?1,?2,?3,?4,?5,0,0)",
                params![id, name, model_ref, repo_id, i64::from(strict)],
            )
            .unwrap();
    }

    async fn wait_for_pending(ctx: &Ctx, agent_id: &str) -> Value {
        for _ in 0..100 {
            let list = rpc_pending(ctx, &json!({ "agent_id": agent_id })).unwrap();
            if let Some(first) = list.as_array().and_then(|a| a.first()).cloned() {
                return first;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for pending access request for {agent_id}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blocking_request_approve_and_deny() {
        let ctx = setup_ctx();
        let base = test_dir("access-approve-deny");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        seed_agent(&ctx, "ag-1", "worker-1", Some("r-a"), false, "anthropic/claude-sonnet");

        // 1. Concurrent approve
        let c1 = ctx.clone();
        let req_task = tokio::spawn(async move {
            request_access(&c1, "ag-1", "Beta", None, vec![], "Need Beta API types", 30).await
        });

        let pending = wait_for_pending(&ctx, "ag-1").await;
        assert_eq!(pending_count(&ctx), 1);
        assert_eq!(pending["target_kind"], "repo");
        assert_eq!(pending["resolved_repo_id"], "r-b");
        assert_eq!(pending["provider"], "anthropic");
        assert_eq!(pending["strict"], false);
        assert_eq!(pending["requested_scopes"], json!(["read", "code"]));

        let req_id = pending["id"].as_str().unwrap().to_string();
        let decided = call(
            &ctx,
            "decide",
            json!({
                "request_id": req_id,
                "decision": "approve",
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();
        assert_eq!(decided["request"]["status"], "approved");
        assert_eq!(decided["grant"]["target_repo_id"], "r-b");
        assert_eq!(pending_count(&ctx), 0);

        let res = req_task.await.unwrap().unwrap();
        assert_eq!(res["status"], "approved");
        assert_eq!(res["grant"]["target_repo_id"], "r-b");
        assert_eq!(res["grant"]["scopes"], json!(["read", "code"]));

        // 2. Concurrent deny on a folder
        let folder = base.join("notes");
        std::fs::create_dir_all(&folder).unwrap();
        let c2 = ctx.clone();
        let f_str = folder.to_string_lossy().into_owned();
        let deny_task = tokio::spawn(async move {
            request_access(&c2, "ag-1", &f_str, Some("folder"), vec!["read".into()], "Read notes", 30).await
        });

        let pending2 = wait_for_pending(&ctx, "ag-1").await;
        let req_id2 = pending2["id"].as_str().unwrap().to_string();
        let denied = call(
            &ctx,
            "decide",
            json!({
                "request_id": req_id2,
                "decision": "deny",
                "note": "Not needed for this task",
            }),
        )
        .await
        .unwrap();
        assert_eq!(denied["request"]["status"], "denied");
        assert_eq!(denied["request"]["decision_note"], "Not needed for this task");

        let res2 = deny_task.await.unwrap().unwrap();
        assert_eq!(res2["status"], "denied");
        assert_eq!(res2["request_id"], req_id2);
        assert_eq!(res2["note"], "Not needed for this task");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn already_granted_fast_path_skips_prompt() {
        let ctx = setup_ctx();
        let base = test_dir("access-fast-path");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        let specs = base.join("specs");
        let spec_file = specs.join("q3.md");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        std::fs::create_dir_all(&specs).unwrap();
        std::fs::write(&spec_file, "# Q3").unwrap();
        seed_agent(&ctx, "ag-1", "worker-1", Some("r-a"), false, "openai/gpt-5");

        // Pre-grant repo Beta with read + code
        let grant = call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-1",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "code"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();

        // Requesting the same repo & subset of scopes returns immediately with already_granted
        let fast = request_access(&ctx, "ag-1", "beta", None, vec!["read".into()], "check file", 30)
            .await
            .unwrap();
        assert_eq!(fast["status"], "already_granted");
        assert_eq!(fast["grant"]["id"], grant["id"]);
        assert_eq!(pending_count(&ctx), 0);

        // Pre-grant folder specs; requesting a file inside specs also hits fast-path
        call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-1",
                "target_kind": "folder",
                "target_path": specs.to_string_lossy(),
                "scopes": ["read"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();

        let fast_sub = request_access(
            &ctx,
            "ag-1",
            spec_file.to_str().unwrap(),
            None,
            vec!["read".into()],
            "read q3",
            30,
        )
        .await
        .unwrap();
        assert_eq!(fast_sub["status"], "already_granted");
        assert_eq!(pending_count(&ctx), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn human_picked_target_override_for_vague_hint() {
        let ctx = setup_ctx();
        let base = test_dir("access-vague-hint");
        let repo_a = base.join("repo-a");
        let actual_specs = base.join("q3-specs-real");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        std::fs::create_dir_all(&actual_specs).unwrap();
        seed_agent(&ctx, "ag-1", "worker-1", Some("r-a"), false, "anthropic/claude");

        let c = ctx.clone();
        let task = tokio::spawn(async move {
            request_access(&c, "ag-1", "Q3 specs", None, vec!["read".into()], "Need Q3 product specs", 30).await
        });

        let pending = wait_for_pending(&ctx, "ag-1").await;
        assert_eq!(pending["target_hint"], "Q3 specs");
        assert!(pending["resolved_path"].is_null());
        assert!(pending["resolved_repo_id"].is_null());

        let req_id = pending["id"].as_str().unwrap().to_string();
        // Approving without providing a target fails cleanly
        assert!(
            call(&ctx, "decide", json!({ "request_id": req_id, "decision": "approve" }))
                .await
                .is_err()
        );

        // Human picks the actual folder on disk
        let decided = call(
            &ctx,
            "decide",
            json!({
                "request_id": req_id,
                "decision": "approve",
                "target_kind": "folder",
                "target_path": actual_specs.to_string_lossy(),
                "label": "Q3 Specs",
                "scopes": ["read"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();
        let canon = std::fs::canonicalize(&actual_specs).unwrap();
        assert_eq!(decided["grant"]["target_path"], canon.to_string_lossy().as_ref());
        assert_eq!(decided["grant"]["label"], "Q3 Specs");

        let res = task.await.unwrap().unwrap();
        assert_eq!(res["status"], "approved");
        assert_eq!(res["grant"]["target_path"], canon.to_string_lossy().as_ref());

        // Resolve by human-chosen label (case-insensitive)
        let resolved = resolve_grant(&ctx, "ag-1", "q3 specs", "read", false).unwrap();
        assert_eq!(resolved.target_path, canon);
    }

    #[test]
    fn blocked_roots_and_allowed_subfolders() {
        let home = home_dir();
        assert!(is_blocked_root(Path::new("/")).is_some());
        assert!(is_blocked_root(&home).is_some());
        assert!(is_blocked_root(Path::new("~")).is_some());
        assert!(is_blocked_root(Path::new("~/.ssh")).is_some());
        assert!(is_blocked_root(Path::new("~/.ssh/id_ed25519")).is_some());
        assert!(is_blocked_root(Path::new("~/.aws")).is_some());
        assert!(is_blocked_root(Path::new("~/.aws/credentials")).is_some());
        assert!(is_blocked_root(Path::new("~/Documents")).is_some());
        assert!(is_blocked_root(Path::new("~/Library/Keychains")).is_some());
        assert!(is_blocked_root(Path::new("~/Library/Application Support/dev.gunnarsson.philium")).is_some());
        assert!(is_blocked_root(&home.join("Documents/..")).is_some());

        // Secret vault must be blocked
        let vault_base = test_dir("access-vault-check");
        crate::secrets::init(&vault_base);
        let vault_dir = crate::secrets::dir().expect("secrets dir initialized");
        assert!(is_blocked_root(vault_dir).is_some());
        assert!(is_blocked_root(&vault_dir.join("vault.json")).is_some());

        // Subfolders inside personal dirs and dedicated temp dirs ARE allowed
        assert_eq!(is_blocked_root(Path::new("~/Documents/specs")), None);
        assert_eq!(is_blocked_root(&home.join("Documents/specs/q3")), None);
        assert_eq!(is_blocked_root(Path::new("~/Philium/my-project")), None);
        let allowed_tmp = test_dir("access-allowed-tmp");
        assert_eq!(is_blocked_root(&allowed_tmp), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn strict_mode_enforces_scope_and_persistent_isolation() {
        let ctx = setup_ctx();
        let base = test_dir("access-strict");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        seed_agent(&ctx, "ag-strict", "strict-1", Some("r-a"), true, "untrusted/model-x");
        seed_agent(&ctx, "ag-std", "std-1", Some("r-a"), false, "anthropic/claude");

        // Strict request for `context` or `communicate` is hard-denied
        assert!(
            request_access(&ctx, "ag-strict", "r-b", None, vec!["context".into()], "notes", 10)
                .await
                .is_err()
        );
        assert!(
            request_access(&ctx, "ag-strict", "r-b", None, vec!["communicate".into()], "ping", 10)
                .await
                .is_err()
        );

        // Direct grant_create for strict agent also rejects `context` and `communicate`
        assert!(
            call(
                &ctx,
                "grant_create",
                json!({
                    "agent_id": "ag-strict",
                    "target_kind": "repo",
                    "target_repo_id": "r-b",
                    "scopes": ["read", "context"],
                    "lifetime": "session",
                }),
            )
            .await
            .is_err()
        );

        // Persistent grant with strict_allowed = false: visible to Standard, invisible to Strict
        let p_std = call(
            &ctx,
            "grant_create",
            json!({
                "source_repo_id": "r-a",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "code", "context", "communicate"],
                "lifetime": "persistent",
                "strict_allowed": false,
            }),
        )
        .await
        .unwrap();
        assert_eq!(p_std["strict_allowed"], false);

        assert!(resolve_grant(&ctx, "ag-std", "r-b", "context", false).is_ok());
        assert!(resolve_grant(&ctx, "ag-strict", "r-b", "read", false).is_err());
        assert!(list_active_grants(&ctx, "ag-strict").unwrap().is_empty());

        // Persistent grant with strict_allowed = true: Strict gets read/code, NEVER context/communicate
        call(
            &ctx,
            "grant_create",
            json!({
                "source_repo_id": "r-a",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "code", "context", "communicate"],
                "lifetime": "persistent",
                "strict_allowed": true,
            }),
        )
        .await
        .unwrap();

        let strict_grants = list_active_grants(&ctx, "ag-strict").unwrap();
        assert_eq!(strict_grants.len(), 1);
        assert_eq!(strict_grants[0]["scopes"], json!(["read", "code"]));
        assert!(resolve_grant(&ctx, "ag-strict", "r-b", "read", false).is_ok());
        assert!(resolve_grant(&ctx, "ag-strict", "r-b", "code", false).is_ok());
        assert!(resolve_grant(&ctx, "ag-strict", "r-b", "context", false).is_err());
        assert!(resolve_grant(&ctx, "ag-strict", "r-b", "communicate", false).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_scope_rejected_on_repo_allowed_on_folder() {
        let ctx = setup_ctx();
        let base = test_dir("access-write-scope");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        let shared = base.join("shared-docs");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        std::fs::create_dir_all(&shared).unwrap();
        seed_agent(&ctx, "ag-1", "worker-1", Some("r-a"), false, "anthropic/claude");

        // Requesting `write` on a repo fails
        assert!(
            request_access(&ctx, "ag-1", "r-b", None, vec!["read".into(), "write".into()], "edit", 10)
                .await
                .is_err()
        );
        // Creating a repo grant with `write` fails
        assert!(
            call(
                &ctx,
                "grant_create",
                json!({
                    "agent_id": "ag-1",
                    "target_kind": "repo",
                    "target_repo_id": "r-b",
                    "scopes": ["read", "write"],
                    "lifetime": "session",
                }),
            )
            .await
            .is_err()
        );
        // Creating a folder grant with `code`/`context`/`communicate` fails
        for bad in ["code", "context", "communicate"] {
            assert!(
                call(
                    &ctx,
                    "grant_create",
                    json!({
                        "agent_id": "ag-1",
                        "target_kind": "folder",
                        "target_path": shared.to_string_lossy(),
                        "scopes": ["read", bad],
                        "lifetime": "session",
                    }),
                )
                .await
                .is_err()
            );
        }
        // Folder grant with `read` + `write` succeeds
        let g = call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-1",
                "target_kind": "folder",
                "target_path": shared.to_string_lossy(),
                "scopes": ["read", "write"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();
        assert_eq!(g["target_kind"], "folder");
        assert_eq!(g["scopes"], json!(["read", "write"]));
        let resolved = resolve_grant(&ctx, "ag-1", g["id"].as_str().unwrap(), "write", false).unwrap();
        assert_eq!(resolved.target_kind, "folder");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn once_grant_auto_revokes_and_cleanup_agent_cancels_and_revokes() {
        let ctx = setup_ctx();
        let base = test_dir("access-once-cleanup");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        let folder_1 = base.join("f1");
        let folder_2 = base.join("f2");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        std::fs::create_dir_all(&folder_1).unwrap();
        std::fs::create_dir_all(&folder_2).unwrap();
        seed_agent(&ctx, "ag-1", "worker-1", Some("r-a"), false, "anthropic/claude");

        // 1. One-time grant
        let once_g = call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-1",
                "target_kind": "folder",
                "target_path": folder_1.to_string_lossy(),
                "scopes": ["read"],
                "lifetime": "once",
            }),
        )
        .await
        .unwrap();
        let once_id = once_g["id"].as_str().unwrap();
        assert_eq!(once_g["uses_remaining"], 1);

        // Non-consuming resolve does not revoke
        assert!(resolve_grant(&ctx, "ag-1", once_id, "read", false).is_ok());
        // Consuming resolve succeeds once
        assert!(resolve_grant(&ctx, "ag-1", once_id, "read", true).is_ok());
        // Second resolve fails because grant auto-revoked
        assert!(resolve_grant(&ctx, "ag-1", once_id, "read", true).is_err());

        // 2. Session grant + persistent grant + pending request -> cleanup_agent
        let sess_g = call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-1",
                "target_kind": "folder",
                "target_path": folder_2.to_string_lossy(),
                "scopes": ["read"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();
        let pers_g = call(
            &ctx,
            "grant_create",
            json!({
                "source_repo_id": "r-a",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "code"],
                "lifetime": "persistent",
            }),
        )
        .await
        .unwrap();

        let c = ctx.clone();
        let pending_task = tokio::spawn(async move {
            request_access(&c, "ag-1", "unmapped hint", None, vec!["read".into()], "pending", 30).await
        });
        let _ = wait_for_pending(&ctx, "ag-1").await;
        assert_eq!(pending_count(&ctx), 1);

        cleanup_agent(&ctx, "ag-1");

        // Pending request woke up as cancelled
        let cancelled = pending_task.await.unwrap().unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(pending_count(&ctx), 0);

        // Session grant was revoked, persistent repo grant survived
        assert!(resolve_grant(&ctx, "ag-1", sess_g["id"].as_str().unwrap(), "read", false).is_err());
        assert!(resolve_grant(&ctx, "ag-1", pers_g["id"].as_str().unwrap(), "read", false).is_ok());

        // 3. Stale pending rows are expired on re-init
        ctx.db
            .exec(
                "INSERT INTO access_requests(
                    id, agent_id, agent_name, target_kind, target_hint,
                    requested_scopes, reason, status, created_at
                 ) VALUES('stale-1','ag-1','worker-1','folder','hint','[\"read\"]','crash','pending',0)",
                [],
            )
            .unwrap();
        assert_eq!(pending_count(&ctx), 1);
        init(&ctx).unwrap();
        assert_eq!(pending_count(&ctx), 0);
        let stale = load_request_json(&ctx, "stale-1").unwrap();
        assert_eq!(stale["status"], "expired");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn can_communicate_prompt_catalog_and_revoke() {
        let ctx = setup_ctx();
        let base = test_dir("access-comm-misc");
        let repo_a = base.join("repo-a");
        let repo_b = base.join("repo-b");
        let repo_c = base.join("repo-c");
        seed_repo(&ctx, "r-a", "Alpha", &repo_a);
        seed_repo(&ctx, "r-b", "Beta", &repo_b);
        seed_repo(&ctx, "r-c", "Gamma", &repo_c);
        seed_agent(&ctx, "ag-std", "std-1", Some("r-a"), false, "anthropic/claude");
        seed_agent(&ctx, "ag-strict", "strict-1", Some("r-a"), true, "untrusted/m");

        // Initially no communicate permission and no prompt
        assert!(!can_communicate(&ctx, "ag-std", "r-b"));
        assert!(!can_communicate(&ctx, "ag-strict", "r-b"));
        assert!(active_grants_prompt(&ctx, "ag-std").is_none());

        // Grant read+code only (no communicate)
        let g1 = call(
            &ctx,
            "grant_create",
            json!({
                "agent_id": "ag-std",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "code"],
                "lifetime": "session",
            }),
        )
        .await
        .unwrap();
        assert!(!can_communicate(&ctx, "ag-std", "r-b"));

        // Grant communicate via persistent repo link (even with strict_allowed=true, strict stays false!)
        let g2 = call(
            &ctx,
            "grant_create",
            json!({
                "source_repo_id": "r-a",
                "target_kind": "repo",
                "target_repo_id": "r-b",
                "scopes": ["read", "communicate"],
                "lifetime": "persistent",
                "strict_allowed": true,
            }),
        )
        .await
        .unwrap();
        assert!(can_communicate(&ctx, "ag-std", "r-b"));
        assert!(!can_communicate(&ctx, "ag-strict", "r-b"));
        assert!(!can_communicate(&ctx, "ag-std", "r-c"));

        // Prompt contains active grant IDs, labels, kinds, scopes
        let prompt = active_grants_prompt(&ctx, "ag-std").unwrap();
        assert!(prompt.contains(g1["id"].as_str().unwrap()));
        assert!(prompt.contains(g2["id"].as_str().unwrap()));
        assert!(prompt.contains("Beta"));
        assert!(prompt.contains("communicate"));

        // Catalog excludes source repo
        let cat = call(&ctx, "catalog", json!({ "exclude_repo_id": "r-a" }))
            .await
            .unwrap();
        let names: Vec<&str> = cat
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert_eq!(names, vec!["Beta", "Gamma"]);

        // Revoking g2 revokes communicate permission
        let rev = call(&ctx, "revoke", json!({ "id": g2["id"] })).await.unwrap();
        assert_eq!(rev["revoked"], true);
        assert!(!can_communicate(&ctx, "ag-std", "r-b"));

        // `grants` RPC respects include_revoked and joins agent_name
        let active_repo_grants = call(&ctx, "grants", json!({ "repo_id": "r-a" })).await.unwrap();
        assert_eq!(active_repo_grants.as_array().unwrap().len(), 1);
        assert_eq!(active_repo_grants[0]["agent_name"], "std-1");
        let all_repo_grants = call(&ctx, "grants", json!({ "repo_id": "r-a", "include_revoked": true }))
            .await
            .unwrap();
        assert_eq!(all_repo_grants.as_array().unwrap().len(), 2);

        // Blocked roots are rejected in both request_access and grant_create
        assert!(request_access(&ctx, "ag-std", "~/.ssh", None, vec!["read".into()], "keys", 10).await.is_err());
        assert!(request_access(&ctx, "ag-std", "/", None, vec!["read".into()], "root", 10).await.is_err());
        assert!(
            call(
                &ctx,
                "grant_create",
                json!({
                    "agent_id": "ag-std",
                    "target_kind": "folder",
                    "target_path": "/",
                    "scopes": ["read"],
                }),
            )
            .await
            .is_err()
        );
    }
}
