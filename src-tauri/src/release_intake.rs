//! User-initiated origin metadata only. No source upload, candidate code or ship verdict.
use crate::{
    ctx::{arg_str, Ctx},
    db::{now, Db},
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
static FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const ANCHORS:&str="CREATE TABLE IF NOT EXISTS release_review_checkpoints(seq INTEGER PRIMARY KEY REFERENCES release_refs(seq), marked INTEGER NOT NULL)";
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
pub(crate) fn record(
    db: &Db,
    repo: &str,
    origin: &str,
    pr: i64,
    v: &Value,
) -> Result<Value, String> {
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
const ORIGIN_TOKEN: &str = "origin:github";
fn sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
/// A harness-made disposable profile may point origin lookups at a loopback stand-in, so the
/// form can be taken through a moved head or an outage. A person's profile never has the marker,
/// and with the stand-in active the real origin and its CLI are not contacted at all.
pub(crate) fn fixture_origin() -> Option<String> {
    if !crate::pilot::disposable() {
        return None;
    }
    let port = std::env::var("PHILIUM_FIXTURE_ORIGIN_PORT")
        .ok()?
        .parse::<u16>()
        .ok()
        .filter(|p| *p >= 1024)?;
    Some(format!("http://127.0.0.1:{port}"))
}
pub(crate) async fn github_public(path: &str) -> Option<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .user_agent("Philium-release-review")
        .build()
        .ok()?;
    let mut request = client
        .get(format!(
            "{}/repos/{path}",
            fixture_origin().as_deref().unwrap_or("https://api.github.com")
        ))
        .header("Cache-Control", "no-cache");
    // A read-only token the person saved for private repositories. It lives in the vault,
    // which agents cannot read, and is only ever sent to this one origin.
    if let Some(token) = crate::secrets::get(ORIGIN_TOKEN) {
        request = request.bearer_auth(token);
    }
    let mut r = request.send().await.ok()?;
    if r.status() != 200 || r.headers().get("link").is_some() {
        return None;
    }
    let mut buf = vec![];
    while let Some(chunk) = r.chunk().await.ok()? {
        if buf.len() + chunk.len() > 262144 {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buf).ok()
}

// Only object IDs leave this reader. No subjects, patch contents, hooks or origin requests.
pub(crate) async fn local_git(path: &Path, args: &[&str]) -> Result<String, String> {
    local_git_output(path, args, false).await
}
pub(crate) async fn local_git_output(
    path: &Path,
    args: &[&str],
    exact: bool,
) -> Result<String, String> {
    local_git_limited(path, args, exact, 131072).await
}
pub(crate) async fn local_git_limited(
    path: &Path,
    args: &[&str],
    exact: bool,
    max: u64,
) -> Result<String, String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("/usr/bin/git")
        .current_dir(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/var/empty")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager", "-c", "core.commitGraph=false"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Local Git unavailable")?;
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut buf = Vec::new();
        child
            .stdout
            .take()
            .ok_or("No Git output")?
            .take(max + 1)
            .read_to_end(&mut buf)
            .await
            .map_err(|_| "Cannot read history")?;
        if buf.len() as u64 > max {
            return Err("History exceeds the inspection limit".into());
        }
        if !child.wait().await.is_ok_and(|s| s.success()) {
            return Err("Pinned history unavailable locally. No fetch was attempted.".into());
        }
        String::from_utf8(buf)
            .map(|s| if exact { s } else { s.trim_end().to_owned() })
            .map_err(|_| "Invalid Git response".into())
    })
    .await
    .map_err(|_| "Local history inspection timed out")?
}
pub(crate) async fn graph(
    path: &Path,
    expected: &str,
    base: &str,
    head: &str,
) -> Result<Value, String> {
    if !sha(base) || !sha(head) || !path.join(".git").exists() {
        return Err("Pinned checkout unavailable".into());
    }
    if origin(&local_git(path, &["config", "--local", "--get", "remote.origin.url"]).await?)?
        != expected
    {
        return Err("Checkout origin changed".into());
    }
    if local_git(path, &["rev-parse", "--is-shallow-repository"]).await? != "false" {
        return Err("A shallow checkout cannot establish complete ancestry".into());
    }
    let ancestor = local_git(path, &["merge-base", "--all", base, head]).await?;
    if !sha(&ancestor) {
        return Err("No unique merge base for this generation".into());
    }
    let range = format!("{base}..{head}");
    let lines = local_git(
        path,
        &[
            "rev-list",
            "--parents",
            "--topo-order",
            "--max-count=513",
            &range,
            "--",
        ],
    )
    .await?;
    let mut commits = Vec::new();
    for line in lines.lines() {
        let ids: Vec<_> = line.split_ascii_whitespace().collect();
        if ids.is_empty() || !ids.iter().all(|id| sha(id)) {
            return Err("Invalid ancestry response".into());
        }
        if commits.len() == 512 {
            return Err("Over 512 included commits. No partial graph reported.".into());
        }
        commits.push(json!({"oid":ids[0],"parents":&ids[1..]}));
    }
    Ok(json!({"merge_base":ancestor,"target":base,"head":head,"commits":commits}))
}
pub async fn call(ctx: &Ctx, method: &str, a: Value) -> Result<Value, String> {
    let repo = arg_str(&a, "repo_id")?;
    let r = ctx
        .db
        .query("SELECT remote_url,path FROM repos WHERE id=?", [&repo])?;
    let o = origin(
        r.first()
            .and_then(|v| v["remote_url"].as_str())
            .unwrap_or(""),
    )?;
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    ctx.db.with(|c| c.execute_batch(ANCHORS))?;
    match method {
        "origin_access" => {
            return Ok(json!({"token_saved":crate::secrets::get(ORIGIN_TOKEN).is_some()}));
        }
        "origin_token_set" => {
            let token = arg_str(&a, "token")?;
            let token = token.trim();
            if token.is_empty() || token.len() > 255 || !token.bytes().all(|b| b.is_ascii_graphic()) {
                return Err("Enter one GitHub token without spaces".into());
            }
            crate::secrets::set(ORIGIN_TOKEN, token)?;
            ctx.log_event("origin_token_saved", None, &json!({"origin":"github.com"}));
            return Ok(json!({"token_saved":true}));
        }
        "origin_token_clear" => {
            crate::secrets::delete(ORIGIN_TOKEN);
            ctx.log_event("origin_token_removed", None, &json!({"origin":"github.com"}));
            return Ok(json!({"token_saved":false}));
        }
        _ => {}
    }
    if method == "decision" {
        let seq = a["generation"].as_i64().ok_or("Select a generation")?;
        if ctx
            .db
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='release_decisions'",
                [],
            )?
            .is_empty()
        {
            return Ok(Value::Null);
        }
        return match ctx.db.query("SELECT d.summary FROM release_decisions d JOIN release_refs r ON r.seq=d.seq WHERE d.repo=? AND r.repo=d.repo AND r.origin=? AND d.seq=?",params![repo,o,seq])?.pop(){Some(v)=>serde_json::from_str(v["summary"].as_str().unwrap_or("")).map_err(|_|"Invalid saved decision".into()),None=>Ok(Value::Null)};
    }
    if matches!(method, "members" | "discover_members") {
        return crate::release_members::call(
            ctx,
            &repo,
            &o,
            Path::new(r[0]["path"].as_str().ok_or("Missing checkout")?),
            a["generation"].as_i64().ok_or("Select generation")?,
            method == "discover_members",
        )
        .await;
    }
    if method.starts_with("sql_") {
        return crate::release_sql::call(
            ctx,
            method,
            &a,
            &repo,
            &o,
            Path::new(r[0]["path"].as_str().ok_or("Missing checkout")?),
        )
        .await;
    }
    match method{
 "checkpoint"|"followup"=>{
    let seq=a["generation"].as_i64().ok_or("Select a recorded generation")?;
    let rows=ctx.db.query("SELECT base,head,pr FROM release_refs WHERE repo=? AND origin=? AND seq=?",params![repo,o,seq])?;
    let row=rows.first().ok_or("Generation does not belong to this checkout")?;
    if method=="checkpoint"{ctx.db.exec("INSERT OR IGNORE INTO release_review_checkpoints(seq,marked) VALUES(?,?)",params![seq,now()])?;return Ok(json!({"recorded":seq}));}
    let anchors=ctx.db.query("SELECT r.seq,r.base,r.head FROM release_refs r JOIN release_review_checkpoints c ON c.seq=r.seq WHERE repo=? AND origin=? AND pr=? AND r.seq<=? ORDER BY r.seq DESC LIMIT 1",params![repo,o,row["pr"].as_i64(),seq])?;
    let Some(anchor)=anchors.first() else{return Ok(json!({"mode":"full","reason":"No completed code-review checkpoint"}));};
    if anchor["seq"]==seq{return Ok(json!({"mode":"checkpoint","reason":"Code review recorded. This is not a workflow pass or approval."}));}
    if anchor["base"]!=row["base"]{return Ok(json!({"mode":"full","reason":"The target revision changed"}));}
    let old=anchor["head"].as_str().ok_or("Invalid checkpoint")?;let head=row["head"].as_str().ok_or("Invalid head")?;let path=Path::new(r[0]["path"].as_str().ok_or("Missing checkout")?);
    let full=|reason:&str|json!({"mode":"full","reason":reason});
    let delta=match graph(path,&o,old,head).await{Ok(g) if g["merge_base"]==old=>g,Ok(_)=>return Ok(full("History was rewritten")),Err(e)=>return Ok(full(&e))};
    // A merge from the target may advance the merge base even if the target tip did not move.
    let base=row["base"].as_str().ok_or("Invalid target")?;
    let previous=local_git(path,&["merge-base","--all",base,old]).await;
    let current=local_git(path,&["merge-base","--all",base,head]).await;
    if !matches!((previous,current),(Ok(a),Ok(b)) if sha(&a)&&a==b){return Ok(full("Merge base changed or cannot be verified"));}
    Ok(json!({"mode":"followup","from":old,"to":head,"commits":delta["commits"],"reason":"Review the cumulative change and prior findings. Broaden to full review if scope changed. Required workflows still need current-head evidence."}))},
 "graph"=>{let seq=a["generation"].as_i64().ok_or("Select a recorded generation")?;let rows=ctx.db.query("SELECT base,head FROM release_refs WHERE repo=? AND origin=? AND seq=?",params![repo,o,seq])?;let row=rows.first().ok_or("Generation does not belong to this checkout")?;graph(Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?),&o,row["base"].as_str().ok_or("Invalid base")?,row["head"].as_str().ok_or("Invalid head")?).await},
 "history"=>Ok(json!(ctx.db.query("SELECT seq,pr,base,head,observed FROM release_refs WHERE repo=? AND origin=? ORDER BY seq DESC LIMIT 30",params![repo,o])?)),
 "resolve"=>{let n=target(&arg_str(&a,"url")?,&o)?;let _g=FETCH.try_lock().map_err(|_|"Another origin lookup is in progress")?;
 if let Some(v)=github_public(&format!("{o}/pulls/{n}")).await{if v["base"]["repo"]["full_name"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(&o)){return record(&ctx.db,&repo,&o,n,&json!({"number":v["number"],"url":v["html_url"],"baseRefOid":v["base"]["sha"],"headRefOid":v["head"]["sha"]}));}}
 if fixture_origin().is_some(){return Err("Origin lookup failed. The origin did not answer.".into());}
 let exe=["/opt/homebrew/bin/gh","/usr/local/bin/gh","/usr/bin/gh"].into_iter().find(|p|Path::new(p).is_file()).ok_or("Origin lookup failed. Public metadata was not readable and the GitHub CLI is not installed for private read access.")?;
 let mut cmd=tokio::process::Command::new(exe);cmd.current_dir("/").args(["pr","view",&format!("https://github.com/{o}/pull/{n}"),"--json","number,url,baseRefOid,headRefOid"]).env("GH_PROMPT_DISABLED","1").env("GH_HOST","github.com").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
 let out=tokio::time::timeout(Duration::from_secs(12),cmd.output()).await.map_err(|_|"Origin lookup timed out")?.map_err(|_|"Could not start GitHub CLI")?;if !out.status.success()||out.stdout.len()>16384{return Err("Origin lookup failed. Check the network and GitHub read access for this repository.".into())}let refs=serde_json::from_slice(&out.stdout).map_err(|_|"Invalid origin response")?;record(&ctx.db,&repo,&o,n,&refs)},_=>Err("Unknown intake action".into())}
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
