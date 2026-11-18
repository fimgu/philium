//! Board RPC: list, get, create, update, claim, release, delete, deps_set,
//! mail_send, mail_inbox, mail_threads, playbooks, playbook_get/set/reset.
//! Events: board:changed and mail:new. Agent-facing functions below use names or IDs.
use crate::{
    ctx::Ctx,
    db::{new_id, now},
};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
static WRITE: Mutex<()> = Mutex::new(());
pub const JOBS: [&str; 7] = [
    "commit",
    "open_pr",
    "test_pr",
    "review_pr",
    "docs",
    "release",
    "teardown",
];

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    for sql in [
        "ALTER TABLE tasks ADD COLUMN owner TEXT",
        "ALTER TABLE tasks ADD COLUMN step INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE tasks ADD COLUMN check_results TEXT NOT NULL DEFAULT '[]'",
        // Who the work was handed to. Unlike the lease, this survives a stop, a quit and a crash.
        "ALTER TABLE tasks ADD COLUMN assigned TEXT",
        "ALTER TABLE tasks ADD COLUMN assigned_agent_id TEXT",
    ] {
        if let Err(e) = ctx.db.exec(sql, []) {
            if !e.contains("duplicate column") {
                return Err(e);
            }
        }
    }
    ctx.db.with(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS task_events(id INTEGER PRIMARY KEY AUTOINCREMENT,task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,actor TEXT NOT NULL,kind TEXT NOT NULL,body TEXT NOT NULL,created_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS mail_reads(message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,agent TEXT NOT NULL,read_at INTEGER NOT NULL,PRIMARY KEY(message_id,agent)); UPDATE tasks SET status='open' WHERE status='todo';"))?;
    // No agent process survives a restart. Their claims reopen now instead of looking owned
    // until a lease runs out; the assignment stays, so the board still shows whose work it was.
    for t in ctx.db.query("SELECT id,owner FROM tasks WHERE status='claimed' AND owner_agent_id IS NOT NULL", [])? {
        if let Some(id) = t["id"].as_str() {
            reopen(ctx, id, t["owner"].as_str().unwrap_or("system"), "agent_interrupted")?;
        }
    }
    let weak = Arc::downgrade(ctx);
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(15));
        let Some(ctx) = weak.upgrade() else { break };
        let _lock = WRITE.lk();
        let _ = expire(&ctx);
    });
    Ok(())
}
fn changed(ctx: &Arc<Ctx>) {
    ctx.emit("board:changed", json!({}));
    ctx.notify("board");
}
fn event(ctx: &Arc<Ctx>, id: &str, actor: &str, kind: &str, body: &Value) -> Result<(), String> {
    // History names the agent, never its internal id.
    let actor = identity(ctx, actor).map(|x| x.0).unwrap_or_else(|_| actor.to_owned());
    ctx.db.exec(
        "INSERT INTO task_events(task_id,actor,kind,body,created_at) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![id, actor, kind, body.to_string(), now()],
    )?;
    Ok(())
}
fn raw(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    ctx.db
        .query("SELECT * FROM tasks WHERE id=?1", [id])?
        .into_iter()
        .next()
        .ok_or("Task not found".into())
}
fn identity(ctx: &Arc<Ctx>, actor: &str) -> Result<(String, Option<String>), String> {
    if actor == "you" {
        return Ok((actor.into(), None));
    }
    let rows = ctx
        .db
        .query("SELECT id,name FROM agents WHERE id=?1 OR name=?1", [actor])?;
    let a = rows.first().ok_or("Unknown agent")?;
    Ok((
        a["name"].as_str().unwrap().into(),
        a["id"].as_str().map(String::from),
    ))
}
fn owned(ctx: &Arc<Ctx>, task: &Value, actor: &str) -> Result<(), String> {
    if actor == "you" {
        return Ok(());
    }
    let (name, _) = identity(ctx, actor)?;
    if task["owner"].as_str().is_some_and(|o| o != name) {
        return Err("Only the owner or you may change this task".into());
    }
    Ok(())
}
fn deps(ctx: &Arc<Ctx>, id: &str) -> Result<Vec<Value>, String> {
    ctx.db.query("SELECT t.id,t.title,t.status FROM task_deps d JOIN tasks t ON t.id=d.depends_on WHERE d.task_id=?1", [id])
}
fn expire(ctx: &Arc<Ctx>) -> Result<(), String> {
    let rows = ctx.db.query(
        "SELECT id,owner_agent_id FROM tasks WHERE status='claimed' AND lease_expires_at<=?1",
        [now()],
    )?;
    let mut reopened = false;
    for t in &rows {
        let id = t["id"].as_str().unwrap();
        // A running agent keeps its task without having to remember a heartbeat.
        if t["owner_agent_id"].as_str().is_some_and(|a| crate::agents::runtime(a).is_some()) {
            let end = now() + 900_000;
            ctx.db.exec("UPDATE tasks SET lease_expires_at=?2 WHERE id=?1", rusqlite::params![id, end])?;
            ctx.db.exec("UPDATE leases SET expires_at=?2 WHERE kind='task' AND key=?1", rusqlite::params![id, end])?;
            continue;
        }
        reopen(ctx, id, "system", "lease_expired")?;
        reopened = true;
    }
    if reopened {
        changed(ctx);
    }
    Ok(())
}
/// Record who a task was handed to. Does not start work and does not need a running agent.
pub fn assign(ctx: &Arc<Ctx>, by: &str, task_id: &str, agent: &str) -> Result<Value, String> {
    let _lock = WRITE.lk();
    assign_inner(ctx, by, task_id, agent)?;
    changed(ctx);
    get(ctx, task_id)
}
fn assign_inner(ctx: &Arc<Ctx>, by: &str, task_id: &str, agent: &str) -> Result<(), String> {
    let (name, aid) = identity(ctx, agent)?;
    let aid = aid.ok_or("Choose an agent")?;
    let t = raw(ctx, task_id)?;
    let home = ctx.db.query("SELECT repo_id FROM agents WHERE id=?1", [&aid])?;
    if home.first().map(|r| &r["repo_id"]) != Some(&t["repo_id"]) || t["repo_id"].is_null() {
        return Err("Choose an agent from the task's checkout".into());
    }
    if matches!(t["status"].as_str(), Some("done" | "cancelled")) {
        return Err("This task is closed".into());
    }
    if t["status"] == "claimed" && t["owner"] != name {
        return Err(format!("{} holds this task. Release it first.", t["owner"].as_str().unwrap_or("Another owner")));
    }
    if t["assigned_agent_id"] == aid {
        return Ok(());
    }
    ctx.db.exec("UPDATE tasks SET assigned=?2,assigned_agent_id=?3,updated_at=?4 WHERE id=?1", rusqlite::params![task_id, name, aid, now()])?;
    event(ctx, task_id, by, "assigned", &json!({"to":name}))
}
/// A started or resumed agent takes back the open work that was handed to it.
pub fn reclaim_assigned(ctx: &Arc<Ctx>, agent_id: &str) {
    let rows = ctx.db.query("SELECT id FROM tasks WHERE assigned_agent_id=?1 AND status='open'", [agent_id]).unwrap_or_default();
    for t in rows {
        if let Some(id) = t["id"].as_str() {
            let _ = claim(ctx, agent_id, id, 900);
        }
    }
}
fn reopen(ctx: &Arc<Ctx>, id: &str, actor: &str, reason: &str) -> Result<(), String> {
    ctx.db.exec("UPDATE tasks SET status='open',owner=NULL,owner_agent_id=NULL,lease_expires_at=NULL,updated_at=?2 WHERE id=?1",rusqlite::params![id,now()])?;
    ctx.db
        .exec("DELETE FROM leases WHERE kind='task' AND key=?1", [id])?;
    event(ctx, id, actor, reason, &json!({}))
}
fn decorate(ctx: &Arc<Ctx>, mut t: Value, history: bool) -> Result<Value, String> {
    let id = t["id"].as_str().unwrap().to_owned();
    let ds = deps(ctx, &id)?;
    t["blocked"] = json!(ds.iter().any(|d| d["status"] != "done"));
    t["depends_on"] = json!(ds.iter().map(|d| d["id"].clone()).collect::<Vec<_>>());
    t["dependencies"] = json!(ds);
    t["check_results"] =
        serde_json::from_str(t["check_results"].as_str().unwrap_or("[]")).unwrap_or(json!([]));
    if let Some(job) = t["playbook"].as_str() {
        let path = repo_path(ctx, t["repo_id"].as_str())?;
        t["playbook_progress"] = playbook(ctx, job, path.as_deref())?;
    }
    if let Some(agent) = t["assigned_agent_id"].as_str() {
        let state = ctx.db.query("SELECT state FROM agents WHERE id=?1", [agent])?.pop();
        t["assigned_state"] = state.map(|r| r["state"].clone()).unwrap_or(json!("removed"));
    }
    if history {
        t["history"] = json!(ctx.db.query(
            "SELECT * FROM task_events WHERE task_id=?1 ORDER BY id",
            [&id]
        )?);
    }
    Ok(t)
}
fn repo_path(ctx: &Arc<Ctx>, id: Option<&str>) -> Result<Option<std::path::PathBuf>, String> {
    Ok(if let Some(id) = id {
        ctx.db
            .query("SELECT path FROM repos WHERE id=?1", [id])?
            .first()
            .and_then(|r| r["path"].as_str())
            .map(Into::into)
    } else {
        None
    })
}
pub fn get(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    decorate(ctx, raw(ctx, id)?, true)
}
pub fn list(ctx: &Arc<Ctx>, filter: &Value) -> Result<Value, String> {
    let _lock = WRITE.lk();
    expire(ctx)?;
    let rows = ctx
        .db
        .query("SELECT * FROM tasks ORDER BY created_at DESC,id", [])?;
    let mut out = vec![];
    for t in rows {
        if ["status", "owner", "repo_id"]
            .iter()
            .any(|k| filter.get(*k).is_some_and(|v| !v.is_null() && v != &t[*k]))
        {
            continue;
        }
        if let Some(q) = filter["q"].as_str() {
            if !format!("{} {}", t["title"], t["detail"])
                .to_lowercase()
                .contains(&q.to_lowercase())
            {
                continue;
            }
        }
        out.push(decorate(ctx, t, false)?);
    }
    Ok(json!(out))
}
fn validate_deps(ctx: &Arc<Ctx>, id: &str, ds: &[String]) -> Result<(), String> {
    for dep in ds {
        raw(ctx, dep)?;
        let cycle=ctx.db.query("WITH RECURSIVE chain(id) AS (SELECT ?1 UNION SELECT d.depends_on FROM task_deps d JOIN chain c ON d.task_id=c.id) SELECT id FROM chain WHERE id=?2",rusqlite::params![dep,id])?;
        if dep == id || !cycle.is_empty() {
            return Err("Dependency cycle is not allowed".into());
        }
    }
    Ok(())
}
fn set_deps(ctx: &Arc<Ctx>, id: &str, ds: &[String]) -> Result<(), String> {
    validate_deps(ctx, id, ds)?;
    if raw(ctx, id)?["status"] != "open" {
        return Err("Dependencies can only change while open".into());
    }
    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("DELETE FROM task_deps WHERE task_id=?1", [id])?;
        for d in ds {
            tx.execute(
                "INSERT OR IGNORE INTO task_deps(task_id,depends_on) VALUES(?1,?2)",
                [id, d],
            )?;
        }
        tx.commit()
    })?;
    Ok(())
}
pub fn create(
    ctx: &Arc<Ctx>,
    actor: &str,
    title: &str,
    detail: &str,
    priority: Option<&str>,
    repo_id: Option<&str>,
    depends_on: &[String],
    job: Option<&str>,
) -> Result<Value, String> {
    let _lock = WRITE.lk();
    if title.trim().is_empty() {
        return Err("Title is required".into());
    }
    if let Some(job) = job {
        playbook(ctx, job, repo_path(ctx, repo_id)?.as_deref())?;
    }
    let id = new_id();
    validate_deps(ctx, &id, depends_on)?;
    ctx.db.exec("INSERT INTO tasks(id,title,detail,status,priority,repo_id,playbook,created_at,updated_at) VALUES(?1,?2,?3,'open',?4,?5,?6,?7,?7)",rusqlite::params![id,title.trim(),detail,priority.unwrap_or("normal"),repo_id,job,now()])?;
    set_deps(ctx, &id, depends_on)?;
    event(ctx, &id, actor, "created", &json!({"title":title}))?;
    changed(ctx);
    get(ctx, &id)
}
pub fn claim(
    ctx: &Arc<Ctx>,
    agent_id: &str,
    task_id: &str,
    lease_secs: i64,
) -> Result<Value, String> {
    let _lock = WRITE.lk();
    expire(ctx)?;
    if !(1..=86400).contains(&lease_secs) {
        return Err("Lease must be 1 to 86400 seconds".into());
    }
    let (name, aid) = identity(ctx, agent_id)?;
    let t = raw(ctx, task_id)?;
    if t["status"] != "open" && !(t["status"] == "claimed" && t["owner"] == name) {
        return Err("Task is not available".into());
    }
    if deps(ctx, task_id)?.iter().any(|d| d["status"] != "done") {
        return Err("Dependencies are not done".into());
    }
    let end = now() + lease_secs * 1000;
    ctx.db.with(|c|{let tx=c.unchecked_transaction()?;
        tx.execute("UPDATE tasks SET status='claimed',owner=?2,owner_agent_id=?3,lease_expires_at=?4,updated_at=?5,assigned=CASE WHEN ?3 IS NULL THEN assigned ELSE ?2 END,assigned_agent_id=COALESCE(?3,assigned_agent_id) WHERE id=?1",rusqlite::params![task_id,name,aid,end,now()])?;
        tx.execute("INSERT INTO leases(id,agent_id,kind,key,expires_at) VALUES(?1,?2,'task',?3,?4) ON CONFLICT(kind,key) DO UPDATE SET agent_id=excluded.agent_id,expires_at=excluded.expires_at",rusqlite::params![new_id(),aid.as_deref().unwrap_or(&name),task_id,end])?;tx.commit()})?;
    event(
        ctx,
        task_id,
        &name,
        if t["status"] == "claimed" {
            "heartbeat"
        } else {
            "claimed"
        },
        &json!({"expires_at":end}),
    )?;
    changed(ctx);
    get(ctx, task_id)
}
pub fn update(ctx: &Arc<Ctx>, actor: &str, task_id: &str, patch: &Value) -> Result<Value, String> {
    let _lock = WRITE.lk();
    expire(ctx)?;
    update_inner(ctx, actor, task_id, patch)
}
fn update_inner(ctx: &Arc<Ctx>, actor: &str, id: &str, patch: &Value) -> Result<Value, String> {
    let t = raw(ctx, id)?;
    owned(ctx, &t, actor)?;
    let old = t["status"].as_str().unwrap();
    let next = patch["status"].as_str().unwrap_or(old);
    if next != old
        && !matches!(
            (old, next),
            ("open", "cancelled")
                | ("claimed", "open")
                | ("claimed", "in_review")
                | ("claimed", "cancelled")
                | ("in_review", "done")
                | ("in_review", "claimed")
        )
    {
        return Err(format!("Invalid task transition: {old} -> {next}"));
    }
    if next == "done" && actor != "you" {
        return Err("Only you may accept a task".into());
    }
    if old == "claimed" && next == "in_review" {
        return submit_inner(
            ctx,
            actor,
            id,
            patch["note"].as_str().unwrap_or(""),
            patch["pr_url"].as_str(),
        );
    }
    if next == "claimed" && old == "in_review" && t["owner"].is_null() {
        return Err("Task has no owner".into());
    }
    if let Some(step) = patch.get("step") {
        let p = decorate(ctx, t.clone(), false)?;
        let n = p["playbook_progress"]["steps"]
            .as_array()
            .map_or(0, Vec::len);
        if step.as_u64().is_none_or(|v| v > n as u64) {
            return Err("Invalid playbook step".into());
        }
    }
    for key in ["title", "detail", "priority", "pr_url"] {
        if let Some(v) = patch.get(key) {
            let s = v.as_str().ok_or("Expected text field")?;
            if key == "title" && s.trim().is_empty() {
                return Err("Title is required".into());
            }
        }
    }
    ctx.db.exec("UPDATE tasks SET title=?2,detail=?3,priority=?4,status=?5,pr_url=?6,step=?7,updated_at=?8 WHERE id=?1",rusqlite::params![id,patch["title"].as_str().or(t["title"].as_str()),patch["detail"].as_str().or(t["detail"].as_str()),patch["priority"].as_str().or(t["priority"].as_str()),next,patch["pr_url"].as_str().or(t["pr_url"].as_str()),patch["step"].as_i64().or(t["step"].as_i64()),now()])?;
    if next == "open" && old != "open" {
        reopen(ctx, id, actor, "released")?;
        // An explicit release hands the work back. An interruption does not.
        ctx.db.exec("UPDATE tasks SET assigned=NULL,assigned_agent_id=NULL WHERE id=?1", [id])?;
    } else if next == "claimed" && old == "in_review" {
        let end = now() + 300_000;
        ctx.db.exec(
            "UPDATE tasks SET lease_expires_at=?2 WHERE id=?1",
            rusqlite::params![id, end],
        )?;
        ctx.db.exec(
            "INSERT INTO leases(id,agent_id,kind,key,expires_at) VALUES(?1,?2,'task',?3,?4)",
            rusqlite::params![
                new_id(),
                t["owner_agent_id"].as_str().or(t["owner"].as_str()),
                id,
                end
            ],
        )?;
    } else if matches!(next, "done" | "cancelled") {
        ctx.db
            .exec("DELETE FROM leases WHERE kind='task' AND key=?1", [id])?;
        ctx.db
            .exec("UPDATE tasks SET lease_expires_at=NULL WHERE id=?1", [id])?;
    }
    event(ctx, id, actor, "updated", patch)?;
    changed(ctx);
    get(ctx, id)
}
pub fn submit(
    ctx: &Arc<Ctx>,
    agent_id: &str,
    task_id: &str,
    summary: &str,
    pr_url: Option<&str>,
) -> Result<Value, String> {
    let _lock = WRITE.lk();
    expire(ctx)?;
    submit_inner(ctx, agent_id, task_id, summary, pr_url)
}
fn submit_inner(
    ctx: &Arc<Ctx>,
    actor: &str,
    id: &str,
    summary: &str,
    pr_url: Option<&str>,
) -> Result<Value, String> {
    let t = raw(ctx, id)?;
    owned(ctx, &t, actor)?;
    if t["status"] != "claimed" {
        return Err("Only claimed tasks can be submitted".into());
    }
    let mut results = vec![];
    if let Some(job) = t["playbook"].as_str() {
        let p = playbook(ctx, job, repo_path(ctx, t["repo_id"].as_str())?.as_deref())?;
        for check in p["checks"].as_array().unwrap() {
            let c = check.as_str().unwrap();
            let result = if c == "linked_task" {
                Some(if summary.contains(id) {
                    Ok(())
                } else {
                    Err("Summary must link the task ID".into())
                })
            } else if ["no_coauthor", "no_secrets", "template_complete"].contains(&c) {
                Some(check_text(c, summary))
            } else {
                None
            };
            results.push(match result {
                Some(r) => json!({"check":c,"passed":r.is_ok(),"message":r.err()}),
                None => json!({"check":c,"passed":null,"message":"External evidence required"}),
            });
        }
    }
    ctx.db.exec(
        "UPDATE tasks SET check_results=?2 WHERE id=?1",
        rusqlite::params![id, json!(results).to_string()],
    )?;
    event(ctx, id, actor, "checks", &json!(results))?;
    if results.iter().any(|r| r["passed"] == false) {
        changed(ctx);
        return Err("Playbook check failed. See task check results".into());
    }
    ctx.db.exec("UPDATE tasks SET status='in_review',pr_url=COALESCE(?2,pr_url),lease_expires_at=NULL,updated_at=?3 WHERE id=?1",rusqlite::params![id,pr_url,now()])?;
    ctx.db
        .exec("DELETE FROM leases WHERE kind='task' AND key=?1", [id])?;
    event(
        ctx,
        id,
        actor,
        "submitted",
        &json!({"summary":summary,"pr_url":pr_url}),
    )?;
    changed(ctx);
    get(ctx, id)
}
pub fn release_agent(ctx: &Arc<Ctx>, agent_id: &str) {
    let _lock = WRITE.lk();
    let name = identity(ctx, agent_id)
        .map(|x| x.0)
        .unwrap_or(agent_id.into());
    if let Ok(rows) = ctx.db.query(
        "SELECT id FROM tasks WHERE status='claimed' AND (owner=?1 OR owner_agent_id=?2)",
        rusqlite::params![name, agent_id],
    ) {
        for t in rows {
            let _ = reopen(ctx, t["id"].as_str().unwrap(), &name, "agent_released");
        }
    }
    let _ = ctx.db.exec(
        "DELETE FROM leases WHERE agent_id=?1 OR agent_id=?2",
        rusqlite::params![name, agent_id],
    );
    changed(ctx);
}

