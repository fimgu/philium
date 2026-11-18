//! Human-enrolled SQL contracts. A successful lane is not a release approval.
use crate::{
    ctx::Ctx,
    db::now,
    release_intake::{graph, local_git},
};
use rusqlite::{
    hooks::{AuthAction as A, AuthContext, Authorization as Z},
    limits::Limit,
    params, Connection,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
const DB:&str="CREATE TABLE IF NOT EXISTS sql_profiles(repo TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,driver TEXT NOT NULL,pin TEXT NOT NULL,blob TEXT NOT NULL);CREATE TABLE IF NOT EXISTS sql_receipts(id INTEGER PRIMARY KEY,repo TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,seq INTEGER NOT NULL,report TEXT NOT NULL);CREATE INDEX IF NOT EXISTS sql_receipt_lookup ON sql_receipts(repo,seq,id)";
static LANE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn digest(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}
fn spec(raw: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(raw).map_err(|_| "Invalid driver JSON")?;
    let path = v["schema"].as_str().ok_or("Missing schema path")?;
    if path.len() > 200
        || !path.ends_with(".sql")
        || !path.split('/').all(|p| {
            !p.is_empty()
                && !p.starts_with('.')
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
    {
        return Err("Invalid SQL schema path".into());
    }
    let cs = v["cases"]
        .as_array()
        .filter(|v| (1..=8).contains(&v.len()))
        .ok_or("Driver requires 1 to 8 contracts")?;
    let mut seen = BTreeSet::new();
    for c in cs {
        let id = c["id"].as_str().unwrap_or("");
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !seen.insert(id)
            || !c["query"].is_string()
            || !c["seed"].is_string()
            || !c["expect"].is_array()
        {
            return Err("Invalid or duplicate SQL contract".into());
        }
    }
    Ok(v)
}
fn observe(schema: &str, c: &Value) -> Option<String> {
    let mut db = Connection::open_in_memory().ok()?;
    db.execute_batch(
        "pragma temp_store=memory;pragma trusted_schema=off;pragma max_page_count=2048;",
    )
    .ok()?;
    let cap: i64 = db
        .query_row("pragma hard_heap_limit=33554432", [], |r| r.get(0))
        .ok()?;
    if !(1..=33554432).contains(&cap) {
        return None;
    }
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 262144).ok()?;
    let start = Instant::now();
    db.progress_handler(1000, Some(move || start.elapsed() > Duration::from_secs(1)))
        .ok()?;
    db.authorizer(Some(|c: AuthContext<'_>| {
        if matches!(c.database_name, None | Some("main"))
            && match c.action {
                A::CreateTable { .. }
                | A::CreateIndex { .. }
                | A::CreateView { .. }
                | A::Select
                | A::Read { .. }
                | A::Insert { .. }
                | A::Update { .. }
                | A::Delete { .. }
                | A::Transaction { .. }
                | A::Recursive => true,
                A::Function { function_name: f } => matches!(
                    f,
                    "count"
                        | "min"
                        | "max"
                        | "sum"
                        | "coalesce"
                        | "abs"
                        | "lower"
                        | "upper"
                        | "length"
                ),
                _ => false,
            }
        {
            Z::Allow
        } else {
            Z::Deny
        }
    }))
    .ok()?;
    db.execute_batch(schema).ok()?;
    db.execute_batch(c["seed"].as_str()?).ok()?;
    let mut stmt = db.prepare(c["query"].as_str()?).ok()?;
    let n = stmt.column_count();
    if !stmt.readonly() || n == 0 || n > 16 {
        return None;
    }
    let mut rows = stmt.query([]).ok()?;
    let mut result = vec![];
    let mut size = 0;
    while let Some(r) = rows.next().ok()? {
        if result.len() >= 64 {
            return None;
        }
        let mut cells = vec![];
        for i in 0..n {
            use rusqlite::types::ValueRef::*;
            cells.push(match r.get_ref(i).ok()? {
                Null => Value::Null,
                Integer(i) => json!(i),
                Text(t) if t.len() <= 8192 => json!(std::str::from_utf8(t).ok()?),
                _ => return None,
            });
        }
        size += serde_json::to_vec(&cells).ok()?.len();
        if size > 16384 {
            return None;
        }
        result.push(cells);
    }
    let actual = json!(result);
    Some(format!(
        "{}:{}",
        if c["expect"] == actual {
            "pass"
        } else {
            "mismatch"
        },
        digest(&serde_json::to_vec(&actual).ok()?)
    ))
}
pub fn worker() -> Option<()> {
    let mut b = vec![];
    std::io::stdin().take(270001).read_to_end(&mut b).ok()?;
    if b.len() > 270000 {
        return None;
    }
    let p: Value = serde_json::from_slice(&b).ok()?;
    let schema = p["schema"].as_str()?;
    let cases = p["cases"].as_array()?;
    if cases.len() > 8 {
        return None;
    }
    let rows: Vec<_> = cases
        .iter()
        .map(|c| [observe(schema, c), observe(schema, c)])
        .collect();
    println!("{}", json!(rows));
    Some(())
}
async fn run_one(path: &Path, oid: &str, v: &Value) -> Option<Value> {
    let schema = local_git(
        path,
        &[
            "cat-file",
            "blob",
            &format!("{oid}:{}", v["schema"].as_str()?),
        ],
    )
    .await
    .ok()?;
    let mut p = tokio::process::Command::new(std::env::current_exe().ok()?)
        .arg("--observe-sql-contract")
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stdin = p.stdin.take()?;
        stdin
            .write_all(&serde_json::to_vec(&json!({"schema":schema,"cases":v["cases"]})).ok()?)
            .await
            .ok()?;
        drop(stdin);
        let out = p.wait_with_output().await.ok()?;
        if !out.status.success() || out.stdout.len() > 4096 {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    })
    .await
    .ok()
    .flatten()
}
fn stable<'a>(obs: &'a Value, i: usize) -> Option<&'a str> {
    let a = obs[i][0].as_str()?;
    if Some(a) == obs[i][1].as_str() {
        Some(a)
    } else {
        None
    }
}

