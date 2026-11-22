//! A private ledger of what the person did in Philium, and each time they had to leave it.
//!
//! Actions are recorded at the one entry point every screen uses, by method name and outcome
//! only: no arguments, prompts, paths or output. A fallback (another agent tool, a terminal
//! multiplexer, a manual database edit, outside help) cannot be detected, so the person
//! records it here with one click. Nothing leaves the machine.
use crate::ctx::{arg_str, Ctx};
use serde_json::{json, Value};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workday_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ts INTEGER NOT NULL,kind TEXT NOT NULL,what TEXT NOT NULL,ok INTEGER NOT NULL DEFAULT 1,repo TEXT,note TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS workday_log_kind_ts ON workday_log(kind, ts);";

/// What a person does on purpose. Reads, polling and keystrokes are not actions.
const ACTIONS: &[(&str, &str)] = &[
    ("workspace.repo_add", "Registered a checkout"),
    ("workspace.repo_clone", "Cloned a repository"),
    ("workspace.repo_remove", "Unregistered a checkout"),
    ("workspace.worktree_create", "Created a worktree"),
    ("workspace.worktree_remove", "Removed a worktree"),
    ("providers.save", "Saved a model provider"),
    ("providers.remove", "Removed a model provider"),
    ("providers.oauth_paste", "Signed in to a subscription"),
    ("agents.spawn", "Started an agent"),
    ("agents.send", "Sent a message to an agent"),
    ("agents.cancel", "Interrupted an agent's turn"),
    ("agents.compact", "Compacted an agent's context"),
    ("agents.stop", "Stopped an agent"),
    ("agents.resume", "Resumed an agent"),
    ("agents.remove", "Removed an agent"),
    ("agents.permission", "Answered an approval request"),
    ("agents.batch_permission", "Answered approval requests"),
    ("agents.set_model", "Changed an agent's model"),
    ("agents.stop_cluster", "Stopped a group of agents"),
    ("board.create", "Created a task"),
    ("board.assign", "Handed a task to an agent"),
    ("board.claim", "Claimed a task"),
    ("board.update", "Updated a task"),
    ("board.submit", "Submitted a task for review"),
    ("board.release", "Released a task"),
    ("board.delete", "Deleted a task"),
    ("board.mail_send", "Sent mail"),
    ("terminals.open", "Opened a terminal"),
    ("terminals.kill", "Stopped a terminal"),
    ("checks.start", "Started local checks"),
    ("checks.cancel", "Cancelled local checks"),
    ("managed.launch", "Started a task sandbox"),
    ("managed.release", "Released a task sandbox"),
    ("release_intake.verify_url", "Verified a release pull request"),
    ("release_intake.sql_enroll", "Pinned a release verifier"),
    ("release_intake.sql_compare", "Ran pinned contracts"),
    ("release_intake.checkpoint", "Recorded a code review checkpoint"),
    ("release_intake.origin_token_set", "Saved an origin read token"),
    ("app.chat_create", "Started a chat"),
];
const TOOLS: [&str; 5] = [
    "Jcode",
    "Herdr",
    "Manual database edit",
    "Assistant intervention",
    "Other",
];

pub fn init(ctx: &std::sync::Arc<Ctx>) -> Result<(), String> {
    schema(ctx)
}
fn schema(ctx: &Ctx) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))
}

/// The identifiers of a request that say which checkout it concerned. Taken before the
/// request runs, so nothing else of its arguments is kept.
pub struct Scope {
    repo: Option<String>,
    agent: Option<String>,
    task: Option<String>,
}
pub fn scope(args: Option<&Value>) -> Scope {
    let get = |k: &str| args.and_then(|a| a.get(k)).and_then(Value::as_str).map(str::to_owned);
    Scope { repo: get("repo_id"), agent: get("id"), task: get("task_id") }
}

/// Called for every screen request after it finished. Unlisted methods cost one slice scan.
pub fn record(ctx: &Ctx, method: &str, scope: &Scope, ok: bool) {
    if !ACTIONS.iter().any(|(m, _)| *m == method) {
        return;
    }
    let one = |sql: &str, id: &str| {
        ctx.db.query(sql, [id]).ok().and_then(|mut r| r.pop()).and_then(|r| r["repo"].as_str().map(str::to_owned))
    };
    // Only a registered checkout is kept, found from the request's own checkout, agent or task.
    let repo = scope.repo.as_deref().and_then(|id| one("SELECT id AS repo FROM repos WHERE id=?1", id))
        .or_else(|| scope.agent.as_deref().filter(|_| method.starts_with("agents.")).and_then(|id| one("SELECT repo_id AS repo FROM agents WHERE id=?1", id)))
        .or_else(|| scope.task.as_deref().filter(|_| method.starts_with("board.")).and_then(|id| one("SELECT repo_id AS repo FROM tasks WHERE id=?1", id)));
    let _ = ctx.db.exec(
        "INSERT INTO workday_log(ts,kind,what,ok,repo) VALUES(?1,'action',?2,?3,?4)",
        rusqlite::params![crate::db::now(), method, ok as i64, repo],
    );
}

fn label(method: &str) -> &str {
    ACTIONS
        .iter()
        .find(|(m, _)| *m == method)
        .map_or(method, |(_, l)| l)
}

