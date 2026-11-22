//! Philium's agent tools, served over MCP (Streamable HTTP, JSON responses)
//! on a loopback port. Each agent's HAL connects with its own bearer token, so
//! every call is attributed to exactly one agent (SPEC 8).
//!
//! Tools (model-visible as `mcp_philium_<name>`):
//!   board_list, board_get, board_create, board_claim, board_update, board_submit,
//!   mail_send, mail_read, agent_spawn, agent_status, agent_stop,
//!   shell_run, shell_read, shell_write, shell_kill,
//!   browser_open, browser_snapshot, playbook_get, resource_register
//!
//! rpc: status {} -> {url, agents_connected}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::ctx::Ctx;

fn tokens() -> &'static Mutex<HashMap<String, String>> {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// New MCP bearer token for an agent (replaces any previous one).
pub fn issue_token(agent_id: &str) -> String {
    revoke(agent_id);
    let t = crate::hal::random_token();
    tokens().lk().insert(t.clone(), agent_id.to_string());
    t
}

pub fn revoke(agent_id: &str) {
    tokens().lk().retain(|_, a| a != agent_id);
}

fn who(headers: &HeaderMap) -> Option<String> {
    let presented = headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")?;
    let map = tokens().lk();
    map.iter()
        .find(|(t, _)| {
            t.len() == presented.len() && t.bytes().zip(presented.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        })
        .map(|(_, a)| a.clone())
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let url = format!("http://{}/mcp", listener.local_addr().map_err(|e| e.to_string())?);
    let _ = ctx.mcp_url.set(url);
    let app = router(ctx.clone());
    tauri::async_runtime::spawn(async move {
        let l = tokio::net::TcpListener::from_std(listener).expect("mcp listener");
        if let Err(e) = axum::serve(l, app).await {
            eprintln!("philium: mcp stopped: {e}");
        }
    });
    Ok(())
}

pub fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/mcp", post(handle).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::OK }))
        .with_state(ctx)
}

