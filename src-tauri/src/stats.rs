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
        tx.execute("INSERT INTO requests(ts,agent_id,provider_id,model,status,tokens_in,tokens_out,ttft_ms,duration_ms,tps,cost,guard_action) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",rusqlite::params![a.started_ms as i64,a.agent,a.provider,model,a.status,a.input_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,a.output_tokens.unwrap_or(0).min(i64::MAX as u64) as i64,a.ttft_ms.map(|n|n.min(i64::MAX as u64) as i64),a.duration_ms.map(|n|n.min(i64::MAX as u64) as i64),tps,cost,a.guard])?;
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
fn aggregate(rows: &[&Value]) -> Value {
    let n = rows.len();
    let sum = |k: &str| rows.iter().map(|r| number(r, k)).sum::<f64>();
    let avg = |k: &str| {
        let vals: Vec<_> = rows.iter().filter_map(|r| r[k].as_f64()).collect();
        if vals.is_empty() {
            None
        } else {
            Some(vals.iter().sum::<f64>() / vals.len() as f64)
        }
    };
    let mut tt: Vec<_> = rows.iter().filter_map(|r| r["ttft_ms"].as_f64()).collect();
    tt.sort_by(f64::total_cmp);
    let median = if tt.is_empty() {
        None
    } else {
        Some((tt[(tt.len() - 1) / 2] + tt[tt.len() / 2]) / 2.0)
    };
    let mut guards = BTreeMap::<String, usize>::new();
    for r in rows {
        *guards
            .entry(r["guard_action"].as_str().unwrap_or("pass").into())
            .or_default() += 1;
    }
    json!({"requests":n,"tokens_in":sum("tokens_in"),"tokens_out":sum("tokens_out"),"tokens_cached":sum("tokens_cached"),"avg_ttft_ms":avg("ttft_ms"),"p50_ttft_ms":median,"avg_tps":avg("tps"),"cost":sum("cost"),"unpriced_requests":rows.iter().filter(|r|r["cost"].is_null()).count(),"error_rate":if n==0{0.0}else{rows.iter().filter(|r|number(r,"status")>=400.0 || matches!(r["final_state"].as_str(),Some("error"|"blocked"))).count() as f64/n as f64},"guard_actions":guards})
}
fn groups(rows: &[Value], keys: &[&str]) -> Vec<Value> {
    let mut groups = BTreeMap::<Vec<String>, Vec<&Value>>::new();
    for r in rows {
        groups
            .entry(
                keys.iter()
                    .map(|k| r[*k].as_str().unwrap_or("desk").into())
                    .collect(),
            )
            .or_default()
            .push(r);
    }
    groups
        .into_iter()
        .map(|(key, rs)| {
            let mut v = aggregate(&rs);
            for (k, s) in keys.iter().zip(key) {
                v[*k] = json!(s);
            }
            v
        })
        .collect()
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let start = since(&args)?;
    let rows = ctx
        .db
        .query("SELECT r.*,o.state AS final_state FROM requests r LEFT JOIN stats_outcomes o ON o.request_id=r.id WHERE r.ts>=?1 ORDER BY r.ts", [start])?;
    match method {
        "summary" => Ok(
            json!({"totals":aggregate(&rows.iter().collect::<Vec<_>>()),"by_provider":groups(&rows,&["provider_id"]),"by_model":groups(&rows,&["provider_id","model"]),"by_agent":groups(&rows,&["agent_id"])}),
        ),
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
            let mut buckets = BTreeMap::<i64, Vec<&Value>>::new();
            for r in &rows {
                buckets
                    .entry(r["ts"].as_i64().unwrap_or(0) / bucket * bucket)
                    .or_default()
                    .push(r);
            }
            let mut out = Vec::new();
            let mut ts = start / bucket * bucket;
            while ts <= db::now() {
                if out.len() >= 50000 {
                    return Err("too many buckets".into());
                }
                let rs = buckets.remove(&ts).unwrap_or_default();
                let mut v = aggregate(&rs);
                v["ts"] = json!(ts);
                out.push(v);
                ts += bucket;
            }
            Ok(json!(out))
        }
        "efficiency" => {
            let mut gs = groups(&rows, &["provider_id", "model"]);
            for g in &mut gs {
                let i = number(g, "tokens_in");
                let o = number(g, "tokens_out");
                g["output_input_ratio"] = if i > 0.0 { json!(o / i) } else { Value::Null };
                // Gateway Activity currently omits cache telemetry. Null is not zero.
                g["cached_share"] = Value::Null;
                g["cost_per_1k_output"] = if o > 0.0 && number(g, "unpriced_requests") == 0.0 {
                    json!(number(g, "cost") * 1000.0 / o)
                } else {
                    Value::Null
                };
                g["cache_available"] = json!(false);
            }
            Ok(json!(gs))
        }
        _ => Err(format!("unknown stats.{method}")),
    }
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
}
