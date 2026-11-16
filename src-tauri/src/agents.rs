//! Agents: named workers, one HAL process each (SPEC 5.1, 6.1, 7.2).
//!
//! Every agent gets: its own HAL (own data dir and session), its own gateway
//! token (attribution + provider allowlist), its own MCP token (Philium's agent
//! tools), its own worktree when it works on a repo, a port block, a scratch dir
//! and a TMPDIR. Agents on untrusted providers always run Strict: Seatbelt
//! sandbox, egress proxy, no subscriptions, only that provider.
//!
//! HAL's built-in shell is denied by permission rules, so every command goes
//! through Philium's `shell_run` MCP tool (visible terminal, attributed, reapable).
//!
//! rpc:
//!   list {}                                   -> [agent]
//!   get {id}                                  -> agent + live info
//!   spawn {name?, role?, model?, repo_id?, task_id?, brief?, parent_id?, strict?} -> agent
//!   send {id, text}                           -> HAL turn ids
//!   cancel {id}                               -> stop current turn
//!   stop {id}                                 -> stop HAL, keep agent + worktree
//!   resume {id}                               -> restart a stopped agent's HAL, same session
//!   remove {id, force?}                       -> teardown (may need confirmation) + delete
//!   events {id, after?, limit?}               -> HAL event replay
//!   messages {id}                             -> HAL conversation
//!   permissions {id}                          -> pending approvals
//!   permission {id, request_id, decision: approve|deny, scope?}
//!   set_model {id, model}
//!   diff {id}                                 -> git diff of the agent's workspace
//!   names {}                                  -> suggested free names
//! events: `agents:changed` {id}, `agents:events` {id, events:[...]} (batched ~10 Hz)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

use crate::ctx::{self, Ctx};
use crate::db::{new_id, now};
use crate::hal::{self, Hal};
use crate::providers::{self, ConfigOpts};

/// Live state for one running agent (not persisted).
pub struct Runtime {
    pub hal: Arc<Hal>,
    pub session: String,
    pub name: String,
    pub cwd: PathBuf,
    pub strict: bool,
    /// Environment for commands the agent runs (shell_run terminals).
    pub env: Vec<(String, String)>,
    /// Seatbelt policy for Strict agents; commands they run are wrapped too.
    pub policy: Option<crate::sandbox::Policy>,
    pump: tokio::task::JoinHandle<()>,
}

type Runtimes = Mutex<HashMap<String, Arc<Runtime>>>;

fn runtimes() -> &'static Runtimes {
    static R: OnceLock<Runtimes> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn runtime(id: &str) -> Option<Arc<Runtime>> {
    runtimes().lock().unwrap().get(id).cloned()
}

const NAMES: &[&str] = &[
    "lion", "otter", "falcon", "lynx", "heron", "orca", "badger", "raven", "ibis", "puma", "wren", "bison",
    "gecko", "manta", "koala", "panda", "tapir", "okapi", "egret", "dingo", "marten", "condor", "jaguar",
    "narwhal", "osprey", "quokka", "stoat", "toucan", "walrus", "yak", "zebra", "beaver", "cobra", "dove",
    "eagle", "ferret", "gazelle", "hare", "impala", "jackal", "kestrel", "lemur", "moose", "newt", "owl",
    "pelican", "robin", "seal", "tiger", "vole", "wolf", "fox", "crane", "finch", "mole", "moth", "swan",
];

/// Agent names are lowercase slugs: they appear in paths, branches, env vars.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !n.starts_with('-')
        && !matches!(n, "you" | "all" | "philium" | "user" | "system")
}

