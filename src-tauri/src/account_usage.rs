//! RPC `usage.accounts {force?}`: live, account-wide usage for every configured
//! provider, read from the provider itself (not from Philium's own traffic).
//!
//! Mirrors `jcode usage`: Anthropic OAuth `/api/oauth/usage` (five_hour, seven_day,
//! seven_day_opus, model-scoped weekly limits, extra usage, session-reset offer),
//! ChatGPT `wham/usage` (windows, plan, banked resets) and Antigravity
//! `fetchAvailableModels` quotaInfo (grouped by identical percent and reset).
//! API-key providers get a free `/models` validity probe.
//!
//! Read-only. Accounts are fetched in parallel, each with its own timeout. The
//! last good result per account is kept and served with the new error when a
//! refresh fails. Tokens are never logged or returned.

use crate::ctx::Ctx;
use crate::providers::{Kind, Provider};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const ANTHROPIC_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Same endpoint, asked for the session-limit reset offer without spending it.
const ANTHROPIC_RESET_QUERY: &str = "?at_wall=1&skip_spend=1";
const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Per-account budget; one slow provider never holds the others.
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(12);
/// Calls inside this window are served from cache unless forced.
const MIN_REFRESH_MS: i64 = 20_000;
/// Burn-rate samples are stored at most this often per window.
const SAMPLE_EVERY_MS: i64 = 5 * 60_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Window {
    pub name: String,
    /// "session" (hours), "weekly", "model" (model-scoped) or "other".
    pub scope: &'static str,
    pub used_percent: f64,
    /// Unix seconds.
    pub resets_at: Option<u64>,
    /// Models sharing this exact quota (Antigravity groups).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Unix seconds when, at the recent pace, this window would reach 100%.
    pub projected_full_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Banked {
    pub available: u64,
    pub next_available_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Account {
    pub provider_id: String,
    pub provider_name: String,
    pub kind: String,
    /// Masked email.
    pub account: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<Window>,
    pub banked_resets: Option<Banked>,
    /// Provider-reported hard stop (ChatGPT `limit_reached`).
    pub limit_reached: bool,
    /// Ordered label/value facts (extra usage, session resets, key status...).
    pub details: Vec<(String, String)>,
    /// "valid", "invalid", "missing" or "unreachable" for API-key providers.
    pub key_status: Option<String>,
    /// Last request through Philium (unix ms). Philium-local, not account-wide.
    pub last_used_ms: Option<i64>,
    /// Unix ms of the data shown (the last successful fetch).
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
    /// True when `error` is set but older good data is shown.
    pub stale: bool,
    pub enabled: bool,
}

fn now_ms() -> i64 {
    crate::db::now()
}

pub fn mask_email(e: &str) -> String {
    match e.split_once('@') {
        Some((user, domain)) => format!("{}***@{domain}", user.chars().next().map(String::from).unwrap_or_default()),
        None => "***".into(),
    }
}

fn ts(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.trim().parse::<u64>().ok().or_else(|| crate::gateway::quota::rfc3339(s.trim())),
        Value::Number(n) => n.as_f64().filter(|f| *f > 0.0).map(|f| if f > 1e12 { (f / 1000.0) as u64 } else { f as u64 }),
        _ => None,
    }
}

fn pct(v: &Value) -> Option<f64> {
    let f = v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())?;
    f.is_finite().then(|| f.clamp(0.0, 100.0))
}

fn win(name: impl Into<String>, scope: &'static str, used: f64, resets_at: Option<u64>) -> Window {
    Window { name: name.into(), scope, used_percent: used, resets_at, models: vec![], projected_full_at: None }
}

// ---------- parsers (pure, fixture-tested) ----------

