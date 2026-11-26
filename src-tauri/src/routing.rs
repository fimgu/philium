//! Helper model routing. Contract: docs/helper-routing.md.
//!
//! RPC (`routing.*`):
//!   get {repo_id?}                      -> {config, source, providers}
//!   set {repo_id?, config|null}         -> {ok}
//!   preview {repo_id, parent_model, job} -> {model_ref, reason}
//!
//! Off by default and fail closed: missing or unparsable config means off.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ctx::Ctx;

/// At most one rate-limit switch per helper in this window.
pub const SWITCH_COOLDOWN_MS: i64 = 10 * 60_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Jobs {
    #[serde(default)]
    pub explore: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub jobs: Jobs,
    #[serde(default = "default_pct")]
    pub avoid_above_pct: f64,
    #[serde(default = "default_true")]
    pub verify_elsewhere: bool,
}

fn default_pct() -> f64 {
    85.0
}
fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, allowed: vec![], jobs: Jobs::default(), avoid_above_pct: 85.0, verify_elsewhere: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Explore,
    Code,
    Verify,
}

impl Job {
    pub fn parse(s: &str) -> Result<Job, String> {
        match s {
            "explore" => Ok(Job::Explore),
            "code" => Ok(Job::Code),
            "verify" => Ok(Job::Verify),
            _ => Err(format!("unknown job {s}: use explore, code or verify")),
        }
    }
    /// Explicit job, else from the role: reviewer/verifier verify, everything else codes.
    pub fn infer(job: Option<&str>, role: Option<&str>) -> Result<Job, String> {
        match job.filter(|j| !j.is_empty()) {
            Some(j) => Job::parse(j),
            None if matches!(role, Some("reviewer" | "verifier")) => Ok(Job::Verify),
            None => Ok(Job::Code),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Job::Explore => "explore",
            Job::Code => "code",
            Job::Verify => "verify",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Job::Explore => "Explore",
            Job::Code => "Code",
            Job::Verify => "Verify",
        }
    }
}

/// What the chooser needs to know about a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Prov {
    pub id: String,
    pub name: String,
    pub trusted: bool,
    pub enabled: bool,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub pct: f64,
    pub limit_reached: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Route {
    pub model_ref: String,
    pub reason: String,
}

fn pid(r: &str) -> &str {
    r.split('/').next().unwrap_or("")
}

/// The providers a helper may use: allowed list plus the parent's own, trusted and enabled only.
fn pickable<'a>(cfg: &Config, parent: &str, provs: &'a [Prov], id: &str) -> Option<&'a Prov> {
    let allowed = id == pid(parent) || cfg.allowed.iter().any(|a| a == id);
    provs.iter().find(|p| p.id == id && p.trusted && p.enabled).filter(|_| allowed)
}