async fn handle(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    // Host header check blocks DNS rebinding from a browser page.
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    if !(host.starts_with("127.0.0.1:") || host.starts_with("localhost:")) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(agent) = who(&headers) else {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    };
    if let Value::Array(batch) = req {
        let mut out = vec![];
        for r in batch {
            if let Some(v) = dispatch(&ctx, &agent, r).await {
                out.push(v);
            }
        }
        return Json(Value::Array(out)).into_response();
    }
    match dispatch(&ctx, &agent, req).await {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// One JSON-RPC message. Notifications (no id) return None.
pub async fn dispatch(ctx: &Arc<Ctx>, agent: &str, req: Value) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let method = req["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "philium", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("").to_string();
            let args = req["params"]["arguments"].clone();
            let args = if args.is_null() { json!({}) } else { args };
            ctx.log_event("tool_call", Some(agent), &json!({ "tool": name }));
            let _ = ctx.db.exec(
                "UPDATE agents SET last_active_at=?2 WHERE id=?1",
                rusqlite::params![agent, crate::db::now()],
            );
            let r = call_tool(ctx, agent, &name, args.clone()).await;
            let (is_err, error_text) = match &r {
                Err(e) => (true, e.clone()),
                Ok(v) => {
                    let exit_err = v
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0);
                    if exit_err {
                        let out = v["output"].as_str().unwrap_or("");
                        (true, format!("exit_code={} {}", v["exit_code"], out.chars().take(500).collect::<String>()))
                    } else {
                        (false, String::new())
                    }
                }
            };
            let mut res = match r {
                Ok(v) if v.get("__mcp_content").is_some() => {
                    json!({ "content": v["__mcp_content"].clone(), "isError": false })
                }
                Ok(v) => {
                    let text = match v {
                        Value::String(s) => s,
                        v => serde_json::to_string_pretty(&v).unwrap_or_default(),
                    };
                    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                }
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            };
            // An overlap with another agent's work reaches this agent on its next tool
            // call, whatever the tool, so it can stop before the two changes collide.
            if let (Some(note), Some(items)) = (crate::codeindex::take_notes(agent), res["content"].as_array_mut()) {
                items.push(json!({ "type": "text", "text": note }));
            }
            // Event-driven prospective reminders (on_error, path_match, queued on_compact) for Standard agents ONLY.
            if let Ok(arow) = agent_row(ctx, agent) {
                if arow["strict"].as_i64() != Some(1) {
                    let repo_id = arow["repo_id"].as_str().filter(|s| !s.is_empty());
                    if let Some(items) = res["content"].as_array_mut() {
                        if let Some(queued) = crate::library::take_agent_reminders(agent) {
                            items.push(json!({ "type": "text", "text": queued }));
                        }
                        if is_err {
                            let err_rems = crate::library::error_reminders(ctx, repo_id, &error_text);
                            if let Some(txt) = crate::library::format_trigger_reminders("on_error", &err_rems) {
                                items.push(json!({ "type": "text", "text": txt }));
                            }
                        }
                        let path_hay = [
                            args["path"].as_str(),
                            args["file_path"].as_str(),
                            args["cwd"].as_str(),
                            args["command"].as_str(),
                            args["symbol"].as_str(),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" ");
                        if !path_hay.trim().is_empty() {
                            let path_rems = crate::library::path_reminders_once(ctx, agent, repo_id, &path_hay);
                            if let Some(txt) = crate::library::format_trigger_reminders("path_match", &path_rems) {
                                items.push(json!({ "type": "text", "text": txt }));
                            }
                        }
                    }
                }
            }
            Ok(res)
        }
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

fn t(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({ "name": name, "description": desc,
            "inputSchema": { "type": "object", "properties": props, "required": required } })
}

pub fn tool_list() -> Vec<Value> {
    let s = json!({ "type": "string" });
    let i = json!({ "type": "integer" });
    let b = json!({ "type": "boolean" });
    vec![
        t("board_list", "List tasks in your registered workspace. Filter by status, owner name or q (text).",
          json!({ "status": s, "owner": s, "q": s }), &[]),
        t("board_get", "Read one workspace task's description, owner, status and playbook step.", json!({ "id": s }), &["id"]),
        t("board_create", "Add a task to the master board.",
          json!({ "title": s, "detail": s, "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
                  "depends_on": { "type": "array", "items": s }, "playbook": s }), &["title"]),
        t("board_claim", "Claim a task with an exclusive lease, or renew your lease (heartbeat). Only open tasks whose dependencies are done can be claimed.",
          json!({ "id": s, "lease_secs": i }), &["id"]),
        t("board_update", "Update a task you own: note progress, change status, set the current playbook step or PR URL.",
          json!({ "id": s, "status": s, "note": s, "step": s, "pr_url": s, "detail": s }), &["id"]),
        t("board_submit", "Submit a claimed task for review with a summary and the PR URL. Playbook checks run.",
          json!({ "id": s, "summary": s, "pr_url": s }), &["id", "summary"]),
        t("mail_send", "Message a named agent in your workspace with the same model and trust level, or \"you\" for the human. No broadcasts.",
          json!({ "to": s, "body": s }), &["to", "body"]),
        t("mail_read", "Read your inbox (marks messages read).", json!({ "unread_only": b }), &[]),
        t("agent_spawn", "Start a helper agent. It gets its own worktree of the same repo and its own session.",
          json!({ "name": s, "brief": s, "model": s, "role": s, "task_id": s }), &["brief"]),
        t("agent_status", "Status of workspace agents, or one by name.", json!({ "name": s }), &[]),
        t("agent_stop", "Stop a helper agent you spawned.", json!({ "name": s }), &["name"]),
        t("shell_run", "Run a command in your persistent interactive shell (bash), visible live to the human. State carries over (cd, exports, venvs). Waits at most 30 seconds, or timeout_secs if shorter, then returns terminal, running, finished, output and exit_code. A running response is NOT failure: do not retry the command. Poll shell_read with the terminal id until finished, and check exit_code. Pass shell to use another shell. Use background=true for builds, test suites, dev servers and watchers to return a terminal id immediately.",
          json!({ "command": s, "cwd": s, "timeout_secs": i, "background": b, "title": s, "shell": s }), &["command"]),
        t("shell_open", "Open another persistent shell by name (for example \"dev\" or \"tests\") when your main shell is busy or you want a separate session. Use the name with shell_run's shell argument. Returns the terminal id.",
          json!({ "name": s }), &["name"]),
        t("shell_list", "Your terminals: shells and background commands, with state.", json!({}), &[]),
        t("shell_read", "Recent terminal output plus running, finished and exit_code. For persistent shells, these describe the command, not the shell process. finished=null means no pending command result. Poll a running command instead of launching it again. A finished command with nonzero exit_code failed.", json!({ "id": s, "max_bytes": i }), &["id"]),
        t("shell_write", "Send input to one of your terminals (include \\n to press Enter).", json!({ "id": s, "data": s }), &["id", "data"]),
        t("shell_kill", "Stop one of your terminals.", json!({ "id": s }), &["id"]),
        t("playbook_get", "The standard steps and checks for a job: commit, open_pr, test_pr, review_pr, docs, release, teardown.",
          json!({ "job": s }), &["job"]),
        t("skill_list", "Your skills (reusable procedures like review-pr, make-plan, babysit): name and when to use each.", json!({}), &[]),
        t("skill_get", "Read a listed skill (built-in review-pr, or imported skills in Standard mode).", json!({ "name": s }), &["name"]),
        t("memory_search", "Search active, non-superseded memories saved under this agent's registered repository ID.",
          json!({ "q": s, "limit": i }), &["q"]),
        t("memory_slate", "Fetch a pre-resolved, provenance-tagged, token-budgeted context slate (current decisions, working handoff note, and repo facts) for this workspace.",
          json!({ "q": s, "budget_tokens": i }), &[]),
        t("memory_save", "Save a repository-scoped memory. Optional fact_key enables code-level supersession; agent writes cannot silently override operator decisions (they become proposed). Category: fact, preference, correction, entity, procedure.",
          json!({ "content": s, "category": s, "project": b, "fact_key": s, "supersedes_id": s, "trust_tier": s, "provenance": s }), &["content", "category"]),
        t("working_note_get", "Read the structured working-state handoff note (requirements, current decisions, failed attempts, next steps) for this repository.",
          json!({}), &[]),
        t("working_note_set", "Partially update the structured working-state handoff note for this repository; omitted fields preserve their existing values.",
          json!({ "requirements": s, "decisions": s, "failed_attempts": s, "next_steps": s }), &[]),
        t("review_plan", "Release review, orchestrator only: write the plan once, riskiest check first. Each item is {title, detail (what to verify and how)}. The checks appear on the board and in Review.",
          json!({ "items": { "type": "array", "items": { "type": "object" } } }), &["items"]),
        t("review_result", "Release review: record the outcome of one item you checked. result is pass, fail or blocked. evidence is the exact commands you ran and what they showed, on which commit; for blocked, what you need. For a fail, severity and origin are required. severity is 0 to 5: 0 and 1 block the release (0: data loss, security, the release does not install or upgrade; 1: a main behaviour is broken), 2 to 5 should be looked at, in decreasing importance. origin: run the same check on the target too; introduced (fails on the head, passes on the target), pre_existing (fails the same way on the target) or unverified (the target could not be checked, or fails differently).",
          json!({ "task_id": s, "result": { "type": "string", "enum": ["pass", "fail", "blocked"] }, "severity": { "type": "integer", "minimum": 0, "maximum": 5 }, "origin": { "type": "string", "enum": ["introduced", "pre_existing", "unverified"] }, "evidence": s }), &["task_id", "result", "evidence"]),
        t("review_report", "Release review, orchestrator: save the final report in Markdown, verdict first.", json!({ "markdown": s }), &["markdown"]),
        t("checks_propose", "Propose release checks for your checkout: facts that must be true in every release. Send {contracts:{schema?,cases:[...]}} once, when asked to draft checks. A person reviews and confirms them in Review; nothing counts before that.",
          json!({ "contracts": { "type": "object" } }), &["contracts"]),
        t("session_search", "Search same-repository Philium sessions and bounded raw turn/tool-output excerpts bound to this repository ID. Cross-repo and imported histories are excluded.",
          json!({ "q": s, "repo_only": b, "limit": i }), &[]),
        t("resource_register", "Declare something you created outside Philium's tools (container, process, temp dir) so it is cleaned up with you. Use kind_cluster for managed clusters.",
          json!({ "kind": s, "handle": s, "note": s }), &["kind", "handle"]),
        t("kind_cluster", "Manage an isolated Kubernetes-in-Docker (kind) test cluster scoped to your agent (`philium-<agent>-<suffix>`), pre-registered in Philium's Reaper and wired to your isolated KUBECONFIG and port block. action: create | list | delete.",
          json!({ "action": s, "suffix": s, "cluster": s }), &["action"]),
    ].into_iter().chain(crate::browser::mcp_tools()).chain(crate::codeindex::mcp_tools()).chain(crate::surfaces::mcp_tools()).collect()
}

fn arg<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    a[k].as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("missing `{k}`"))
}

fn agent_row(ctx: &Ctx, agent: &str) -> Result<Value, String> {
    crate::agents::get_row(ctx, agent).ok_or_else(|| "unknown agent".to_string())
}

