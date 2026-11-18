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
fn safe_path(p: &str, ext: &str) -> bool {
    p.len() <= 200
        && p.ends_with(ext)
        && p.split('/').all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
}
fn spec(raw: &str) -> Result<Value, String> {
    let v = crate::release_document::strict_value(raw).ok_or("Invalid or ambiguous driver JSON")?;
    if !v["schema"].is_null() && !v["schema"].as_str().is_some_and(|p| safe_path(p, ".sql")) {
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
            || !(if c.get("document").is_some() {
                c["document"]
                    .as_str()
                    .is_some_and(|p| safe_path(p, ".json"))
                    && c["pointer"].is_string()
                    && c.get("expect").is_some()
            } else {
                v["schema"].is_string()
                    && c["query"].is_string()
                    && c["seed"].is_string()
                    && c["expect"].is_array()
            })
        {
            return Err("Invalid or duplicate SQL contract".into());
        }
    }
    if let Some(live) = v.get("live_cases") {
        let xs = live
            .as_array()
            .filter(|xs| (1..=4).contains(&xs.len()))
            .ok_or("Choose 1 to 4 live contracts")?;
        for c in xs {
            let id = c["id"].as_str().unwrap_or("");
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                || !seen.insert(id)
                || !c["path"]
                    .as_str()
                    .is_some_and(crate::release_http::valid_path)
                || !c["pointer"].is_string()
                || c.get("expect").is_none()
            {
                return Err("Invalid or duplicate live contract".into());
            }
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
    let cases = v["cases"].as_array()?;
    let sql: Vec<_> = cases
        .iter()
        .filter(|c| c.get("document").is_none())
        .collect();
    let rows = if sql.is_empty() {
        Value::Null
    } else {
        run_sql(path, oid, &json!({"schema":v["schema"],"cases":sql}))
            .await
            .unwrap_or(Value::Null)
    };
    let mut i = 0;
    let mut out = vec![];
    for c in cases {
        if let Some(p) = c["document"].as_str() {
            let mut attempts = vec![];
            for _ in 0..2 {
                let s = crate::release_intake::local_git_output(
                    path,
                    &["cat-file", "blob", &format!("{oid}:{p}")],
                    true,
                )
                .await
                .ok();
                attempts.push(s.and_then(|d| crate::release_document::observe(&d, c)));
            }
            out.push(json!(attempts));
        } else {
            out.push(rows[i].clone());
            i += 1;
        }
    }
    Some(json!(out))
}
async fn run_sql(path: &Path, oid: &str, v: &Value) -> Option<Value> {
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
    ctx.db.with(|c|c.execute_batch("CREATE TABLE IF NOT EXISTS release_stage_ports(repo TEXT PRIMARY KEY REFERENCES repos(id) ON DELETE CASCADE,port INTEGER NOT NULL)"))?;
    if method == "sql_drivers" {
        return Ok(json!(ctx.db.query(
            "SELECT id,name FROM repos WHERE id!=? ORDER BY name,id LIMIT 200",
            [repo]
        )?));
    }
    if method == "sql_profile" {
        return Ok(ctx
            .db
            .query("SELECT p.driver,p.pin,d.name AS driver_name,d.path AS driver_path,(SELECT port FROM release_stage_ports s WHERE s.repo=p.repo) AS observer_port FROM sql_profiles p LEFT JOIN repos d ON d.id=p.driver WHERE p.repo=?", [repo])?
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
        let port = if a["stage_port"].is_null() {
            0
        } else {
            a["stage_port"]
                .as_u64()
                .filter(|p| (1024..=65535).contains(p))
                .ok_or("Choose an explicit unprivileged local observer port")?
        };
        ctx.db.with(|c| {
            let tx = c.unchecked_transaction()?;
            tx.execute(
                "INSERT OR REPLACE INTO sql_profiles(repo,driver,pin,blob) VALUES(?,?,?,?)",
                params![repo, driver, pin, snapshot],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO release_stage_ports(repo,port) VALUES(?,?)",
                params![repo, port as i64],
            )?;
            tx.commit()
        })?;
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
    let mut v = spec(raw)?;
    let base = r["base"].as_str().ok_or("Missing base")?;
    let head = r["head"].as_str().ok_or("Missing head")?;
    let mut record_pin = Value::Null;
    let mut verified_evidence = json!({});
    let mut evidence_status = json!({});
    let records = v["release_records"] == true;
    if v["release_records"] == true {
        v["artifacts"] = json!({});
        v["release_review"] = Value::Null;
        let load = async {
            let row = ctx
                .db
                .query(
                    "SELECT path FROM repos WHERE id=?",
                    [profile["driver"].as_str()?],
                )
                .ok()?
                .pop()?;
            let dp = Path::new(row["path"].as_str()?);
            let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
            let d = std::fs::canonicalize(local_git(dp, &args).await.ok()?).ok()?;
            let p = std::fs::canonicalize(local_git(path, &args).await.ok()?).ok()?;
            if d == p {
                return None;
            }
            let rev = local_git(dp, &["rev-parse", "--verify", "HEAD^{commit}"])
                .await
                .ok()?;
            let pin = profile["pin"].as_str()?;
            if local_git(dp, &["merge-base", "--all", &rev, pin])
                .await
                .ok()?
                != pin
            {
                return None;
            }
            let bytes = crate::release_intake::local_git_output(
                dp,
                &[
                    "cat-file",
                    "blob",
                    &format!("{rev}:philium/releases/{head}.json"),
                ],
                true,
            )
            .await
            .ok()?;
            if bytes.len() > 32768 {
                return None;
            }
            let att = crate::release_document::strict_value(&bytes)?;
            if att["driver_pin"] != pin
                || att["origin"] != o
                || att["pr"] != r["pr"]
                || att["base"] != base
                || att["head"] != head
            {
                return None;
            }
            let mut evidence = json!({});
            // Why each named evidence file did or did not count, so the operator sees the cause.
            let mut status = json!({});
            if let Some(rows) = att["evidence"].as_object().filter(|m| m.len() <= 12) {
                for (id, r) in rows {
                    let Some(p) = r["path"]
                        .as_str()
                        .filter(|p| p.starts_with("philium/evidence/") && safe_path(p, ".json"))
                    else {
                        status[id] = json!("invalid");
                        continue;
                    };
                    let Some(bytes) = crate::release_intake::local_git_output(
                        dp,
                        &["cat-file", "blob", &format!("{rev}:{p}")],
                        true,
                    )
                    .await
                    .ok() else {
                        status[id] = json!("missing");
                        continue;
                    };
                    if bytes.len() > 32768 {
                        status[id] = json!("invalid");
                        continue;
                    }
                    let hash = digest(bytes.as_bytes());
                    if r["sha256"] != hash {
                        status[id] = json!("altered");
                        continue;
                    }
                    let Some(e) = crate::release_document::strict_value(&bytes) else {
                        status[id] = json!("invalid");
                        continue;
                    };
                    if e["case"] == id.as_str()
                        && e["head"] == head
                        && e["status"] == "pass"
                        && e["observation"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty())
                    {
                        evidence[id] = json!(hash);
                        status[id] = json!("verified");
                    } else {
                        status[id] = json!("not_for_this_head");
                    }
                }
            }
            Some((rev, att, evidence, status))
        }
        .await;
        if let Some((pin, att, evidence, status)) = load {
            evidence_status = status;
            verified_evidence = evidence;
            record_pin = json!(pin);
            v["artifacts"][head] = att["artifact"].clone();
            v["release_review"] = att;
        }
    }
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
    let membership = crate::release_members::call(ctx, repo, o, path, seq, false).await?;
    let children = membership["landed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r["pr"].as_i64()?, r["oid"].as_str()?)))
        .filter(|(_, oid)| {
            g["commits"]
                .as_array()
                .is_some_and(|cs| cs.iter().any(|c| c["oid"] == *oid))
        })
        .collect::<Vec<_>>();
    let mut pins = BTreeSet::from([head, base, ancestor]);
    if parents.len() == 2 {
        pins.extend(parents.iter().copied());
    }
    for pin in pins {
        obs.insert(pin, run_one(path, pin, &v).await.unwrap_or(Value::Null));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    for (_, pin) in children.iter().take(8) {
        if !obs.contains_key(pin) {
            let Ok(value) = tokio::time::timeout_at(deadline, run_one(path, pin, &v)).await else {
                break;
            };
            obs.insert(*pin, value.unwrap_or(Value::Null));
        }
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
        let landed_evidence=children.iter().map(|(pr,oid)|{let child=s(oid);json!({"pr":pr,"oid":oid,"result":if child.is_none(){"untested"}else if pass(child){"pass"}else if child==h{"same_failure_as_candidate"}else{"different_failure"}})}).collect::<Vec<_>>();
        cases.push(json!({"id":c["id"],"state":state,"landed_evidence":landed_evidence}));
    }
    let blocked = cases.iter().any(|c| {
        matches!(
            c["state"].as_str(),
            Some("composed_head_regression" | "release_range_regression")
        )
    });
    let port = ctx
        .db
        .query("SELECT port FROM release_stage_ports WHERE repo=?", [repo])?
        .pop()
        .and_then(|r| r["port"].as_u64())
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| *n >= 1024);
    let provenance = stage_probe(port, head, v["artifacts"][head].as_str().unwrap_or("")).await;
    let mut live = vec![];
    for c in v["live_cases"].as_array().into_iter().flatten() {
        let mut observations = vec![];
        if let (Some(port), Some(hash), Some(endpoint)) = (
            port,
            v["artifacts"][head]
                .as_str()
                .and_then(|s| s.strip_prefix("sha256:")),
            c["path"].as_str(),
        ) {
            if provenance["state"] == "match" {
                for _ in 0..2 {
                    observations.push(
                        crate::release_http::sample(port, endpoint, head, hash)
                            .await
                            .and_then(|body| crate::release_document::observe(&body, c)),
                    );
                }
            }
        }
        let s = match observations.as_slice() {
            [Some(a), Some(b)] if a == b => {
                if a.starts_with("pass:") {
                    "pass"
                } else {
                    "mismatch"
                }
            }
            _ => "unknown",
        };
        live.push(json!({"id":c["id"],"state":s,"observations":observations}));
    }
    let after = if live.is_empty() {
        Value::Null
    } else {
        stage_probe(port, head, v["artifacts"][head].as_str().unwrap_or("")).await
    };
    let report = json!({"generation":seq,"pr":r["pr"],"observed":now(),"target":base,"head":head,"ancestor":ancestor,"parents":parents,"driver":profile["driver"],"driver_pin":profile["pin"],"contract_digest":digest(raw.as_bytes()),"sqlite":rusqlite::version(),"cases":cases,"observations":obs,"staging":"not_verified","deployment_identity":provenance,"deployment_after":after,"live_cases":live,"release_decision":if blocked{"not_ready_at_tested_head"}else{"incomplete"}});
    let mut report = report;
    report["base"] = json!(base);
    // The independently recorded artifact for exactly this head, or null when there is none.
    report["artifact"] = v["artifacts"][head].clone();
    report["release_records"] = json!(records);
    report["observer_configured"] = json!(port.is_some());
    let att = &v["release_review"];
    report["review_attestation"] = if att.is_object() {
        json!({"recorded":true,"all_changes_accounted_for":att["all_changes_accounted_for"]==true,"code_review_complete":att["code_review_complete"]==true,"cleanup_verified":att["cleanup_verified"]==true,"unresolved_findings":att["unresolved_findings"].as_array().map(Vec::len),"required_cases":att["required_cases"]})
    } else {
        json!({"recorded":false})
    };
    report["evidence_status"] = evidence_status;
    report["release_record_pin"] = record_pin;
    report["verified_evidence"] = verified_evidence;
    report["review_scope_verified"] = json!(crate::release_verify::reviewed(
        &v["release_review"],
        &json!({"pr":r["pr"],"base":base,"head":head}),
        &report
    ));
    ctx.db.exec(
        "INSERT INTO sql_receipts(repo,seq,report) VALUES(?,?,?)",
        params![repo, seq, report.to_string()],
    )?;
    Ok(report)
}

pub(crate) async fn stage_probe(port: Option<u16>, head: &str, digest: &str) -> Value {
    let observed = async {
        let port = port?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;
        let mut res = client
            .get(format!(
                "http://127.0.0.1:{port}/.well-known/philium/deployment"
            ))
            .send()
            .await
            .ok()?;
        if res.status() != reqwest::StatusCode::OK {
            return None;
        }
        let mut buf = vec![];
        while let Some(chunk) = res.chunk().await.ok()? {
            if buf.len() + chunk.len() > 32768 {
                return None;
            }
            buf.extend_from_slice(&chunk);
        }
        crate::release_document::strict_value(std::str::from_utf8(&buf).ok()?)
            .filter(Value::is_object)
    }
    .await
    .unwrap_or_else(|| {
        if port.is_some() {
            json!({})
        } else {
            Value::Null
        }
    });
    crate::release_stage::assess(head, digest, &observed, now())
}
