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
/// Would this head merge onto the target as it is now? Asked of Git itself, locally. The trees
/// Git computes on the way go to a throwaway object folder, never into the checkout.
pub(crate) async fn mergeable(path: &Path, base: &str, head: &str) -> Value {
    use tokio::io::AsyncReadExt;
    let unknown = |why: &str| json!({"state":"unknown","detail":why});
    if !sha(base) || !sha(head) {
        return unknown("Revisions not pinned");
    }
    let Ok(objects) = local_git(path, &["rev-parse", "--path-format=absolute", "--git-path", "objects"]).await else {
        return unknown("Checkout unavailable");
    };
    let scratch = std::env::temp_dir().join(format!("philium-merge-{}-{}", std::process::id(), crate::db::now()));
    if std::fs::create_dir_all(&scratch).is_err() {
        return unknown("No scratch folder");
    }
    let trial = async {
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
            .env("GIT_OBJECT_DIRECTORY", &scratch)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects)
            .args(["--no-pager", "-c", "core.commitGraph=false", "merge-tree", "--write-tree", "--name-only", "--no-messages", base, head])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut buf = Vec::new();
        child.stdout.take()?.take(262_145).read_to_end(&mut buf).await.ok()?;
        let status = child.wait().await.ok()?;
        Some((status.code(), String::from_utf8_lossy(&buf).into_owned()))
    };
    let out = tokio::time::timeout(Duration::from_secs(30), trial).await;
    let _ = std::fs::remove_dir_all(&scratch);
    match out {
        Ok(Some((Some(0), _))) => json!({"state":"clean"}),
        // Exit 1: the first line is the tree, the rest are the files in conflict.
        Ok(Some((Some(1), text))) => {
            let mut lines = text.lines();
            // Git also exits 1 when it cannot read a revision. Only a tree followed by file
            // names is a conflict.
            if !lines.next().is_some_and(|l| sha(l.trim())) {
                return unknown("Git could not compute the merge");
            }
            let mut files: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
            files.dedup();
            if files.is_empty() {
                return unknown("Git reported a conflict without naming a file");
            }
            json!({"state":"conflicts","count":files.len(),"files":files.iter().take(20).collect::<Vec<_>>()})
        }
        _ => unknown("Git could not compute the merge"),
    }
}
/// The files a pull request changed that a pinned check reads.
fn touched(changed: &[&str], read: &[String]) -> Vec<String> {
    read.iter().filter(|f| changed.contains(&f.as_str())).cloned().collect()
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
 // A review led by an orchestrator. Starting one records which pull request it is about and
 // returns what the orchestrator is told; the items it then plans are read back for the screen.
 "review_start"|"review_items"=>{
    let seq=a["generation"].as_i64().ok_or("Verify a pull request first")?;
    let rows=ctx.db.query("SELECT pr,base,head FROM release_refs WHERE repo=? AND origin=? AND seq=?",params![repo,o,seq])?;
    let row=rows.first().ok_or("Generation does not belong to this checkout")?;
    let pr=row["pr"].as_i64().ok_or("Invalid pull request")?;
    if method=="review_items"{return crate::review_run::items(ctx,&repo,pr,seq);}
    let (base,head)=(row["base"].as_str().ok_or("Invalid base")?,row["head"].as_str().ok_or("Invalid head")?);
    let mut facts:Vec<String>=Vec::new();
    if let Ok(m)=Box::pin(call(ctx,"members",a.clone())).await{
        let landed:Vec<String>=m["landed"].as_array().into_iter().flatten().filter_map(|p|p["pr"].as_i64()).map(|n|format!("#{n}")).collect();
        if !landed.is_empty(){facts.push(format!("{} pull requests landed in it: {}.",landed.len(),landed.join(", ")));}
        let nested:Vec<String>=m["edges"].as_array().into_iter().flatten().map(|e|format!("#{} contains #{}",e["outer"],e["inner"])).collect();
        if !nested.is_empty(){facts.push(format!("Nested: {}.",nested.join(", ")));}
    }
    let merge=mergeable(Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?),base,head).await;
    match merge["state"].as_str(){
        Some("conflicts")=>facts.push(format!("It does not merge onto the target as it is now: conflicts in {}.",merge["files"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))),
        Some("clean")=>facts.push("It merges onto the target without conflicts.".into()),
        _=>{}
    }
    crate::review_run::start(ctx,&repo,pr,seq)?;
    Ok(json!({"pr":pr,"brief":crate::review_run::brief(pr,base,head,&facts.join(" "))}))
 },
 // Which landed pull requests change a file that a pinned check reads. It informs: a pull
 // request no check touches is named, and the verdict is not changed by it.
 "coverage"=>{
    let members=Box::pin(call(ctx,"members",a.clone())).await?;
    let profile=Box::pin(call(ctx,"sql_profile",a.clone())).await?;
    let read:Vec<String>=profile["checks"]["files"].as_array().into_iter().flatten().filter_map(|f|f.as_str().map(str::to_owned)).collect();
    let path=Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?);
    let mut prs=Vec::new();
    for p in members["landed"].as_array().into_iter().flatten().take(100){
        let oid=p["oid"].as_str().filter(|o|sha(o)).ok_or("Invalid landed revision")?;
        let line=local_git(path,&["rev-list","--parents","-n","1",oid,"--"]).await?;
        let Some(first)=line.split_ascii_whitespace().nth(1).filter(|o|sha(o)) else{continue};
        let names=local_git_limited(path,&["diff","--name-only","--no-renames",first,oid,"--"],false,1_048_576).await?;
        let changed:Vec<&str>=names.lines().collect();
        prs.push(json!({"pr":p["pr"],"files":changed.len(),"read":touched(&changed,&read)}));
    }
    let untouched:Vec<Value>=prs.iter().filter(|p|p["read"].as_array().is_some_and(|x|x.is_empty())).map(|p|p["pr"].clone()).collect();
    Ok(json!({"check_files":read.len(),"prs":prs,"untouched":untouched}))
 },
 "mergeable"=>{let seq=a["generation"].as_i64().ok_or("Select a recorded generation")?;let rows=ctx.db.query("SELECT base,head FROM release_refs WHERE repo=? AND origin=? AND seq=?",params![repo,o,seq])?;let row=rows.first().ok_or("Generation does not belong to this checkout")?;Ok(mergeable(Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?),row["base"].as_str().ok_or("Invalid base")?,row["head"].as_str().ok_or("Invalid head")?).await)},
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
    /// The trial merge names the files in conflict, says "unknown" when it cannot tell, and
    /// leaves the checkout's own object store exactly as it was.
    #[tokio::test]
    async fn a_trial_merge_names_conflicts_without_writing_into_the_checkout() {
        let dir = std::env::temp_dir().join(format!("philium-mergeable-{}-{}", std::process::id(), crate::db::now()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |a: &[&str]| crate::workspace::git(&dir, a).unwrap();
        let commit = |m: &str| {
            git(&["add", "."]);
            git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-qm", m]);
            git(&["rev-parse", "HEAD"]).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join("b.txt"), "one\n").unwrap();
        let root = commit("root");
        std::fs::write(dir.join("a.txt"), "target\n").unwrap();
        let target = commit("target");
        git(&["checkout", "-q", "-b", "side", &root]);
        std::fs::write(dir.join("b.txt"), "side\n").unwrap();
        let clean_head = commit("side changes b");
        std::fs::write(dir.join("a.txt"), "side\n").unwrap();
        let conflict_head = commit("side changes a");
        let objects = || git(&["count-objects", "-v"]);
        let before = objects();
        assert_eq!(mergeable(&dir, &target, &clean_head).await["state"], "clean");
        let c = mergeable(&dir, &target, &conflict_head).await;
        assert_eq!((c["state"].as_str(), c["count"].as_u64()), (Some("conflicts"), Some(1)), "{c}");
        assert_eq!(c["files"], json!(["a.txt"]));
        assert_eq!(mergeable(&dir, &target, "not-a-revision").await["state"], "unknown");
        assert_eq!(mergeable(&dir, &target, &"f".repeat(40)).await["state"], "unknown");
        assert_eq!(objects(), before, "nothing was written into the checkout");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn a_pull_request_is_covered_only_by_a_check_that_reads_what_it_changed() {
        let read = vec!["docs/manifest.json".to_string(), "migrations/62.sql".to_string()];
        assert_eq!(touched(&["migrations/62.sql", "cmd/main.go"], &read), ["migrations/62.sql"]);
        assert!(touched(&["helm/values.yaml", "docs/manifest.json.bak"], &read).is_empty());
        assert!(touched(&[], &read).is_empty());
    }
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