pub fn send(ctx: &Arc<Ctx>, from: &str, to: &str, body: &str) -> Result<Value, String> {
    if body.trim().is_empty() || to.trim().is_empty() {
        return Err("Recipient and message are required".into());
    }
    let mut text = body.to_owned();
    let mut findings = crate::guard::Findings::default();
    crate::guard::scan_text(&mut text, true, &mut findings);
    let msg=ctx.db.with(|c|{
        let tx = c.unchecked_transaction()?;
        tx.execute("INSERT INTO messages(from_agent,to_target,body,guard_findings,created_at) VALUES(?1,?2,?3,?4,?5)",rusqlite::params![from,to,text,json!(findings).to_string(),now()])?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO agent_mail_delivery SELECT ?1,a.id,a.repo_id,a.model_ref FROM agents a JOIN repos r ON r.id=a.repo_id WHERE a.name=?2 AND ?2 NOT IN ('all','you') AND (?3='you' OR EXISTS(SELECT 1 FROM agents s WHERE s.name=?3 AND s.repo_id=a.repo_id AND s.model_ref=a.model_ref AND s.strict=a.strict))",rusqlite::params![id,to,from])?;
        tx.commit()?; Ok(id)
    })?;
    let v = ctx
        .db
        .query("SELECT * FROM messages WHERE id=?1", [msg])?
        .remove(0);
    ctx.emit("mail:new", v.clone());
    ctx.notify("mail");
    // A report or a redirect reaches an idle agent's conversation without anyone polling.
    if let Some(agent) = ctx.db.query("SELECT agent_id FROM agent_mail_delivery WHERE message_id=?1", [msg])?.pop() {
        if let Some(id) = agent["agent_id"].as_str() {
            crate::agents::deliver_mail(ctx, id);
        }
    }
    Ok(v)
}
/// Unread mail an idle agent is told about on its own: from the person, from its lead, or
/// from a helper it started. Mail between unrelated agents stays in the inbox. Marks it read.
pub fn take_deliverable(ctx: &Arc<Ctx>, id: &str, name: &str) -> Result<Vec<Value>, String> {
    let rows = ctx.db.query("SELECT m.id,m.from_agent,m.body FROM agent_mail_delivery d JOIN agents a ON a.id=d.agent_id AND a.repo_id=d.repo_id AND a.model_ref=d.model_ref JOIN messages m ON m.id=d.message_id LEFT JOIN mail_reads r ON r.message_id=m.id AND r.agent=a.name LEFT JOIN agents s ON s.name=m.from_agent WHERE a.id=?1 AND r.read_at IS NULL AND (m.from_agent='you' OR s.id=a.parent_id OR s.parent_id=a.id) ORDER BY m.id LIMIT 20", [id])?;
    finish_inbox(ctx, name, rows.clone(), true)?;
    Ok(rows)
}
pub fn inbox(
    ctx: &Arc<Ctx>,
    agent: &str,
    unread_only: bool,
    mark_read: bool,
) -> Result<Value, String> {
    let rows=ctx.db.query("SELECT m.*,r.read_at AS recipient_read_at FROM messages m LEFT JOIN mail_reads r ON r.message_id=m.id AND r.agent=?1 WHERE (m.to_target=?1 OR m.to_target='all') AND (?2=0 OR r.read_at IS NULL) ORDER BY m.id DESC",rusqlite::params![agent,unread_only])?;
    finish_inbox(ctx, agent, rows, mark_read)
}

