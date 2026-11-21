//! RPC: summary {range:24h|7d|30d}, timeseries {range,bucket:hour|day|milliseconds},
//! efficiency {range?}. Summary includes totals, by_provider, by_model, by_agent.
use crate::{ctx::Ctx, db, gateway::Activity};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};
static SESSION: OnceLock<String> = OnceLock::new();
pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.exec("CREATE TABLE IF NOT EXISTS stats_seen (session TEXT NOT NULL, activity_id INTEGER NOT NULL, PRIMARY KEY(session,activity_id))",[])?;
    ctx.db.exec("CREATE TABLE IF NOT EXISTS stats_outcomes (request_id INTEGER PRIMARY KEY REFERENCES requests(id) ON DELETE CASCADE, state TEXT NOT NULL)",[])?;
    let _ = ctx.db.exec("ALTER TABLE requests ADD COLUMN repo_id TEXT", []);
    ctx.db.exec("CREATE INDEX IF NOT EXISTS requests_repo_ts ON requests(repo_id, ts)", [])?;
    let _ = ctx.db.exec(
        "UPDATE requests SET repo_id = (SELECT a.repo_id FROM agents a WHERE a.name = requests.agent_id OR a.id = requests.agent_id LIMIT 1) WHERE repo_id IS NULL AND agent_id IS NOT NULL",
        [],
    );
    ctx.db.exec(
        "DELETE FROM requests WHERE ts < ?1",
        [db::now() - 30 * 86_400_000],
    )?;
    ctx.db.exec(
        "DELETE FROM stats_seen WHERE session != ?1",
        [SESSION.get_or_init(db::new_id)],
    )?;
    Ok(())
}
pub fn record(ctx: &Arc<Ctx>, a: &Activity) {
    if !matches!(a.state, "done" | "error" | "blocked") {
        return;
    }
    let session = SESSION.get_or_init(db::new_id);
    let model = a.model.as_deref().unwrap_or("unknown");
    let tps = match (a.output_tokens, a.duration_ms, a.ttft_ms) {
        (Some(n), Some(d), Some(t)) if d > t => Some(n as f64 * 1000.0 / (d - t) as f64),
        _ => None,
    };
    let result=ctx.db.with(|c|{
        let tx=c.unchecked_transaction()?;
        if tx.execute("INSERT OR IGNORE INTO stats_seen(session,activity_id) VALUES(?1,?2)",rusqlite::params![session,a.id as i64])?==0{return Ok(())}
        let prices=tx.query_row("SELECT price_in,price_out FROM models WHERE provider_id=?1 AND model_id=?2",rusqlite::params![a.provider,model],|r|Ok((r.get::<_,Option<f64>>(0)?,r.get::<_,Option<f64>>(1)?))).ok();
        let cost=prices.and_then(|(i,o)|Some((a.input_tokens.unwrap_or(0) as f64*i?+a.output_tokens.unwrap_or(0) as f64*o?)/1_000_000.0));
        let repo_id: Option<String> = match a.agent.as_deref() {
            Some(ag) if !ag.is_empty() => tx
                .query_row(
                    "SELECT repo_id FROM agents WHERE name=?1 OR id=?1 LIMIT 1",
                    rusqlite::params![ag],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten(),
            _ => None,
        };
        tx.execute("INSERT INTO requests(ts,agent_id,provider_id,model,status,tokens_in,tokens_out,tokens_cached,ttft_ms,duration_ms,tps,cost,guard_action,repo_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",rusqlite::params![a.started_ms as i64,a.agent,a.provider,model,a.status,a.input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,a.output_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,a.cached_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,a.ttft_ms.map(|n|n.min(i64::MAX as u64) as i64),a.duration_ms.map(|n|n.min(i64::MAX as u64) as i64),tps,cost,a.guard,repo_id])?;
        tx.execute("INSERT INTO stats_outcomes(request_id,state) VALUES(?1,?2)",rusqlite::params![tx.last_insert_rowid(),a.state])?;
        tx.commit()
    });
    if let Err(e) = result {
        eprintln!("stats record: {e}");
    }
}
fn since(args: &Value) -> Result<i64, String> {
    let span = match args["range"].as_str().unwrap_or("24h") {
        "24h" => 86_400_000,
        "7d" => 7 * 86_400_000,
        "30d" => 30 * 86_400_000,
        _ => return Err("range must be 24h, 7d or 30d".into()),
    };
    Ok(db::now() - span)
}
fn number(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.0)
}