/// Anthropic `/api/oauth/usage`. Utilization is already a percent.
pub fn parse_anthropic(v: &Value) -> (Vec<Window>, Vec<(String, String)>) {
    let mut out = vec![];
    let mut weekly_reset = None;
    for (key, name, scope) in [("five_hour", "5-hour window", "session"), ("seven_day", "7-day window", "weekly")] {
        if let Some(u) = pct(&v[key]["utilization"]) {
            let r = ts(&v[key]["resets_at"]);
            if key == "seven_day" {
                weekly_reset = r;
            }
            out.push(win(name, scope, u, r));
        }
    }
    if let Some(u) = pct(&v["seven_day_opus"]["utilization"]) {
        out.push(win("7-day Opus window", "model", u, ts(&v["seven_day_opus"]["resets_at"]).or(weekly_reset)));
    }
    for l in v["limits"].as_array().into_iter().flatten() {
        if l["kind"] != "weekly_scoped" {
            continue;
        }
        let (Some(model), Some(u)) = (l.pointer("/scope/model/display_name").and_then(Value::as_str), pct(&l["percent"])) else { continue };
        out.push(win(format!("7-day {model} window"), "model", u, ts(&l["resets_at"])));
    }
    let mut details = vec![];
    if v.get("extra_usage").is_some() || !out.is_empty() {
        let on = v.pointer("/extra_usage/is_enabled").and_then(Value::as_bool).unwrap_or(false);
        details.push(("Extra usage (long context)".into(), if on { "enabled" } else { "disabled" }.into()));
    }
    (out, details)
}

/// Session-limit reset offer from the at-wall usage response. Only an explicit
/// `eligible: false` is "Not eligible"; anything missing stays unknown.
pub fn parse_anthropic_reset(v: &Value) -> (Option<Banked>, Option<String>) {
    let Some(b) = v.get("juniper_tide").filter(|b| b.is_object()) else { return (None, None) };
    if b["eligible"] == false {
        return (None, Some("Not eligible".into()));
    }
    if b["eligible"] == true && b["arm"] == "reset" {
        let available = b["available"].as_bool().unwrap_or(false);
        let per_week = b["resets_per_week"].as_u64().unwrap_or(1).max(1);
        let next = ts(&b["next_available_at"]);
        let label = if available { format!("Available ({per_week} per week)") } else { "Used this week".into() };
        return (Some(Banked { available: u64::from(available), next_available_at: next }), Some(label));
    }
    (None, None)
}

fn window_label(secs: u64) -> String {
    const H: u64 = 3600;
    const D: u64 = 86_400;
    if (28 * D..=32 * D).contains(&secs) {
        "Monthly window".into()
    } else if secs >= D && secs % D == 0 {
        format!("{}-day window", secs / D)
    } else if secs >= H && secs % H == 0 {
        format!("{}-hour window", secs / H)
    } else {
        format!("{}-minute window", secs / 60)
    }
}

fn scope_of(name: &str) -> &'static str {
    if name.contains("hour") || name.contains("minute") { "session" } else { "weekly" }
}

fn wham_window(w: &Value, fallback: &str) -> Option<Window> {
    let used = pct(&w["used_percent"])?;
    let name = w["limit_window_seconds"].as_u64().or(w["window_seconds"].as_u64()).map(window_label).unwrap_or_else(|| fallback.into());
    let scope = scope_of(&name);
    Some(win(name, scope, used, ts(&w["reset_at"])))
}

fn wham_rate_limit(rl: &Value) -> Vec<Window> {
    [("primary_window", "5-hour window"), ("secondary_window", "7-day window")]
        .into_iter()
        .filter_map(|(k, f)| rl.get(k).filter(|w| !w.is_null()).and_then(|w| wham_window(w, f)))
        .collect()
}

pub struct Chatgpt {
    pub windows: Vec<Window>,
    pub plan: Option<String>,
    pub banked: Option<Banked>,
    pub limit_reached: bool,
}

/// ChatGPT `wham/usage`.
pub fn parse_chatgpt(v: &Value) -> Chatgpt {
    let mut windows = wham_rate_limit(&v["rate_limit"]);
    for e in v["additional_rate_limits"].as_array().into_iter().flatten() {
        let label = e["limit_name"].as_str().unwrap_or("Additional");
        for mut w in wham_rate_limit(&e["rate_limit"]) {
            let d = w.name.trim_end_matches(" window").replace("5-hour", "5h").replace("7-day", "7d");
            w.name = format!("{label} ({d})");
            w.scope = "model";
            windows.push(w);
        }
    }
    let truthy = |x: &Value| x.as_bool().or_else(|| x.as_str().map(|s| s.eq_ignore_ascii_case("true"))) == Some(true);
    let limit_reached = truthy(&v["limit_reached"]) || v.pointer("/rate_limit/allowed").and_then(Value::as_bool) == Some(false)
        || v.pointer("/rate_limit/limit_reached").is_some_and(truthy);
    let banked = v.pointer("/rate_limit_reset_credits/available_count").and_then(Value::as_u64).map(|n| Banked {
        available: n,
        next_available_at: v.pointer("/rate_limit_reset_credits/next_available_at").and_then(ts),
    });
    let plan = ["plan_type", "plan", "subscription_type"].iter().find_map(|k| v[*k].as_str()).map(str::to_string);
    Chatgpt { windows, plan, banked, limit_reached }
}