pub async fn call(
    ctx: &Ctx,
    method: &str,
    a: &Value,
    repo: &str,
    o: &str,
    path: &Path,
) -> Result<Value, String> {
    ctx.db.with(|c| c.execute_batch(DB))?;
    if method == "sql_drivers" {
        return Ok(json!(ctx.db.query(
            "SELECT id,name FROM repos WHERE id!=? ORDER BY name,id LIMIT 200",
            [repo]
        )?));
    }
    if method == "sql_profile" {
        return Ok(ctx
            .db
            .query("SELECT driver,pin FROM sql_profiles WHERE repo=?", [repo])?
            .pop()
            .unwrap_or(Value::Null));
    }
    if method == "sql_enroll" {
        let _guard = LANE.try_lock().map_err(|_| "Comparison in progress")?;
        let driver = a["driver"]
            .as_str()
            .filter(|d| *d != repo)
            .ok_or("Choose a separate trusted driver checkout")?;
        let r = ctx
            .db
            .query("SELECT path FROM repos WHERE id=?", [driver])?
            .pop()
            .ok_or("Driver not registered")?;
        let p = Path::new(r["path"].as_str().ok_or("Missing driver path")?);
        let common = |dir: &Path| dir.canonicalize().map_err(|_| "Missing checkout");
        let driver_git = local_git(
            p,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?;
        let product_git = local_git(
            path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .await?;
        if common(Path::new(&driver_git))? == common(Path::new(&product_git))? {
            return Err("Driver cannot share the candidate object repository".into());
        }
        let pin = local_git(p, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
        let raw = local_git(
            p,
            &[
                "cat-file",
                "blob",
                &format!("{pin}:philium/sql-contracts.json"),
            ],
        )
        .await?;
        let v = spec(&raw)?;
        let snapshot = serde_json::to_string(&v).map_err(|_| "Invalid contract")?;
        if snapshot.len() > 32768 {
            return Err("Driver exceeds 32 KiB".into());
        }
        ctx.db.exec(
            "INSERT OR REPLACE INTO sql_profiles(repo,driver,pin,blob) VALUES(?,?,?,?)",
            params![repo, driver, pin, snapshot],
        )?;
        return Ok(json!({"pin":pin,"cases":v["cases"].as_array().unwrap().len()}));
    }
    let seq = a["generation"].as_i64().ok_or("Select a generation")?;
    let r = ctx
        .db
        .query(
            "SELECT base,head,pr FROM release_refs WHERE repo=? AND origin=? AND seq=?",
            params![repo, o, seq],
        )?
        .pop()
        .ok_or("Generation does not belong to this checkout")?;
    if method == "sql_receipt" {
        let rows = ctx.db.query(
            "SELECT report FROM sql_receipts WHERE repo=? AND seq=? ORDER BY id DESC LIMIT 1",
            params![repo, seq],
        )?;
        return match rows.first() {
            None => Ok(Value::Null),
            Some(row) => serde_json::from_str(row["report"].as_str().unwrap_or(""))
                .map_err(|_| "Invalid receipt".into()),
        };
    }
    if method != "sql_compare" {
        return Err("Unknown contract action".into());
    }
    let _guard = LANE
        .try_lock()
        .map_err(|_| "A local comparison is already running")?;
    let profile = ctx
        .db
        .query(
            "SELECT driver,pin,blob FROM sql_profiles WHERE repo=?",
            [repo],
        )?
        .pop()
        .ok_or("Enroll a trusted driver first")?;
    let raw = profile["blob"].as_str().ok_or("Missing enrolled driver")?;
    let v = spec(raw)?;
    let base = r["base"].as_str().ok_or("Missing base")?;
    let head = r["head"].as_str().ok_or("Missing head")?;
    let g = graph(path, o, base, head).await?;
    let ancestor = g["merge_base"].as_str().ok_or("Missing common ancestor")?;
    let parents: Vec<&str> = g["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["oid"] == head)
        .and_then(|x| x["parents"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let preflight = now();
    crate::checks::remember(
        &ctx.db,
        repo,
        preflight,
        crate::environments::call("profiles", json!({})).await,
    )?;
    let mut obs = BTreeMap::new();
    let mut pins = BTreeSet::from([head, base, ancestor]);
    if parents.len() == 2 {
        pins.extend(parents.iter().copied());
    }
    for pin in pins {
        obs.insert(pin, run_one(path, pin, &v).await.unwrap_or(Value::Null));
    }
    let mut cases = vec![];
    for (i, c) in v["cases"].as_array().unwrap().iter().enumerate() {
        let s = |rev: &str| obs.get(rev).and_then(|x| stable(x, i));
        let b = s(base);
        let h = s(head);
        let pass = |r: Option<&str>| r.is_some_and(|s| s.starts_with("pass:"));
        let state = match (b, h) {
            (Some(_), Some(_)) if pass(h) => "candidate_pass",
            (Some(_), Some(_)) if b == h => "same_failure_on_target",
            (Some(_), Some(_)) if pass(b) && pass(s(ancestor)) => {
                if parents.len() == 2 && parents.iter().all(|p| pass(s(p))) {
                    "composed_head_regression"
                } else {
                    "release_range_regression"
                }
            }
            (Some(_), Some(_)) => "failure_unattributed",
            _ => "incomplete",
        };
        cases.push(json!({"id":c["id"],"state":state}));
    }
    let report = json!({"generation":seq,"pr":r["pr"],"observed":now(),"target":base,"head":head,"ancestor":ancestor,"parents":parents,"driver":profile["driver"],"driver_pin":profile["pin"],"contract_digest":digest(raw.as_bytes()),"sqlite":rusqlite::version(),"cases":cases,"observations":obs,"staging":"not_verified","release_decision":"incomplete"});
    ctx.db.exec(
        "INSERT INTO sql_receipts(repo,seq,report) VALUES(?,?,?)",
        params![repo, seq, report.to_string()],
    )?;
    Ok(report)
}
