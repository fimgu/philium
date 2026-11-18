use crate::{ctx::Ctx, release_intake::graph};
use rusqlite::params;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;
const SCHEMA:&str="CREATE TABLE IF NOT EXISTS release_membership(seq INTEGER PRIMARY KEY REFERENCES release_refs(seq),summary TEXT NOT NULL)";
static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn lookup(o: &str, ids: &[&Value]) -> Result<Value, String> {
    let (owner, name) = o.split_once('/').ok_or("Missing repository identity")?;
    let mut body = String::new();
    for (i, row) in ids.iter().enumerate() {
        let id = row["oid"]
            .as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("Invalid commit")?;
        body+=&format!(" c{i}:object(oid:\"{id}\"){{ ... on Commit {{ oid associatedPullRequests(first:32){{pageInfo{{hasNextPage}} nodes{{number merged mergeCommit{{oid}} repository{{nameWithOwner}}}}}} }} }}");
    }
    let query=format!("query($owner:String!,$name:String!){{repository(owner:$owner,name:$name){{nameWithOwner {body}}}}}");
    let exe = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .ok_or("Origin reader unavailable")?;
    let mut child = tokio::process::Command::new(exe)
        .current_dir("/")
        .args([
            "api",
            "--hostname",
            "github.com",
            "graphql",
            "-f",
            &format!("query={query}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Cannot start origin reader")?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut buf = Vec::new();
        child
            .stdout
            .take()
            .ok_or("Missing output")?
            .take(262145)
            .read_to_end(&mut buf)
            .await
            .map_err(|_| "Origin unavailable")?;
        if buf.len() > 262144 || !child.wait().await.is_ok_and(|s| s.success()) {
            return Err("Origin unavailable".into());
        }
        let v: Value = serde_json::from_slice(&buf).map_err(|_| "Invalid origin reply")?;
        parse(o, ids, &v)
    })
    .await
    .map_err(|_| "Origin timed out")?
}
fn parse(o: &str, ids: &[&Value], v: &Value) -> Result<Value, String> {
    let r = &v["data"]["repository"];
    if !v["errors"].is_null()
        || !r["nameWithOwner"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(o))
    {
        return Err("Origin identity or coverage unavailable".into());
    }
    let mut out = json!({});
    for (i, row) in ids.iter().enumerate() {
        let c = &r[format!("c{i}")];
        let p = &c["associatedPullRequests"];
        if c["oid"] != row["oid"] || p["pageInfo"]["hasNextPage"] != false {
            continue;
        }
        let Some(nodes) = p["nodes"].as_array() else {
            continue;
        };
        let mut a = vec![];
        let mut valid = true;
        for n in nodes {
            if !n["repository"]["nameWithOwner"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(o))
            {
                valid = false;
                break;
            }
            if n["merged"] == false {
                continue;
            }
            if n["merged"] != true
                || n["number"].as_i64().unwrap_or(0) < 1
                || !n["mergeCommit"]["oid"].is_string()
            {
                valid = false;
                break;
            }
            a.push(json!({"merged":true,"merge":n["mergeCommit"]["oid"],"number":n["number"]}));
        }
        if valid {
            out[row["oid"].as_str().ok_or("Missing pin")?] = json!(a);
        }
    }
    Ok(out)
}
#[cfg(test)]
#[test]
fn origin_reader_requires_identity_exact_pin_and_complete_page() {
    let id = "a".repeat(40);
    let row = json!({"oid":id});
    let mut v = json!({"data":{"repository":{"nameWithOwner":"Test/Repo","c0":{"oid":id,"associatedPullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[{"number":7,"merged":true,"mergeCommit":{"oid":id},"repository":{"nameWithOwner":"Test/Repo"}}]}}}}});
    let p = |v: &Value| parse("test/repo", &[&row], v);
    assert_eq!(p(&v).unwrap()[&id][0]["number"], 7);
    v["data"]["repository"]["c0"]["associatedPullRequests"]["pageInfo"]["hasNextPage"] =
        json!(true);
    assert_eq!(p(&v).unwrap(), json!({}));
    v["data"]["repository"]["nameWithOwner"] = json!("other/repo");
    assert!(p(&v).is_err());
}
pub async fn call(
    ctx: &Ctx,
    repo: &str,
    o: &str,
    path: &Path,
    seq: i64,
    refresh: bool,
) -> Result<Value, String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    let r = ctx
        .db
        .query(
            "SELECT pr,base,head FROM release_refs WHERE repo=? AND origin=? AND seq=?",
            params![repo, o, seq],
        )?
        .pop()
        .ok_or("Generation not in checkout")?;
    if !refresh {
        return match ctx
            .db
            .query("SELECT summary FROM release_membership WHERE seq=?", [seq])?
            .pop()
        {
            None => Ok(Value::Null),
            Some(v) => serde_json::from_str(v["summary"].as_str().unwrap_or(""))
                .map_err(|_| "Invalid saved membership".into()),
        };
    }
    let _guard = BUSY
        .try_lock()
        .map_err(|_| "Origin discovery already running")?;
    let g = graph(
        path,
        o,
        r["base"].as_str().ok_or("Missing target")?,
        r["head"].as_str().ok_or("Missing head")?,
    )
    .await?;
    let commits = &g["commits"];
    let ids = commits
        .as_array()
        .ok_or("Missing graph")?
        .iter()
        .collect::<Vec<_>>();
    let mut associations = json!({});
    let start = std::time::Instant::now();
    for chunk in ids.chunks(16) {
        if start.elapsed() > Duration::from_secs(30) {
            break;
        }
        let part = match lookup(o, chunk).await {
            Ok(v) => v,
            Err(_) => {
                let mut found = json!({});
                for item in chunk {
                    if start.elapsed() > Duration::from_secs(30) {
                        break;
                    }
                    let Some(oid) = item["oid"].as_str() else {
                        continue;
                    };
                    let Some(v) = crate::release_intake::github_public(&format!(
                        "{o}/commits/{oid}/pulls?per_page=32"
                    ))
                    .await
                    else {
                        break;
                    };
                    let Some(rows) = v.as_array() else { break };
                    if rows.iter().all(|r| {
                        r["base"]["repo"]["full_name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case(o))
                            && r.get("merged_at").is_some()
                            && r["number"].as_i64().is_some_and(|n| n > 0)
                    }) {
                        found[oid]=json!(rows.iter().filter(|r|r["merged_at"].is_string()).map(|r|json!({"merged":true,"merge":r["merge_commit_sha"],"number":r["number"]})).collect::<Vec<_>>());
                    }
                }
                found
            }
        };
        associations
            .as_object_mut()
            .unwrap()
            .extend(part.as_object().cloned().unwrap_or_default());
    }
    save(ctx, &r, seq, commits, &associations)
}
// Internal origin adapter. No RPC accepts client-supplied associations.
pub(crate) fn save(
    ctx: &Ctx,
    r: &Value,
    seq: i64,
    commits: &Value,
    associations: &Value,
) -> Result<Value, String> {
    let mut report = crate::release_composition::verified_range(
        r["pr"].as_i64().ok_or("Invalid PR")?,
        commits,
        &associations,
    )?;
    report["generation"] = json!(seq);
    report["head"] = r["head"].clone();
    report["observed_ms"] = json!(crate::db::now());
    ctx.db.exec(
        "INSERT OR REPLACE INTO release_membership(seq,summary) VALUES(?,?)",
        params![seq, report.to_string()],
    )?;
    Ok(report)
}