/// Antigravity `fetchAvailableModels`: one quota per model, many identical.
/// Grouped by (percent, reset) so twenty models with one shared quota are one row.
pub fn parse_antigravity(v: &Value) -> Vec<Window> {
    let Some(map) = v["models"].as_object() else { return vec![] };
    let mut ids: Vec<&String> = map.keys().collect();
    ids.sort();
    let mut seen = std::collections::HashSet::new();
    let mut groups: Vec<Window> = vec![];
    for id in ids {
        if id.starts_with("chat_") || id.starts_with("tab_") {
            continue;
        }
        let m = &map[id];
        let Some(rem) = m.pointer("/quotaInfo/remainingFraction").and_then(Value::as_f64).filter(|f| f.is_finite()) else { continue };
        let name = m["displayName"].as_str().unwrap_or(id).to_string();
        if !seen.insert(name.clone()) {
            continue;
        }
        let used = ((1.0 - rem.clamp(0.0, 1.0)) * 1000.0).round() / 10.0;
        let reset = m.pointer("/quotaInfo/resetTime").and_then(ts);
        match groups.iter_mut().find(|g| (g.used_percent - used).abs() < 0.05 && g.resets_at == reset) {
            Some(g) => g.models.push(name),
            None => {
                let mut w = win(String::new(), "model", used, reset);
                w.models.push(name);
                groups.push(w);
            }
        }
    }
    for g in &mut groups {
        g.models.sort();
        g.name = match g.models.len() {
            1 => g.models[0].clone(),
            n => format!("{} + {} more", g.models[0], n - 1),
        };
    }
    groups.sort_by(|a, b| b.used_percent.total_cmp(&a.used_percent));
    groups
}

// ---------- fetchers ----------

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(ACCOUNT_TIMEOUT).build().unwrap_or_default())
}

async fn get_json(req: reqwest::RequestBuilder, what: &str) -> Result<Value, String> {
    let r = req.send().await.map_err(|e| format!("{what} unreachable: {}", e.without_url()))?;
    let s = r.status();
    if !s.is_success() {
        let hint = if matches!(s.as_u16(), 401 | 403) { ". Sign in again on the Providers screen" } else if s.as_u16() == 429 { ". Rate limited, retrying later" } else { "" };
        return Err(format!("{what} returned HTTP {}{hint}", s.as_u16()));
    }
    r.json().await.map_err(|e| format!("{what} sent unreadable data: {e}"))
}

async fn fetch_claude(p: &Provider, a: &mut Account) -> Result<(), String> {
    let c = crate::oauth::access(&p.id, p.kind).await?;
    a.account = c.email.as_deref().map(mask_email);
    a.plan = c.plan.clone();
    let req = |url: String| {
        client().get(url).bearer_auth(&c.access).header("accept", "application/json")
            .header("user-agent", crate::oauth::claude_cli_user_agent())
            .header("anthropic-beta", crate::pins::CLAUDE_PROFILE_BETA)
    };
    let (usage, reset) = tokio::join!(
        get_json(req(ANTHROPIC_USAGE_URL.into()), "Claude usage"),
        get_json(req(format!("{ANTHROPIC_USAGE_URL}{ANTHROPIC_RESET_QUERY}")), "Claude reset status"),
    );
    let (windows, details) = parse_anthropic(&usage?);
    a.windows = windows;
    a.details = details;
    // Read-only lookup; failure must not hide otherwise valid usage.
    if let Ok(r) = reset {
        let (banked, label) = parse_anthropic_reset(&r);
        a.banked_resets = banked;
        if let Some(l) = label {
            a.details.push(("Session resets".into(), l));
        }
    }
    Ok(())
}