pub fn free_names(ctx: &Ctx, n: usize) -> Vec<String> {
    let taken: Vec<String> = ctx
        .db
        .query("SELECT name FROM agents", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    let mut out = vec![];
    for round in 0.. {
        for base in NAMES {
            let name = if round == 0 { base.to_string() } else { format!("{base}-{}", round + 1) };
            if !taken.contains(&name) && !out.contains(&name) {
                out.push(name);
                if out.len() >= n {
                    return out;
                }
            }
        }
    }
    out
}

pub fn get_row(ctx: &Ctx, id_or_name: &str) -> Option<Value> {
    ctx.db
        .query("SELECT * FROM agents WHERE id=?1 OR name=?1", [id_or_name])
        .ok()?
        .into_iter()
        .next()
}

fn set_state(ctx: &Ctx, id: &str, state: &str) {
    let _ = ctx.db.exec(
        "UPDATE agents SET state=?2, last_active_at=?3 WHERE id=?1",
        rusqlite::params![id, state, now()],
    );
    ctx.emit("agents:changed", json!({ "id": id, "state": state }));
}

/// States (SPEC 5.1). `idle` = running HAL, no turn in progress.
pub const LIVE: &[&str] = &["provisioning", "running", "idle", "waiting"];

/// Pick a free block of 20 ports, starting at 42000.
fn port_base(ctx: &Ctx) -> i64 {
    let used: Vec<i64> = ctx
        .db
        .query("SELECT port_base FROM agents WHERE port_base IS NOT NULL", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r["port_base"].as_i64())
        .collect();
    (0..).map(|i| 42000 + i * 20).find(|p| !used.contains(p)).unwrap()
}

/// HAL permission rules for an agent. Last matching rule wins.
pub fn permission_rules(strict: bool) -> Value {
    let mut rules = vec![
        json!({"action": "read", "resource": "*", "effect": "allow"}),
        // Secrets never readable: they could end up in a provider request.
        json!({"action": "read", "resource": "**/.env*", "effect": "deny"}),
        json!({"action": "read", "resource": "**/*.pem", "effect": "deny"}),
        json!({"action": "read", "resource": "**/*.key", "effect": "deny"}),
        json!({"action": "read", "resource": "**/id_*", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.ssh/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.aws/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.kube/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.config/gh/**", "effect": "deny"}),
        // Edits inside the workspace (relative paths) are fine; absolute
        // paths are outside it and ask; parent escapes are denied.
        json!({"action": "edit", "resource": "**", "effect": "allow"}),
        json!({"action": "edit", "resource": "/**", "effect": "ask"}),
        json!({"action": "edit", "resource": "../**", "effect": "deny"}),
        json!({"action": "edit", "resource": "**/.env*", "effect": "deny"}),
        // All execution goes through Philium (shell_run): visible and reapable.
        json!({"action": "shell", "resource": "*", "effect": "deny"}),
        json!({"action": "mcp", "resource": "philium/*", "effect": "allow"}),
        json!({"action": "subagent", "resource": "*", "effect": "allow"}),
        json!({"action": "browser", "resource": "*", "effect": "deny"}),
    ];
    if strict {
        // Untrusted provider: anything leaving the worktree always asks.
        rules.push(json!({"action": "read", "resource": "/**", "effect": "ask"}));
        rules.push(json!({"action": "subagent", "resource": "*", "effect": "deny"}));
    }
    json!({ "default": "ask", "ask_timeout_secs": 3600, "rules": rules })
}

fn instructions(name: &str, strict: bool, worktree: Option<&Path>) -> String {
    let mut s = format!(
        "You are agent \"{name}\" inside Philium, a workstation where many agents work in parallel.\n\
         Rules:\n\
         - Run every command with the philium shell_run tool (it opens a visible terminal). The built-in shell is disabled.\n\
         - Coordinate through the master board (board_* tools) and mail (mail_* tools). Claim a task before working on it and keep its lease alive.\n\
         - Follow the playbook for each job (playbook_get). Finished work lands as a GitHub pull request.\n\
         - Never add Co-authored-by trailers, \"Generated with\" lines or agent signatures to commits or PRs.\n\
         - Use the port range in $PHILIUM_PORT_BASE (20 ports) for servers. Put throwaway files in $TMPDIR.\n\
         - Register anything you create outside Philium's tools with resource_register so it is cleaned up.\n"
    );
    if let Some(w) = worktree {
        s.push_str(&format!("- Your git worktree is {} on your own branch. Stay inside it.\n", w.display()));
    }
    if strict {
        s.push_str("- You run in Strict mode: sandboxed, network only through Philium's proxy to allowed hosts.\n");
    }
    s
}

pub struct SpawnReq {
    pub name: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub repo_id: Option<String>,
    pub task_id: Option<String>,
    pub brief: Option<String>,
    pub parent_id: Option<String>,
    pub strict: bool,
}

/// Create the agent row and start provisioning in the background.
pub fn spawn(ctx: &Arc<Ctx>, req: SpawnReq) -> Result<Value, String> {
    let (provider, model) = providers::resolve(ctx, req.model.as_deref())?;
    let model_ref = format!("{}/{}", provider.id, model);
    let strict = req.strict || !provider.trusted;
    if strict && provider.kind.is_subscription() {
        return Err("subscriptions cannot run in Strict mode".into());
    }
    let name = match req.name.filter(|n| !n.trim().is_empty()) {
        Some(n) => {
            let n = n.trim().to_lowercase();
            if !valid_name(&n) {
                return Err("names use a-z, 0-9 and -, up to 32 characters".into());
            }
            if get_row(ctx, &n).is_some() {
                return Err(format!("an agent named {n} already exists"));
            }
            n
        }
        None => free_names(ctx, 1).remove(0),
    };
    if let Some(r) = &req.repo_id {
        if ctx.db.query("SELECT id FROM repos WHERE id=?1", [r])?.is_empty() {
            return Err("unknown repo".into());
        }
    }
    let id = new_id();
    let t = now();
    ctx.db.exec(
        "INSERT INTO agents(id,name,role,model_ref,state,parent_id,repo_id,task_id,strict,port_base,created_at,last_active_at)
         VALUES(?1,?2,?3,?4,'provisioning',?5,?6,?7,?8,?9,?10,?10)",
        rusqlite::params![
            id,
            name,
            req.role.as_deref().unwrap_or("worker"),
            model_ref,
            req.parent_id,
            req.repo_id,
            req.task_id,
            strict as i64,
            port_base(ctx),
            t
        ],
    )?;
    ctx.log_event("agent_spawned", Some(&id), &json!({ "name": name, "model": model_ref, "strict": strict }));
    ctx.emit("agents:changed", json!({ "id": id }));
    let c = ctx.clone();
    let id2 = id.clone();
    let brief = req.brief.clone();
    tokio::spawn(async move {
        if let Err(e) = provision(&c, &id2, true, brief).await {
            let _ = c.db.kv_set(&format!("agent_error.{id2}"), &e);
            c.log_event("agent_failed", Some(&id2), &json!({ "error": e }));
            set_state(&c, &id2, "failed");
        }
    });
    Ok(get_row(ctx, &id).unwrap_or(Value::Null))
}

/// Bring up the agent's HAL (fresh or resume) and its event pump.
async fn provision(ctx: &Arc<Ctx>, id: &str, fresh: bool, brief: Option<String>) -> Result<(), String> {
    let row = get_row(ctx, id).ok_or("agent vanished")?;
    let name = row["name"].as_str().unwrap_or_default().to_string();
    let model_ref = row["model_ref"].as_str().unwrap_or_default().to_string();
    let strict = row["strict"].as_i64() == Some(1);
    let port = row["port_base"].as_i64().unwrap_or(42000);
    let (provider, _) = providers::resolve(ctx, Some(&model_ref))?;
    if strict {
        crate::sandbox::available()?;
    }

    // Dirs (scratch is a ledger resource deleted with the agent).
    let dir = ctx.agent_dir(&name);
    let (tmp, scratch, hal_dir) = (dir.join("tmp"), dir.join("scratch"), dir.join("hal"));
    for d in [&tmp, &scratch, &hal_dir, &scratch.join("home")] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    if fresh {
        crate::reaper::register(ctx, Some(id), "scratch_dir", &dir.to_string_lossy(), json!({ "agent": name }));
    }

    // Worktree.
    let mut worktree: Option<PathBuf> = None;
    if let Some(repo) = row["repo_id"].as_str() {
        let existing = ctx
            .db
            .query("SELECT path FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [id])?;
        worktree = match existing.first().and_then(|r| r["path"].as_str()) {
            Some(p) => Some(PathBuf::from(p)),
            None => {
                let (c, repo, n, aid) = (ctx.clone(), repo.to_string(), name.clone(), id.to_string());
                let wt = tokio::task::spawn_blocking(move || {
                    crate::workspace::create_worktree(&c, &repo, Some(&aid), &n, None)
                })
                .await
                .map_err(|e| e.to_string())??;
                Some(wt.path)
            }
        };
    }
    let cwd = worktree.clone().unwrap_or_else(|| scratch.clone());

    // Tokens: gateway (model traffic, attribution) and MCP (agent tools).
    let only = if strict { Some(vec![provider.id.clone()]) } else { None };
    let gw_token = ctx.gateway.issue_token(&name, only.clone());
    let mcp_token = crate::mcp::issue_token(id);

    let setup = providers::hal_setup(
        ctx,
        &hal_dir.join("hal"),
        &ConfigOpts { token: Some(gw_token), only, no_subscriptions: strict, default_ref: Some(model_ref.clone()) },
    );
    let mut config = setup.config;
    config["server"]["workspace"] = json!(cwd);
    config["permissions"] = permission_rules(strict);
    config["limits"] = json!({ "workspace": { "allow_outside": false } });
    config["prompt"] = json!({ "extra": instructions(&name, strict, worktree.as_deref()) });
    if let Some(url) = ctx.mcp_url.get() {
        config["mcp"] = json!({ "servers": { "philium": {
            "transport": "streamable_http",
            "url": url,
            "headers": { "Authorization": format!("Bearer {mcp_token}") },
            "timeout_secs": 900,
        }}});
    }

    let path = format!(
        "{}:{}",
        ctx.data_dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())
    );
    let mut env = vec![
        ("PHILIUM_AGENT".to_string(), name.clone()),
        ("PHILIUM_PORT_BASE".into(), port.to_string()),
        ("PORT".into(), port.to_string()),
        ("TMPDIR".into(), tmp.to_string_lossy().into()),
        ("KUBECONFIG".into(), dir.join("kubeconfig").to_string_lossy().into()),
        ("COMPOSE_PROJECT_NAME".into(), format!("philium-{name}")),
        ("PATH".into(), path),
    ];
    if strict {
        env.extend(crate::sandbox::env(ctx, &name));
    }
    let mut launch_env = env.clone();
    launch_env.extend(setup.env);

    let policy = strict.then(|| crate::sandbox::Policy {
        workdir: cwd.clone(),
        scratch: dir.clone(),
        hal_dir: hal_dir.clone(),
        read_only: worktree
            .as_ref()
            .and_then(|_| row["repo_id"].as_str())
            .and_then(|r| crate::workspace::repo(ctx, r))
            .and_then(|r| r["path"].as_str().map(PathBuf::from))
            .into_iter()
            .collect(),
        allow_hosts: vec![],
    });

    let (cfg, hd, cwd2, allow_logins) = (config, hal_dir.clone(), cwd.clone(), setup.allow_logins);
    let wrap_policy = policy.clone();
    let hal = tokio::task::spawn_blocking(move || {
        Hal::start(hal::Launch {
            dir: &hd,
            config: cfg,
            cwd: Some(&cwd2),
            env: launch_env,
            allow_logins,
            wrap: wrap_policy.map(|p| {
                let w = crate::sandbox::wrap(&p);
                Box::new(move |b: &Path| w(b)) as Box<dyn Fn(&Path) -> (PathBuf, Vec<String>)>
            }),
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let hal = Arc::new(hal);
    if fresh {
        crate::reaper::register(ctx, Some(id), "process", &hal.pid.to_string(), json!({ "what": "hal" }));
    }

    // Session: reuse on resume (HAL keeps it in its own SQLite).
    let session = match row["hal_session_id"].as_str().filter(|_| !fresh) {
        Some(s) if hal.call(reqwest::Method::GET, &format!("/sessions/{s}"), None).await.is_ok() => s.to_string(),
        _ => {
            let s = hal
                .call(
                    reqwest::Method::POST,
                    "/sessions",
                    Some(json!({ "workspace": cwd, "title": name, "model": model_ref })),
                )
                .await?;
            s["id"].as_str().ok_or("HAL returned no session id")?.to_string()
        }
    };
    ctx.db.exec(
        "UPDATE agents SET hal_session_id=?2, token_hash=?3 WHERE id=?1",
        rusqlite::params![id, session, short_hash(&mcp_token)],
    )?;

    let pump = tokio::spawn(pump_events(ctx.clone(), id.to_string(), hal.clone(), session.clone()));
    let rt = Arc::new(Runtime { hal: hal.clone(), session: session.clone(), name: name.clone(), cwd, strict, env, policy, pump });
    if let Some(old) = runtimes().lock().unwrap().insert(id.to_string(), rt) {
        old.pump.abort();
        old.hal.stop();
    }
    let _ = ctx.db.kv_set(&format!("agent_error.{id}"), "");
    set_state(ctx, id, "idle");

    if let Some(b) = brief.filter(|b| !b.trim().is_empty()) {
        send(ctx, id, &b).await?;
    }
    Ok(())
}

fn short_hash(s: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(s.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Follow HAL's SSE stream, keep agent state current, batch events to the UI.
async fn pump_events(ctx: Arc<Ctx>, id: String, hal: Arc<Hal>, session: String) {
    use futures_util::StreamExt;
    let mut after: i64 = ctx
        .db
        .kv_get(&format!("agent_seq.{id}"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    loop {
        if !hal.alive() {
            return;
        }
        let url = format!("{}/sessions/{session}/events?stream=true&after={after}", hal.base);
        let resp = match hal.client.get(&url).bearer_auth(&hal.token).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut batch: Vec<Value> = vec![];
        let mut last_flush = std::time::Instant::now();
        loop {
            let next = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buf.find("\n\n") {
                        let block: String = buf.drain(..pos + 2).collect();
                        for ev in parse_sse(&block) {
                            if let Some(s) = ev["seq"].as_i64() {
                                after = after.max(s);
                            }
                            apply_event(&ctx, &id, &ev);
                            batch.push(ev);
                        }
                    }
                }
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => {} // timeout tick: flush below
            }
            if !batch.is_empty() && (last_flush.elapsed().as_millis() >= 100 || batch.len() > 200) {
                ctx.emit("agents:events", json!({ "id": id, "events": std::mem::take(&mut batch) }));
                let _ = ctx.db.kv_set(&format!("agent_seq.{id}"), &after.to_string());
                last_flush = std::time::Instant::now();
            }
        }
        if !batch.is_empty() {
            ctx.emit("agents:events", json!({ "id": id, "events": batch }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

pub fn parse_sse(block: &str) -> Vec<Value> {
    let data: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim_start)
        .collect();
    if data.is_empty() {
        return vec![];
    }
    serde_json::from_str::<Value>(&data.join("\n")).ok().into_iter().filter(|v| v.get("type").is_some()).collect()
}

/// Agent state machine driven by HAL events.
pub fn next_state(current: &str, event_type: &str) -> Option<&'static str> {
    if matches!(current, "stopped" | "failed" | "done" | "tearing_down") {
        return None;
    }
    match event_type {
        "turn.started" | "model.started" | "tool.started" | "permission.resolved" => Some("running"),
        "permission.requested" => Some("waiting"),
        "assistant.completed" | "turn.cancelled" => Some("idle"),
        "turn.failed" => Some("idle"),
        _ => None,
    }
}

fn apply_event(ctx: &Arc<Ctx>, id: &str, ev: &Value) {
    let ty = ev["type"].as_str().unwrap_or("");
    let current = get_row(ctx, id).and_then(|r| r["state"].as_str().map(str::to_string)).unwrap_or_default();
    if let Some(s) = next_state(&current, ty) {
        if s != current {
            set_state(ctx, id, s);
        }
    } else if ty != "model.delta" {
        let _ = ctx.db.exec("UPDATE agents SET last_active_at=?2 WHERE id=?1", rusqlite::params![id, now()]);
    }
    match ty {
        "permission.requested" => ctx.emit("agents:approval", json!({ "id": id, "event": ev })),
        "turn.failed" => ctx.log_event("agent_turn_failed", Some(id), &json!({ "error": ev["error"] })),
        _ => {}
    }
}

fn rt(id: &str) -> Result<Arc<Runtime>, String> {
    runtime(id).ok_or_else(|| "agent is not running (resume it first)".to_string())
}

pub async fn send(ctx: &Arc<Ctx>, id: &str, text: &str) -> Result<Value, String> {
    let r = rt(id)?;
    set_state(ctx, id, "running");
    r.hal
        .call(reqwest::Method::POST, &format!("/sessions/{}/messages", r.session), Some(json!({ "text": text })))
        .await
}

/// Stop the HAL and everything that runs on the agent's behalf. Keeps the
/// worktree, scratch and record so it can resume or be reviewed.
pub fn stop(ctx: &Arc<Ctx>, id: &str, state: &str) {
    if let Some(rt) = runtimes().lock().unwrap().remove(id) {
        rt.pump.abort();
        rt.hal.stop();
    }
    if let Some(row) = get_row(ctx, id) {
        if let Some(n) = row["name"].as_str() {
            ctx.gateway.revoke_agent(n);
        }
    }
    crate::mcp::revoke(id);
    crate::terminals::kill_agent(ctx, id);
    crate::board::release_agent(ctx, id);
    set_state(ctx, id, state);
}

/// Called on app exit: stop every agent HAL; containers/worktrees are kept.
pub fn shutdown(ctx: &Arc<Ctx>) {
    let ids: Vec<String> = runtimes().lock().unwrap().keys().cloned().collect();
    for id in ids {
        stop(ctx, &id, "stopped");
    }
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    // A previous run's agents have no HAL any more.
    ctx.db.exec(
        "UPDATE agents SET state='stopped' WHERE state IN ('provisioning','running','idle','waiting')",
        [],
    )?;
    Ok(())
}

fn with_live(ctx: &Ctx, mut row: Value) -> Value {
    let id = row["id"].as_str().unwrap_or_default().to_string();
    row["live"] = json!(runtime(&id).is_some());
    row["strict"] = json!(row["strict"].as_i64() == Some(1));
    if let Some(e) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) {
        row["error"] = json!(e);
    }
    if let Some(rt) = runtime(&id) {
        row["cwd"] = json!(rt.cwd);
        row["pid"] = json!(rt.hal.pid);
    }
    row
}

pub fn list(ctx: &Ctx) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT a.*, t.title AS task_title, r.name AS repo_name, w.path AS worktree, w.branch AS branch
         FROM agents a LEFT JOIN tasks t ON t.id=a.task_id LEFT JOIN repos r ON r.id=a.repo_id
         LEFT JOIN worktrees w ON w.agent_id=a.id AND w.state='active'
         ORDER BY a.created_at",
        [],
    )?;
    Ok(Value::Array(rows.into_iter().map(|r| with_live(ctx, r)).collect()))
}

async fn hal_get(id: &str, path: &str) -> Result<Value, String> {
    let r = rt(id)?;
    r.hal.call(reqwest::Method::GET, &format!("/sessions/{}{path}", r.session), None).await
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let id = || -> Result<String, String> {
        let raw = ctx::arg_str(&args, "id")?;
        get_row(ctx, &raw)
            .and_then(|r| r["id"].as_str().map(str::to_string))
            .ok_or_else(|| format!("no agent {raw}"))
    };
    match method {
        "list" => list(ctx),
        "names" => Ok(json!(free_names(ctx, 8))),
        "get" => {
            let id = id()?;
            let mut row = with_live(ctx, get_row(ctx, &id).ok_or("gone")?);
            if runtime(&id).is_some() {
                row["session"] = hal_get(&id, "").await.unwrap_or(Value::Null);
            }
            row["resources"] = json!(ctx.db.query(
                "SELECT * FROM resources WHERE agent_id=?1 AND state!='released' ORDER BY created_at",
                [&id]
            )?);
            row["children"] = json!(ctx.db.query("SELECT id,name,state FROM agents WHERE parent_id=?1", [&id])?);
            Ok(row)
        }
        "spawn" => spawn(
            ctx,
            SpawnReq {
                name: ctx::arg_opt_str(&args, "name"),
                role: ctx::arg_opt_str(&args, "role"),
                model: ctx::arg_opt_str(&args, "model"),
                repo_id: ctx::arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty()),
                task_id: ctx::arg_opt_str(&args, "task_id").filter(|s| !s.is_empty()),
                brief: ctx::arg_opt_str(&args, "brief"),
                parent_id: ctx::arg_opt_str(&args, "parent_id"),
                strict: args["strict"].as_bool().unwrap_or(false),
            },
        ),
        "send" => send(ctx, &id()?, &ctx::arg_str(&args, "text")?).await,
        "cancel" => {
            let r = rt(&id()?)?;
            r.hal.call(reqwest::Method::POST, &format!("/sessions/{}/cancel", r.session), None).await
        }
        "stop" => {
            stop(ctx, &id()?, "stopped");
            Ok(Value::Null)
        }
        "resume" => {
            let id = id()?;
            if runtime(&id).is_some() {
                return Ok(Value::Null);
            }
            set_state(ctx, &id, "provisioning");
            let c = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = provision(&c, &id, false, None).await {
                    let _ = c.db.kv_set(&format!("agent_error.{id}"), &e);
                    set_state(&c, &id, "failed");
                }
            });
            Ok(Value::Null)
        }
        "remove" => {
            let id = id()?;
            let force = args["force"].as_bool().unwrap_or(false);
            stop(ctx, &id, "tearing_down");
            let (c, i) = (ctx.clone(), id.clone());
            let report = tokio::task::spawn_blocking(move || crate::reaper::teardown_agent(&c, &i, force))
                .await
                .map_err(|e| e.to_string())??;
            if report["needs_confirmation"].as_bool() == Some(true) {
                set_state(ctx, &id, "stopped");
                return Ok(report);
            }
            if let Some(row) = get_row(ctx, &id) {
                if let Some(n) = row["name"].as_str() {
                    let _ = std::fs::remove_dir_all(ctx.agent_dir(n));
                }
            }
            ctx.db.exec("DELETE FROM agents WHERE id=?1", [&id])?;
            for k in ["agent_error", "agent_seq"] {
                let _ = ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("{k}.{id}")]);
            }
            ctx.log_event("agent_removed", Some(&id), &report);
            ctx.emit("agents:changed", json!({ "id": id, "removed": true }));
            Ok(report)
        }
        "events" => {
            let after = args["after"].as_i64().unwrap_or(0);
            let limit = args["limit"].as_i64().unwrap_or(2000).clamp(1, 10000);
            hal_get(&id()?, &format!("/events?after={after}&limit={limit}")).await
        }
        "messages" => hal_get(&id()?, "/messages").await,
        "permissions" => hal_get(&id()?, "/permissions").await,
        "permission" => {
            let r = rt(&id()?)?;
            let rid = ctx::arg_str(&args, "request_id")?;
            if !rid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("bad request id".into());
            }
            let (path, body) = match ctx::arg_str(&args, "decision")?.as_str() {
                "approve" => (
                    "approve",
                    Some(json!({ "scope": ctx::arg_opt_str(&args, "scope").unwrap_or_else(|| "once".into()) })),
                ),
                "deny" => ("deny", None),
                _ => return Err("decision must be approve or deny".into()),
            };
            r.hal
                .call(reqwest::Method::POST, &format!("/sessions/{}/permissions/{rid}/{path}", r.session), body)
                .await
        }
        "set_model" => {
            let id = id()?;
            let model = ctx::arg_str(&args, "model")?;
            let row = get_row(ctx, &id).ok_or("gone")?;
            let (p, m) = providers::resolve(ctx, Some(&model))?;
            let strict = row["strict"].as_i64() == Some(1);
            if strict && !(p.trusted == false && row["model_ref"].as_str().unwrap_or("").starts_with(&format!("{}/", p.id))) {
                return Err("a Strict agent can only switch models within its provider".into());
            }
            if !strict && !p.trusted {
                return Err("switching to an untrusted provider needs a new Strict agent".into());
            }
            let r = format!("{}/{}", p.id, m);
            if let Some(rt) = runtime(&id) {
                rt.hal
                    .call(reqwest::Method::PATCH, &format!("/sessions/{}", rt.session), Some(json!({ "model": r })))
                    .await?;
            }
            ctx.db.exec("UPDATE agents SET model_ref=?2 WHERE id=?1", rusqlite::params![id, r])?;
            ctx.emit("agents:changed", json!({ "id": id }));
            Ok(json!(r))
        }
        "diff" => {
            let id = id()?;
            let wt = ctx.db.query("SELECT id FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [&id])?;
            match wt.first().and_then(|w| w["id"].as_str()) {
                Some(w) => crate::workspace::call(ctx, "diff", json!({ "worktree_id": w })).await,
                None => Ok(json!({ "stat": "", "patch": "" })),
            }
        }
        _ => Err(format!("unknown agents.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_slugs_and_unique() {
        let ctx = Ctx::test();
        assert!(valid_name("lion"));
        assert!(valid_name("lion-2"));
        for bad in ["", "Lion", "a b", "../x", "-x", "you", "all", &"x".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
        let n = free_names(&ctx, 3);
        assert_eq!(n.len(), 3);
        assert!(n.iter().all(|x| valid_name(x)));
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('a','lion','p/m',0,0)",
                [],
            )
            .unwrap();
        assert!(!free_names(&ctx, NAMES.len() + 5).contains(&"lion".to_string()));
    }

    #[test]
    fn port_blocks_do_not_overlap() {
        let ctx = Ctx::test();
        let a = port_base(&ctx);
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,port_base,created_at,last_active_at) VALUES('a','a','p/m',?1,0,0)",
                [a],
            )
            .unwrap();
        let b = port_base(&ctx);
        assert_eq!(b - a, 20);
    }

    #[test]
    fn state_machine() {
        assert_eq!(next_state("idle", "turn.started"), Some("running"));
        assert_eq!(next_state("running", "permission.requested"), Some("waiting"));
        assert_eq!(next_state("waiting", "permission.resolved"), Some("running"));
        assert_eq!(next_state("running", "assistant.completed"), Some("idle"));
        assert_eq!(next_state("stopped", "turn.started"), None);
        assert_eq!(next_state("running", "model.delta"), None);
    }

    #[test]
    fn sse_parsing() {
        let ev = parse_sse("id: 7\nevent: model.delta\ndata: {\"seq\":7,\"type\":\"model.delta\",\"delta\":\"hi\"}\n\n");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["delta"], "hi");
        assert!(parse_sse(": ping\n\n").is_empty());
        assert!(parse_sse("event: ping\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn shell_is_denied_and_secrets_unreadable() {
        let p = permission_rules(false);
        let rules = p["rules"].as_array().unwrap();
        // Last matching rule wins: shell deny must not be followed by a shell allow.
        let last_shell = rules.iter().rev().find(|r| r["action"] == "shell").unwrap();
        assert_eq!(last_shell["effect"], "deny");
        assert!(rules.iter().any(|r| r["resource"] == "**/.env*" && r["effect"] == "deny"));
        let s = permission_rules(true);
        let last_sub = s["rules"].as_array().unwrap().iter().rev().find(|r| r["action"] == "subagent").unwrap();
        assert_eq!(last_sub["effect"], "deny");
    }

    #[tokio::test]
    async fn spawn_validates_before_starting() {
        let ctx = Ctx::test();
        // No providers configured: resolve fails, nothing is created.
        assert!(spawn(&ctx, SpawnReq {
            name: Some("lion".into()), role: None, model: Some("nope/x".into()), repo_id: None,
            task_id: None, brief: None, parent_id: None, strict: false
        })
        .is_err());
        assert!(ctx.db.query("SELECT * FROM agents", []).unwrap().is_empty());
    }
}
