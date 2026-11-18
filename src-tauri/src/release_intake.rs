//! User-initiated origin metadata only. No source upload, candidate code or ship verdict.
use crate::{
    ctx::{arg_str, Ctx},
    db::{now, Db},
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
static FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const SCHEMA:&str="CREATE TABLE IF NOT EXISTS release_refs(seq INTEGER PRIMARY KEY,repo TEXT NOT NULL,origin TEXT NOT NULL,pr INTEGER NOT NULL,base TEXT NOT NULL,head TEXT NOT NULL,observed INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS release_lookup ON release_refs(repo,origin,pr,seq)";
fn slug(s: &str) -> Result<String, String> {
    let parts: Vec<_> = s.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|p| {
            p.is_empty()
                || p.len() > 100
                || p.starts_with('.')
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_ .".contains(&b) && b != b' ')
        })
    {
        return Err("Invalid GitHub repository identity".into());
    }
    Ok(s.to_ascii_lowercase())
}
fn origin(s: &str) -> Result<String, String> {
    let r = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|p| s.strip_prefix(p))
    .ok_or("Select a checkout with a GitHub origin")?
    .trim_end_matches('/');
    slug(r.strip_suffix(".git").unwrap_or(r))
}
fn target(url: &str, expected: &str) -> Result<i64, String> {
    let (r, n) = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .and_then(|u| u.split_once("/pull/"))
        .ok_or("Enter a GitHub pull request URL")?;
    if slug(r)? != expected || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err("PR must belong to the selected checkout".into());
    }
    n.parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or("Invalid PR number".into())
}
fn record(db: &Db, repo: &str, origin: &str, pr: i64, v: &Value) -> Result<Value, String> {
    if v["number"].as_i64() != Some(pr) || target(v["url"].as_str().unwrap_or(""), origin)? != pr {
        return Err("Origin returned a different PR".into());
    }
    let oid = |k: &str| {
        v[k].as_str()
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase)
            .ok_or("Missing exact commit identity")
    };
    let (base, head) = (oid("baseRefOid")?, oid("headRefOid")?);
    let sampled = now();
    db.with(|c|{let tx=c.unchecked_transaction()?;tx.execute("INSERT INTO release_refs(repo,origin,pr,base,head,observed) SELECT ?1,?2,?3,?4,?5,?6 WHERE NOT EXISTS(SELECT 1 FROM (SELECT base,head FROM release_refs WHERE repo=?1 AND origin=?2 AND pr=?3 ORDER BY seq DESC LIMIT 1) WHERE base=?4 AND head=?5)",params![repo,origin,pr,base,head,sampled])?;let generation:i64=tx.query_row("SELECT MAX(seq) FROM release_refs WHERE repo=? AND origin=? AND pr=?",params![repo,origin,pr],|r|r.get(0))?;tx.commit()?;Ok(json!({"generation":generation,"pr":pr,"base":base,"head":head,"checked":sampled}))})
}
pub async fn call(ctx: &Ctx, method: &str, a: Value) -> Result<Value, String> {
    let repo = arg_str(&a, "repo_id")?;
    let r = ctx
        .db
        .query("SELECT remote_url FROM repos WHERE id=?", [&repo])?;
    let o = origin(
        r.first()
            .and_then(|v| v["remote_url"].as_str())
            .unwrap_or(""),
    )?;
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    match method{
 "history"=>Ok(json!(ctx.db.query("SELECT seq,pr,base,head,observed FROM release_refs WHERE repo=? AND origin=? ORDER BY seq DESC LIMIT 30",params![repo,o])?)),
 "resolve"=>{let n=target(&arg_str(&a,"url")?,&o)?;let _g=FETCH.try_lock().map_err(|_|"Another origin lookup is in progress")?;
 let exe=["/opt/homebrew/bin/gh","/usr/local/bin/gh","/usr/bin/gh"].into_iter().find(|p|Path::new(p).is_file()).ok_or("GitHub CLI unavailable")?;
 let mut cmd=tokio::process::Command::new(exe);cmd.current_dir("/").args(["pr","view",&format!("https://github.com/{o}/pull/{n}"),"--json","number,url,baseRefOid,headRefOid"]).env("GH_PROMPT_DISABLED","1").env("GH_HOST","github.com").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
 let out=tokio::time::timeout(Duration::from_secs(12),cmd.output()).await.map_err(|_|"Origin lookup timed out")?.map_err(|_|"Could not start GitHub CLI")?;if !out.status.success()||out.stdout.len()>16384{return Err("Origin lookup failed. Check read access for this repository.".into())}let refs=serde_json::from_slice(&out.stdout).map_err(|_|"Invalid origin response")?;record(&ctx.db,&repo,&o,n,&refs)},_=>Err("Unknown intake action".into())}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generations_and_scope() {
        let db = Db::memory();
        db.with(|c| c.execute_batch(SCHEMA)).unwrap();
        let o = origin("git@github.com:Org/Repo.git").unwrap();
        let url = "https://github.com/org/repo/pull/7";
        assert_eq!(target(url, &o).unwrap(), 7);
        for bad in [
            "https://github.com/org/peer/pull/7",
            "https://github.com.evil/org/repo/pull/7",
            "https://github.com/org/repo/pull/7?x",
        ] {
            assert!(target(bad, &o).is_err())
        }
        let mut v =
            json!({"number":7,"url":url,"baseRefOid":"a".repeat(40),"headRefOid":"b".repeat(40)});
        let run = |v: &Value| {
            record(&db, "checkout", &o, 7, v).unwrap()["generation"]
                .as_i64()
                .unwrap()
        };
        let first = run(&v);
        assert_eq!(first, run(&v));
        v["baseRefOid"] = json!("c".repeat(40));
        assert!(run(&v) > first);
        v["baseRefOid"] = json!("a".repeat(40));
        assert!(run(&v) > first + 1);
        v["headRefOid"] = json!("moving");
        assert!(record(&db, "checkout", &o, 7, &v).is_err());
    }
}