/// Pure routing decision. `exclude` lists providers that must not be picked (rate-limit switch).
#[allow(clippy::too_many_arguments)]
pub fn choose(
    cfg: Option<&Config>,
    parent: &str,
    parent_strict: bool,
    job: Job,
    explicit: Option<&str>,
    provs: &[Prov],
    usage: &HashMap<String, Usage>,
    exclude: &[String],
) -> Result<Route, String> {
    let cfg = match cfg.filter(|c| c.enabled && !parent_strict) {
        Some(c) => c,
        None => {
            if explicit.is_some_and(|m| m != parent) {
                return Err("Helpers must keep the parent's model".into());
            }
            return Ok(Route { model_ref: parent.to_string(), reason: "Same as parent".into() });
        }
    };
    let name = |id: &str| provs.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| id.to_string());
    if let Some(m) = explicit.filter(|m| !m.is_empty()) {
        let p = pid(m);
        if pickable(cfg, parent, provs, p).is_none() {
            return Err(format!("{p} is not allowed for helpers in this workspace"));
        }
        let model_ref = if m.contains('/') {
            m.to_string()
        } else {
            let d = provs.iter().find(|x| x.id == p).and_then(|x| x.default_model.clone()).ok_or_else(|| format!("no default model for {p}"))?;
            format!("{p}/{d}")
        };
        return Ok(Route { model_ref, reason: "Asked for by the agent".into() });
    }

    let default_of = |id: &str| -> Option<String> {
        pickable(cfg, parent, provs, id).and_then(|p| p.default_model.as_ref()).map(|m| format!("{id}/{m}"))
    };
    let others: Vec<&String> = cfg.allowed.iter().filter(|a| a.as_str() != pid(parent)).collect();
    let mut note = String::new();
    // (model_ref, reason when picked first)
    let mut cands: Vec<(String, String)> = vec![];
    let job_ref = match job {
        Job::Explore => &cfg.jobs.explore,
        Job::Code => &cfg.jobs.code,
        Job::Verify => &cfg.jobs.verify,
    };
    if let Some(r) = job_ref.as_deref().filter(|r| !r.is_empty()) {
        if pickable(cfg, parent, provs, pid(r)).is_some() && r.contains('/') {
            cands.push((r.to_string(), format!("{} job: configured model", job.label())));
        } else {
            note = format!("{} job model ignored ({} is not allowed, trusted and enabled). ", job.label(), name(pid(r)));
        }
    }
    if job == Job::Verify && cfg.verify_elsewhere {
        for o in &others {
            if let Some(r) = default_of(o) {
                cands.push((r, "Second opinion on another provider".into()));
            }
        }
    }
    cands.push((parent.to_string(), "Same as parent".into()));
    for o in &others {
        if let Some(r) = default_of(o) {
            cands.push((r, format!("Used {}", name(o))));
        }
    }
    let mut seen = std::collections::HashSet::new();
    cands.retain(|(r, _)| seen.insert(r.clone()) && !exclude.iter().any(|e| e == pid(r)));
    if cands.is_empty() {
        return Err("No allowed provider left for this helper".into());
    }

    let use_of = |r: &str| usage.get(pid(r)).copied();
    let busy = |r: &str| use_of(r).is_some_and(|u| u.limit_reached || u.pct >= cfg.avoid_above_pct);
    let busy_text = |r: &str| {
        let u = use_of(r).unwrap_or_default();
        if u.limit_reached { format!("{} hit its limit", name(pid(r))) } else { format!("{} at {:.0}%", name(pid(r)), u.pct) }
    };
    if let Some(i) = cands.iter().position(|(r, _)| !busy(r)) {
        let (r, why) = &cands[i];
        let reason = if i > 0 && busy(&cands[0].0) && pid(&cands[0].0) != pid(r) {
            format!("{}, used {}", busy_text(&cands[0].0), name(pid(r)))
        } else {
            why.clone()
        };
        return Ok(Route { model_ref: r.clone(), reason: format!("{note}{reason}") });
    }
    let key = |r: &str| use_of(r).map(|u| if u.limit_reached { f64::MAX } else { u.pct }).unwrap_or(0.0);
    let (r, _) = cands
        .iter()
        .min_by(|a, b| key(&a.0).partial_cmp(&key(&b.0)).unwrap_or(std::cmp::Ordering::Equal))
        .expect("non-empty");
    Ok(Route { model_ref: r.clone(), reason: format!("{note}All allowed providers are busy, used {} (lowest usage)", name(pid(r))) })
}

/// Validate a config before storing it.
pub fn validate(cfg: &Config, provs: &[Prov]) -> Result<(), String> {
    if !(1.0..=100.0).contains(&cfg.avoid_above_pct) {
        return Err("Usage threshold must be between 1 and 100".into());
    }
    for a in &cfg.allowed {
        if !provs.iter().any(|p| &p.id == a) {
            return Err(format!("unknown provider {a}"));
        }
    }
    for r in [&cfg.jobs.explore, &cfg.jobs.code, &cfg.jobs.verify].into_iter().flatten() {
        if crate::providers::split_ref(r).is_none() {
            return Err(format!("{r} is not a provider/model reference"));
        }
    }
    Ok(())
}