/// Terminal ids an agent may touch: only its own.
fn own_terminal(ctx: &Ctx, agent: &str, id: &str) -> Result<(), String> {
    let rows = ctx.db.query("SELECT agent_id FROM terminals WHERE id=?1", [id])?;
    match rows.first() {
        Some(r) if r["agent_id"].as_str() == Some(agent) => Ok(()),
        Some(_) => Err("that terminal belongs to someone else".into()),
        None => Err("no such terminal".into()),
    }
}

/// Keep tool output small: head + tail of long text.
pub fn trim_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = max / 4;
    let tail = max - head;
    let cut = |i: usize| (0..=i).rev().find(|j| s.is_char_boundary(*j)).unwrap_or(0);
    let h = &s[..cut(head)];
    let t = &s[cut(s.len() - tail)..];
    format!("{h}\n... [{} bytes omitted] ...\n{t}", s.len() - h.len() - t.len())
}

/// Resolve a requested cwd inside the agent's workspace.
fn inside(base: &PathBuf, rel: Option<&str>) -> Result<PathBuf, String> {
    let Some(r) = rel.filter(|r| !r.is_empty()) else { return Ok(base.clone()) };
    let p = if r.starts_with('/') { PathBuf::from(r) } else { base.join(r) };
    let canon = p.canonicalize().map_err(|e| format!("cwd {r}: {e}"))?;
    let b = base.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(&b) {
        return Err("cwd must be inside your workspace".into());
    }
    Ok(canon)
}

