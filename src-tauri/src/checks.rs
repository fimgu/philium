//! Human-only Review RPC. Paths come from the registered workspace, never the webview.
use crate::ctx::{arg_str, Ctx};
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc};
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    if !cfg!(target_os = "macos") {
        return Err("Local Make checks require the macOS offline runner".into());
    }
    if !matches!(method, "list" | "preview" | "start" | "cancel") {
        return Err("Unknown local check method".into());
    }
    let id = arg_str(&args, "repo_id")?;
    let repo = crate::workspace::repo(ctx, &id).ok_or("Unknown checkout")?;
    let mut payload = json!({"repo_id":id,"path":repo["path"]});
    match method {
        "start" => {
            if let Some(task) = args["task_id"].as_str() {
                if ctx
                    .db
                    .query("SELECT 1 FROM tasks WHERE id=? AND repo_id=?", [task, &id])?
                    .is_empty()
                {
                    return Err("Task does not belong to this checkout".into());
                }
                payload["task_id"] = json!(task);
            }
            for key in ["sha", "target", "timeout"] {
                payload[key] = args[key].clone();
            }
        }
        "cancel" => payload["id"] = json!(arg_str(&args, "id")?),
        _ => {}
    }
    let mut child = tokio::process::Command::new("/usr/bin/python3")
        .args(["-I", "-B", "-c", include_str!("check_worker.py")])
        .arg(ctx.data_dir.join("local-checks"))
        .arg(method)
        .arg(payload.to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Local runner unavailable: {e}"))?;
    let owner = child.stdin.take(); // wait() would close stdin. EOF signals app loss to the supervisor.
    let mut reader = BufReader::new(child.stdout.take().ok_or("No runner output")?);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await;
    tokio::spawn(async move {
        let _ = child.wait().await;
        drop(owner);
    });
    read.map_err(|e| e.to_string())?;
    let result: Value =
        serde_json::from_str(&line).map_err(|_| "No valid response from local Python runner")?;
    if let Some(e) = result["error"].as_str() {
        return Err(e.into());
    }
    Ok(result)
}