/// Parse a stored config. Anything unparsable is off.
pub fn parse(s: Option<&str>) -> Option<Config> {
    serde_json::from_str(s?).ok()
}

fn key(repo: Option<&str>) -> String {
    match repo {
        Some(r) => format!("routing:{r}"),
        None => "routing".into(),
    }
}

/// A present key is a decision, even if empty or garbled: it parses to off and never falls back
/// to a wider setting. Only a missing key (Reset deletes it) inherits.
fn stored(ctx: &Ctx, k: &str) -> Option<String> {
    ctx.db.kv_get(k)
}

/// Effective config for a workspace and where it came from. Override replaces global; no merge.
pub fn load(ctx: &Ctx, repo: Option<&str>) -> (Option<Config>, &'static str) {
    if let Some(r) = repo {
        if let Some(v) = stored(ctx, &key(Some(r))) {
            return (parse(Some(&v)), "workspace");
        }
    }
    match stored(ctx, "routing") {
        Some(v) => (parse(Some(&v)), "global"),
        None => (None, "default"),
    }
}

pub fn providers(ctx: &Ctx) -> Vec<Prov> {
    crate::providers::all(ctx)
        .into_iter()
        .map(|p| {
            let default_model = p
                .enabled
                .then(|| crate::providers::resolve(ctx, Some(&p.id)).ok().map(|(_, m)| m))
                .flatten();
            Prov { id: p.id, name: p.name, trusted: p.trusted, enabled: p.enabled, default_model }
        })
        .collect()
}

/// Per provider: highest window percent and limit flag, from the last usage snapshot (no
/// network) merged with the gateway's live header quotas.
pub fn usage(ctx: &Ctx) -> HashMap<String, Usage> {
    let mut out: HashMap<String, Usage> = HashMap::new();
    for a in crate::account_usage::last_snapshot() {
        let e = out.entry(a.provider_id.clone()).or_default();
        for w in &a.windows {
            e.pct = e.pct.max(w.used_percent);
        }
        e.limit_reached |= a.limit_reached;
    }
    for (p, q) in ctx.gateway.quotas() {
        let e = out.entry(p).or_default();
        for w in &q.windows {
            e.pct = e.pct.max(w.used_percent);
        }
        e.limit_reached |= e.pct >= 100.0;
    }
    out
}

/// Route a helper of a non-strict or strict parent. Impure wrapper around `choose`.
pub fn route(ctx: &Ctx, repo: Option<&str>, parent: &str, parent_strict: bool, job: Job, explicit: Option<&str>, exclude: &[String]) -> Result<Route, String> {
    let (cfg, _) = load(ctx, repo);
    choose(cfg.as_ref(), parent, parent_strict, job, explicit, &providers(ctx), &usage(ctx), exclude)
}

/// Enforcement: may a helper of `parent` in `repo` run on `model`?
pub fn helper_model_ok(ctx: &Ctx, repo: Option<&str>, parent: &str, model: &str) -> bool {
    if model == parent {
        return true;
    }
    let (Some(cfg), _) = load(ctx, repo) else { return false };
    cfg.enabled && pickable(&cfg, parent, &providers(ctx), pid(model)).is_some()
}

/// Pure: may a helper switch now, given when it last switched?
pub fn may_switch(last_ms: Option<i64>, now_ms: i64) -> bool {
    last_ms.is_none_or(|t| now_ms - t >= SWITCH_COOLDOWN_MS)
}