pub fn agent_inbox(ctx: &Arc<Ctx>, id: &str, name: &str, unread: bool) -> Result<Value,String> {
    let rows = ctx.db.query("SELECT m.*,r.read_at AS recipient_read_at FROM agent_mail_delivery d JOIN agents a ON a.id=d.agent_id AND a.repo_id=d.repo_id AND a.model_ref=d.model_ref JOIN messages m ON m.id=d.message_id LEFT JOIN mail_reads r ON r.message_id=m.id AND r.agent=a.name WHERE a.id=?1 AND (?2=0 OR r.read_at IS NULL) ORDER BY m.id DESC LIMIT 100", rusqlite::params![id,unread])?;
    finish_inbox(ctx,name,rows,true)
}

fn finish_inbox(ctx: &Arc<Ctx>,agent: &str,rows: Vec<Value>,mark_read: bool) -> Result<Value,String> {
    if mark_read {
        ctx.db.with(|c| {
            let tx = c.unchecked_transaction()?;
            for r in &rows {
                tx.execute(
                    "INSERT OR IGNORE INTO mail_reads(message_id,agent,read_at) VALUES(?1,?2,?3)",
                    rusqlite::params![r["id"].as_i64(), agent, now()],
                )?;
            }
            tx.commit()
        })?;
    }
    Ok(json!(rows))
}
pub fn thread(ctx: &Arc<Ctx>, a: &str, b: &str, limit: i64) -> Result<Value, String> {
    Ok(json!(ctx.db.query("SELECT * FROM (SELECT * FROM messages WHERE (from_agent=?1 AND to_target=?2) OR (from_agent=?2 AND to_target=?1) OR (?2='all' AND to_target='all') ORDER BY id DESC LIMIT ?3) ORDER BY id",rusqlite::params![a,b,limit.clamp(1,1000)])?))
}

