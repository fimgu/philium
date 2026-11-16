//! Philium's agent tools, served over MCP (Streamable HTTP, JSON responses)
//! on a loopback port. Each agent's HAL connects with its own bearer token, so
//! every call is attributed to exactly one agent (SPEC 8).
//!
//! Tools (model-visible as `mcp_philium_<name>`):
//!   board_list, board_get, board_create, board_claim, board_update, board_submit,
//!   mail_send, mail_read, agent_spawn, agent_status, agent_stop,
//!   shell_run, shell_read, shell_write, shell_kill,
//!   browser_open, browser_snapshot, graph_query, playbook_get, resource_register
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
            let r = call_tool(ctx, agent, &name, args).await;
            Ok(match r {
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
            })
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
        t("board_list", "List tasks on the shared master board. Filter by status (open, claimed, in_review, done, cancelled), owner (agent name) or q (text).",
          json!({ "status": s, "owner": s, "q": s }), &[]),
        t("board_get", "Full task: description, dependencies, playbook progress, history.", json!({ "id": s }), &["id"]),
        t("board_create", "Add a task to the master board.",
          json!({ "title": s, "detail": s, "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
                  "depends_on": { "type": "array", "items": s }, "playbook": s }), &["title"]),
        t("board_claim", "Claim a task with an exclusive lease, or renew your lease (heartbeat). Only open tasks whose dependencies are done can be claimed.",
          json!({ "id": s, "lease_secs": i }), &["id"]),
        t("board_update", "Update a task you own: note progress, change status, set the current playbook step or PR URL.",
          json!({ "id": s, "status": s, "note": s, "step": s, "pr_url": s, "detail": s }), &["id"]),
        t("board_submit", "Submit a claimed task for review with a summary and the PR URL. Playbook checks run.",
          json!({ "id": s, "summary": s, "pr_url": s }), &["id", "summary"]),
        t("mail_send", "Message another agent by name, \"all\" for every agent, or \"you\" for the human.",
          json!({ "to": s, "body": s }), &["to", "body"]),
        t("mail_read", "Read your inbox (marks messages read).", json!({ "unread_only": b }), &[]),
        t("agent_spawn", "Start a helper agent. It gets its own worktree of the same repo and its own session.",
          json!({ "name": s, "brief": s, "model": s, "role": s, "task_id": s }), &["brief"]),
        t("agent_status", "Status of agents: everyone, or one by name.", json!({ "name": s }), &[]),
        t("agent_stop", "Stop a helper agent you spawned.", json!({ "name": s }), &["name"]),
        t("shell_run", "Run a command in your persistent interactive shell (bash), visible live to the human. State carries over (cd, exports, venvs). Waits at most 30 seconds, or timeout_secs if shorter, then returns terminal, running, finished, output and exit_code. A running response is NOT failure: do not retry the command. Poll shell_read with the terminal id until finished, and check exit_code. Pass shell to use another shell. Use background=true for builds, test suites, dev servers and watchers to return a terminal id immediately.",
          json!({ "command": s, "cwd": s, "timeout_secs": i, "background": b, "title": s, "shell": s }), &["command"]),
        t("shell_open", "Open another persistent shell by name (for example \"dev\" or \"tests\") when your main shell is busy or you want a separate session. Use the name with shell_run's shell argument. Returns the terminal id.",
          json!({ "name": s }), &["name"]),
        t("shell_list", "Your terminals: shells and background commands, with state.", json!({}), &[]),
        t("shell_read", "Recent terminal output plus running, finished and exit_code. For persistent shells, these describe the command, not the shell process. finished=null means no pending command result. Poll a running command instead of launching it again. A finished command with nonzero exit_code failed.", json!({ "id": s, "max_bytes": i }), &["id"]),
        t("shell_write", "Send input to one of your terminals (include \\n to press Enter).", json!({ "id": s, "data": s }), &["id", "data"]),
        t("shell_kill", "Stop one of your terminals.", json!({ "id": s }), &["id"]),
        t("graph_query", "Code graph of your repo: which files import which. Cheaper than reading files. Optional `path` focuses on one file's neighbours.",
          json!({ "path": s }), &[]),
        t("playbook_get", "The standard steps and checks for a job: commit, open_pr, test_pr, review_pr, docs, release, teardown.",
          json!({ "job": s }), &["job"]),
        t("resource_register", "Declare something you created outside Philium's tools (container, cluster, process, temp dir) so it is cleaned up with you.",
          json!({ "kind": s, "handle": s, "note": s }), &["kind", "handle"]),
    ].into_iter().chain(crate::browser::mcp_tools()).chain(crate::codeindex::mcp_tools()).collect()
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
    match name {
        "board_list" => {
            let mut f = a.clone();
            if let Some(o) = a["owner"].as_str() {
                f["owner"] = json!(crate::agents::get_row(ctx, o).and_then(|r| r["id"].as_str().map(str::to_string)).unwrap_or_default());
            }
            crate::board::list(ctx, &f)
        }
        "board_get" => crate::board::get(ctx, arg(&a, "id")?),
        "board_create" => {
            let deps: Vec<String> = a["depends_on"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            crate::board::create(
                ctx,
                &me,
                arg(&a, "title")?,
                a["detail"].as_str().unwrap_or(""),
                a["priority"].as_str(),
                row["repo_id"].as_str(),
                &deps,
                a["playbook"].as_str(),
            )
        }
        "board_claim" => {
            let r = crate::board::claim(ctx, agent, arg(&a, "id")?, a["lease_secs"].as_i64().unwrap_or(900).clamp(60, 7200))?;
            let _ = ctx.db.exec("UPDATE agents SET task_id=?2 WHERE id=?1", rusqlite::params![agent, arg(&a, "id")?]);
            ctx.emit("agents:changed", json!({ "id": agent }));
            Ok(r)
        }
        "board_update" => {
            let mut patch = a.clone();
            if let Some(o) = patch.as_object_mut() {
                o.remove("id");
            }
            crate::board::update(ctx, agent, arg(&a, "id")?, &patch)
        }
        "board_submit" => crate::board::submit(ctx, agent, arg(&a, "id")?, arg(&a, "summary")?, a["pr_url"].as_str()),
        "mail_send" => {
            let to = arg(&a, "to")?;
            if !matches!(to, "all" | "you") && crate::agents::get_row(ctx, to).is_none() {
                return Err(format!("no agent named {to}"));
            }
            crate::board::send(ctx, &me, to, arg(&a, "body")?)
        }
        "mail_read" => crate::board::inbox(ctx, &me, a["unread_only"].as_bool().unwrap_or(true), true),
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
                },
            )?;
            Ok(json!({ "name": r["name"], "id": r["id"], "state": r["state"] }))
        }
        "agent_status" => {
            let all = crate::agents::list(ctx)?;
            let pick = |r: &Value| {
                json!({ "name": r["name"], "state": r["state"], "role": r["role"], "model": r["model_ref"],
                        "task": r["task_title"], "branch": r["branch"] })
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
        "graph_query" => {
            let path = crate::agents::runtime(agent).map(|r| r.cwd.clone()).ok_or("agent not running")?;
            let g = crate::graph::call(ctx, "get", json!({ "repo_path": path })).await?;
            Ok(match a["path"].as_str() {
                Some(p) => focus(&g, p),
                None => g,
            })
        }
        // --- codeindex tools ---
        "code_map" | "code_find" | "code_refs" | "code_impact" => crate::codeindex::mcp_call(ctx, agent, name, a).await,
        // --- end codeindex tools ---
        "playbook_get" => {
            let repo = crate::agents::runtime(agent).map(|r| r.cwd.clone());
            crate::board::playbook(ctx, arg(&a, "job")?, repo.as_deref())
        }
        "resource_register" => {
            let kind = arg(&a, "kind")?;
            const KINDS: &[&str] =
                &["process", "container", "compose_project", "kind_cluster", "port", "tmpdir", "branch", "other"];
            if !KINDS.contains(&kind) {
                return Err(format!("kind must be one of {}", KINDS.join(", ")));
            }
            let id = crate::reaper::register(ctx, Some(agent), kind, arg(&a, "handle")?, json!({ "note": a["note"], "by": "agent" }));
            Ok(json!({ "resource_id": id }))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Neighbourhood of one file in the code graph.
fn focus(g: &Value, path: &str) -> Value {
    let edges: Vec<&Value> = g["edges"]
        .as_array()
        .map(|e| {
            e.iter()
                .filter(|e| {
                    e["from"].as_str().is_some_and(|f| f.ends_with(path)) || e["to"].as_str().is_some_and(|t| t.ends_with(path))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "file": path, "edges": edges })
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

    #[test]
    fn trims_long_output() {
        let s = "x".repeat(100_000);
        let t = trim_output(&s, 1000);
        assert!(t.len() < 1100 && t.contains("omitted"));
        assert_eq!(trim_output("short", 1000), "short");
    }

    #[tokio::test]
    async fn shell_read_reports_background_completion_and_isolates_peers() {
        let ctx = Ctx::test();
        for (id, name) in [("poll-owner", "poll-owner"), ("poll-peer", "poll-peer")] {
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES(?1,?2,'p/m',0,0)", [id, name]).unwrap();
        }
        let id = crate::terminals::open(&ctx, crate::terminals::Opts {
            title: "poll-test".into(),
            command: Some(vec!["/bin/bash".into(), "-c".into(), "sleep 0.3; echo poll-complete; exit 7".into()]),
            cwd: ctx.data_dir.clone(), env: vec![], agent_id: Some("poll-owner".into()),
        }).unwrap();
        let first = call_tool(&ctx, "poll-owner", "shell_read", json!({"id":id})).await.unwrap();
        assert_eq!(first["running"], true);
        assert_eq!(first["finished"], false);
        assert!(call_tool(&ctx, "poll-peer", "shell_read", json!({"id":id})).await.unwrap_err().contains("someone else"));
        let mut completed = None;
        for _ in 0..200 {
            let result = call_tool(&ctx, "poll-owner", "shell_read", json!({"id":id})).await.unwrap();
            if result["finished"] == true { completed = Some(result); break; }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let result = completed.expect("background command did not expose completion");
        assert_eq!(result["running"], false);
        assert_eq!(result["exit_code"], 7);
        assert!(result["output"].as_str().unwrap().contains("poll-complete"));
        assert!(!crate::terminals::alive(&ctx, &id));
    }

    #[test]
    fn cwd_stays_inside() {
        let base = std::env::temp_dir().join(format!("mcp-cwd-{}", crate::db::new_id()));
        std::fs::create_dir_all(base.join("sub")).unwrap();
        assert!(inside(&base, Some("sub")).is_ok());
        assert!(inside(&base, Some("..")).is_err());
        assert!(inside(&base, Some("/etc")).is_err());
        assert_eq!(inside(&base, None).unwrap(), base);
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