/// Pick a replacement for a routed helper whose provider hit a limit. None: stay put.
pub fn switch_target(ctx: &Ctx, helper: &Value, parent: &Value, limited: &str, now_ms: i64) -> Option<Route> {
    if helper["strict"].as_i64() == Some(1) || parent["strict"].as_i64() == Some(1) || helper["route_reason"].is_null() {
        return None;
    }
    let id = helper["id"].as_str()?;
    if pid(helper["model_ref"].as_str()?) != limited {
        return None;
    }
    let last = ctx.db.kv_get(&format!("routing.switched.{id}")).and_then(|v| v.parse().ok());
    if !may_switch(last, now_ms) {
        return None;
    }
    let job = Job::infer(ctx.db.kv_get(&format!("agent.job.{id}")).as_deref(), helper["role"].as_str()).unwrap_or(Job::Code);
    let (cfg, _) = load(ctx, helper["repo_id"].as_str());
    let provs = providers(ctx);
    let r = choose(cfg.as_ref(), parent["model_ref"].as_str()?, false, job, None, &provs, &usage(ctx), &[limited.to_string()]).ok()?;
    let name = provs.iter().find(|p| p.id == limited).map(|p| p.name.clone()).unwrap_or_else(|| limited.into());
    Some(Route { model_ref: r.model_ref, reason: format!("Switched: {name} hit its limit") })
}

/// Watch gateway activity for real rate-limit stops on routed helpers.
pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    let mut rx = ctx.gateway.events.subscribe();
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let a = match rx.recv().await {
                Ok(a) => a,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let limited = a.status == Some(429) || usage(&c).get(&a.provider).is_some_and(|u| u.limit_reached);
            let Some(agent) = a.agent.as_deref().filter(|_| limited && a.state != "pending") else { continue };
            if let Err(e) = on_limit(&c, agent, &a.provider).await {
                eprintln!("philium: routing switch for {agent}: {e}");
            }
        }
    });
    Ok(())
}