async fn fetch_chatgpt(p: &Provider, a: &mut Account) -> Result<(), String> {
    let c = crate::oauth::access(&p.id, p.kind).await?;
    a.account = c.email.as_deref().map(mask_email);
    let mut req = client().get(CHATGPT_USAGE_URL).bearer_auth(&c.access).header("accept", "application/json")
        .header("originator", crate::oauth::CHATGPT_ORIGINATOR);
    if let Some(id) = &c.account_id {
        req = req.header("chatgpt-account-id", id);
    }
    let g = parse_chatgpt(&get_json(req, "ChatGPT usage").await?);
    a.windows = g.windows;
    a.plan = g.plan.or(c.plan);
    a.banked_resets = g.banked;
    a.limit_reached = g.limit_reached;
    Ok(())
}

async fn fetch_antigravity(p: &Provider, a: &mut Account) -> Result<(), String> {
    use crate::gateway::gemini;
    let c = crate::oauth::access(&p.id, p.kind).await?;
    a.account = c.email.as_deref().map(mask_email);
    a.plan = c.plan.clone();
    let body = c.account_id.as_ref().map(|proj| json!({ "project": proj })).unwrap_or_else(|| json!({}));
    let url = format!("{}/v1internal:fetchAvailableModels", p.base_url.trim_end_matches('/'));
    let req = client().post(url).bearer_auth(&c.access).header("user-agent", gemini::user_agent())
        .header("x-goog-api-client", gemini::API_CLIENT).header("client-metadata", gemini::CLIENT_METADATA).json(&body);
    a.windows = parse_antigravity(&get_json(req, "Antigravity quota").await?);
    Ok(())
}

/// Free `/models` probe: says whether the key works, never what it costs.
async fn check_key(p: &Provider, a: &mut Account) -> Result<(), String> {
    let key = crate::providers::get_key(&p.id).unwrap_or_default();
    if key.is_empty() {
        a.key_status = Some("missing".into());
        a.details.push(("Key status".into(), "no key stored".into()));
        return Ok(());
    }
    let url = format!("{}/models", p.base_url.trim_end_matches('/'));
    let req = match p.kind {
        Kind::Anthropic => client().get(url).header("x-api-key", &key).header("anthropic-version", crate::pins::ANTHROPIC_VERSION),
        _ => client().get(url).bearer_auth(&key),
    };
    let (status, label) = match req.send().await {
        Ok(r) if r.status().is_success() => ("valid", "valid".to_string()),
        Ok(r) if matches!(r.status().as_u16(), 401 | 403) => ("invalid", format!("invalid or unauthorized ({})", r.status().as_u16())),
        Ok(r) => ("unreachable", format!("unknown (HTTP {})", r.status().as_u16())),
        Err(e) => ("unreachable", format!("unreachable: {}", e.without_url())),
    };
    a.key_status = Some(status.into());
    a.details.push(("Key status".into(), label));
    Ok(())
}

async fn fetch_one(p: Provider) -> (Account, Result<(), String>) {
    let mut a = Account { provider_id: p.id.clone(), provider_name: p.name.clone(), kind: p.kind.as_str().into(), enabled: p.enabled, ..Default::default() };
    let fut = async {
        match p.kind {
            Kind::ClaudeOauth => fetch_claude(&p, &mut a).await,
            Kind::ChatgptOauth => fetch_chatgpt(&p, &mut a).await,
            Kind::AntigravityOauth => fetch_antigravity(&p, &mut a).await,
            _ => check_key(&p, &mut a).await,
        }
    };
    let r = match tokio::time::timeout(ACCOUNT_TIMEOUT + Duration::from_secs(3), fut).await {
        Ok(r) => r,
        Err(_) => Err("Timed out reading usage".into()),
    };
    (a, r)
}

// ---------- cache, merge and burn rate ----------

struct Cache {
    at: i64,
    good: HashMap<String, Account>,
    last: Vec<Account>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { at: 0, good: HashMap::new(), last: vec![] }))
}