fn median_from_csv(csv: Option<&str>) -> Option<f64> {
    let s = csv?;
    let mut tt: Vec<f64> = s.split(',').filter_map(|x| x.parse::<f64>().ok()).collect();
    if tt.is_empty() {
        return None;
    }
    tt.sort_by(f64::total_cmp);
    Some((tt[(tt.len() - 1) / 2] + tt[tt.len() / 2]) / 2.0)
}

fn empty_agg() -> Value {
    json!({
        "requests": 0usize,
        "tokens_in": 0.0,
        "tokens_out": 0.0,
        "tokens_cached": 0.0,
        "avg_ttft_ms": Value::Null,
        "p50_ttft_ms": Value::Null,
        "avg_tps": Value::Null,
        "cost": 0.0,
        "unpriced_requests": 0usize,
        "error_rate": 0.0,
        "guard_actions": BTreeMap::<String, usize>::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn agg_json(
    n: usize,
    tokens_in: f64,
    tokens_out: f64,
    tokens_cached: f64,
    avg_ttft_ms: Option<f64>,
    p50_ttft_ms: Option<f64>,
    avg_tps: Option<f64>,
    cost: f64,
    unpriced: usize,
    errors: usize,
    guards: BTreeMap<String, usize>,
) -> Value {
    json!({
        "requests": n,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "tokens_cached": tokens_cached,
        "avg_ttft_ms": avg_ttft_ms,
        "p50_ttft_ms": p50_ttft_ms,
        "avg_tps": avg_tps,
        "cost": cost,
        "unpriced_requests": unpriced,
        "error_rate": if n == 0 { 0.0 } else { errors as f64 / n as f64 },
        "guard_actions": guards,
    })
}

const AGG_COLS: &str = "
    COUNT(*) AS n,
    COALESCE(SUM(CAST(r.tokens_in AS REAL)), 0.0),
    COALESCE(SUM(CAST(r.tokens_out AS REAL)), 0.0),
    COALESCE(SUM(CAST(r.tokens_cached AS REAL)), 0.0),
    AVG(CAST(r.ttft_ms AS REAL)),
    GROUP_CONCAT(r.ttft_ms),
    AVG(r.tps),
    COALESCE(SUM(r.cost), 0.0),
    COALESCE(SUM(CASE WHEN r.cost IS NULL THEN 1 ELSE 0 END), 0),
    COALESCE(SUM(CASE WHEN COALESCE(r.status, 0) >= 400 OR o.state IN ('error', 'blocked') THEN 1 ELSE 0 END), 0)
";

fn read_agg_row(row: &rusqlite::Row<'_>, offset: usize, guards: BTreeMap<String, usize>) -> rusqlite::Result<Value> {
    let n = row.get::<_, i64>(offset)?.max(0) as usize;
    if n == 0 {
        return Ok(empty_agg());
    }
    let tokens_in = row.get::<_, f64>(offset + 1)?;
    let tokens_out = row.get::<_, f64>(offset + 2)?;
    let tokens_cached = row.get::<_, f64>(offset + 3)?;
    let avg_ttft_ms = row.get::<_, Option<f64>>(offset + 4)?;
    let ttft_csv = row.get::<_, Option<String>>(offset + 5)?;
    let p50_ttft_ms = median_from_csv(ttft_csv.as_deref());
    let avg_tps = row.get::<_, Option<f64>>(offset + 6)?;
    let cost = row.get::<_, f64>(offset + 7)?;
    let unpriced = row.get::<_, i64>(offset + 8)?.max(0) as usize;
    let errors = row.get::<_, i64>(offset + 9)?.max(0) as usize;
    Ok(agg_json(
        n,
        tokens_in,
        tokens_out,
        tokens_cached,
        avg_ttft_ms,
        p50_ttft_ms,
        avg_tps,
        cost,
        unpriced,
        errors,
        guards,
    ))
}

fn sql_totals(c: &rusqlite::Connection, start: i64, repo_id: Option<&str>) -> rusqlite::Result<Value> {
    let mut guards = BTreeMap::<String, usize>::new();
    {
        let mut st = c.prepare(
            "SELECT COALESCE(r.guard_action, 'pass') AS g, COUNT(*)
             FROM requests r
             WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)
             GROUP BY g",
        )?;
        let rows = st.query_map(rusqlite::params![start, repo_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
        })?;
        for item in rows {
            let (g, cnt) = item?;
            guards.insert(g, cnt);
        }
    }
    let sql = format!(
        "SELECT {AGG_COLS}
         FROM requests r
         LEFT JOIN stats_outcomes o ON o.request_id = r.id
         WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)"
    );
    c.query_row(&sql, rusqlite::params![start, repo_id], |r| read_agg_row(r, 0, guards))
}

fn sql_groups(
    c: &rusqlite::Connection,
    start: i64,
    repo_id: Option<&str>,
    keys: &[&str],
) -> rusqlite::Result<Vec<Value>> {
    let exprs: Vec<String> = keys
        .iter()
        .map(|k| format!("COALESCE(r.{k}, 'desk')"))
        .collect();
    let select_keys = exprs.join(", ");
    let klen = keys.len();

    let mut guards_by_key: BTreeMap<Vec<String>, BTreeMap<String, usize>> = BTreeMap::new();
    {
        let gsql = format!(
            "SELECT {select_keys}, COALESCE(r.guard_action, 'pass') AS g, COUNT(*)
             FROM requests r
             WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)
             GROUP BY {select_keys}, g"
        );
        let mut st = c.prepare(&gsql)?;
        let rows = st.query_map(rusqlite::params![start, repo_id], |r| {
            let mut kvec = Vec::with_capacity(klen);
            for i in 0..klen {
                kvec.push(r.get::<_, String>(i)?);
            }
            let g = r.get::<_, String>(klen)?;
            let cnt = r.get::<_, i64>(klen + 1)?.max(0) as usize;
            Ok((kvec, g, cnt))
        })?;
        for item in rows {
            let (kvec, g, cnt) = item?;
            guards_by_key.entry(kvec).or_default().insert(g, cnt);
        }
    }

    let sql = format!(
        "SELECT {select_keys}, {AGG_COLS}
         FROM requests r
         LEFT JOIN stats_outcomes o ON o.request_id = r.id
         WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)
         GROUP BY {select_keys}
         ORDER BY {select_keys}"
    );
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params![start, repo_id], |r| {
        let mut kvec = Vec::with_capacity(klen);
        for i in 0..klen {
            kvec.push(r.get::<_, String>(i)?);
        }
        let guards = guards_by_key.remove(&kvec).unwrap_or_default();
        let mut v = read_agg_row(r, klen, guards)?;
        for (k, val) in keys.iter().zip(kvec) {
            v[*k] = json!(val);
        }
        Ok(v)
    })?;
    rows.collect()
}

