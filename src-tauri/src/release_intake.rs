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
/// GitHub through the GitHub CLI's own sign-in on this Mac, once the person has connected it.
/// Philium keeps no token: it asks the CLI when it needs one, and remembers only which signed-in
/// account reads which origin.
const GH_LOGINS: &str = "origin:github-cli";
fn gh_exe() -> Option<&'static str> {
    ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"].into_iter().find(|p| Path::new(p).is_file())
}
/// Run the CLI as the person would: with their own home, where it keeps its sign-in, and without
/// any token or configuration override from this process.
async fn gh(args: &[&str]) -> Option<(bool, String, String)> {
    let mut c = tokio::process::Command::new(gh_exe()?);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(home) = crate::pilot::real_home() {
        c.env("HOME", home);
    }
    for k in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN", "GH_HOST", "GH_ENTERPRISE_TOKEN"] {
        c.env_remove(k);
    }
    let out = tokio::time::timeout(Duration::from_secs(8), c.output()).await.ok()?.ok()?;
    Some((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}
/// The GitHub CLI as the person, for `origin`. Reads use the token or account connected for it;
/// writes (`write`) use only the connected CLI account, never a saved read-only token, and fall
/// back to the CLI's active account. `input` goes to stdin, for `gh api --input -`.
pub(crate) async fn gh_as(origin: &str, args: &[&str], input: Option<&str>, write: bool, secs: u64) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    if fixture_origin().is_some() {
        return Err("The GitHub CLI is not used with a stand-in origin".into());
    }
    let exe = gh_exe().ok_or("The GitHub CLI is not installed. Install it and sign in with `gh auth login`.")?;
    let mut c = tokio::process::Command::new(exe);
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    if let Some(home) = crate::pilot::real_home() {
        c.env("HOME", home);
    }
    for k in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        c.env_remove(k);
    }
    c.env("GH_PROMPT_DISABLED", "1").env("GH_HOST", "github.com").env("NO_COLOR", "1");
    let slug = origin.to_ascii_lowercase();
    let token = if write {
        match gh_logins().get(&slug).and_then(Value::as_str).map(str::to_owned) {
            Some(login) => gh_token(&login).await,
            None => None,
        }
    } else {
        bearer_for(&slug).await
    };
    if let Some(t) = token {
        c.env("GH_TOKEN", t);
    }
    let mut child = c.spawn().map_err(|_| "Could not start the GitHub CLI")?;
    if let (Some(text), Some(mut w)) = (input, child.stdin.take()) {
        w.write_all(text.as_bytes()).await.map_err(|_| "Could not send the request to the GitHub CLI")?;
        drop(w);
    }
    let out = tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output())
        .await
        .map_err(|_| "The GitHub CLI timed out")?
        .map_err(|_| "The GitHub CLI failed")?;
    if !out.status.success() {
        let why: String = String::from_utf8_lossy(&out.stderr).trim().chars().take(400).collect();
        let body: String = String::from_utf8_lossy(&out.stdout).trim().chars().take(400).collect();
        return Err(format!("GitHub CLI: {} {}", why, body).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
/// The accounts the CLI is signed in to on github.com, the active one first as the CLI lists them.
fn gh_accounts(status: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.split("Logged in to github.com account ").nth(1) {
            if let Some(name) = rest.split_whitespace().next().filter(|n| n.len() <= 39 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) {
                if !out.iter().any(|x| x == name) {
                    out.push(name.to_owned());
                }
            }
        }
    }
    out
}
async fn gh_token(login: &str) -> Option<String> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, token)) = cache.lock().ok()?.get(login).cloned() {
        if at.elapsed() < Duration::from_secs(300) {
            return Some(token);
        }
    }
    let (ok, stdout, _) = gh(&["auth", "token", "--hostname", "github.com", "--user", login]).await?;
    let token = stdout.trim().to_owned();
    if !ok || token.len() < 20 || token.len() > 300 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    cache.lock().ok()?.insert(login.to_owned(), (std::time::Instant::now(), token.clone()));
    Some(token)
}
fn gh_logins() -> serde_json::Map<String, Value> {
    crate::secrets::get(GH_LOGINS).and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
/// What to present to GitHub for this request: a token the person saved, else the connected
/// CLI account for that origin, else nothing (public repositories need nothing).
/// The GitHub token Review reads this origin with, for other read-only uses (the checks' mirror).
pub(crate) async fn origin_bearer(origin: &str) -> Option<String> {
    bearer_for(origin).await
}
/// The `owner/name` a checkout's remote points at.
pub(crate) fn origin_of(remote_url: &str) -> Result<String, String> {
    origin(remote_url)
}
async fn bearer_for(path: &str) -> Option<String> {
    if let Some(token) = crate::secrets::get(ORIGIN_TOKEN) {
        return Some(token);
    }
    if fixture_origin().is_some() {
        return None;
    }
    let slug = path.split('/').take(2).collect::<Vec<_>>().join("/").to_ascii_lowercase();
    let login = gh_logins().get(&slug)?.as_str()?.to_owned();
    gh_token(&login).await
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
    // A token the person saved, or the GitHub CLI account they connected. Either is only ever
    // sent to this one origin, and agents can read neither.
    if let Some(token) = bearer_for(path).await {
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
fn lead_key(kind: &str, repo: &str) -> String {
    format!("release.lead.{kind}.{repo}")
}
/// The agent Review started for a job ("review" or "draft") and how it is doing, so a start that
/// failed is shown in Review instead of a screen that waits for nothing.
pub(crate) fn lead(ctx: &Ctx, kind: &str, repo: &str) -> Value {
    let Some(r) = ctx.db.kv_get(&lead_key(kind, repo)).filter(|s| !s.is_empty()).and_then(|id| crate::agents::get_row(ctx, &id)) else {
        return Value::Null;
    };
    let error = r["id"].as_str().and_then(|id| ctx.db.kv_get(&format!("agent_error.{id}"))).filter(|e| !e.is_empty());
    let limited = r["id"].as_str().and_then(|id| crate::agents::retry_state(ctx, id));
    json!({"id": r["id"], "name": r["name"], "state": r["state"], "error": error, "started": r["hal_session_id"].is_string(), "rate_limited": limited})
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
        // Review names the agent it just started for this checkout's review or draft.
        "lead" => {
            let kind = arg_str(&a, "kind")?;
            if kind != "review" && kind != "draft" {
                return Err("kind is review or draft".into());
            }
            let row = crate::agents::get_row(ctx, &arg_str(&a, "agent")?).ok_or("Unknown agent")?;
            if row["repo_id"].as_str() != Some(repo.as_str()) {
                return Err("That agent works in another checkout".into());
            }
            ctx.db.kv_set(&lead_key(&kind, &repo), row["id"].as_str().unwrap_or_default())?;
            ctx.emit("release:review", json!({ "repo_id": repo }));
            return Ok(lead(ctx, &kind, &repo));
        }
        "drafter" => return Ok(lead(ctx, "draft", &repo)),
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
    if method=="review_items"{let mut v=crate::review_run::items(ctx,&repo,pr,seq)?;v["lead"]=lead(ctx,"review",&repo);return Ok(v);}
    let (base,head)=(row["base"].as_str().ok_or("Invalid base")?,row["head"].as_str().ok_or("Invalid head")?);
    let mut facts:Vec<String>=Vec::new();
    if let Ok(m)=Box::pin(call(ctx,"members",a.clone())).await{
        let landed:Vec<String>=m["landed"].as_array().into_iter().flatten().filter_map(|p|p["pr"].as_i64()).map(|n|format!("#{n}")).collect();
        if !landed.is_empty(){facts.push(format!("{} pull requests landed in it: {}.",landed.len(),landed.join(", ")));}
        let nested:Vec<String>=m["edges"].as_array().into_iter().flatten().map(|e|format!("#{} contains #{}",e["outer"],e["inner"])).collect();
        if !nested.is_empty(){facts.push(format!("Nested: {}.",nested.join(", ")));}
    }
    // The agents start on the pull request's head: bring it into the checkout read-only if it
    // is not there yet (objects and refs/philium/pr/<n> only; no branch or working file changes).
    crate::pipeline::app::ensure_commits(Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?),&o,pr,&[head,base]).await.map_err(|e|format!("The agents start on #{pr}'s head, which this checkout does not have and Philium could not fetch: {e}"))?;
    let merge=mergeable(Path::new(r[0]["path"].as_str().ok_or("Checkout unavailable")?),base,head).await;
    match merge["state"].as_str(){
        Some("conflicts")=>facts.push(format!("It does not merge onto the target as it is now: conflicts in {}.",merge["files"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))),
        Some("clean")=>facts.push("It merges onto the target without conflicts.".into()),
        _=>{}
    }
    // A new run gets a new lead. The one before is returned so Review can tidy up one that never started.
    let previous=lead(ctx,"review",&repo);
    ctx.db.kv_set(&lead_key("review",&repo),"")?;
    crate::review_run::start(ctx,&repo,pr,seq)?;
    Ok(json!({"pr":pr,"head":head,"previous_lead":previous,"brief":crate::review_run::brief(pr,base,head,&facts.join(" "))}))
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
 // Use the GitHub CLI's sign-in for this checkout's origin. The person asks for it with one
 // press. Of the accounts signed in on this Mac, the first that can read the origin is used.
 "origin_connect"=>{
    if fixture_origin().is_some(){return Err("GitHub is replaced by a stand-in in this profile".into());}
    if gh_exe().is_none(){return Err("The GitHub CLI (gh) is not installed on this Mac. Install it and run `gh auth login`, or save a read-only token under Checks for this checkout.".into());}
    let (_,out,err)=gh(&["auth","status","--hostname","github.com"]).await.ok_or("The GitHub CLI did not answer")?;
    let accounts=gh_accounts(&format!("{out}\n{err}"));
    if accounts.is_empty(){return Err("The GitHub CLI on this Mac is not signed in. Run `gh auth login` in a terminal, then press Connect GitHub again.".into());}
    let slug=o.to_ascii_lowercase();
    for login in &accounts{
        let Some(token)=gh_token(login).await else{continue};
        let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(8)).user_agent("Philium-release-review").build().map_err(|e|e.to_string())?;
        let can=client.get(format!("https://api.github.com/repos/{slug}")).bearer_auth(&token).send().await.is_ok_and(|r|r.status()==200);
        if can{
            let mut map=gh_logins();map.insert(slug.clone(),json!(login));
            crate::secrets::set(GH_LOGINS,&Value::Object(map).to_string())?;
            ctx.log_event("origin_connected",None,&json!({"origin":"github.com","through":"github-cli"}));
            return Ok(json!({"login":login,"origin":slug}));
        }
    }
    Err(format!("None of the GitHub accounts signed in with the GitHub CLI on this Mac ({}) can read {slug}. Sign in to the right account with `gh auth login`.",accounts.join(", ")))
 },
 "origin_disconnect"=>{
    let mut map=gh_logins();map.remove(&o.to_ascii_lowercase());
    crate::secrets::set(GH_LOGINS,&Value::Object(map).to_string())?;
    Ok(json!({"login":Value::Null}))
 },
 "origin_connection"=>Ok(json!({"login":gh_logins().get(&o.to_ascii_lowercase()).cloned().unwrap_or(Value::Null),"origin":o,"token_saved":crate::secrets::get(ORIGIN_TOKEN).is_some()})),
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
    fn the_github_cli_accounts_are_read_from_its_status() {
        let status = "github.com\n  ✓ Logged in to github.com account first-one (keyring)\n  - Active account: true\n  - Token: gho_************\n\n  ✓ Logged in to github.com account second (keyring)\n  ✓ Logged in to github.com account first-one (keyring)\n";
        assert_eq!(gh_accounts(status), ["first-one", "second"]);
        assert!(gh_accounts("You are not logged into any GitHub hosts. To log in, run: gh auth login").is_empty());
        assert!(gh_accounts("Logged in to github.com account bad/name (keyring)").is_empty());
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

    /// Who a PR review would be posted as, without posting: `gh_as(write=true)` with GET-only
    /// requests must authenticate as the CLI account connected for the origin (not the active
    /// one, not a stray GH_TOKEN), and stdin must reach `gh api --input -`.
    #[tokio::test]
    #[ignore = "opt-in, read-only; needs signed-in gh and PHILIUM_TEST_DATA_DIR with a connected origin"]
    async fn live_gh_as_writes_as_connected_account() {
        let data = std::env::var("PHILIUM_TEST_DATA_DIR").expect("set PHILIUM_TEST_DATA_DIR");
        let origin = std::env::var("PHILIUM_TEST_PR_ORIGIN").expect("set PHILIUM_TEST_PR_ORIGIN");
        crate::secrets::init(std::path::Path::new(&data));
        let want = gh_logins().get(&origin.to_ascii_lowercase()).and_then(Value::as_str).map(str::to_owned).expect("origin connected in this data dir");
        std::env::set_var("GH_TOKEN", "not-a-real-token");
        let me: Value = serde_json::from_str(&gh_as(&origin, &["api", "user"], None, true, 20).await.unwrap()).unwrap();
        assert_eq!(me["login"].as_str(), Some(want.as_str()), "writes must use the connected account");
        let q = json!({"query": format!("query{{repository(owner:\"{}\",name:\"{}\"){{viewerPermission}}}}", origin.split('/').next().unwrap(), origin.split('/').nth(1).unwrap())}).to_string();
        let r: Value = serde_json::from_str(&gh_as(&origin, &["api", "graphql", "--input", "-"], Some(&q), true, 20).await.unwrap()).unwrap();
        let perm = r["data"]["repository"]["viewerPermission"].as_str().unwrap_or("");
        println!("posts as {want}, permission {perm}");
        assert!(["WRITE", "MAINTAIN", "ADMIN", "TRIAGE", "READ"].contains(&perm), "{r}");
        assert!(gh_as(&origin, &["api", "repos/{owner}/does-not-exist-philium"], None, true, 20).await.is_err());
    }
}