pub(crate) const REVIEW_POLICY: &str = include_str!("../playbooks/review_pr.md");
pub(crate) fn review_policy_digest() -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(REVIEW_POLICY.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
fn builtin(job: &str) -> Result<&'static str, String> {
    Ok(match job {
        "commit" => include_str!("../playbooks/commit.md"),
        "open_pr" => include_str!("../playbooks/open_pr.md"),
        "test_pr" => include_str!("../playbooks/test_pr.md"),
        "review_pr" => REVIEW_POLICY,
        "docs" => include_str!("../playbooks/docs.md"),
        "release" => include_str!("../playbooks/release.md"),
        "teardown" => include_str!("../playbooks/teardown.md"),
        _ => return Err("Unknown playbook job".into()),
    })
}
fn parse(job: &str, body: &str, source: &str) -> Result<Value, String> {
    let mut lines = body.lines();
    if lines.next() != Some("---") {
        return Err("Playbook requires front matter".into());
    }
    let mut checks = vec![];
    let mut declared = None;
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if let Some(v) = line.strip_prefix("job:") {
            declared = Some(v.trim().to_string())
        }
        if let Some(v) = line.strip_prefix("checks:") {
            let v = v.trim();
            if !v.starts_with('[') || !v.ends_with(']') {
                return Err("checks must be a bracketed list".into());
            }
            checks = v[1..v.len() - 1]
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if !closed || declared.as_deref() != Some(job) {
        return Err("Playbook job must match its name".into());
    }
    let steps: Vec<_> = lines
        .filter_map(|l| {
            let (n, s) = l.trim().split_once('.')?;
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                Some(s.trim().to_string())
            } else {
                None
            }
        })
        .collect();
    if steps.is_empty() {
        return Err("Playbook requires numbered steps".into());
    }
    Ok(json!({"job":job,"steps":steps,"checks":checks,"body":body,"source":source}))
}
pub fn playbook(ctx: &Arc<Ctx>, job: &str, repo_path: Option<&Path>) -> Result<Value, String> {
    let default = builtin(job)?;
    if job == "review_pr" {
        let mut p = parse(job, default, "builtin")?;
        p["policy_sha256"] = json!(review_policy_digest());
        return Ok(p);
    }
    if let Some(repo) = repo_path {
        let p = repo.join(".philium/playbooks").join(format!("{job}.md"));
        match std::fs::read_to_string(p) {
            Ok(body) => return parse(job, &body, "repo"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if let Some(body) = ctx.db.kv_get(&format!("playbook.{job}")) {
        return parse(job, &body, "user");
    }
    parse(job, default, "builtin")
}
pub fn check_text(check: &str, text: &str) -> Result<(), String> {
    let lower = text.to_lowercase();
    match check {
        "no_coauthor" => {
            if [
                "co-authored-by",
                "generated with",
                "generated by",
                "signed-off-by: agent",
                "written by claude",
                "written by chatgpt",
                "written by jcode",
                "🤖",
                "agent signature",
            ]
            .iter()
            .any(|s| lower.contains(s))
                || regex::Regex::new(r"(?im)^\s*(?:(?:signed(?:-off)?(?:-by| by)|authored by|written by|powered by)\s*:?\s*|[-~]{1,2}\s*)(?:claude|chatgpt|jcode|codex|gemini|copilot|an? ai|agent)\b").unwrap().is_match(text)
            {
                Err("Remove co-author trailers and agent signatures".into())
            } else {
                Ok(())
            }
        }
        "no_secrets" => {
            let mut f = crate::guard::Findings::default();
            crate::guard::scan_text(&mut text.to_owned(), false, &mut f);
            if f.is_empty() {
                Ok(())
            } else {
                Err("Remove secrets from the text".into())
            }
        }
        "template_complete" => {
            let headings: Vec<String> = lower
                .lines()
                .map(|s| {
                    s.trim()
                        .trim_start_matches('#')
                        .trim()
                        .trim_matches('*')
                        .trim_end_matches(':')
                        .trim()
                        .to_string()
                })
                .collect();
            if ["what", "why", "how tested", "risk"].iter().all(|h| {
                headings
                    .iter()
                    .any(|s| s == h || (*h == "how tested" && s == "how it was tested"))
            }) {
                Ok(())
            } else {
                Err("Include What, Why, How tested, and Risk headings".into())
            }
        }
        _ => Err(format!("Check {check} requires external evidence")),
    }
}
fn strings(v: &Value) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or("Expected dependency array")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(String::from)
                .ok_or("Expected dependency ID".into())
        })
        .collect()
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str().ok_or(format!("Missing {k}"))
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "list" => list(ctx, &args),
        "get" => get(ctx, s(&args, "task_id").or_else(|_| s(&args, "id"))?),
        "create" => create(
            ctx,
            "you",
            s(&args, "title")?,
            args["detail"].as_str().unwrap_or(""),
            args["priority"].as_str(),
            args["repo_id"].as_str(),
            &if args["depends_on"].is_null() {
                vec![]
            } else {
                strings(&args["depends_on"])?
            },
            args["playbook"].as_str(),
        ),
        "claim" => claim(
            ctx,
            "you",
            s(&args, "task_id")?,
            args["lease_secs"].as_i64().unwrap_or(300),
        ),
        "update" => update(
            ctx,
            "you",
            s(&args, "task_id")?,
            args.get("patch").unwrap_or(&args),
        ),
        "submit" => submit(
            ctx,
            "you",
            s(&args, "task_id")?,
            s(&args, "summary")?,
            args["pr_url"].as_str(),
        ),
        "release" => update(ctx, "you", s(&args, "task_id")?, &json!({"status":"open"})),
        "assign" => {
            let (task, agent) = (s(&args, "task_id")?, s(&args, "agent")?);
            let r = assign(ctx, "you", task, agent)?;
            // A running agent holds the task at once. A stopped one takes it back on Resume.
            if crate::agents::runtime(r["assigned_agent_id"].as_str().unwrap_or("")).is_some() && r["status"] == "open" {
                return claim(ctx, agent, task, 900);
            }
            Ok(r)
        }
        "delete" => {
            let _lock = WRITE.lk();
            let id = s(&args, "task_id")?;
            let t = raw(ctx, id)?;
            if t["status"] == "claimed" || t["status"] == "in_review" {
                return Err("Release or finish task before deleting".into());
            }
            ctx.db
                .exec("DELETE FROM leases WHERE kind='task' AND key=?1", [id])?;
            ctx.db.exec("DELETE FROM tasks WHERE id=?1", [id])?;
            changed(ctx);
            Ok(json!(true))
        }
        "deps_set" => {
            let _lock = WRITE.lk();
            let id = s(&args, "task_id")?;
            set_deps(ctx, id, &strings(&args["depends_on"])?)?;
            event(ctx, id, "you", "dependencies", &args["depends_on"])?;
            changed(ctx);
            get(ctx, id)
        }
        "mail_send" => send(ctx, "you", s(&args, "to")?, s(&args, "body")?),
        "mail_inbox" => inbox(
            ctx,
            args["agent"].as_str().unwrap_or("you"),
            args["unread_only"].as_bool().unwrap_or(false),
            args["mark_read"].as_bool().unwrap_or(true),
        ),
        "mail_threads" => thread(
            ctx,
            args["a"].as_str().unwrap_or("you"),
            s(&args, "b")?,
            args["limit"].as_i64().unwrap_or(200),
        ),
        "playbooks" => Ok(json!(JOBS
            .iter()
            .map(|j| playbook(ctx, j, None))
            .collect::<Result<Vec<_>, _>>()?)),
        "playbook_get" => playbook(ctx, s(&args, "job")?, None),
        "playbook_set" => {
            let job = s(&args, "job")?;
            if job == "review_pr" { return Err("The bundled review policy cannot be replaced by a playbook override".into()); }
            builtin(job)?;
            let body = s(&args, "body")?;
            let v = parse(job, body, "user")?;
            ctx.db.kv_set(&format!("playbook.{job}"), body)?;
            changed(ctx);
            Ok(v)
        }
        "playbook_reset" => {
            let job = s(&args, "job")?;
            builtin(job)?;
            ctx.db
                .exec("DELETE FROM kv WHERE k=?1", [format!("playbook.{job}")])?;
            changed(ctx);
            playbook(ctx, job, None)
        }
        _ => Err(format!("Unknown board method: {method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx() -> Arc<Ctx> {
        let c = Ctx::test();
        init(&c).unwrap();
        for (id, name) in [("a1", "alice"), ("a2", "bob")] {
            c.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES(?1,?2,'test',0,0)",[id,name]).unwrap();
        }
        c
    }
    fn task(c: &Arc<Ctx>, ds: &[String], job: Option<&str>) -> String {
        create(c, "you", "A task", "Details", None, None, ds, job).unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    #[test]
    fn handover_survives_stop_and_restart() {
        let c = ctx();
        for (id, path) in [("r1", "/fixture/a"), ("r2", "/fixture/b")] {
            c.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES(?1,'same',?2,'main',0)", [id, path]).unwrap();
        }
        c.db.exec("UPDATE agents SET repo_id='r1' WHERE id='a1'", []).unwrap();
        c.db.exec("UPDATE agents SET repo_id='r2' WHERE id='a2'", []).unwrap();
        let id = create(&c, "you", "Handed", "", None, Some("r1"), &[], None).unwrap()["id"].as_str().unwrap().to_owned();
        // An agent from the peer checkout cannot be given this task.
        assert!(assign(&c, "you", &id, "a2").is_err());
        let t = assign(&c, "lead", &id, "a1").unwrap();
        assert_eq!((t["assigned"].as_str(), t["status"].as_str(), t["owner"].is_null()), (Some("alice"), Some("open"), true));
        // Once it runs, the agent holds the task without the model asking.
        reclaim_assigned(&c, "a1");
        assert_eq!(get(&c, &id).unwrap()["owner"], "alice");
        assert!(assign(&c, "you", &id, "a2").is_err());
        // Stop: the lease goes, the handover stays visible.
        release_agent(&c, "a1");
        let t = get(&c, &id).unwrap();
        assert_eq!((t["status"].as_str(), t["owner"].is_null(), t["assigned"].as_str(), t["assigned_state"].as_str()), (Some("open"), true, Some("alice"), Some("idle")));
        // Crash with the claim still on disk: reopened at the next start, still handed to alice.
        claim(&c, "a1", &id, 900).unwrap();
        init(&c).unwrap();
        let t = get(&c, &id).unwrap();
        assert_eq!((t["status"].as_str(), t["owner"].is_null(), t["assigned"].as_str()), (Some("open"), true, Some("alice")));
        let kinds: Vec<_> = t["history"].as_array().unwrap().iter().map(|e| (e["actor"].as_str().unwrap().to_owned(), e["kind"].as_str().unwrap().to_owned())).collect();
        assert!(kinds.contains(&("lead".into(), "assigned".into())) && kinds.contains(&("alice".into(), "agent_interrupted".into())), "{kinds:?}");
        // Resume takes it back. An explicit release hands it back for good.
        reclaim_assigned(&c, "a1");
        assert_eq!(get(&c, &id).unwrap()["owner"], "alice");
        let t = update(&c, "you", &id, &json!({"status":"open"})).unwrap();
        assert!(t["assigned"].is_null() && t["owner"].is_null());
        reclaim_assigned(&c, "a1");
        assert!(get(&c, &id).unwrap()["owner"].is_null());
        // A removed agent is shown as such, never as an owner.
        assign(&c, "you", &id, "a1").unwrap();
        c.db.exec("DELETE FROM agents WHERE id='a1'", []).unwrap();
        assert_eq!(get(&c, &id).unwrap()["assigned_state"], "removed");
    }
    #[test]
    fn lifecycle_and_owner_enforcement() {
        let c = ctx();
        let id = task(&c, &[], None);
        assert!(update(&c, "you", &id, &json!({"status":"done"})).is_err());
        assert!(update(&c, "you", &id, &json!({"status":"claimed"})).is_err());
        assert_eq!(claim(&c, "a1", &id, 60).unwrap()["owner"], "alice");
        assert!(claim(&c, "bob", &id, 60).is_err());
        assert!(update(&c, "bob", &id, &json!({"title":"Stolen"})).is_err());
        assert!(submit(&c, "bob", &id, "summary", None).is_err());
        assert_eq!(
            submit(
                &c,
                "alice",
                &id,
                "summary",
                Some("https://example.test/pr/1")
            )
            .unwrap()["status"],
            "in_review"
        );
        assert!(update(&c, "alice", &id, &json!({"status":"done"})).is_err());
        assert_eq!(
            update(
                &c,
                "you",
                &id,
                &json!({"status":"claimed","note":"Fix this"})
            )
            .unwrap()["status"],
            "claimed"
        );
        assert!(get(&c, &id).unwrap()["lease_expires_at"].as_i64().unwrap() > now());
        submit(&c, "alice", &id, "fixed", None).unwrap();
        assert_eq!(
            update(&c, "you", &id, &json!({"status":"done"})).unwrap()["status"],
            "done"
        );
        assert!(claim(&c, "alice", &id, 60).is_err());
        assert!(update(&c, "you", &id, &json!({"status":"open"})).is_err());
        assert!(get(&c, &id).unwrap()["history"].as_array().unwrap().len() >= 7);
    }
    #[test]
    fn dependencies_and_cycles() {
        let c = ctx();
        let first = task(&c, &[], None);
        let second = task(&c, &[first.clone()], None);
        assert_eq!(get(&c, &second).unwrap()["blocked"], true);
        assert!(claim(&c, "alice", &second, 60).is_err());
        assert!(set_deps(&c, &first, &[second.clone()]).is_err());
        assert!(set_deps(&c, &first, &[first.clone()]).is_err());
        assert!(create(
            &c,
            "you",
            "missing dep",
            "",
            None,
            None,
            &["missing".into()],
            None
        )
        .is_err());
        claim(&c, "alice", &first, 60).unwrap();
        submit(&c, "alice", &first, "done", None).unwrap();
        update(&c, "you", &first, &json!({"status":"done"})).unwrap();
        assert_eq!(get(&c, &second).unwrap()["blocked"], false);
        claim(&c, "bob", &second, 60).unwrap();
        assert!(set_deps(&c, &second, &[]).is_err());
    }
    #[test]
    fn heartbeat_expiry_and_release() {
        let c = ctx();
        let id = task(&c, &[], None);
        let end = claim(&c, "alice", &id, 30).unwrap()["lease_expires_at"]
            .as_i64()
            .unwrap();
        assert!(
            claim(&c, "a1", &id, 60).unwrap()["lease_expires_at"]
                .as_i64()
                .unwrap()
                > end
        );
        assert!(claim(&c, "alice", &id, 0).is_err());
        c.db.exec("UPDATE tasks SET lease_expires_at=0 WHERE id=?1", [&id])
            .unwrap();
        assert_eq!(list(&c, &json!({})).unwrap()[0]["status"], "open");
        assert_eq!(get(&c, &id).unwrap()["owner"], Value::Null);
        assert!(c
            .db
            .query("SELECT * FROM leases WHERE key=?1", [&id])
            .unwrap()
            .is_empty());
        claim(&c, "bob", &id, 30).unwrap();
        release_agent(&c, "a2");
        assert_eq!(get(&c, &id).unwrap()["status"], "open");
        claim(&c, "you", &id, 30).unwrap();
        update(&c, "you", &id, &json!({"status":"cancelled"})).unwrap();
        assert!(claim(&c, "alice", &id, 30).is_err());
    }
    #[test]
    fn concurrent_claim_has_one_winner() {
        let c = ctx();
        let id = task(&c, &[], None);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(|a| {
                let c = c.clone();
                let id = id.clone();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    claim(&c, a, &id, 60).is_ok()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .filter(|w| w.thread().id() != std::thread::current().id())
                .map(|w| w.join().unwrap() as usize)
                .sum::<usize>(),
            1
        );
    }
    #[test]
    fn mail_redaction_broadcast_reads_and_threads() {
        let c = ctx();
        let secret = "sk-ant-abcdefghijklmnopqrstuvwxyz1234567890";
        let msg = send(&c, "alice", "all", secret).unwrap();
        assert!(!msg["body"].as_str().unwrap().contains(secret));
        assert_ne!(msg["guard_findings"], "{}");
        assert_eq!(
            inbox(&c, "bob", true, true)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(inbox(&c, "bob", true, false)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            inbox(&c, "you", true, false)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        send(&c, "alice", "you", "hello").unwrap();
        send(&c, "you", "alice", "reply").unwrap();
        send(&c, "bob", "you", "private").unwrap();
        let thread = thread(&c, "you", "alice", 100).unwrap();
        assert_eq!(thread.as_array().unwrap().len(), 2);
        assert_eq!(thread[0]["body"], "hello");
        assert_eq!(thread[1]["body"], "reply");
        assert!(send(&c, "you", "all", "").is_err());
    }
    #[test]
    fn playbook_resolution_and_validation() {
        let c = ctx();
        for job in JOBS {
            let p = playbook(&c, job, None).unwrap();
            assert_eq!(p["source"], "builtin");
            assert!(!p["steps"].as_array().unwrap().is_empty());
        }
        let body = "---\njob: commit\nchecks: [no_coauthor]\n---\n# Commit\n1. Personal step.\n";
        c.db.kv_set("playbook.commit", body).unwrap();
        assert_eq!(playbook(&c, "commit", None).unwrap()["source"], "user");
        let repo = c.root.join("repo");
        std::fs::create_dir_all(repo.join(".philium/playbooks")).unwrap();
        let weaker = body.replace("commit", "review_pr");
        c.db.kv_set("playbook.review_pr", &weaker).unwrap();
        std::fs::write(repo.join(".philium/playbooks/review_pr.md"), &weaker).unwrap();
        let rubric = playbook(&c, "review_pr", Some(&repo)).unwrap();
        assert_eq!(rubric["body"], REVIEW_POLICY);
        assert_eq!(rubric["policy_sha256"], review_policy_digest());
        std::fs::write(
            repo.join(".philium/playbooks/commit.md"),
            body.replace("Personal", "Repo"),
        )
        .unwrap();
        assert_eq!(
            playbook(&c, "commit", Some(&repo)).unwrap()["source"],
            "repo"
        );
        assert_eq!(
            playbook(&c, "commit", Some(&repo)).unwrap()["steps"][0],
            "Repo step."
        );
        assert!(playbook(&c, "../secret", Some(&repo)).is_err());
        assert!(parse("commit", "bad", "user").is_err());
        assert!(parse("docs", body, "user").is_err());
        assert!(parse("commit", "---\njob: commit\n---\nno steps", "user").is_err());
    }
    #[test]
    fn checks_and_submit_results() {
        for bad in [
            "Co-AUTHORED-by: Someone",
            "Generated WITH Jcode",
            "Generated by Claude",
            "Agent signature: bot",
        ] {
            assert!(check_text("no_coauthor", bad).is_err());
        }
        assert!(check_text("no_coauthor", "fix: improve board").is_ok());
        assert!(check_text("no_secrets", "sk-ant-abcdefghijklmnopqrstuvwxyz1234567890").is_err());
        assert!(check_text("no_secrets", "ordinary words").is_ok());
        assert!(check_text(
            "template_complete",
            "## What\nA\n## Why\nB\n## How tested\nC\n## Risk\nD"
        )
        .is_ok());
        assert!(check_text("template_complete", "What why how tested risk").is_err());
        let c = ctx();
        let id = task(&c, &[], Some("open_pr"));
        claim(&c, "alice", &id, 60).unwrap();
        assert!(submit(&c, "alice", &id, "Generated with agent", None).is_err());
        let t = get(&c, &id).unwrap();
        assert_eq!(t["status"], "claimed");
        assert!(t["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["passed"] == false));
        let summary = format!(
            "## What\nTask {id}\n## Why\nRequired\n## How tested\ncargo test\n## Risk\nLow"
        );
        let t = submit(&c, "alice", &id, &summary, None).unwrap();
        assert_eq!(t["status"], "in_review");
        assert!(t["check_results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["passed"].is_null()));
    }
    #[tokio::test]
    async fn rpc_filters_notes_dependencies_and_settings() {
        let c = ctx();
        let first = task(&c, &[], None);
        let id = task(&c, &[], None);
        call(&c, "deps_set", json!({"task_id":id,"depends_on":[first]}))
            .await
            .unwrap();
        call(
            &c,
            "update",
            json!({"task_id":id,"patch":{"note":"Remember this","priority":"high"}}),
        )
        .await
        .unwrap();
        assert_eq!(
            list(&c, &json!({"q":"details","status":"open"}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(list(&c, &json!({"owner":"alice"}))
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        call(
            &c,
            "playbook_set",
            json!({"job":"commit","body":builtin("commit").unwrap()}),
        )
        .await
        .unwrap();
        assert_eq!(
            call(&c, "playbook_get", json!({"job":"commit"}))
                .await
                .unwrap()["source"],
            "user"
        );
        assert_eq!(
            call(&c, "playbook_reset", json!({"job":"commit"}))
                .await
                .unwrap()["source"],
            "builtin"
        );
        call(&c, "delete", json!({"task_id":id})).await.unwrap();
        assert!(get(&c, &id).is_err());
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