async fn on_limit(ctx: &Arc<Ctx>, agent: &str, provider: &str) -> Result<(), String> {
    let Some(h) = crate::agents::get_row(ctx, agent) else { return Ok(()) };
    let Some(parent) = h["parent_id"].as_str().and_then(|p| crate::agents::get_row(ctx, p)) else { return Ok(()) };
    let Some(r) = switch_target(ctx, &h, &parent, provider, crate::db::now()) else { return Ok(()) };
    let id = h["id"].as_str().unwrap_or_default().to_string();
    ctx.db.kv_set(&format!("routing.switched.{id}"), &crate::db::now().to_string())?;
    crate::agents::routed_switch(ctx, &id, &r.model_ref, &r.reason).await
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let repo = args["repo_id"].as_str().filter(|r| !r.is_empty());
    match method {
        "get" => {
            let (cfg, source) = load(ctx, repo);
            let u = usage(ctx);
            let provs: Vec<Value> = providers(ctx)
                .into_iter()
                .map(|p| {
                    let x = u.get(&p.id);
                    json!({ "id": p.id, "name": p.name, "trusted": p.trusted, "enabled": p.enabled,
                            "usage_pct": x.map(|x| x.pct), "limit_reached": x.is_some_and(|x| x.limit_reached) })
                })
                .collect();
            Ok(json!({ "config": cfg.unwrap_or_default(), "source": source, "providers": provs }))
        }
        "set" => {
            let k = key(repo);
            if args["config"].is_null() {
                if repo.is_none() {
                    return Err("The global setting cannot be removed; turn it off instead".into());
                }
                ctx.db.exec("DELETE FROM kv WHERE k=?1", [&k])?;
            } else {
                let cfg: Config = serde_json::from_value(args["config"].clone()).map_err(|e| format!("invalid config: {e}"))?;
                validate(&cfg, &providers(ctx))?;
                ctx.db.kv_set(&k, &serde_json::to_string(&cfg).map_err(|e| e.to_string())?)?;
            }
            ctx.emit("routing:changed", json!({ "repo_id": repo }));
            Ok(json!({ "ok": true }))
        }
        "preview" => {
            let parent = crate::ctx::arg_str(&args, "parent_model")?;
            let job = Job::parse(args["job"].as_str().unwrap_or("code"))?;
            Ok(json!(route(ctx, repo, &parent, false, job, None, &[])?))
        }
        _ => Err(format!("unknown routing.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(id: &str, trusted: bool, enabled: bool) -> Prov {
        Prov { id: id.into(), name: id.to_uppercase(), trusted, enabled, default_model: Some("m1".into()) }
    }
    fn provs() -> Vec<Prov> {
        vec![prov("a", true, true), prov("b", true, true), prov("c", true, true), prov("u", false, true), prov("d", true, false)]
    }
    fn cfg(allowed: &[&str]) -> Config {
        Config { enabled: true, allowed: allowed.iter().map(|s| s.to_string()).collect(), ..Config::default() }
    }
    fn none() -> HashMap<String, Usage> {
        HashMap::new()
    }
    fn u(list: &[(&str, f64, bool)]) -> HashMap<String, Usage> {
        list.iter().map(|(p, pct, l)| (p.to_string(), Usage { pct: *pct, limit_reached: *l })).collect()
    }
    fn pick(c: Option<&Config>, job: Job, usage: &HashMap<String, Usage>) -> String {
        choose(c, "a/m1", false, job, None, &provs(), usage, &[]).unwrap().model_ref
    }

    #[test]
    fn off_keeps_parent() {
        assert_eq!(pick(None, Job::Code, &none()), "a/m1");
        let mut c = cfg(&["b"]);
        c.enabled = false;
        assert_eq!(pick(Some(&c), Job::Explore, &none()), "a/m1");
        assert!(choose(None, "a/m1", false, Job::Code, Some("b/m1"), &provs(), &none(), &[]).is_err());
    }

    #[test]
    fn strict_never_routed() {
        let mut c = cfg(&["b"]);
        c.jobs.code = Some("b/m1".into());
        let r = choose(Some(&c), "u/m1", true, Job::Code, None, &provs(), &none(), &[]).unwrap();
        assert_eq!(r.model_ref, "u/m1");
        assert!(choose(Some(&c), "u/m1", true, Job::Code, Some("b/m1"), &provs(), &none(), &[]).is_err());
    }

    #[test]
    fn explicit_model_must_be_allowed() {
        let c = cfg(&["b"]);
        let e = choose(Some(&c), "a/m1", false, Job::Code, Some("c/m1"), &provs(), &none(), &[]).unwrap_err();
        assert_eq!(e, "c is not allowed for helpers in this workspace");
        let busy = u(&[("b", 99.0, true)]);
        assert_eq!(choose(Some(&c), "a/m1", false, Job::Code, Some("b/m2"), &provs(), &busy, &[]).unwrap().model_ref, "b/m2");
    }

    #[test]
    fn untrusted_and_disabled_never_chosen() {
        let mut c = cfg(&["u", "d"]);
        c.jobs.code = Some("u/m1".into());
        c.jobs.explore = Some("d/m1".into());
        let busy = u(&[("a", 99.0, false)]);
        for j in [Job::Code, Job::Explore, Job::Verify] {
            assert_eq!(pick(Some(&c), j, &busy), "a/m1");
        }
        assert!(choose(Some(&c), "a/m1", false, Job::Code, Some("u/m1"), &provs(), &none(), &[]).is_err());
        let r = choose(Some(&c), "a/m1", false, Job::Code, None, &provs(), &none(), &[]).unwrap();
        assert!(r.reason.contains("ignored"), "{}", r.reason);
    }

    #[test]
    fn over_threshold_skipped() {
        let mut c = cfg(&["b"]);
        c.jobs.code = Some("a/fast".into());
        let r = choose(Some(&c), "a/m1", false, Job::Code, None, &provs(), &u(&[("a", 92.0, false)]), &[]).unwrap();
        assert_eq!(r.model_ref, "b/m1");
        assert_eq!(r.reason, "A at 92%, used B");
        assert_eq!(pick(Some(&c), Job::Code, &u(&[("a", 10.0, true)])), "b/m1");
        assert_eq!(pick(Some(&c), Job::Code, &u(&[("a", 84.0, false)])), "a/fast");
    }

    #[test]
    fn all_busy_picks_lowest() {
        let c = cfg(&["b", "c"]);
        let r = choose(Some(&c), "a/m1", false, Job::Code, None, &provs(), &u(&[("a", 95.0, false), ("b", 90.0, false), ("c", 99.0, false)]), &[]).unwrap();
        assert_eq!(r.model_ref, "b/m1");
        assert!(r.reason.contains("busy"));
    }

    #[test]
    fn unknown_usage_is_available() {
        let c = cfg(&["b"]);
        assert_eq!(pick(Some(&c), Job::Code, &u(&[("a", 95.0, false)])), "b/m1");
        assert_eq!(pick(Some(&c), Job::Code, &none()), "a/m1");
    }

    #[test]
    fn verify_elsewhere_prefers_other_provider() {
        let mut c = cfg(&["a", "b"]);
        let r = choose(Some(&c), "a/m1", false, Job::Verify, None, &provs(), &none(), &[]).unwrap();
        assert_eq!(r.model_ref, "b/m1");
        assert_eq!(r.reason, "Second opinion on another provider");
        c.verify_elsewhere = false;
        assert_eq!(pick(Some(&c), Job::Verify, &none()), "a/m1");
    }

    #[test]
    fn exclude_drops_provider() {
        let c = cfg(&["b"]);
        assert_eq!(choose(Some(&c), "a/m1", false, Job::Code, None, &provs(), &none(), &["a".into()]).unwrap().model_ref, "b/m1");
        assert!(choose(Some(&cfg(&[])), "a/m1", false, Job::Code, None, &provs(), &none(), &["a".into()]).is_err());
    }

    #[test]
    fn bad_json_is_off_and_override_replaces_global() {
        let ctx = Ctx::test();
        assert_eq!(load(&ctx, Some("r1")), (None, "default"));
        ctx.db.kv_set("routing", &serde_json::to_string(&cfg(&["b", "c"])).unwrap()).unwrap();
        assert_eq!(load(&ctx, Some("r1")).1, "global");
        let mut w = cfg(&["c"]);
        w.avoid_above_pct = 50.0;
        ctx.db.kv_set("routing:r1", &serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(load(&ctx, Some("r1")), (Some(w), "workspace"));
        assert_eq!(load(&ctx, Some("r2")).0.unwrap().allowed, vec!["b", "c"]);
        ctx.db.kv_set("routing:r1", "").unwrap();
        let (c, src) = load(&ctx, Some("r1"));
        assert!(c.is_none() && src == "workspace", "empty override is off, never the wider global");
        ctx.db.kv_set("routing:r1", "{not json").unwrap();
        assert_eq!(load(&ctx, Some("r1")), (None, "workspace"));
        assert!(!helper_model_ok(&ctx, Some("r1"), "a/m1", "b/m1"));
    }

    #[test]
    fn validation() {
        let p = provs();
        assert!(validate(&cfg(&["b"]), &p).is_ok());
        assert!(validate(&cfg(&["zz"]), &p).is_err());
        let mut c = cfg(&[]);
        c.avoid_above_pct = 0.0;
        assert!(validate(&c, &p).is_err());
        let mut c = cfg(&[]);
        c.jobs.code = Some("nomodel".into());
        assert!(validate(&c, &p).is_err());
    }

    #[test]
    fn switch_cap() {
        assert!(may_switch(None, 0));
        assert!(!may_switch(Some(1_000), 1_000 + SWITCH_COOLDOWN_MS - 1));
        assert!(may_switch(Some(1_000), 1_000 + SWITCH_COOLDOWN_MS));
    }

    #[test]
    fn job_inference() {
        assert_eq!(Job::infer(None, Some("reviewer")).unwrap(), Job::Verify);
        assert_eq!(Job::infer(None, Some("worker")).unwrap(), Job::Code);
        assert_eq!(Job::infer(Some("explore"), Some("reviewer")).unwrap(), Job::Explore);
        assert!(Job::infer(Some("nope"), None).is_err());
    }
}