pub async fn call_tool(ctx: &Arc<Ctx>, agent: &str, name: &str, a: Value) -> Result<Value, String> {
    let row = agent_row(ctx, agent)?;
    let me = row["name"].as_str().unwrap_or("agent").to_string();
    let repo = || row["repo_id"].as_str().filter(|id| crate::workspace::repo(ctx,id).is_some()).ok_or("Registered workspace required");
    let task_here = |id: &str| -> Result<(),String> {
        if ctx.db.query("SELECT id FROM tasks WHERE repo_id=?1 AND id=?2", [repo()?,id])?.is_empty() {return Err("Task unavailable in this workspace".into());}
        Ok(())
    };
    if matches!(name,"board_get"|"board_claim"|"board_update"|"board_submit") { task_here(arg(&a,"id")?)?; }
    match name {
        "board_list" => {
            Ok(json!(ctx.db.query("SELECT id,title,detail,status,priority,owner FROM tasks WHERE repo_id=?1 AND (?2 IS NULL OR status=?2) AND (?3 IS NULL OR owner=?3) AND instr(lower(title||' '||detail),lower(?4))>0 ORDER BY created_at DESC LIMIT 100", rusqlite::params![repo()?,a["status"].as_str(),a["owner"].as_str(),a["q"].as_str().unwrap_or("")])?))
        }
        "board_get" => Ok(ctx.db.query("SELECT id,title,detail,status,priority,owner,playbook,step FROM tasks WHERE repo_id=?1 AND id=?2",[repo()?,arg(&a,"id")?])?.into_iter().next().ok_or("Task unavailable")?),
        "board_create" => {
            let deps: Vec<String> = a["depends_on"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            for dep in &deps { task_here(dep)?; }
            crate::board::create(
                ctx,
                &me,
                arg(&a, "title")?,
                a["detail"].as_str().unwrap_or(""),
                a["priority"].as_str(),
                Some(repo()?),
                &deps,
                a["playbook"].as_str(),
            ).map(|r| json!({"id":r["id"],"status":r["status"]}))
        }
        "board_claim" => {
            let r = crate::board::claim(ctx, agent, arg(&a, "id")?, a["lease_secs"].as_i64().unwrap_or(900).clamp(60, 7200))?;
            let _ = ctx.db.exec("UPDATE agents SET task_id=?2 WHERE id=?1", rusqlite::params![agent, arg(&a, "id")?]);
            ctx.emit("agents:changed", json!({ "id": agent }));
            Ok(json!({"id":r["id"],"status":r["status"]}))
        }
        "board_update" => {
            let mut patch = a.clone();
            if let Some(o) = patch.as_object_mut() {
                o.remove("id");
            }
            crate::board::update(ctx, agent, arg(&a, "id")?, &patch).map(|r| json!({"id":r["id"],"status":r["status"]}))
        }
        "board_submit" => {
            let r = crate::board::submit(ctx, agent, arg(&a, "id")?, arg(&a, "summary")?, a["pr_url"].as_str())?;
            let mut out = json!({"id": r["id"], "status": r["status"]});
            if row["strict"].as_i64() != Some(1) {
                if let Some(rems) = r.get("reminders").filter(|v| v.as_array().is_some_and(|arr| !arr.is_empty())) {
                    out["reminders"] = rems.clone();
                }
            }
            Ok(out)
        }
        "mail_send" => {
            let to = arg(&a, "to")?;
            if to != "you" && (to == "all" || ctx.db.query("SELECT id FROM agents WHERE name=?1 AND repo_id=?2 AND model_ref=?3 AND strict=?4",rusqlite::params![to,repo()?,row["model_ref"].as_str(),row["strict"].as_i64()])?.is_empty()) {
                return Err("Choose a direct recipient in the same workspace and model scope".into());
            }
            crate::board::send(ctx, &me, to, arg(&a, "body")?)
        }
        "mail_read" => crate::board::agent_inbox(ctx,agent,&me,a["unread_only"].as_bool().unwrap_or(true)),
        "agent_spawn" => {
            let strict = row["strict"].as_i64() == Some(1);
            let model = a["model"].as_str().map(str::to_string).or_else(|| row["model_ref"].as_str().map(str::to_string));
            // A Strict agent's helpers stay on the same untrusted provider, Strict too.
            if strict {
                let mine = row["model_ref"].as_str().unwrap_or("").split('/').next().unwrap_or("").to_string();
                if model.as_deref().and_then(|m| m.split('/').next()) != Some(mine.as_str()) {
                    return Err("Strict agents can only spawn helpers on their own provider".into());
                }
            }
            let r = crate::agents::spawn(
                ctx,
                crate::agents::SpawnReq {
                    name: a["name"].as_str().map(str::to_string),
                    role: a["role"].as_str().map(str::to_string).or(Some("helper".into())),
                    model,
                    repo_id: row["repo_id"].as_str().map(str::to_string),
                    task_id: a["task_id"].as_str().map(str::to_string),
                    brief: Some(format!("{}\n\n(Spawned by {me}. Report back with mail_send to {me}.)", arg(&a, "brief")?)),
                    parent_id: Some(agent.to_string()),
                    strict,
                    base: None,
                },
            )?;
            Ok(json!({ "name": r["name"], "id": r["id"], "state": r["state"] }))
        }
        "agent_status" => {
            let all = json!(ctx.db.query("SELECT a.name,a.state,a.role,a.model_ref,t.title AS task_title,w.branch FROM agents a LEFT JOIN tasks t ON t.id=a.task_id AND t.repo_id=a.repo_id LEFT JOIN worktrees w ON w.agent_id=a.id AND w.repo_id=a.repo_id AND w.state='active' WHERE a.repo_id=?1 ORDER BY a.name",[repo()?])?);
            let pick = |r: &Value| {
                json!({
                    "name": r["name"],
                    "state": r["state"],
                    "role": r["role"],
                    "model": r["model_ref"],
                    "task": r["task_title"],
                    "branch": r["branch"],
                })
            };
            match a["name"].as_str() {
                Some(n) => all
                    .as_array()
                    .and_then(|v| v.iter().find(|r| r["name"] == n).map(pick))
                    .ok_or_else(|| format!("no agent named {n}")),
                None => Ok(Value::Array(all.as_array().map(|v| v.iter().map(pick).collect()).unwrap_or_default())),
            }
        }
        "agent_stop" => {
            let target = crate::agents::get_row(ctx, arg(&a, "name")?).ok_or("no such agent")?;
            if target["parent_id"].as_str() != Some(agent) {
                return Err("you can only stop helpers you spawned".into());
            }
            crate::agents::stop(ctx, target["id"].as_str().unwrap_or_default(), "stopped");
            Ok(json!("stopped"))
        }
        "shell_run" => shell_run(ctx, agent, &me, &a).await,
        "shell_open" => {
            let name = crate::agentshell::shell_name(a["name"].as_str())?;
            let id = crate::agentshell::open(ctx, agent, &name).await?;
            Ok(json!({ "shell": name, "terminal": id }))
        }
        "shell_list" => Ok(json!(ctx.db.query(
            "SELECT id,title,state,exit_code,created_at FROM terminals WHERE agent_id=?1 ORDER BY created_at DESC LIMIT 50",
            [agent],
        )?)),
        "shell_read" => {
            let id = arg(&a, "id")?;
            own_terminal(ctx, agent, id)?;
            let max = a["max_bytes"].as_u64().unwrap_or(8192).min(65536) as usize;
            let output = crate::terminals::snapshot(ctx, id, max).unwrap_or_default();
            let mut status = crate::agentshell::command_status(ctx, agent, id).unwrap_or_else(|| {
                let row = ctx.db.query("SELECT state,exit_code FROM terminals WHERE id=?1", [id])
                    .ok().and_then(|rows| rows.into_iter().next()).unwrap_or(json!({}));
                let running = crate::terminals::alive(ctx, id);
                json!({"state":row["state"], "running":running, "finished":!running && row["exit_code"].is_number(), "exit_code":row["exit_code"]})
            });
            status["terminal"] = json!(id);
            status["output"] = json!(output);
            Ok(status)
        }
        "shell_write" => {
            let id = arg(&a, "id")?;
            own_terminal(ctx, agent, id)?;
            crate::terminals::write(ctx, id, arg(&a, "data")?.as_bytes())?;
            Ok(json!("ok"))
        }
        "shell_kill" => {
            let id = arg(&a, "id")?;
            own_terminal(ctx, agent, id)?;
            crate::terminals::kill(ctx, id)?;
            Ok(json!("killed"))
        }
        n if n.starts_with("browser_") => {
            crate::browser::mcp_call(ctx, agent, &me, row["strict"].as_i64() == Some(1), n, a).await
        }
        n if n.starts_with("surface_") => {
            crate::surfaces::mcp_call(ctx, agent, &me, row["strict"].as_i64() == Some(1), n, a).await
        }
        // --- codeindex tools ---
        "code_map" | "code_outline" | "code_symbol" | "code_find" | "code_refs" | "code_impact" | "repo_context" => {
            let mut a = a;
            if name == "repo_context" {
                if a.get("pr").is_some() || a["include_prs"] == true { return Err("Remote PR context requires separate permission".into()); }
                a["include_prs"] = json!(false);
            }
            crate::codeindex::mcp_call(ctx, agent, name, a).await
        }
        // --- end codeindex tools ---
        "playbook_get" => {
            let repo = crate::agents::runtime(agent).map(|r| r.cwd.clone());
            crate::board::playbook(ctx, arg(&a, "job")?, repo.as_deref())
        }
        "skill_list" | "skill_get" => {
            let repo = row["repo_id"].as_str().ok_or("Skills require a registered workspace")?;
            if crate::workspace::repo(ctx, repo).is_none() { return Err("Workspace unavailable".into()); }
            let strict = row["strict"].as_i64() == Some(1);
            // Built-in review-pr is always authoritative and cannot be replaced by an imported skill.
            let builtin_review = json!({
                "name": "review-pr",
                "description": "Check a PR against its real workflow",
                "source": "Philium built-in",
                "policy_sha256": crate::board::review_policy_digest(),
            });
            if name == "skill_list" {
                let mut list = vec![builtin_review];
                if !strict {
                    if let Ok(Value::Array(imported)) = crate::library::skill_list(ctx) {
                        for item in imported {
                            if item["name"].as_str() != Some("review-pr") {
                                list.push(item);
                            }
                        }
                    }
                }
                return Ok(Value::Array(list));
            }
            let requested = arg(&a, "name")?;
            if requested == "review-pr" {
                let mut result = builtin_review;
                result["body"] = json!(crate::board::REVIEW_POLICY);
                result["files"] = json!({});
                return Ok(result);
            }
            if strict {
                return Err("Strict agents may only read built-in skills".into());
            }
            crate::library::skill_get(ctx, requested)
        }
        "review_plan" => crate::review_run::plan(ctx, &me, &row, &a["items"]),
        "review_result" => crate::review_run::result(ctx, agent, &me, &row, arg(&a, "task_id")?, arg(&a, "result")?, a["severity"].as_i64().or_else(|| a["severity"].as_str().and_then(|s| s.trim().trim_start_matches(['P', 'p']).parse().ok())), a["origin"].as_str(), arg(&a, "evidence")?),
        "review_report" => crate::review_run::report(ctx, &me, &row, arg(&a, "markdown")?),
        "checks_propose" => {
            let repo = row["repo_id"].as_str().filter(|id| !id.is_empty()).ok_or("Release checks belong to a registered checkout")?;
            crate::release_sql::propose(ctx, repo, &me, &a["contracts"])
        }
        "memory_search" | "memory_slate" | "memory_save" | "working_note_get" | "working_note_set" | "session_search" => {
            // Personal memory and past sessions never go to agents on untrusted (Strict) providers.
            if row["strict"].as_i64() == Some(1) {
                return Err("memory and past sessions are not available to Strict agents".into());
            }
            let repo = row["repo_id"].as_str().filter(|id| !id.is_empty()).ok_or("A registered repository is required for memory and history")?;
            if ctx.db.query("SELECT id FROM repos WHERE id=?1", [repo])?.is_empty() {
                return Err("Repository identity is unavailable".into());
            }
            match name {
                "memory_search" => {
                    crate::library::agent_memories_async(ctx, arg(&a, "q")?, repo, a["limit"].as_i64().unwrap_or(10)).await
                }
                "memory_slate" => {
                    let budget = a["budget_tokens"]
                        .as_u64()
                        .filter(|b| *b > 0)
                        .map(|b| b as usize)
                        .unwrap_or_else(|| crate::library::model_memory_budget(ctx, row["model_ref"].as_str()));
                    crate::library::build_context_slate_async(
                        ctx,
                        Some(repo),
                        a["q"].as_str().unwrap_or(""),
                        budget,
                        false,
                        false,
                        vec![],
                    )
                    .await
                }
                "working_note_get" => crate::library::working_note_get(ctx, &format!("repo:{repo}")),
                "working_note_set" => crate::library::working_note_patch(
                    ctx,
                    &format!("repo:{repo}"),
                    a.get("requirements").and_then(Value::as_str),
                    a.get("decisions").and_then(Value::as_str),
                    a.get("failed_attempts").and_then(Value::as_str),
                    a.get("next_steps").and_then(Value::as_str),
                    None,
                    &format!("agent:{me}"),
                ),
                "session_search" => {
                    if a["repo_only"] == false { return Err("Cross-repository history is unavailable".into()); }
                    let c = ctx.clone();
                    let q = a["q"].as_str().unwrap_or("").to_string();
                    let repo = repo.to_string();
                    let limit = a["limit"].as_u64().unwrap_or(8) as usize;
                    tokio::task::spawn_blocking(move || crate::library::agent_sessions(&c, &q, &repo, limit))
                        .await
                        .map_err(|e| e.to_string())?
                }
                _ => {
                    if a["project"] == false { return Err("Global memory writes are unavailable".into()); }
                    let content = arg(&a, "content")?.to_string();
                    let category = arg(&a, "category")?.to_string();
                    let fact_key = a["fact_key"].as_str().map(str::to_string);
                    let supersedes_id = a["supersedes_id"].as_str().map(str::to_string);
                    let trust_tier = a["trust_tier"].as_str().map(str::to_string);
                    let provenance = a["provenance"].as_str().map(str::to_string);
                    let (c, repo_s, me_s, content_embed) = (ctx.clone(), repo.to_string(), me.clone(), content.clone());
                    let res = tokio::task::spawn_blocking(move || {
                        crate::library::save_agent_memory_rich(
                            &c,
                            &content,
                            &category,
                            &repo_s,
                            &me_s,
                            fact_key.as_deref(),
                            supersedes_id.as_deref(),
                            trust_tier.as_deref(),
                            provenance.as_deref(),
                        )
                    })
                    .await
                    .map_err(|e| e.to_string())??;
                    if res["status"].as_str() == Some("active") {
                        if let Some(mid) = res["id"].as_str() {
                            let _ = crate::library::embed_memory_async(ctx, mid, &content_embed).await;
                        }
                    }
                    Ok(res)
                }
            }
        }
        "resource_register" => {
            let kind = arg(&a, "kind")?;
            const KINDS: &[&str] =
                &["process", "container", "compose_project", "port", "tmpdir", "branch", "other"];
            if !KINDS.contains(&kind) {
                return Err(format!("kind must be one of {}", KINDS.join(", ")));
            }
            let id = crate::reaper::register(ctx, Some(agent), kind, arg(&a, "handle")?, json!({ "note": a["note"], "by": "agent" }));
            Ok(json!({ "resource_id": id }))
        }
        "kind_cluster" => match arg(&a, "action")? {
            "list" => crate::agents::call(ctx, "kind_list", json!({ "id": agent })).await,
            "create" | "delete" if row["strict"].as_i64() == Some(1) => Err("Strict agents cannot manage host kind clusters".into()),
            "create" => crate::agents::call(ctx, "kind_create", json!({ "id": agent, "suffix": a["suffix"] })).await,
            "delete" => crate::agents::call(ctx, "kind_delete", json!({ "id": agent, "cluster": arg(&a, "cluster")? })).await,
            other => Err(format!("unknown kind_cluster action `{other}` (use create, list, or delete)")),
        },
        _ => Err(format!("unknown tool {name}")),
    }
}

async fn shell_run(ctx: &Arc<Ctx>, agent: &str, me: &str, a: &Value) -> Result<Value, String> {
    let rt = crate::agents::runtime(agent).ok_or("agent not running")?;
    let command = arg(a, "command")?.to_string();
    let cwd = inside(&rt.cwd, a["cwd"].as_str())?;
    let timeout = a["timeout_secs"].as_u64().unwrap_or(120).clamp(1, 3600);
    if a["background"].as_bool() != Some(true) {
        let shell = crate::agentshell::shell_name(a["shell"].as_str())?;
        let dir = a["cwd"].as_str().filter(|s| !s.is_empty()).map(|_| cwd);
        return crate::agentshell::run(ctx, agent, &shell, &command, dir, std::time::Duration::from_secs(timeout)).await;
    }
    let argv = crate::sandbox::wrap_argv(rt.policy.as_ref(), vec!["/bin/sh".to_string(), "-lc".to_string(), command.clone()]);
    let title = a["title"].as_str().map(str::to_string).unwrap_or_else(|| {
        let c: String = command.chars().take(40).collect();
        format!("{me}: {c}")
    });
    let id = crate::terminals::open(
        ctx,
        crate::terminals::Opts { title, command: Some(argv), cwd, env: rt.env.clone(), agent_id: Some(agent.to_string()) },
    )?;
    // Give fast failures (bad command, port in use) a moment to show up.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let out = crate::terminals::snapshot(ctx, &id, 4096).unwrap_or_default();
    Ok(json!({ "terminal": id, "running": crate::terminals::alive(ctx, &id), "output": trim_output(&out, 4000) }))
}

pub async fn call(_ctx: &Arc<Ctx>, method: &str, _args: Value) -> Result<Value, String> {
    match method {
        "status" => Ok(json!({
            "url": _ctx.mcp_url.get(),
            "agents_connected": tokens().lk().len(),
        })),
        _ => Err(format!("unknown mcp.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protocol_and_auth() {
        let ctx = Ctx::test();
        ctx.db
            .exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('a1','lion','p/m',0,0)", [])
            .unwrap();
        let init = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}))
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.is_none());
        let list = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"shell_run") && names.contains(&"board_claim") && names.contains(&"mail_send"));
        let bad = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope"}})).await.unwrap();
        assert_eq!(bad["result"]["isError"], true);

        let t = issue_token("a1");
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {t}").parse().unwrap());
        assert_eq!(who(&h).as_deref(), Some("a1"));
        revoke("a1");
        assert_eq!(who(&h), None);
    }

    #[tokio::test]
    async fn skills_and_memory_tools_respect_strict() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        crate::library::init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','same','/one/same',0),('rb','same','/two/same',0)", []).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,strict,created_at,last_active_at) VALUES('t1','owl','p/m',0,0,0),('s1','fox','p/m',1,0,0)", []).unwrap();
        ctx.db.exec("INSERT INTO tasks(id,title,repo_id,created_at,updated_at) VALUES('a','scoped-marker-A','ra',0,0),('b','scoped-marker-B','rb',0,0)", []).unwrap();
        ctx.db.exec("UPDATE agents SET repo_id='ra',task_id='a' WHERE id='t1'", []).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,repo_id,model_ref,task_id,created_at,last_active_at) VALUES('peer','ibis','rb','p/m','b',0,0),('none','duck',NULL,'p/m',NULL,0,0)", []).unwrap();
        ctx.db.exec("INSERT INTO skills(name,description,body,source,updated_at) VALUES('review-pr','Review','do it','jcode',0)", []).unwrap();
        let call = |agent: &'static str, name: &'static str, args: Value| {
            let ctx = ctx.clone();
            async move { dispatch(&ctx, agent, json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).await.unwrap() }
        };
        let text = |r: &Value| r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        crate::board::init(&ctx).unwrap();
        let tasks = text(&call("t1","board_list",json!({"repo_id":"rb"})).await);
        assert!(tasks.contains("scoped-marker-A") && !tasks.contains("scoped-marker-B"));
        assert!(text(&call("t1","board_get",json!({"id":"a"})).await).contains("scoped-marker-A"));
        for tool in ["board_get","board_claim","board_update","board_submit"] {
            assert_eq!(call("t1",tool,json!({"id":"b"})).await["result"]["isError"],true);
        }
        let roster = text(&call("t1","agent_status",json!({})).await);
        assert!(roster.contains("owl") && !roster.contains("ibis"));
        for to in ["all","ibis"] { assert_eq!(call("t1","mail_send",json!({"to":to,"body":"denied"})).await["result"]["isError"],true); }
        crate::board::send(&ctx,"you","all","legacy-broadcast").unwrap();
        crate::board::send(&ctx,"you","owl","direct-marker").unwrap();
        let mail = text(&call("t1","mail_read",json!({})).await);
        assert!(mail.contains("direct-marker") && !mail.contains("legacy-broadcast"));
        ctx.db.exec("UPDATE agents SET model_ref='q/m' WHERE id='t1'",[]).unwrap();
        assert!(!text(&call("t1","mail_read",json!({"unread_only":false})).await).contains("direct-marker"));
        ctx.db.exec("UPDATE agents SET model_ref='p/m' WHERE id='t1'",[]).unwrap();
        assert_eq!(call("s1", "skill_get", json!({"name":"review-pr"})).await["result"]["isError"], true);
        let selected = text(&call("t1", "skill_get", json!({"name":"review-pr"})).await);
        assert!(selected.contains("Philium built-in") && !selected.contains("do it"));
        let delivered:Value=serde_json::from_str(&selected).unwrap();
        assert_eq!(delivered["policy_sha256"],crate::board::review_policy_digest());
        assert_eq!(delivered["body"],crate::board::REVIEW_POLICY);
        assert!(crate::library::skill_get(&ctx,"review-pr").unwrap()["body"] == "do it");
        for args in [json!({"task_id":"b"}), json!({"model":"other/m"}), json!({"model":"p/other"})] {
            let mut attempt = args; attempt["brief"] = json!("test");
            let denied = call("t1", "agent_spawn", attempt).await;
            assert_eq!(denied["result"]["isError"], true);
            assert!(text(&denied).to_lowercase().contains("helper"));
        }
        let saved = call("t1", "memory_save", json!({"content":"Filip likes terse replies","category":"preference"})).await;
        assert_ne!(saved["result"]["isError"], true, "{saved}");
        assert!(text(&saved).contains("repo:ra"));
        crate::library::memory_save(&ctx, "terse legacy-global", "preference", None, "fixture").unwrap();
        crate::library::save_agent_memory(&ctx, "terse sibling-private", "fact", "rb", "fixture").unwrap();
        assert_eq!(call("t1", "memory_search", json!({"q":""})).await["result"]["isError"], true);
        for q in ["terse", "*"] {
            let hits = text(&call("t1", "memory_search", json!({"q":q,"repo_id":"rb"})).await);
            assert!(hits.contains("terse replies") && !hits.contains("legacy-global") && !hits.contains("sibling-private"), "fixture query {q:?}: {hits}");
        }
        let hits = text(&call("t1", "session_search", json!({"q":"scoped-marker","repo_id":"rb"})).await);
        assert!(hits.contains("scoped-marker-A") && !hits.contains("scoped-marker-B"));
        for (agent, tool, args) in [
            ("none", "session_search", json!({})), ("none", "memory_search", json!({"q":"terse"})),
            ("none", "memory_save", json!({"category":"fact","content":"blocked"})),
            ("t1", "session_search", json!({"repo_only":false})),
            ("t1", "memory_save", json!({"project":false,"category":"fact","content":"blocked"})),
        ] { assert_eq!(call(agent, tool, args).await["result"]["isError"], true); }
        assert_eq!(ctx.db.query("SELECT count(*) AS n FROM memories WHERE content='blocked'", []).unwrap()[0]["n"],0);
        assert!(text(&call("t1", "memory_search", json!({"q":"terse"})).await).contains("terse replies"));
        let denied = call("s1", "memory_search", json!({"q":"terse"})).await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(!text(&denied).contains("terse replies"));

        // Imported skills are visible to Standard agents, never to Strict agents; built-in review-pr always wins.
        ctx.db.exec(
            "INSERT INTO skills(name,description,body,files,source,updated_at) VALUES('make-plan','Plan work','# Plan steps','{\"check.sh\":\"echo plan\"}','jcode',0)",
            [],
        ).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,repo_id,model_ref,strict,created_at,last_active_at) VALUES('s2','strict-repo','ra','p/m',1,0,0)", []).unwrap();

        let std_list: Value = serde_json::from_str(&text(&call("t1", "skill_list", json!({})).await)).unwrap();
        let std_names: Vec<&str> = std_list.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(std_names, vec!["review-pr", "make-plan"]);
        let std_imported: Value = serde_json::from_str(&text(&call("t1", "skill_get", json!({"name":"make-plan"})).await)).unwrap();
        assert_eq!(std_imported["body"], "# Plan steps");
        assert_eq!(std_imported["files"]["check.sh"], "echo plan");

        let strict_list: Value = serde_json::from_str(&text(&call("s2", "skill_list", json!({})).await)).unwrap();
        let strict_names: Vec<&str> = strict_list.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(strict_names, vec!["review-pr"]);
        let strict_builtin: Value = serde_json::from_str(&text(&call("s2", "skill_get", json!({"name":"review-pr"})).await)).unwrap();
        assert_eq!(strict_builtin["body"], crate::board::REVIEW_POLICY);
        assert_eq!(call("s2", "skill_get", json!({"name":"make-plan"})).await["result"]["isError"], true);

        // board_submit surfaces matching on_pr / pre_release reminders to Standard agents ONLY, NEVER Strict.
        crate::library::intention_add(&ctx, "repo:ra", "on_pr", "", "Verify changelog entry", "user").unwrap();
        crate::library::intention_add(&ctx, "repo:ra", "pre_release", "release", "Run smoke suite", "user").unwrap();
        crate::library::intention_add(&ctx, "repo:rb", "on_pr", "", "Sibling repo reminder", "user").unwrap();
        ctx.db.exec(
            "INSERT INTO tasks(id,title,repo_id,status,created_at,updated_at) VALUES('t_std','Ship release','ra','open',0,0),('t_strict','Strict task','ra','open',0,0)",
            [],
        ).unwrap();
        assert_ne!(call("t1", "board_claim", json!({"id":"t_std"})).await["result"]["isError"], true);
        let sub_std: Value = serde_json::from_str(&text(&call("t1", "board_submit", json!({"id":"t_std","summary":"Ready for release"})).await)).unwrap();
        let rems = sub_std["reminders"].as_array().expect("Standard board_submit gets reminders");
        assert_eq!(rems.len(), 2, "{sub_std}");
        assert!(!sub_std.to_string().contains("Sibling repo reminder"));

        assert_ne!(call("s2", "board_claim", json!({"id":"t_strict"})).await["result"]["isError"], true);
        let sub_strict: Value = serde_json::from_str(&text(&call("s2", "board_submit", json!({"id":"t_strict","summary":"Ready for release"})).await)).unwrap();
        assert!(sub_strict.get("reminders").is_none(), "Strict board_submit must never include reminders: {sub_strict}");

        // Partial-patch working_note_set preserves omitted fields instead of wiping them with "".
        let wn1: Value = serde_json::from_str(&text(&call("t1", "working_note_set", json!({
            "requirements": "Offline startup required",
            "decisions": "Use SQLite FTS5",
            "failed_attempts": "ENETUNREACH on HF download",
            "next_steps": "Run smoke test"
        })).await)).unwrap();
        assert_eq!(wn1["requirements"], "Offline startup required");
        assert!(wn1["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));
        let wn2: Value = serde_json::from_str(&text(&call("t1", "working_note_set", json!({
            "next_steps": "Verify release bundle"
        })).await)).unwrap();
        assert_eq!(wn2["requirements"], "Offline startup required");
        assert_eq!(wn2["decisions"], "Use SQLite FTS5");
        assert_eq!(wn2["failed_attempts"], "ENETUNREACH on HF download");
        assert_eq!(wn2["next_steps"], "Verify release bundle");

        // on_error and path_match reminders fire on MCP tool calls for Standard agents ONLY, NEVER Strict.
        crate::library::intention_add(&ctx, "repo:ra", "on_error", "unavailable", "Check workspace task ID before retrying", "user").unwrap();
        crate::library::intention_add(&ctx, "repo:ra", "path_match", "migrations/", "Do not edit applied migrations", "user").unwrap();
        let err_std = call("t1", "board_get", json!({"id": "b"})).await;
        assert_eq!(err_std["result"]["isError"], true);
        let err_blocks: Vec<&str> = err_std["result"]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
        assert!(err_blocks.iter().any(|b| b.contains("Check workspace task ID before retrying")), "{err_blocks:?}");
        let err_strict = call("s2", "board_get", json!({"id": "b"})).await;
        let strict_blocks: Vec<&str> = err_strict["result"]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
        assert!(!strict_blocks.iter().any(|b| b.contains("Check workspace task ID")), "{strict_blocks:?}");

        let path_std = call("t1", "code_outline", json!({"path": "db/migrations/001.sql"})).await;
        let path_blocks: Vec<&str> = path_std["result"]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
        assert!(path_blocks.iter().any(|b| b.contains("Do not edit applied migrations")), "{path_blocks:?}");
        // Deduped on immediate second call to the same path.
        let path_again = call("t1", "code_outline", json!({"path": "db/migrations/001.sql"})).await;
        let again_blocks: Vec<&str> = path_again["result"]["content"].as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
        assert!(!again_blocks.iter().any(|b| b.contains("Do not edit applied migrations")), "{again_blocks:?}");
    }

    #[test]
    fn trims_long_output() {
        let s = "x".repeat(100_000);
        let t = trim_output(&s, 1000);
        assert!(t.len() < 1100 && t.contains("omitted"));
        assert_eq!(trim_output("short", 1000), "short");
    }

    #[tokio::test]
    async fn shell_read_over_http_reports_completion_and_isolates_peers() {
        let ctx = Ctx::test();
        for (id, name) in [("poll-owner", "poll-owner"), ("poll-peer", "poll-peer")] {
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES(?1,?2,'p/m',0,0)", [id, name]).unwrap();
        }
        // Use the production router, authentication and JSON-RPC serialization
        // over a real TCP listener, not a mocked tool result or dispatcher call.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = router(ctx.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(3)).build().unwrap();
        let owner = issue_token("poll-owner");
        let peer = issue_token("poll-peer");
        let init = json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2025-03-26"}});
        let response = client.post(&url).bearer_auth(&owner).json(&init).send().await.unwrap();
        assert!(response.status().is_success());
        assert_eq!(response.json::<Value>().await.unwrap()["result"]["serverInfo"]["name"], "philium");
        let id = crate::terminals::open(&ctx, crate::terminals::Opts {
            title: "poll-test".into(),
            command: Some(vec!["/bin/bash".into(), "-c".into(), "sleep 0.3; echo poll-complete; exit 7".into()]),
            cwd: ctx.data_dir.clone(), env: vec![], agent_id: Some("poll-owner".into()),
        }).unwrap();
        let request = json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"shell_read", "arguments":{"id":id}}});
        let read = |token: String| {
            let client = client.clone();
            let url = url.clone();
            let request = request.clone();
            async move {
                let response = client.post(url).bearer_auth(token).json(&request).send().await.unwrap();
                assert!(response.status().is_success());
                response.json::<Value>().await.unwrap()
            }
        };
        let decode = |response: Value| -> Value {
            assert_eq!(response["result"]["isError"], false);
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
        };
        let first = decode(read(owner.clone()).await);
        assert_eq!(first["running"], true);
        assert_eq!(first["finished"], false);
        let denied = read(peer).await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["content"][0]["text"].as_str().unwrap().contains("someone else"));
        assert_eq!(client.post(&url).json(&request).send().await.unwrap().status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(client.post(&url).bearer_auth(&owner).header("host", "untrusted.example").json(&request).send().await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
        let mut completed = None;
        for _ in 0..200 {
            let result = decode(read(owner.clone()).await);
            if result["finished"] == true { completed = Some(result); break; }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let result = completed.expect("background command did not expose completion");
        assert_eq!(result["running"], false);
        assert_eq!(result["exit_code"], 7);
        assert!(result["output"].as_str().unwrap().contains("poll-complete"));
        assert!(!crate::terminals::alive(&ctx, &id));
        // A second query retains the terminal's final result.
        let second = decode(read(owner).await);
        assert_eq!(second["finished"], true);
        assert_eq!(second["exit_code"], 7);
        revoke("poll-owner");
        revoke("poll-peer");
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn cwd_stays_inside() {
        let base = crate::ctx::test_dir("mcp-cwd");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        assert!(inside(&base, Some("sub")).is_ok());
        assert!(inside(&base, Some("..")).is_err());
        assert!(inside(&base, Some("/etc")).is_err());
        assert_eq!(inside(&base, None).unwrap(), base);
    }

    #[tokio::test]
    async fn codegraph_tools_and_focus_edges_work_end_to_end() {
        let ctx = Ctx::test();
        crate::codeindex::init(&ctx).unwrap();
        crate::graph::init(&ctx).unwrap();
        let repo_dir = ctx.data_dir.join("repo-cg");
        std::fs::create_dir_all(repo_dir.join("src")).unwrap();
        std::fs::write(
            repo_dir.join("src/a.rs"),
            "pub struct Engine;\nimpl Engine {\n    pub fn run(&self) -> i32 { helper(41) }\n}\npub fn helper(x: i32) -> i32 { x + 1 }\n",
        )
        .unwrap();
        std::fs::write(
            repo_dir.join("src/b.rs"),
            "use crate::a::{Engine, helper};\npub fn entry() -> i32 { let e = Engine; e.run() + helper(1) }\n",
        )
        .unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["add", "."],
            vec!["commit", "-m", "init"],
        ] {
            assert!(std::process::Command::new("git").args(&args).current_dir(&repo_dir).status().unwrap().success());
        }
        let repo_str = repo_dir.to_string_lossy().to_string();
        ctx.db
            .exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r1','cg',?1,'main',0)", [&repo_str])
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES('a1','lion','p/m','r1',0,0)",
                [],
            )
            .unwrap();

        let pmap = crate::codeindex::prompt_map(&ctx, "r1", None, 1200, false).expect("prompt_map should render");
        assert!(pmap.contains("## Codebase map") && pmap.contains("src/a.rs  ") && pmap.contains("helper 5"), "{pmap}");
        assert!(!pmap.contains("## Worktrees and branches"), "a worker's map carries no landscape: {pmap}");

        let call = |name: &'static str, args: Value| {
            let ctx = ctx.clone();
            async move {
                let r = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}))
                    .await
                    .unwrap();
                assert_ne!(r["result"]["isError"], true, "{r}");
                r["result"]["content"][0]["text"].as_str().unwrap().to_string()
            }
        };

        let outline = call("code_outline", json!({"path":"src/a.rs"})).await;
        assert!(outline.contains("outline src/a.rs:") && outline.contains("Engine.run") && outline.contains("helper"), "{outline}");

        let sym = call("code_symbol", json!({"symbol":"Engine.run"})).await;
        assert!(sym.contains("symbol Engine.run (method) in src/a.rs:3-3") && sym.contains("helper(41)"), "{sym}");

        let refs = call("code_refs", json!({"symbol":"Engine.run"})).await;
        assert!(refs.contains("calls (1 indexed symbols): helper"), "{refs}");

        // Words find the function whose body mentions them, not only names.
        let found = call("code_find", json!({"query":"engine run"})).await;
        assert!(found.contains("src/b.rs:2-2 fn entry") && found.contains("[engine, run]"), "{found}");

        // An overlap found by a clash scan reaches the agent on its next tool call, once.
        ctx.db.exec("INSERT INTO worktrees(id,repo_id,agent_id,path,branch,base_commit,state,created_at) VALUES('w1','r1','a1','/nowhere','b1','x','active',0)", []).unwrap();
        let clash = crate::codeindex::Clash::test_pair("w1", "ext:r1:x");
        crate::codeindex::publish(&ctx, &[clash], Some("r1"));
        let first = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"code_outline","arguments":{"path":"src/a.rs"}}})).await.unwrap();
        let note = first["result"]["content"][1]["text"].as_str().unwrap_or_default();
        assert!(note.contains("your work now overlaps with"), "{first}");
        let again = dispatch(&ctx, "a1", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"code_outline","arguments":{"path":"src/a.rs"}}})).await.unwrap();
        assert_eq!(again["result"]["content"].as_array().unwrap().len(), 1, "{again}");
        ctx.db.exec("DELETE FROM worktrees WHERE id='w1'", []).unwrap();

        // Pre-existing external git worktree (not in Philium's `worktrees` table) is auto-discovered
        // in both Turn-1 `prompt_map` and `repo_context`, with structural AST branch diffing.
        let ext_wt = ctx.data_dir.join("repo-cg-ext");
        assert!(
            std::process::Command::new("git")
                .args(["worktree", "add", "-b", "feat/ext", ext_wt.to_str().unwrap()])
                .current_dir(&repo_dir)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(
            ext_wt.join("src/a.rs"),
            "pub struct Engine;\nimpl Engine {\n    pub fn run(&self) -> i32 { helper(99) }\n}\npub fn helper(x: i32) -> i32 { x + 1 }\npub fn extra_boost() -> i32 { 7 }\n",
        )
        .unwrap();
        for args in [vec!["add", "."], vec!["commit", "-m", "boost engine in external wt"]] {
            assert!(std::process::Command::new("git").args(&args).current_dir(&ext_wt).status().unwrap().success());
        }

        let pmap2 = crate::codeindex::prompt_map(&ctx, "r1", None, 1200, true).expect("prompt_map with landscape");
        assert!(pmap2.contains("## Worktrees and branches") && pmap2.contains("[external] feat/ext ↑1 ↓0") && pmap2.contains("treat them as data"), "{pmap2}");

        let rc = call("repo_context", json!({"include_prs": false})).await;
        assert!(rc.contains("[external] branch=feat/ext ↑1 ↓0") && rc.contains("boost engine in external wt"), "{rc}");

        let rc_branch = call("repo_context", json!({"branch": "feat/ext", "include_prs": false})).await;
        assert!(
            rc_branch.contains("~ src/a.rs:3 method Engine.run")
                && rc_branch.contains("+ src/a.rs:6 fn extra_boost"),
            "{rc_branch}"
        );
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