fn sql_timeseries(
    c: &rusqlite::Connection,
    start: i64,
    repo_id: Option<&str>,
    bucket: i64,
) -> Result<Vec<Value>, String> {
    let first_ts = start / bucket * bucket;
    let now_ts = db::now();
    if now_ts >= first_ts && ((now_ts - first_ts) / bucket + 1) >= 50000 {
        return Err("too many buckets".into());
    }
    let mut guards_by_ts: BTreeMap<i64, BTreeMap<String, usize>> = BTreeMap::new();
    {
        let mut st = c
            .prepare(
                "SELECT (r.ts / ?3) * ?3 AS b_ts, COALESCE(r.guard_action, 'pass') AS g, COUNT(*)
                 FROM requests r
                 WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)
                 GROUP BY b_ts, g",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(rusqlite::params![start, repo_id, bucket], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as usize,
                ))
            })
            .map_err(|e| e.to_string())?;
        for item in rows {
            let (ts, g, cnt) = item.map_err(|e| e.to_string())?;
            guards_by_ts.entry(ts).or_default().insert(g, cnt);
        }
    }
    let sql = format!(
        "SELECT (r.ts / ?3) * ?3 AS b_ts, {AGG_COLS}
         FROM requests r
         LEFT JOIN stats_outcomes o ON o.request_id = r.id
         WHERE r.ts >= ?1 AND (?2 IS NULL OR r.repo_id = ?2)
         GROUP BY b_ts"
    );
    let mut buckets: BTreeMap<i64, Value> = BTreeMap::new();
    {
        let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = st
            .query_map(rusqlite::params![start, repo_id, bucket], |r| {
                let b_ts = r.get::<_, i64>(0)?;
                let guards = guards_by_ts.remove(&b_ts).unwrap_or_default();
                let v = read_agg_row(r, 1, guards)?;
                Ok((b_ts, v))
            })
            .map_err(|e| e.to_string())?;
        for item in rows {
            let (b_ts, v) = item.map_err(|e| e.to_string())?;
            buckets.insert(b_ts, v);
        }
    }
    let mut out = Vec::new();
    let mut ts = first_ts;
    while ts <= now_ts {
        if out.len() >= 50000 {
            return Err("too many buckets".into());
        }
        let mut v = buckets.remove(&ts).unwrap_or_else(empty_agg);
        v["ts"] = json!(ts);
        out.push(v);
        ts += bucket;
    }
    Ok(out)
}