fn summary(ctx: &Ctx, since: i64) -> Result<Value, String> {
    let rows = ctx.db.query("SELECT w.what,w.repo,r.name AS checkout,SUM(w.ok) AS completed,SUM(1-w.ok) AS failed,MAX(w.ts) AS last FROM workday_log w LEFT JOIN repos r ON r.id=w.repo WHERE w.kind='action' AND w.ts>=?1 GROUP BY w.what,w.repo ORDER BY last DESC", [since])?;
    let actions: Vec<Value> = rows
        .into_iter()
        .map(|r| json!({"action":label(r["what"].as_str().unwrap_or("")),"method":r["what"],"checkout":r["checkout"],"repo":r["repo"],"completed":r["completed"],"failed":r["failed"],"last":r["last"]}))
        .collect();
    let fallbacks = ctx.db.query("SELECT id,ts,what AS tool,note FROM workday_log WHERE kind='fallback' AND ts>=?1 ORDER BY ts DESC", [since])?;
    let completed: i64 = actions.iter().filter_map(|a| a["completed"].as_i64()).sum();
    Ok(json!({"since":since,"until":crate::db::now(),"actions":actions,"completed":completed,"fallbacks":fallbacks,"tools":TOOLS}))
}

pub async fn call(ctx: &std::sync::Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let started = |c: &Ctx| {
        c.db
            .kv_get("workday.started_at")
            .and_then(|s| s.parse::<i64>().ok())
    };
    match method {
        "summary" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let st = started(&c);
                let mut v = summary(&c, st.unwrap_or(0))?;
                v["started"] = json!(st);
                Ok(v)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        // Begin an observation window. Earlier entries stay in the log but leave the summary.
        "start" => {
            ctx.db
                .kv_set("workday.started_at", &crate::db::now().to_string())?;
            Ok(json!({"started":started(ctx)}))
        }
        "fallback" => {
            let tool = arg_str(&args, "tool")?;
            if !TOOLS.contains(&tool.as_str()) {
                return Err("Choose what you used instead".into());
            }
            let note = args["note"].as_str().unwrap_or("").trim();
            if note.len() > 2000 {
                return Err("Keep the note under 2000 characters".into());
            }
            ctx.db.exec(
                "INSERT INTO workday_log(ts,kind,what,ok,note) VALUES(?1,'fallback',?2,0,?3)",
                rusqlite::params![crate::db::now(), tool, note],
            )?;
            Ok(json!(true))
        }
        // A file the person can keep or hand over. Written inside the profile, never uploaded.
        "export" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || {
                let v = summary(&c, started(&c).unwrap_or(0))?;
                let dir = c.data_dir.join("workday");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(format!("workday-{}.json", crate::db::now()));
                crate::hal::write_private(
                    &path,
                    serde_json::to_string_pretty(&v)
                        .map_err(|e| e.to_string())?
                        .as_bytes(),
                )
                .map_err(|e| e.to_string())?;
                Ok(json!({"path":path}))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Err(format!("unknown workday.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Db, gateway::Gateway, hal};
    #[tokio::test]
    async fn records_actions_and_fallbacks_without_arguments() {
        let dir = std::env::temp_dir().join(format!("philium-workday-{}", hal::random_token()));
        std::fs::create_dir(&dir).unwrap();
        let ctx = Ctx::new(
            Db::memory(),
            dir.clone(),
            dir.clone(),
            Gateway::new(hal::random_token(), None),
            String::new(),
        );
        schema(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r1','same','/fixture','main',0)", []).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES('a1','wren','fixture/m','r1',0,0)", []).unwrap();
        let at = |repo: Option<&str>, agent: Option<&str>| Scope { repo: repo.map(str::to_owned), agent: agent.map(str::to_owned), task: None };
        record(&ctx, "agents.list", &at(None, None), true);
        record(&ctx, "terminals.write", &at(None, None), true);
        record(&ctx, "agents.send", &at(None, Some("a1")), true);
        record(&ctx, "agents.send", &at(Some("r1"), None), false);
        record(&ctx, "board.create", &at(Some("not-a-checkout"), None), true);
        let v = call(&ctx, "summary", Value::Null).await.unwrap();
        assert_eq!(v["completed"], 2);
        let sent = v["actions"].as_array().unwrap().iter().find(|a| a["method"] == "agents.send").unwrap();
        assert_eq!((sent["completed"].as_i64(), sent["failed"].as_i64(), sent["checkout"].as_str()), (Some(1), Some(1), Some("same")));
        assert!(v["actions"].as_array().unwrap().iter().all(|a| a["method"] != "agents.list" && a["method"] != "terminals.write"));
        assert!(v["actions"].as_array().unwrap().iter().any(|a| a["method"] == "board.create" && a["repo"].is_null()));
        assert!(call(&ctx, "fallback", json!({"tool":"Emacs"})).await.is_err());
        call(&ctx, "fallback", json!({"tool":"Herdr","note":"needed a split pane"})).await.unwrap();
        let v = call(&ctx, "summary", Value::Null).await.unwrap();
        assert_eq!(v["fallbacks"][0]["tool"], "Herdr");
        let out = call(&ctx, "export", Value::Null).await.unwrap();
        let text = std::fs::read_to_string(out["path"].as_str().unwrap()).unwrap();
        assert!(text.contains("needed a split pane") && text.contains("Sent a message to an agent"));
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        call(&ctx, "start", Value::Null).await.unwrap();
        let v = call(&ctx, "summary", Value::Null).await.unwrap();
        assert_eq!((v["completed"].as_i64(), v["fallbacks"].as_array().unwrap().len()), (Some(0), 0));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
