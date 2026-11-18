//! Human-only Review RPC. Paths come from the registered workspace, never the webview.
use crate::ctx::{arg_str, Ctx};
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc};
use tokio::io::{AsyncBufReadExt, BufReader};

// A socket is a discovery hint, not proof of a healthy daemon, a deployment or ownership.
// Store only profile names, scan status and time. No logs, paths, environment or secrets.
const PREFLIGHT:&str="CREATE TABLE IF NOT EXISTS check_preflight(repo TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE, observed INTEGER NOT NULL, profiles TEXT, CHECK(profiles IS NULL OR length(profiles)<=18000))";
pub(crate) fn remember(
    db: &crate::db::Db,
    repo: &str,
    at: i64,
    scan: Result<Value, String>,
) -> Result<(), String> {
    let names = scan.ok().map(|v| v.to_string());
    db.with(|c| c.execute_batch(PREFLIGHT))?;
    db.exec("INSERT INTO check_preflight(repo,observed,profiles) VALUES(?,?,?) ON CONFLICT(repo) DO UPDATE SET observed=excluded.observed,profiles=excluded.profiles WHERE excluded.observed>=check_preflight.observed",rusqlite::params![repo,at,names])?;
    Ok(())
}
fn recalled(db: &crate::db::Db, repo: &str) -> Result<Value, String> {
    db.with(|c| c.execute_batch(PREFLIGHT))?;
    let Some(row) = db
        .query(
            "SELECT observed,profiles FROM check_preflight WHERE repo=?",
            [repo],
        )?
        .pop()
    else {
        return Ok(Value::Null);
    };
    let profiles: Value = match row["profiles"].as_str() {
        Some(s) => serde_json::from_str(s).map_err(|_| "Invalid saved inventory")?,
        None => Value::Null,
    };
    let observed = row["observed"].as_i64().ok_or("Invalid observation time")?;
    Ok(json!({"observed":observed,"expires":observed.saturating_add(300_000),"profiles":profiles}))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    if !cfg!(target_os = "macos") {
        return Err("Local Make checks require the macOS offline runner".into());
    }
    if !matches!(
        method,
        "list" | "preview" | "start" | "cancel" | "remembered"
    ) {
        return Err("Unknown local check method".into());
    }
    let id = arg_str(&args, "repo_id")?;
    let repo = crate::workspace::repo(ctx, &id).ok_or("Unknown checkout")?;
    if method == "remembered" {
        return recalled(&ctx.db, &id);
    }
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
    if matches!(method, "preview" | "start") {
        let at = crate::db::now();
        let scan = crate::environments::call("profiles", json!({})).await;
        remember(&ctx.db, &id, at, scan)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{new_id, Db};
    #[test]
    fn preflight_recovers_only_last_observation_per_checkout() {
        let dir = std::env::temp_dir().join(format!("preflight-{}", new_id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let cleanup = Cleanup(dir.clone());
        let path = dir.join("fixture.sqlite");
        {
            let db = Db::open(&path).unwrap();
            for repo in ["a", "b"] {
                db.exec(
                    "INSERT INTO repos(id,name,path,added_at) VALUES(?1,'same',?1,1)",
                    [repo],
                )
                .unwrap();
            }
            remember(&db, "a", 20, Ok(json!(["shared"]))).unwrap();
            remember(&db, "a", 10, Ok(json!(["obsolete"]))).unwrap();
            assert!(recalled(&db, "b").unwrap().is_null());
        }
        {
            let db = Db::open(&path).unwrap();
            assert_eq!(
                recalled(&db, "a").unwrap(),
                json!({"observed":20,"expires":300020,"profiles":["shared"]})
            );
            remember(&db, "a", 30, Err("private runtime detail".into())).unwrap();
            assert_eq!(
                recalled(&db, "a").unwrap(),
                json!({"observed":30,"expires":300030,"profiles":null})
            );
            remember(&db, "b", 31, Ok(json!([]))).unwrap();
            assert_eq!(recalled(&db, "b").unwrap()["profiles"], json!([]));
            assert_eq!(
                db.query("SELECT count(*) AS n FROM check_preflight", [])
                    .unwrap()[0]["n"],
                2
            );
        }
        drop(cleanup);
        assert!(!dir.exists());
    }
}