/// Combine a fresh attempt with the last good snapshot for that account.
pub fn merge(fresh: Account, r: Result<(), String>, last_good: Option<&Account>, now: i64) -> Account {
    match r {
        Ok(()) => Account { fetched_at: Some(now), ..fresh },
        Err(e) => match last_good {
            Some(g) => Account { error: Some(e), stale: true, enabled: fresh.enabled, provider_name: fresh.provider_name, ..g.clone() },
            None => Account { error: Some(e), fetched_at: None, ..fresh },
        },
    }
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.exec(
        "CREATE TABLE IF NOT EXISTS usage_samples (provider_id TEXT NOT NULL, window TEXT NOT NULL, ts INTEGER NOT NULL, pct REAL NOT NULL, resets_at INTEGER, PRIMARY KEY(provider_id, window, ts))",
        [],
    )?;
    ctx.db.exec("DELETE FROM usage_samples WHERE ts < ?1", [now_ms() - 8 * 86_400_000])?;
    Ok(())
}

/// Least-squares-free burn: first and latest sample inside the current reset
/// period. Needs 10 minutes of history and a rising percent to say anything.
pub fn project(samples: &[(i64, f64)], used: f64, now_ms: i64, resets_at: Option<u64>) -> Option<u64> {
    let (t0, p0) = *samples.first()?;
    let dt = now_ms - t0;
    if dt < 10 * 60_000 || used <= p0 || used >= 100.0 {
        return None;
    }
    let rate = (used - p0) / dt as f64; // percent per ms
    let full = now_ms as f64 + (100.0 - used) / rate;
    let full_s = (full / 1000.0) as u64;
    resets_at.is_none_or(|r| full_s < r).then_some(full_s)
}

fn sample_and_project(ctx: &Ctx, a: &mut Account, now: i64) {
    for w in &mut a.windows {
        let rows = ctx.db.query(
            "SELECT ts, pct FROM usage_samples WHERE provider_id=?1 AND window=?2 AND COALESCE(resets_at,0) BETWEEN ?3 - 120 AND ?3 + 120 ORDER BY ts",
            rusqlite::params![a.provider_id, w.name, w.resets_at.unwrap_or(0) as i64],
        ).unwrap_or_default();
        let s: Vec<(i64, f64)> = rows.iter().filter_map(|r| Some((r["ts"].as_i64()?, r["pct"].as_f64()?))).collect();
        if !a.stale && s.last().is_none_or(|(t, _)| now - t >= SAMPLE_EVERY_MS) {
            let _ = ctx.db.exec(
                "INSERT OR IGNORE INTO usage_samples(provider_id, window, ts, pct, resets_at) VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![a.provider_id, w.name, now, w.used_percent, w.resets_at.map(|r| r as i64)],
            );
        }
        w.projected_full_at = project(&s, w.used_percent, now, w.resets_at);
    }
}

fn last_used(ctx: &Ctx, provider: &str) -> Option<i64> {
    ctx.db.query("SELECT MAX(ts) AS t FROM requests WHERE provider_id=?1", [provider]).ok()?.first()?["t"].as_i64()
}

