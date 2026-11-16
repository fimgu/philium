//! Ledger and reaper RPC: ledger {agent_id?,state?}, proposals, approve {ids}, dismiss {ids},
//! sweep_now, orphans. Events: reaper:proposals, reaper:changed. All destruction is audited.
//! register/release are synchronous for resource creators. Register intent BEFORE creation.
//! Process metadata must carry pid (and optionally pgid); process identity is captured at registration.
use crate::{
    ctx::Ctx,
    db::{new_id, now},
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
static REAP_QUEUE: Mutex<()> = Mutex::new(());
const HOUR: i64 = 3_600_000;

fn schema(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c|c.execute_batch("CREATE TABLE IF NOT EXISTS teardown_proposals (id TEXT PRIMARY KEY, resource_id TEXT NOT NULL, reason TEXT NOT NULL, unsaved INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL); CREATE UNIQUE INDEX IF NOT EXISTS proposal_pending ON teardown_proposals(resource_id) WHERE state='pending';"))
}
pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    schema(ctx)?;
    let weak = Arc::downgrade(ctx);
    std::thread::Builder::new()
        .name("philium-reaper".into())
        .spawn(move || loop {
            let Some(ctx) = weak.upgrade() else { break };
            if let Err(e) = sweep(&ctx) {
                ctx.log_event("reaper:error", None, &json!({"error":e}));
            }
            drop(ctx);
            std::thread::sleep(Duration::from_secs(300));
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().into())
}
fn identity(pid: i64) -> Option<String> {
    if pid <= 1 {
        return None;
    }
    command("ps", &["-p", &pid.to_string(), "-o", "lstart="])
        .ok()
        .filter(|s| !s.is_empty())
}
pub fn register(
    ctx: &Arc<Ctx>,
    agent_id: Option<&str>,
    kind: &str,
    handle: &str,
    mut meta: Value,
) -> String {
    let id = new_id();
    if !meta.is_object() {
        meta = json!({"value":meta});
    }
    if let Some(agent) = agent_id {
        if let Ok(rows) = ctx.db.query("SELECT name FROM agents WHERE id=?1", [agent]) {
            if let Some(name) = rows.first().and_then(|r| r["name"].as_str()) {
                meta["agent_name"] = json!(name);
            }
        }
    }
    if kind == "process" || kind == "terminal" {
        let pid = meta["pid"].as_i64().or_else(|| handle.parse().ok());
        if let Some(pid) = pid {
            meta["pid"] = json!(pid);
            if let Some(start) = identity(pid) {
                meta["process_identity"] = json!(start);
            }
        }
    }
    // Signature cannot return an error: fail closed rather than create an untracked resource.
    ctx.db.exec("INSERT INTO resources(id,agent_id,kind,handle,meta,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,agent_id,kind,handle,meta.to_string(),now()]).expect("resource registration must persist before creation");
    ctx.log_event(
        "reaper:register",
        agent_id,
        &json!({"id":id,"kind":kind,"handle":handle}),
    );
    ctx.emit("reaper:changed", json!({"id":id}));
    id
}
pub fn release(ctx: &Arc<Ctx>, id: &str) {
    if let Err(e) = ctx.db.exec(
        "UPDATE resources SET state='released',released_at=?1 WHERE id=?2",
        params![now(), id],
    ) {
        ctx.log_event("reaper:error", None, &json!({"id":id,"error":e}));
        return;
    }
    let _ = ctx.db.exec(
        "UPDATE teardown_proposals SET state='released' WHERE resource_id=?1 AND state='pending'",
        [id],
    );
    ctx.log_event("reaper:release", None, &json!({"id":id}));
    ctx.emit("reaper:changed", json!({"id":id}));
}
#[derive(Debug, PartialEq)]
enum Decision {
    Keep,
    Ask(&'static str),
    Remove(&'static str),
}
/// Pure policy. `explicit` means agent teardown, not approval to discard unsaved work.
fn policy(
    kind: &str,
    state: Option<&str>,
    age: i64,
    idle: i64,
    idle_hours: i64,
    failed_hours: i64,
    auto: bool,
    unsaved: bool,
    explicit: bool,
    quit: bool,
    colima_allowed: bool,
) -> Decision {
    if quit && matches!(kind, "container" | "kind_cluster" | "colima") {
        return Decision::Keep;
    }
    if kind == "colima" && !colima_allowed {
        return Decision::Keep;
    }
    let work = kind == "worktree";
    if work && matches!(state, Some("review" | "finishing" | "waiting_review")) {
        return Decision::Keep;
    }
    if work && matches!(state, Some("failed" | "stopped")) && age < failed_hours * HOUR {
        return Decision::Keep;
    }
    let terminal = matches!(state, Some("done" | "failed" | "stopped" | "tearing_down"));
    let due = explicit || state.is_none() || terminal || idle >= idle_hours * HOUR;
    if !due {
        return Decision::Keep;
    }
    let reason = if unsaved {
        "Unsaved or unmerged work requires confirmation"
    } else if state.is_none() {
        "Owning agent no longer exists"
    } else if terminal || explicit {
        "Agent teardown"
    } else {
        "Idle timeout"
    };
    if unsaved || !auto {
        Decision::Ask(reason)
    } else {
        Decision::Remove(reason)
    }
}
fn setting_hours(ctx: &Arc<Ctx>, key: &str, default: i64) -> i64 {
    ctx.setting(key, &default.to_string())
        .parse::<i64>()
        .unwrap_or(default)
        .clamp(1, 24 * 365)
}
fn meta(r: &Value) -> Value {
    r["meta"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_else(|| json!({}))
}
fn strval<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("Missing resource {key}"))
}
fn rows(ctx: &Arc<Ctx>, agent: Option<&str>, state: Option<&str>) -> Result<Vec<Value>, String> {
    ctx.db.query("SELECT r.*,a.name AS agent_name,a.state AS agent_state,a.last_active_at FROM resources r LEFT JOIN agents a ON a.id=r.agent_id WHERE (?1 IS NULL OR r.agent_id=?1) AND (?2 IS NULL OR r.state=?2) ORDER BY CASE r.kind WHEN 'process' THEN 0 WHEN 'terminal' THEN 0 WHEN 'port' THEN 9 ELSE 1 END,r.created_at",params![agent,state])
}
fn unsaved(ctx: &Arc<Ctx>, r: &Value) -> bool {
    if r["kind"] != "worktree" {
        return false;
    }
    let m = meta(r);
    let id = m["worktree_id"].as_str().map(str::to_owned).or_else(|| {
        ctx.db
            .query(
                "SELECT id FROM worktrees WHERE path=?1",
                [r["handle"].as_str().unwrap_or("")],
            )
            .ok()?
            .first()?["id"]
            .as_str()
            .map(str::to_owned)
    });
    id.and_then(|id| crate::workspace::worktree_status(ctx, &id).ok())
        .map(|s| s["unsaved"].as_bool().unwrap_or(true))
        .unwrap_or(true)
}
fn propose(ctx: &Arc<Ctx>, r: &Value, reason: &str, unsaved: bool) -> Result<Value, String> {
    schema(ctx)?;
    let rid = strval(r, "id")?;
    ctx.db.exec("INSERT OR IGNORE INTO teardown_proposals(id,resource_id,reason,unsaved,created_at) VALUES(?1,?2,?3,?4,?5)",params![new_id(),rid,reason,unsaved,now()])?;
    let p=ctx.db.query("SELECT p.*,r.kind,r.handle,r.agent_id FROM teardown_proposals p JOIN resources r ON r.id=p.resource_id WHERE p.resource_id=?1 AND p.state='pending'",[rid])?.into_iter().next().ok_or("Proposal was not recorded")?;
    ctx.emit("reaper:proposals", json!([p.clone()]));
    Ok(p)
}
fn safe_child(path: &Path, root: &Path) -> Result<(), String> {
    let p = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    if p == root || !p.starts_with(root) {
        return Err("Refusing deletion outside the owned directory".into());
    }
    if std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Refusing a symlink resource".into());
    }
    Ok(())
}
fn stop_process(r: &Value) -> Result<(), String> {
    let m = meta(r);
    let pid = m["pid"]
        .as_i64()
        .or_else(|| r["handle"].as_str()?.parse().ok())
        .ok_or("Missing process PID")?;
    if pid <= 1 || pid == std::process::id() as i64 {
        return Err("Refusing unsafe PID".into());
    }
    let Some(current) = identity(pid) else {
        return Ok(());
    };
    let expected = m["process_identity"]
        .as_str()
        .ok_or("Process identity unavailable, refusing to signal a potentially reused PID")?;
    if expected != current {
        return Err("PID identity changed, refusing signal".into());
    }
    let target = if let Some(group) = m["pgid"].as_i64() {
        if group != pid {
            return Err("Only a recorded process-group leader may be signalled".into());
        }
        let actual = command("ps", &["-p", &pid.to_string(), "-o", "pgid="])?;
        if actual.trim() != group.to_string() {
            return Err("Process group changed".into());
        }
        format!("-{group}")
    } else {
        pid.to_string()
    };
    command("kill", &["-TERM", "--", &target])?;
    for _ in 0..50 {
        if identity(pid).as_deref() != Some(expected) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if identity(pid).as_deref() == Some(expected) {
        command("kill", &["-KILL", "--", &target])?;
    }
    Ok(())
}
fn destroy(ctx: &Arc<Ctx>, r: &Value, force: bool) -> Result<(), String> {
    let kind = strval(r, "kind")?;
    let handle = strval(r, "handle")?;
    let m = meta(r);
    match kind {
        "worktree" => {
            let id = m["worktree_id"].as_str().map(str::to_owned).or_else(|| {
                ctx.db
                    .query("SELECT id FROM worktrees WHERE path=?1", [handle])
                    .ok()?
                    .first()?["id"]
                    .as_str()
                    .map(str::to_owned)
            });
            if let Some(id) = id {
                crate::workspace::remove_worktree(ctx, &id, force)?;
            } else {
                return Err("Unregistered worktree requires recovery before removal, never raw directory deletion".into());
            }
        }
        "scratch_dir" | "tmpdir" => {
            let p = Path::new(handle);
            if p.exists() {
                safe_child(p, &ctx.data_dir.join("agents"))?;
                let owner = r["agent_name"]
                    .as_str()
                    .or(m["agent_name"].as_str())
                    .ok_or("Scratch owner name missing")?;
                if !owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Err("Invalid scratch owner".into());
                }
                let owner_dir =
                    std::fs::canonicalize(ctx.agent_dir(owner)).map_err(|e| e.to_string())?;
                if !std::fs::canonicalize(p)
                    .map_err(|e| e.to_string())?
                    .starts_with(&owner_dir)
                {
                    return Err("Scratch directory does not belong to this agent".into());
                }
                std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
            }
        }
        "process" | "terminal" => stop_process(r)?,
        "port" | "port_block" => {
            let active=ctx.db.query("SELECT id FROM resources WHERE agent_id=?1 AND kind IN ('process','terminal') AND state!='released'",params![r["agent_id"].as_str()])?;
            if !active.is_empty() {
                return Err("Owning processes must be removed before releasing ports".into());
            }
        }
        "container" => {
            if handle.starts_with('-') {
                return Err("Invalid container handle".into());
            }
            let owner = strval(r, "agent_id")?;
            let inspected = command(
                "docker",
                &[
                    "inspect",
                    "--format",
                    "{{ index .Config.Labels \"philium.agent\" }}",
                    handle,
                ],
            )?;
            let name = r["agent_name"].as_str().unwrap_or(owner);
            if inspected != owner && inspected != name {
                return Err("Container ownership label does not match ledger".into());
            }
            command("docker", &["rm", "-f", handle])?;
        }
        "kind_cluster" => {
            let owner = r["agent_name"]
                .as_str()
                .or(r["agent_id"].as_str())
                .ok_or("Missing cluster owner")?;
            if !handle.starts_with(&format!("philium-{owner}-")) {
                return Err("Cluster ownership prefix does not match".into());
            }
            command("kind", &["delete", "cluster", "--name", handle])?;
        }
        "colima" => {
            if m["started_by_philium"] != true
                || ctx.setting("teardown.stop_colima", "false") != "true"
            {
                return Err("Colima is shared and was not authorized to stop".into());
            }
            let users = ctx.db.query(
                "SELECT id FROM agents WHERE state NOT IN ('done','failed','stopped')",
                [],
            )?;
            if !users.is_empty() {
                return Err("Colima is still in use".into());
            }
            command("colima", &["stop"])?;
        }
        _ => return Err(format!("No safe teardown handler for {kind}")),
    }
    Ok(())
}
fn remove(ctx: &Arc<Ctx>, r: &Value, force: bool) -> Result<Value, String> {
    let id = strval(r, "id")?;
    match destroy(ctx, r, force) {
        Ok(()) => {
            release(ctx, id);
            ctx.log_event(
                "reaper:removed",
                r["agent_id"].as_str(),
                &json!({"id":id,"kind":r["kind"],"handle":r["handle"],"force":force}),
            );
            Ok(json!({"id":id,"state":"released"}))
        }
        Err(e) => {
            ctx.db
                .exec("UPDATE resources SET state='leaked' WHERE id=?1", [id])?;
            ctx.log_event(
                "reaper:leaked",
                r["agent_id"].as_str(),
                &json!({"id":id,"error":e}),
            );
            Err(e)
        }
    }
}
fn process_rows(
    ctx: &Arc<Ctx>,
    resources: Vec<Value>,
    explicit: bool,
    force: bool,
) -> Result<Value, String> {
    let auto = ctx.setting("teardown.mode", "ask") == "auto";
    let idle_hours = setting_hours(ctx, "teardown.idle_hours", 4);
    let failed_hours = setting_hours(ctx, "teardown.failed_hours", 24);
    let mut removed = Vec::new();
    let mut proposals = Vec::new();
    let mut kept = Vec::new();
    let mut errors = Vec::new();
    for r in resources {
        let m = meta(&r);
        // A manually created workspace has no agent by design, not a missing owner.
        let state = r["agent_state"].as_str().or_else(|| {
            if r["agent_id"].is_null() && !m["orphan"].as_bool().unwrap_or(false) {
                Some("running")
            } else {
                None
            }
        });
        let last = r["last_active_at"]
            .as_i64()
            .unwrap_or(r["created_at"].as_i64().unwrap_or(now()));
        let age = now() - last;
        let dirty = unsaved(ctx, &r);
        let colima = m["started_by_philium"] == true
            && ctx.setting("teardown.stop_colima", "false") == "true"
            && ctx
                .db
                .query(
                    "SELECT id FROM agents WHERE state NOT IN ('done','failed','stopped')",
                    [],
                )?
                .is_empty();
        let decision = policy(
            strval(&r, "kind")?,
            state,
            age,
            age,
            idle_hours,
            failed_hours,
            auto,
            dirty,
            explicit,
            false,
            colima,
        );
        // Explicit force is a user-confirmed discard. It does not defeat shared runtime ownership checks.
        let decision = if force && explicit && r["kind"] != "colima" {
            Decision::Remove("Explicitly confirmed teardown")
        } else {
            decision
        };
        match decision {
            Decision::Keep => kept.push(r["id"].clone()),
            Decision::Ask(reason) => proposals.push(propose(ctx, &r, reason, dirty)?),
            Decision::Remove(_) => match remove(ctx, &r, force && explicit) {
                Ok(v) => removed.push(v),
                Err(e) => errors.push(json!({"id":r["id"],"error":e})),
            },
        }
    }
    Ok(json!({"needs_confirmation":!proposals.is_empty() || !kept.is_empty() || !errors.is_empty(),"removed":removed,"proposals":proposals,"kept":kept,"errors":errors}))
}
pub fn teardown_agent(ctx: &Arc<Ctx>, agent_id: &str, force: bool) -> Result<Value, String> {
    let _q = REAP_QUEUE.lock().map_err(|e| e.to_string())?;
    // Recover legacy scratch creation into the ledger before teardown.
    if let Some(a) = ctx
        .db
        .query("SELECT name FROM agents WHERE id=?1", [agent_id])?
        .first()
    {
        if let Some(name) = a["name"].as_str() {
            if !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                let p = ctx.agent_dir(name).join("scratch");
                if p.exists() && ctx.db.query("SELECT id FROM resources WHERE kind='scratch_dir' AND handle=?1 AND state!='released'",[p.to_string_lossy().as_ref()])?.is_empty(){register(ctx,Some(agent_id),"scratch_dir",&p.to_string_lossy(),json!({"agent_name":name}));}
            }
        }
    }
    process_rows(
        ctx,
        rows(ctx, Some(agent_id), None)?
            .into_iter()
            .filter(|r| r["state"] != "released")
            .collect(),
        true,
        force,
    )
}
/// App lifecycle hook. Stop agent processes, never containers, clusters or worktrees.
pub fn shutdown(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let _q = REAP_QUEUE.lock().map_err(|e| e.to_string())?;
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    for r in rows(ctx, None, None)?.into_iter().filter(|r| {
        r["state"] != "released" && matches!(r["kind"].as_str(), Some("process" | "terminal"))
    }) {
        match remove(ctx, &r, false) {
            Ok(v) => removed.push(v),
            Err(e) => errors.push(json!({"id":r["id"],"error":e})),
        }
    }
    Ok(json!({"removed":removed,"errors":errors,"containers":"kept","clusters":"kept"}))
}
fn sweep(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let _q = REAP_QUEUE.lock().map_err(|e| e.to_string())?;
    process_rows(
        ctx,
        rows(ctx, None, None)?
            .into_iter()
            .filter(|r| r["state"] != "released")
            .collect(),
        false,
        false,
    )
}
fn orphans(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let ledger = rows(ctx, None, None)?;
    let known = |kind: &str, handle: &str| {
        ledger
            .iter()
            .any(|r| r["kind"] == kind && r["handle"] == handle && r["state"] != "released")
    };
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let root = ctx.root.join("worktrees");
    if let Ok(repos) = std::fs::read_dir(&root) {
        for repo in repos.flatten() {
            if repo.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Ok(entries) = std::fs::read_dir(repo.path()) {
                    for e in entries.flatten() {
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                            && !known("worktree", &e.path().to_string_lossy())
                        {
                            found.push(json!({"kind":"worktree","handle":e.path(),"unsaved":true,"action":"Inspect and recover registration before removing"}));
                        }
                    }
                }
            }
        }
    }
    match command(
        "docker",
        &[
            "ps",
            "-a",
            "--filter",
            "label=philium.agent",
            "--format",
            "{{.ID}}\t{{.Names}}\t{{.Label \"philium.agent\"}}",
        ],
    ) {
        Ok(s) => {
            for line in s.lines() {
                let p = line.split('\t').collect::<Vec<_>>();
                if let Some(id) = p.first() {
                    if !known("container", id)
                        && !p.get(1).map(|n| known("container", n)).unwrap_or(false)
                    {
                        found.push(json!({"kind":"container","handle":id,"name":p.get(1),"agent":p.get(2)}));
                    }
                }
            }
        }
        Err(e) => errors.push(e),
    }
    match command("kind", &["get", "clusters"]) {
        Ok(s) => {
            for name in s.lines().filter(|n| n.starts_with("philium-")) {
                if !known("kind_cluster", name) {
                    found.push(json!({"kind":"kind_cluster","handle":name}));
                }
            }
        }
        Err(e) => errors.push(e),
    }
    // Registered Git worktrees can survive manual moves, so also compare Git's own registry.
    for r in ctx.db.query("SELECT * FROM repos", [])? {
        match crate::workspace::git(
            Path::new(strval(&r, "path")?),
            &["worktree", "list", "--porcelain"],
        ) {
            Ok(s) => {
                for line in s.lines().filter_map(|l| l.strip_prefix("worktree ")) {
                    if Path::new(line).starts_with(&root)
                        && !known("worktree", line)
                        && !found.iter().any(|f| f["handle"] == line)
                    {
                        found.push(json!({"kind":"worktree","handle":line,"repo_id":r["id"],"unsaved":true}));
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(json!({"resources":found,"errors":errors}))
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let ctx = ctx.clone();
    let method = method.to_owned();
    tokio::task::spawn_blocking(move || dispatch(&ctx, &method, args))
        .await
        .map_err(|e| e.to_string())?
}
fn dispatch(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    schema(ctx)?;
    match method {
        "ledger"=>{let mut r=rows(ctx,args["agent_id"].as_str(),args["state"].as_str())?;for item in &mut r{item["meta"]=meta(item);}Ok(json!(r))}
        "proposals"=>Ok(json!(ctx.db.query("SELECT p.*,r.kind,r.handle,r.agent_id,a.name AS agent_name FROM teardown_proposals p JOIN resources r ON r.id=p.resource_id LEFT JOIN agents a ON a.id=r.agent_id WHERE p.state='pending' AND r.state!='released' ORDER BY p.created_at",[])?)),
        "approve"|"dismiss"=>{
            let _q=REAP_QUEUE.lock().map_err(|e|e.to_string())?;
            let ids=args["ids"].as_array().ok_or("ids must be an array")?;let mut results=Vec::new();
            for id in ids{let id=id.as_str().ok_or("Each proposal id must be text")?;let p=ctx.db.query("SELECT * FROM teardown_proposals WHERE id=?1 AND state='pending'",[id])?.into_iter().next().ok_or("Unknown pending proposal")?;
                if method=="dismiss"{ctx.db.exec("UPDATE teardown_proposals SET state='dismissed' WHERE id=?1",[id])?;ctx.log_event("reaper:dismiss",None,&json!({"id":id}));results.push(json!({"id":id,"state":"dismissed"}));continue;}
                let rid=strval(&p,"resource_id")?;let r=rows(ctx,None,None)?.into_iter().find(|r|r["id"]==rid && r["state"]!="released").ok_or("Resource is no longer live")?;
                // Approval is tied to the displayed snapshot. New unsaved work must ask again.
                if unsaved(ctx,&r) && p["unsaved"]!=1{ctx.db.exec("UPDATE teardown_proposals SET unsaved=1,reason='Work changed since proposal. Confirm unsaved work deletion' WHERE id=?1",[id])?;results.push(json!({"id":id,"needs_confirmation":true}));continue;}
                match remove(ctx,&r,true){Ok(v)=>{ctx.db.exec("UPDATE teardown_proposals SET state='approved' WHERE id=?1",[id])?;results.push(v)},Err(e)=>results.push(json!({"id":id,"error":e}))}
            }ctx.emit("reaper:proposals",json!({"changed":true}));Ok(json!(results))
        }
        "sweep_now"=>sweep(ctx),"orphans"=>orphans(ctx),_=>Err(format!("reaper.{method}: unknown method"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_safety() {
        assert_eq!(
            policy(
                "worktree",
                Some("done"),
                0,
                0,
                4,
                24,
                true,
                true,
                true,
                false,
                false
            ),
            Decision::Ask("Unsaved or unmerged work requires confirmation")
        );
        assert_eq!(
            policy(
                "worktree",
                Some("failed"),
                23 * HOUR,
                23 * HOUR,
                4,
                24,
                true,
                false,
                true,
                false,
                false
            ),
            Decision::Keep
        );
        assert!(matches!(
            policy(
                "worktree",
                Some("failed"),
                25 * HOUR,
                25 * HOUR,
                4,
                24,
                true,
                false,
                false,
                false,
                false
            ),
            Decision::Remove(_)
        ));
        assert_eq!(
            policy(
                "worktree",
                Some("review"),
                99 * HOUR,
                99 * HOUR,
                4,
                24,
                true,
                false,
                true,
                false,
                false
            ),
            Decision::Keep
        );
        assert_eq!(
            policy(
                "container",
                Some("running"),
                0,
                0,
                4,
                24,
                true,
                false,
                true,
                true,
                false
            ),
            Decision::Keep
        );
        assert_eq!(
            policy(
                "colima",
                None,
                99 * HOUR,
                99 * HOUR,
                4,
                24,
                true,
                false,
                true,
                false,
                false
            ),
            Decision::Keep
        );
        assert_eq!(
            policy(
                "process",
                Some("running"),
                0,
                3 * HOUR,
                4,
                24,
                false,
                false,
                false,
                false,
                false
            ),
            Decision::Keep
        );
        assert!(matches!(
            policy(
                "process",
                Some("running"),
                0,
                4 * HOUR,
                4,
                24,
                false,
                false,
                false,
                false,
                false
            ),
            Decision::Ask(_)
        ));
    }
    #[test]
    fn ledger_proposals_and_approval() {
        let ctx = Ctx::test();
        schema(&ctx).unwrap();
        let id = register(&ctx, None, "port", "42000", json!({"orphan":true}));
        assert_eq!(rows(&ctx, None, Some("live")).unwrap().len(), 1);
        let result = sweep(&ctx).unwrap();
        let pid = result["proposals"][0]["id"].as_str().unwrap();
        let approved = dispatch(&ctx, "approve", json!({"ids":[pid]})).unwrap();
        assert_eq!(approved[0]["state"], "released");
        assert!(rows(&ctx, None, Some("live")).unwrap().is_empty());
        release(&ctx, &id);
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
    #[test]
    fn scratch_guard_and_leak() {
        let ctx = Ctx::test();
        schema(&ctx).unwrap();
        let p = ctx.root.parent().unwrap().join("user-data");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("precious"), "keep").unwrap();
        let id = register(
            &ctx,
            None,
            "scratch_dir",
            &p.to_string_lossy(),
            json!({"agent_name":"lion"}),
        );
        let r = rows(&ctx, None, None).unwrap().remove(0);
        assert!(remove(&ctx, &r, true).is_err());
        assert!(p.join("precious").exists());
        assert_eq!(rows(&ctx, None, Some("leaked")).unwrap()[0]["id"], id);
        std::fs::remove_dir_all(ctx.root.parent().unwrap()).unwrap();
    }
}