fn dispatch(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let start = since(&args)?;
    let repo_id = args["repo_id"].as_str().filter(|s| !s.is_empty());
    match method {
        "summary" => ctx.db.with(|c| {
            Ok(json!({
                "totals": sql_totals(c, start, repo_id)?,
                "by_provider": sql_groups(c, start, repo_id, &["provider_id"])?,
                "by_model": sql_groups(c, start, repo_id, &["provider_id", "model"])?,
                "by_agent": sql_groups(c, start, repo_id, &["agent_id"])?,
                "by_repo": sql_groups(c, start, repo_id, &["repo_id"])?,
            }))
        }),
        "timeseries" => {
            let bucket = match args.get("bucket") {
                None => 3_600_000,
                Some(Value::String(s)) => match s.as_str() {
                    "hour" | "1h" => 3_600_000,
                    "day" | "1d" => 86_400_000,
                    _ => return Err("bucket must be hour or day".into()),
                },
                Some(v) => v
                    .as_i64()
                    .filter(|n| *n >= 60_000)
                    .ok_or("bucket must be >=60000 ms")?,
            };
            ctx.db.with(|c| Ok(sql_timeseries(c, start, repo_id, bucket))).and_then(|r| r.map(|v| json!(v)))
        }
        "efficiency" => {
            let mut gs = ctx
                .db
                .with(|c| sql_groups(c, start, repo_id, &["provider_id", "model"]))?;
            for g in &mut gs {
                let i = number(g, "tokens_in");
                let o = number(g, "tokens_out");
                let c = number(g, "tokens_cached");
                g["output_input_ratio"] = if i > 0.0 { json!(o / i) } else { Value::Null };
                g["cached_share"] = if c > 0.0 && i > 0.0 { json!(c / i) } else { Value::Null };
                g["cost_per_1k_output"] = if o > 0.0 && number(g, "unpriced_requests") == 0.0 {
                    json!(number(g, "cost") * 1000.0 / o)
                } else {
                    Value::Null
                };
                g["cache_available"] = json!(c > 0.0 && i > 0.0);
            }
            Ok(json!(gs))
        }
        _ => Err(format!("unknown stats.{method}")),
    }
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let c = ctx.clone();
    let m = method.to_string();
    tokio::task::spawn_blocking(move || dispatch(&c, &m, args))
        .await
        .map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn activity() -> Activity {
        Activity {
            id: 1,
            started_ms: db::now() as u64,
            provider: "p".into(),
            model: Some("m".into()),
            method: "POST".into(),
            path: "/".into(),
            stream: true,
            state: "done",
            guard: "redacted",
            findings: BTreeMap::new(),
            status: Some(200),
            duration_ms: Some(2100),
            req_bytes: 0,
            resp_bytes: 0,
            input_tokens: Some(1000),
            output_tokens: Some(100),
            cached_tokens: None,
            ttft_ms: Some(100),
            agent: None,
            error: None,
        }
    }
    #[tokio::test]
    async fn dedup_cost_tps_and_groups() {
        let c = Ctx::test();
        init(&c).unwrap();
        c.db.exec(
            "INSERT INTO providers(id,name,kind,created_at) VALUES('p','P','openai',0)",
            [],
        )
        .unwrap();
        c.db.exec("INSERT INTO models(provider_id,model_id,price_in,price_out,discovered_at) VALUES('p','m',2,10,0)",[]).unwrap();
        let mut a = activity();
        a.state = "pending";
        record(&c, &a);
        assert!(c.db.query("SELECT * FROM requests", []).unwrap().is_empty());
        a.state = "done";
        record(&c, &a);
        record(&c, &a);
        let s = call(&c, "summary", json!({})).await.unwrap();
        assert_eq!(s["totals"]["requests"], 1);
        assert_eq!(s["totals"]["avg_tps"], 50.0);
        assert!((s["totals"]["cost"].as_f64().unwrap() - 0.003).abs() < 1e-9);
        assert_eq!(s["by_provider"][0]["provider_id"], "p");
        assert_eq!(s["totals"]["p50_ttft_ms"], 100.0);
        assert_eq!(
            call(&c, "efficiency", json!({})).await.unwrap()[0]["output_input_ratio"],
            0.1
        );
        assert_eq!(
            call(&c, "efficiency", json!({})).await.unwrap()[0]["cache_available"],
            false
        );
        a.id = 99;
        a.cached_tokens = Some(400);
        record(&c, &a);
        let eff = call(&c, "efficiency", json!({})).await.unwrap();
        assert_eq!(eff[0]["cache_available"], true);
        assert_eq!(eff[0]["cached_share"], 0.2);
    }
    #[tokio::test]
    async fn errors_ranges_and_empty() {
        let c = Ctx::test();
        init(&c).unwrap();
        assert!(call(&c, "summary", json!({"range":"bad"})).await.is_err());
        let mut a = activity();
        a.state = "blocked";
        a.status = Some(403);
        a.guard = "blocked";
        a.duration_ms = Some(100);
        record(&c, &a);
        let s = call(&c, "summary", json!({"range":"7d"})).await.unwrap();
        assert_eq!(s["totals"]["error_rate"], 1.0);
        assert!(s["totals"]["avg_tps"].is_null());
        assert_eq!(s["totals"]["guard_actions"]["blocked"], 1);
        a.id = 2;
        a.state = "error";
        a.status = Some(200);
        record(&c, &a);
        let s = call(&c, "summary", json!({})).await.unwrap();
        assert_eq!(s["totals"]["error_rate"], 1.0);
        assert_eq!(
            c.db.query("SELECT status FROM requests ORDER BY id DESC LIMIT 1", [])
                .unwrap()[0]["status"],
            200
        );
        assert!(!call(&c, "timeseries", json!({"bucket":"day"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[tokio::test]
    async fn repo_attribution_and_filtering() {
        let c = Ctx::test();
        init(&c).unwrap();
        c.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('r1','R1','/tmp/r1',0)", []).unwrap();
        c.db.exec("INSERT INTO agents(id,name,model_ref,state,repo_id,created_at,last_active_at) VALUES('a1','lion','p/m','running','r1',0,0)", []).unwrap();
        let mut a = activity();
        a.id = 500;
        a.agent = Some("lion".into());
        record(&c, &a);

        let mut desk = activity();
        desk.id = 501;
        desk.agent = None;
        record(&c, &desk);

        let all = call(&c, "summary", json!({})).await.unwrap();
        assert_eq!(all["totals"]["requests"], 2);
        let by_repo = all["by_repo"].as_array().unwrap();
        assert_eq!(by_repo.len(), 2);
        assert!(by_repo.iter().any(|g| g["repo_id"] == "r1" && g["requests"] == 1));

        let filtered = call(&c, "summary", json!({"repo_id": "r1"})).await.unwrap();
        assert_eq!(filtered["totals"]["requests"], 1);
        assert_eq!(filtered["by_agent"][0]["agent_id"], "lion");
    }
}