pub async fn accounts(ctx: &Arc<Ctx>, force: bool) -> Value {
    let now = now_ms();
    {
        let c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if !force && now - c.at < MIN_REFRESH_MS && !c.last.is_empty() {
            return json!({ "accounts": c.last, "checked_at": c.at });
        }
    }
    let providers = crate::providers::all(ctx);
    let results = futures_util::future::join_all(providers.into_iter().map(fetch_one)).await;
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::with_capacity(results.len());
    for (fresh, r) in results {
        let ok = r.is_ok();
        let mut a = merge(fresh.clone(), r, c.good.get(&fresh.provider_id), now);
        if ok {
            c.good.insert(a.provider_id.clone(), a.clone());
        }
        a.last_used_ms = last_used(ctx, &a.provider_id);
        sample_and_project(ctx, &mut a, now);
        out.push(a);
    }
    c.at = now;
    c.last = out.clone();
    json!({ "accounts": out, "checked_at": now })
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "accounts" => Ok(accounts(ctx, args["force"].as_bool().unwrap_or(false)).await),
        _ => Err(format!("unknown usage.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(w: &[Window]) -> Vec<&str> {
        w.iter().map(|w| w.name.as_str()).collect()
    }

    #[test]
    fn anthropic_zero_usage() {
        let (w, d) = parse_anthropic(&json!({
            "five_hour": {"utilization": 0.0, "resets_at": null},
            "seven_day": {"utilization": 0.0, "resets_at": "2026-11-24T14:00:00.115467+00:00"},
            "extra_usage": {"is_enabled": false}
        }));
        assert_eq!(names(&w), ["5-hour window", "7-day window"]);
        assert_eq!(w[0].used_percent, 0.0);
        assert_eq!(w[0].resets_at, None);
        assert_eq!(w[1].resets_at, crate::gateway::quota::rfc3339("2026-11-24T14:00:00Z"));
        assert_eq!(d, vec![("Extra usage (long context)".to_string(), "disabled".to_string())]);
    }

    #[test]
    fn anthropic_multiple_and_model_windows() {
        let (w, _) = parse_anthropic(&json!({
            "five_hour": {"utilization": 2.0, "resets_at": "2026-11-23T19:30:00.115448+00:00"},
            "seven_day": {"utilization": 81.0, "resets_at": "2026-11-24T14:00:00Z"},
            "seven_day_opus": {"utilization": 40.0, "resets_at": null},
            "limits": [
                {"kind": "weekly_scoped", "percent": 26.0, "resets_at": "2026-11-24T14:00:00Z", "scope": {"model": {"display_name": "Fable"}}},
                {"kind": "other", "percent": 99.0},
                {"kind": "weekly_scoped", "percent": 5.0}
            ],
            "extra_usage": {"is_enabled": true}
        }));
        assert_eq!(names(&w), ["5-hour window", "7-day window", "7-day Opus window", "7-day Fable window"]);
        assert_eq!(w[1].used_percent, 81.0);
        assert_eq!(w[2].resets_at, w[1].resets_at, "opus inherits the weekly reset");
        assert_eq!(w[3].scope, "model");
    }

    #[test]
    fn anthropic_missing_fields_are_omitted_not_zero() {
        let (w, d) = parse_anthropic(&json!({ "seven_day": {"utilization": 12} }));
        assert_eq!(names(&w), ["7-day window"]);
        assert_eq!(d[0].1, "disabled");
        let (w, d) = parse_anthropic(&json!({}));
        assert!(w.is_empty() && d.is_empty());
    }

    #[test]
    fn anthropic_reset_offer() {
        assert_eq!(parse_anthropic_reset(&json!({"juniper_tide": {"eligible": false}})).1.as_deref(), Some("Not eligible"));
        assert_eq!(parse_anthropic_reset(&json!({})), (None, None));
        let (b, l) = parse_anthropic_reset(&json!({"juniper_tide": {"eligible": true, "arm": "reset", "available": true, "resets_per_week": 2}}));
        assert_eq!(b.unwrap().available, 1);
        assert_eq!(l.as_deref(), Some("Available (2 per week)"));
    }

    #[test]
    fn chatgpt_windows_plan_and_banked() {
        let g = parse_chatgpt(&json!({
            "plan_type": "self_serve_business_prolite",
            "rate_limit": {
                "allowed": true,
                "primary_window": {"used_percent": 35, "limit_window_seconds": 604800, "reset_at": 1795691648},
                "secondary_window": null
            },
            "additional_rate_limits": [{"limit_name": "Codex Spark", "rate_limit": {"primary_window": {"used_percent": 3, "limit_window_seconds": 18000, "reset_at": 1795000000}}}],
            "rate_limit_reset_credits": {"available_count": 1}
        }));
        assert_eq!(names(&g.windows), ["7-day window", "Codex Spark (5h)"]);
        assert_eq!(g.windows[0].resets_at, Some(1795691648));
        assert_eq!(g.windows[0].scope, "weekly");
        assert_eq!(g.plan.as_deref(), Some("self_serve_business_prolite"));
        assert_eq!(g.banked.unwrap().available, 1);
        assert!(!g.limit_reached);
        assert!(parse_chatgpt(&json!({"rate_limit": {"allowed": false}})).limit_reached);
        let empty = parse_chatgpt(&json!({}));
        assert!(empty.windows.is_empty() && empty.plan.is_none() && empty.banked.is_none());
    }

    #[test]
    fn antigravity_groups_identical_quotas() {
        let q = |r: f64| json!({"remainingFraction": r, "resetTime": "2026-11-28T19:00:20Z"});
        let w = parse_antigravity(&json!({"models": {
            "a": {"displayName": "Gemini Flash", "quotaInfo": q(0.15)},
            "b": {"displayName": "Gemini Pro", "quotaInfo": q(0.15)},
            "b-alias": {"displayName": "Gemini Pro", "quotaInfo": q(0.15)},
            "c": {"displayName": "Claude Sonnet", "quotaInfo": q(0.037)},
            "tab_x": {"quotaInfo": q(0.5)},
            "noquota": {"displayName": "No quota"}
        }}));
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].used_percent, 96.3);
        assert_eq!(w[0].name, "Claude Sonnet");
        assert_eq!(w[1].used_percent, 85.0);
        assert_eq!(w[1].models, ["Gemini Flash", "Gemini Pro"]);
        assert_eq!(w[1].name, "Gemini Flash + 1 more");
        assert!(parse_antigravity(&json!({})).is_empty());
    }

    #[test]
    fn one_failure_keeps_last_good_and_others_render() {
        let good = Account { provider_id: "claude".into(), windows: vec![win("5-hour window", "session", 10.0, None)], fetched_at: Some(5), ..Default::default() };
        let failed = Account { provider_id: "claude".into(), enabled: true, ..Default::default() };
        let m = merge(failed.clone(), Err("Claude usage returned HTTP 401. Sign in again on the Providers screen".into()), Some(&good), 99);
        assert!(m.stale);
        assert_eq!(m.fetched_at, Some(5));
        assert_eq!(m.windows.len(), 1);
        assert!(m.error.unwrap().contains("401"));
        let cold = merge(failed, Err("Signed out. Sign in again on the Providers screen".into()), None, 99);
        assert!(!cold.stale && cold.windows.is_empty() && cold.fetched_at.is_none());
        let ok = merge(Account { provider_id: "gpt".into(), ..Default::default() }, Ok(()), None, 99);
        assert_eq!((ok.fetched_at, ok.error), (Some(99), None));
    }

    #[test]
    fn expired_creds_report_per_account() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let store = crate::oauth::MemStore(Default::default());
        let r = rt.block_on(crate::oauth::fresh(&store, "nobody", Kind::ClaudeOauth, "http://127.0.0.1:9", "http://127.0.0.1:9"));
        assert!(r.unwrap_err().contains("Sign in again"));
    }

    #[test]
    fn burn_projection() {
        let h = 3_600_000;
        // 10% -> 30% over 2h: 10%/h, 70% left = 7h.
        let p = project(&[(0, 10.0)], 30.0, 2 * h, Some(100_000)).unwrap();
        assert_eq!(p, (9 * h / 1000) as u64);
        assert_eq!(project(&[(0, 10.0)], 30.0, 2 * h, Some(3600)), None, "resets first");
        assert_eq!(project(&[(0, 10.0)], 10.0, 2 * h, None), None, "flat");
        assert_eq!(project(&[(0, 10.0)], 30.0, 60_000, None), None, "too little history");
        assert_eq!(project(&[], 30.0, 2 * h, None), None);
    }

    #[test]
    fn masks_email() {
        assert_eq!(mask_email("filip@example.com"), "f***@example.com");
        assert_eq!(mask_email("nope"), "***");
    }

    /// Live check against the real signed-in accounts. Read-only: set
    /// PHILIUM_LIVE_DATA to the app data dir. Emails print masked, tokens never.
    /// `PHILIUM_LIVE_DATA=... cargo test account_usage::tests::live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live() {
        let Some(dir) = std::env::var_os("PHILIUM_LIVE_DATA") else { return };
        crate::secrets::init(std::path::Path::new(&dir));
        for (id, kind, name) in [("claude", Kind::ClaudeOauth, "Claude"), ("gpt", Kind::ChatgptOauth, "ChatGPT"), ("google", Kind::AntigravityOauth, "Antigravity")] {
            let base = crate::providers::upstream(kind).unwrap_or_default().to_string();
            let p = Provider { id: id.into(), name: name.into(), kind, base_url: base, trusted: true, guard: crate::guard::GuardMode::Redact, enabled: true, default_model: None };
            let (a, r) = fetch_one(p).await;
            println!("{}", serde_json::to_string_pretty(&json!({ "account": a, "result": r })).unwrap());
        }
    }
}
