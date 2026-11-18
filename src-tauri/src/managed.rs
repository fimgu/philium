//! Human-issued, checkout/task-owned offline Docker jobs. No kind/profile cleanup, pulls or bind mounts.
use crate::{
    ctx::{arg_str, Ctx},
    db::{new_id, now},
    environments::{socket, valid_id},
};
use reqwest::{Client, Method};
use rusqlite::params;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
static OPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(test)]
#[path = "managed_tests.rs"]
mod tests;
const SCHEMA:&str="CREATE TABLE IF NOT EXISTS managed_jobs(id TEXT PRIMARY KEY, repo TEXT NOT NULL, task TEXT NOT NULL, agent TEXT NOT NULL, profile TEXT NOT NULL, engine TEXT NOT NULL,image TEXT NOT NULL, cid TEXT,created INTEGER NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL,receipt TEXT NOT NULL DEFAULT '',finished INTEGER,exit_code INTEGER);CREATE UNIQUE INDEX IF NOT EXISTS managed_task ON managed_jobs(task) WHERE state!='released'";
fn init(ctx: &Ctx) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))
}
fn key<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Missing {k}"))
}
struct Engine(Client);
impl Engine {
    fn open(profile: &str) -> Result<Self, String> {
        let path = socket(
            &crate::pilot::colima_dir().ok_or("Missing local Colima root")?,
            profile,
        )?;
        Ok(Self(
            Client::builder()
                .no_proxy()
                .unix_socket(path)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()
                .map_err(|_| "Local client unavailable")?,
        ))
    }
    async fn request(
        &self,
        m: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value), String> {
        let mut b = self.0.request(m, format!("http://localhost{path}"));
        if let Some(v) = body {
            b = b.json(&v);
        }
        let mut r = b
            .send()
            .await
            .map_err(|_| "Local runtime unavailable or timed out")?;
        let status = r.status().as_u16();
        let mut buf = Vec::new();
        while let Some(c) = r
            .chunk()
            .await
            .map_err(|_| "Interrupted runtime response")?
        {
            if buf.len() + c.len() > 2 * 1024 * 1024 {
                return Err("Runtime response too large".into());
            }
            buf.extend_from_slice(&c);
        }
        let v = if buf.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&buf).map_err(|_| "Invalid runtime response")?
        };
        Ok((status, v))
    }
    async fn ok(&self, m: Method, p: &str, b: Option<Value>) -> Result<Value, String> {
        let (s, v) = self.request(m, p, b).await?;
        if (200..300).contains(&s) {
            Ok(v)
        } else {
            Err(format!("Local Docker HTTP {s}"))
        }
    }
    async fn identity(&self) -> Result<String, String> {
        Ok(key(&self.ok(Method::GET, "/info", None).await?, "ID")?.into())
    }
}
fn name(id: &str) -> String {
    format!("agent-philium-{id}")
}
fn row(ctx: &Ctx, repo: &str, id: &str) -> Result<Value, String> {
    ctx.db
        .query(
            "SELECT * FROM managed_jobs WHERE id=? AND repo=?",
            [id, repo],
        )?
        .pop()
        .ok_or_else(|| "No managed job in this checkout".into())
}
fn issued(j: &Value, v: &Value) -> bool {
    let labels = &v["Config"]["Labels"];
    let id = v["Id"].as_str().unwrap_or("");
    valid_id(id)
        && j["cid"].as_str().is_none_or(|c| c == id)
        && v["Image"] == j["image"]
        && ["id", "repo", "task", "agent"]
            .iter()
            .all(|k| labels[format!("dev.philium.{k}")] == j[*k])
}
fn released(ctx: &Ctx, j: &Value, note: &str, exit: Option<i64>) -> Result<(), String> {
    ctx.db.exec("UPDATE managed_jobs SET state='released',receipt=?,finished=?,exit_code=COALESCE(?,exit_code) WHERE id=?",params![note,now(),exit,key(j,"id")?])?;
    Ok(())
}
async fn cleanup(ctx: &Ctx, j: &Value, reason: &str) -> Result<(), String> {
    let api = Engine::open(key(j, "profile")?)?;
    if api.identity().await? != key(j, "engine")? {
        return Err("Runtime identity changed; cleanup blocked".into());
    }
    let id = key(j, "id")?;
    let lookup = j["cid"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| name(id));
    let (s, v) = api
        .request(Method::GET, &format!("/containers/{lookup}/json"), None)
        .await?;
    if s == 404 {
        if j["cid"].is_null() {
            // A missing creation name is not proof that a container wasn't renamed.
            let mut u = reqwest::Url::parse("http://localhost/containers/json").unwrap();
            u.query_pairs_mut().append_pair("all", "1").append_pair(
                "filters",
                &json!({"label":[format!("dev.philium.id={id}")]}).to_string(),
            );
            let hits = api
                .ok(
                    Method::GET,
                    &format!("/containers/json?{}", u.query().unwrap()),
                    None,
                )
                .await?;
            if !hits.as_array().is_some_and(Vec::is_empty) {
                return Err("Unresolved issuance identity; no container removed".into());
            }
        }
        if api.identity().await? != key(j, "engine")? {
            return Err("Issuing runtime unavailable; absence not confirmed".into());
        }
        return released(ctx, j, &format!("{reason}: absence verified"), None);
    }
    if s != 200 || !issued(j, &v) {
        return Err("Ownership mismatch; cleanup blocked".into());
    }
    let cid = key(&v, "Id")?;
    let exit = if v["State"]["Status"] == "exited" {
        v["State"]["ExitCode"].as_i64()
    } else {
        None
    };
    ctx.db.exec(
        "UPDATE managed_jobs SET cid=?,state='retiring',exit_code=COALESCE(?,exit_code) WHERE id=?",
        params![cid, exit, id],
    )?;
    let (s, _) = api
        .request(Method::DELETE, &format!("/containers/{cid}?force=1"), None)
        .await?;
    if !matches!(s, 204 | 404) {
        return Err(format!("Removal not confirmed (HTTP {s})"));
    }
    if api.identity().await? != key(j, "engine")?
        || api
            .request(Method::GET, &format!("/containers/{cid}/json"), None)
            .await?
            .0
            != 404
    {
        return Err("Could not verify absence on the issuing runtime".into());
    }
    released(ctx, j, &format!("{reason}: removal verified"), exit)
}
fn candidates(ctx: &Ctx, repo: &str) -> Result<Vec<Value>, String> {
    ctx.db.query("SELECT t.id,t.title,a.id AS agent,a.name AS owner FROM tasks t JOIN agents a ON a.id=t.owner_agent_id AND a.repo_id=t.repo_id WHERE t.repo_id=? AND t.status='claimed' AND t.lease_expires_at>? ORDER BY t.updated_at DESC",params![repo,now()])
}
async fn launch(ctx: &Ctx, a: &Value) -> Result<Value, String> {
    let repo = key(a, "repo_id")?;
    let task = key(a, "task_id")?;
    let profile = key(a, "profile")?;
    let command = key(a, "command")?;
    if command.len() > 4096 || command.contains('\0') {
        return Err("Command exceeds local job limit".into());
    }
    let minutes = a["minutes"]
        .as_i64()
        .filter(|n| (1..=60).contains(n))
        .ok_or("Choose 1 to 60 minutes")?;
    let t = candidates(ctx, repo)?
        .into_iter()
        .find(|t| t["id"] == task)
        .ok_or("Claim this task with an agent in the same checkout first")?;
    let agent = key(&t, "agent")?;
    if agent != key(a, "agent_id")? {
        return Err("Task owner changed. Refresh and preview the sandbox again.".into());
    }
    let api = Engine::open(profile)?;
    let engine = api.identity().await?;
    let im = api
        .ok(Method::GET, "/images/alpine:3.20/json", None)
        .await
        .map_err(|_| "Local alpine:3.20 required. No image was pulled.")?;
    let image = key(&im, "Id")?;
    if !image.strip_prefix("sha256:").is_some_and(valid_id)
        || im["Config"]["Volumes"]
            .as_object()
            .is_some_and(|o| !o.is_empty())
    {
        return Err("Expected a local volume-free pinned image".into());
    }
    let id = format!("{}{}", new_id(), new_id());
    let n = now();
    if ctx.db.exec("INSERT INTO managed_jobs(id,repo,task,agent,profile,engine,image,created,expires,state) SELECT ?,?,?,?,?,?,?,?,?,'issuing' WHERE (SELECT COUNT(*) FROM managed_jobs WHERE state!='released')<8 AND EXISTS(SELECT 1 FROM tasks t JOIN agents a ON a.id=t.owner_agent_id AND a.repo_id=t.repo_id WHERE t.id=? AND t.repo_id=? AND a.id=? AND t.status='claimed' AND t.lease_expires_at>?)",params![id,repo,task,agent,profile,engine,image,n,n+minutes*60000,task,repo,agent,n]).map_err(|_|"Task already has a sandbox or issuance could not be recorded")?!=1{return Err("Capacity or task lease changed".into());}
    let body = json!({"Image":image,"User":"65534:65534","WorkingDir":"/tmp","Entrypoint":["/bin/busybox","timeout","-s","KILL"],"Cmd":[(minutes*60).to_string(),"/bin/sh","-c",command],"Labels":{"dev.philium.id":id,"dev.philium.repo":repo,"dev.philium.task":task,"dev.philium.agent":agent},"HostConfig":{"NetworkMode":"none","ReadonlyRootfs":true,"CapDrop":["ALL"],"SecurityOpt":["no-new-privileges"],"Memory":134217728,"MemorySwap":134217728,"NanoCpus":500000000,"PidsLimit":64,"Tmpfs":{"/tmp":"rw,nosuid,nodev,noexec,size=16m,mode=1777"},"LogConfig":{"Type":"json-file","Config":{"max-size":"1m","max-file":"1"}},"RestartPolicy":{"Name":"no"}}});
    let result: Result<(), String> = async {
        let v = api
            .ok(
                Method::POST,
                &format!("/containers/create?name={}", name(&id)),
                Some(body),
            )
            .await?;
        let cid = key(&v, "Id")?;
        if !valid_id(cid) {
            return Err("Invalid runtime ID".into());
        }
        ctx.db
            .exec("UPDATE managed_jobs SET cid=? WHERE id=?", [cid, &id])?;
        let j = row(ctx, repo, &id)?;
        if !issued(
            &j,
            &api.ok(Method::GET, &format!("/containers/{cid}/json"), None)
                .await?,
        ) || engine != api.identity().await?
        {
            return Err("Issuance identity changed".into());
        }
        api.ok(Method::POST, &format!("/containers/{cid}/start"), None)
            .await?;
        ctx.db
            .exec("UPDATE managed_jobs SET state='active' WHERE id=?", [&id])?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        ctx.db
            .exec("UPDATE managed_jobs SET receipt=? WHERE id=?", [&e, &id])?;
        return Err(format!("{e}. Issuance recorded for reconciliation."));
    }
    row(ctx, repo, &id)
}
async fn reconcile(ctx: &Ctx) -> Result<(), String> {
    let rows=ctx.db.query("SELECT j.*,t.status AS ts,t.owner_agent_id AS owner,t.repo_id AS current_repo,t.lease_expires_at AS lease FROM managed_jobs j LEFT JOIN tasks t ON t.id=j.task WHERE j.state!='released' ORDER BY j.created",[])?;
    for j in rows {
        let n = now();
        let reason = if j["state"] == "issuing" {
            if n - j["created"].as_i64().unwrap_or(n) < 120000 {
                continue;
            }
            "interrupted issuance"
        } else if j["state"] == "retiring" {
            "cleanup retry"
        } else if j["expires"].as_i64().unwrap_or(0) <= n {
            "sandbox expired"
        } else if j["ts"] == "done" || j["ts"] == "cancelled" {
            "task ended"
        } else if j["current_repo"] != j["repo"]
            || j["owner"] != j["agent"]
            || !(j["ts"] == "claimed" || j["ts"] == "in_review")
            || (j["ts"] == "claimed" && j["lease"].as_i64().unwrap_or(0) <= n)
        {
            "task lease ended"
        } else {
            continue;
        };
        if let Err(e) = cleanup(ctx, &j, reason).await {
            ctx.db.exec(
                "UPDATE managed_jobs SET receipt=? WHERE id=?",
                params![e, key(&j, "id")?],
            )?;
        }
    }
    Ok(())
}
pub fn start_watcher(ctx: &Arc<Ctx>) {
    if let Err(e) = init(ctx) {
        eprintln!("managed sandbox init: {e}");
        return;
    }
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            {
                let _guard = OPS.lock().await;
                if let Err(e) = reconcile(&c).await {
                    eprintln!("managed reconcile: {e}");
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, a: Value) -> Result<Value, String> {
    init(ctx)?;
    let repo = arg_str(&a, "repo_id")?;
    let selected_job = || -> Result<Value, String> {
        let j = row(ctx, &repo, key(&a, "id")?)?;
        if a.get("task_id").is_some() && key(&a, "task_id")? != key(&j, "task")? {
            return Err("Sandbox does not belong to the selected task".into());
        }
        Ok(j)
    };
    match method {
        "list" => {
            let task = if a.get("task_id").is_some() {
                Some(key(&a, "task_id")?)
            } else {
                None
            };
            let tasks: Vec<_> = candidates(ctx, &repo)?
                .into_iter()
                .filter(|t| task.is_none() || task == t["id"].as_str())
                .collect();
            let jobs=ctx.db.query("SELECT j.*,t.title AS task_name,a.name AS agent_name FROM managed_jobs j LEFT JOIN tasks t ON t.id=j.task LEFT JOIN agents a ON a.id=j.agent WHERE j.repo=?1 AND (?2 IS NULL OR j.task=?2) ORDER BY (j.state!='released') DESC,j.created DESC LIMIT 80",params![repo,task])?;
            Ok(json!({"tasks":tasks,"jobs":jobs}))
        }
        "launch" => {
            let _g = OPS.lock().await;
            launch(ctx, &a).await
        }
        "release" => {
            let _g = OPS.lock().await;
            let j = selected_job()?;
            if j["state"] == "released" {
                return Ok(j);
            }
            ctx.db.exec(
                "UPDATE managed_jobs SET state='retiring' WHERE id=? AND cid IS NOT NULL",
                [key(&j, "id")?],
            )?;
            if j["cid"].is_null() {
                return Err("Issuance is pending recovery".into());
            }
            cleanup(ctx, &j, "operator released").await?;
            row(ctx, &repo, key(&j, "id")?)
        }
        "logs" => {
            let j = selected_job()?;
            if j["state"] != "active" {
                return Err("Sandbox no longer active".into());
            }
            let cid = key(&j, "cid")?;
            if !valid_id(cid) {
                return Err("Invalid stored container ID".into());
            }
            let api = Engine::open(key(&j, "profile")?)?;
            if api.identity().await? != j["engine"]
                || !issued(
                    &j,
                    &api.ok(Method::GET, &format!("/containers/{cid}/json"), None)
                        .await?,
                )
            {
                return Err("Sandbox ownership changed".into());
            }
            crate::environments::call("logs", json!({"profile":j["profile"],"id":cid})).await
        }
        _ => Err("Unknown managed sandbox action".into()),
    }
}
