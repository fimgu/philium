//! Provider accounts and live model discovery.
//!
//! Two families:
//! - API-key providers (`openai_chat`, `openai_responses`, `anthropic`): the key
//!   lives only in the OS keychain and is injected by the gateway. HAL only
//!   ever sees `http://127.0.0.1:<gw>/p/<id>` plus a gateway token.
//! - Subscriptions (`claude_oauth`, `chatgpt_oauth`): trusted sign-ins owned
//!   by Philium (`oauth.rs`). Tokens live in the Keychain; HAL is configured
//!   as a normal provider pointing at the gateway, which swaps in the OAuth
//!   bearer token and the required headers. Strict agents never get them.
//!
//! Model lists are never hardcoded: they come from each provider's live
//! `/models` (or HAL discovery for subscriptions) and are cached in SQLite.
//! A model reference everywhere in Philium is `<provider id>/<model id>`,
//! which is also exactly what HAL accepts as a session model selector.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ctx::{arg_opt_str, arg_str, Ctx};
use crate::db::now;
use crate::gateway::Route;
use crate::guard::GuardMode;

pub const CLAUDE_UPSTREAM: &str = "https://api.anthropic.com/v1";
pub const CHATGPT_UPSTREAM: &str = "https://chatgpt.com/backend-api/codex";
pub const RESERVED_CLAUDE: &str = "claude-code";
const RESERVED: [&str; 5] = [RESERVED_CLAUDE, "none", "default", "chatgpt", "endpoint"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// OpenAI-compatible `/chat/completions` (most third-party endpoints).
    OpenaiChat,
    /// OpenAI `/responses`.
    OpenaiResponses,
    /// Anthropic Messages `/messages`.
    Anthropic,
    /// ChatGPT subscription, signed in through Philium.
    #[serde(alias = "chatgpt")]
    ChatgptOauth,
    /// Claude subscription, signed in through Philium.
    #[serde(alias = "claude_code")]
    ClaudeOauth,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind, String> {
        serde_json::from_value(json!(s)).map_err(|_| format!("unknown provider kind {s}"))
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::OpenaiChat => "openai_chat",
            Kind::OpenaiResponses => "openai_responses",
            Kind::Anthropic => "anthropic",
            Kind::ChatgptOauth => "chatgpt_oauth",
            Kind::ClaudeOauth => "claude_oauth",
        }
    }
    pub fn is_subscription(self) -> bool {
        matches!(self, Kind::ChatgptOauth | Kind::ClaudeOauth)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Stable slug; HAL model key and gateway route segment.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Real upstream URL (fixed for subscriptions).
    pub base_url: String,
    /// Untrusted providers only ever run agents in Strict mode.
    pub trusted: bool,
    pub guard: GuardMode,
    pub enabled: bool,
    /// Model id used when a caller names just the provider.
    pub default_model: Option<String>,
}

pub fn validate(p: &Provider) -> Result<(), String> {
    let id_ok = !p.id.is_empty()
        && p.id.len() <= 40
        && p.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !p.id.starts_with('-');
    if !id_ok || RESERVED.contains(&p.id.as_str()) {
        return Err("ID must be lowercase letters, digits and dashes (and not a reserved name)".into());
    }
    if p.name.trim().is_empty() {
        return Err("Name is required".into());
    }
    if p.kind.is_subscription() {
        if !p.trusted {
            return Err("Subscriptions must be trusted".into());
        }
        return Ok(());
    }
    let url = reqwest::Url::parse(&p.base_url).map_err(|_| "Base URL is not a valid URL")?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err("Base URL must be https (http only for localhost)".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err("Put credentials in the API key field, not the URL".into());
    }
    if let Some(m) = &p.default_model {
        valid_model_id(m)?;
    }
    Ok(())
}

/// `$` would be env-expanded by HAL's config loader; quotes/newlines break it.
pub fn valid_model_id(m: &str) -> Result<(), String> {
    if m.trim().is_empty() || m.len() > 200 || m.contains(['\n', '\r', '$', '"', '\\']) {
        return Err(format!("invalid model id {m:?}"));
    }
    Ok(())
}

// ---------- keychain ----------

pub fn get_key(id: &str) -> Option<String> {
    crate::secrets::get(&format!("provider:{id}"))
}

pub fn set_key(id: &str, key: &str) -> Result<(), String> {
    crate::secrets::set(&format!("provider:{id}"), key)
}

pub fn delete_key(id: &str) {
    crate::secrets::delete(&format!("provider:{id}"))
}

// ---------- storage ----------

fn from_row(r: &Value) -> Option<Provider> {
    Some(Provider {
        id: r["id"].as_str()?.into(),
        name: r["name"].as_str()?.into(),
        kind: Kind::parse(r["kind"].as_str()?).ok()?,
        base_url: r["base_url"].as_str().unwrap_or("").into(),
        trusted: r["trust"].as_str() != Some("untrusted"),
        guard: serde_json::from_value(r["guard_mode"].clone()).unwrap_or_default(),
        enabled: r["enabled"].as_i64() != Some(0),
        default_model: r["default_model"].as_str().map(str::to_string),
    })
}

pub fn all(ctx: &Ctx) -> Vec<Provider> {
    ctx.db
        .query("SELECT * FROM providers ORDER BY created_at", [])
        .unwrap_or_default()
        .iter()
        .filter_map(from_row)
        .collect()
}

pub fn get(ctx: &Ctx, id: &str) -> Option<Provider> {
    all(ctx).into_iter().find(|p| p.id == id)
}

pub(crate) fn upsert(ctx: &Ctx, p: &Provider) -> Result<(), String> {
    let guard = serde_json::to_value(p.guard).unwrap();
    ctx.db.exec(
        "INSERT INTO providers(id,name,kind,base_url,trust,guard_mode,enabled,default_model,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(id) DO UPDATE SET name=?2,kind=?3,base_url=?4,trust=?5,guard_mode=?6,enabled=?7,default_model=?8",
        rusqlite::params![
            p.id,
            p.name,
            p.kind.as_str(),
            p.base_url,
            if p.trusted { "trusted" } else { "untrusted" },
            guard.as_str().unwrap_or("redact"),
            p.enabled as i64,
            p.default_model,
            now()
        ],
    )?;
    Ok(())
}

/// Split `provider/model` (model ids may themselves contain `/`).
pub fn split_ref(r: &str) -> Option<(&str, &str)> {
    r.split_once('/').filter(|(p, m)| !p.is_empty() && !m.is_empty())
}

/// Resolve a model reference (or the global default) to (provider, model id).
pub fn resolve(ctx: &Ctx, model_ref: Option<&str>) -> Result<(Provider, String), String> {
    let wanted = match model_ref {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => ctx.setting("default_model", ""),
    };
    let (pid, mid) = match split_ref(&wanted) {
        Some((p, m)) => (p.to_string(), Some(m.to_string())),
        None if !wanted.is_empty() => (wanted.clone(), None),
        None => {
            let p = all(ctx).into_iter().find(|p| p.enabled).ok_or("Add a provider first")?;
            (p.id, None)
        }
    };
    let p = get(ctx, &pid).ok_or_else(|| format!("unknown provider {pid}"))?;
    if !p.enabled {
        return Err(format!("provider {pid} is disabled"));
    }
    let mid = mid
        .or_else(|| p.default_model.clone())
        .or_else(|| first_model(ctx, &p.id))
        .ok_or_else(|| format!("no models known for {pid}; refresh its model list"))?;
    Ok((p, mid))
}

/// Never pick review/auto/mini-style variants as the implicit default.
fn is_minor_variant(id: &str) -> bool {
    let l = id.to_lowercase();
    ["review", "auto", "mini", "nano", "lite", "haiku", "flash", "preview", "instruct", "embed", "search", "image", "audio", "tts", "realtime", "transcribe", "moderation"]
        .iter()
        .any(|w| l.contains(w))
}

/// Flagship pick: discovery order (as stored), skipping minor variants;
/// falls back to the first enabled model.
pub fn pick_default<'a>(ids: &'a [String]) -> Option<&'a String> {
    ids.iter().find(|m| !is_minor_variant(m)).or_else(|| ids.first())
}

fn first_model(ctx: &Ctx, pid: &str) -> Option<String> {
    let rows = ctx.db.query("SELECT model_id FROM models WHERE provider_id=?1 AND enabled=1", [pid]).ok()?;
    let enabled: Vec<String> = rows.iter().filter_map(|r| r["model_id"].as_str().map(str::to_string)).collect();
    // Discovery order saved by refresh_models; alphabetical as a fallback.
    let order: Vec<String> = ctx
        .db
        .kv_get(&format!("models_order.{pid}"))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut ids: Vec<String> = order.into_iter().filter(|m| enabled.contains(m)).collect();
    let mut rest: Vec<String> = enabled.into_iter().filter(|m| !ids.contains(m)).collect();
    rest.sort();
    ids.extend(rest);
    pick_default(&ids).cloned()
}

/// Push enabled API-key providers (with keys from the keychain) to the gateway.
pub fn sync_routes(ctx: &Ctx) {
    let routes = all(ctx)
        .into_iter()
        .filter(|p| p.enabled)
        .map(|p| Route { api_key: if p.kind.is_subscription() { None } else { get_key(&p.id) }, provider: p })
        .collect();
    ctx.gateway.set_routes(routes);
}

// ---------- HAL config ----------

/// What a HAL launch may see.
#[derive(Default, Clone)]
pub struct ConfigOpts {
    /// Gateway token to embed (per-agent tokens give attribution). Defaults to
    /// the gateway's own desk token.
    pub token: Option<String>,
    /// Restrict to these provider ids. `None` = every enabled provider.
    pub only: Option<Vec<String>>,
    /// Exclude subscription providers (Strict agents never get them).
    pub no_subscriptions: bool,
    /// Default model ref for new sessions.
    pub default_ref: Option<String>,
}

pub struct HalSetup {
    pub config: Value,
    /// Always false now; kept for the launcher contract.
    pub allow_logins: bool,
    /// Env overrides that hide whichever login is not included.
    pub env: Vec<(String, String)>,
}

/// The HAL config for one launch. API-key models point at the local gateway
/// with only a gateway token; subscriptions are HAL's own login transports.
pub fn hal_setup(ctx: &Ctx, hal_data: &Path, opts: &ConfigOpts) -> HalSetup {
    let token = opts.token.clone().unwrap_or_else(|| ctx.gateway.token.clone());
    let gw = &ctx.gateway_url;
    let providers: Vec<Provider> = all(ctx)
        .into_iter()
        .filter(|p| p.enabled)
        .filter(|p| opts.only.as_ref().is_none_or(|o| o.contains(&p.id)))
        .filter(|p| !(opts.no_subscriptions && p.kind.is_subscription()))
        .collect();
    let mut models = serde_json::Map::new();
    for p in &providers {
        let model = p.default_model.clone().or_else(|| first_model(ctx, &p.id));
        let mut m = {
            let k = p.kind;
            let mut m = json!({
                // HAL rejects `api: anthropic` behind a non-official base_url, so
                // Anthropic kinds speak chat_completions; the gateway translates.
                "provider": "openai-compatible",
                "api": match k { Kind::OpenaiResponses | Kind::ChatgptOauth => "responses", _ => "chat_completions" },
                "base_url": format!("{gw}/p/{}", p.id),
                "api_key": token,
            });
            if matches!(k, Kind::Anthropic | Kind::ClaudeOauth) {
                m["vision"] = json!(true);
            }
            if let Some(cw) = model.as_ref().and_then(|mid| context_window(ctx, &p.id, mid)) {
                m["context_window"] = json!(cw);
            }
            m
        };
        if let Some(mid) = model {
            m["model"] = json!(mid);
        } else {
            m["model"] = json!("unknown");
        }
        models.insert(p.id.clone(), m);
    }
    // HAL would add a `claude-code` model (local Claude CLI) on its own.
    // Occupy that name with a route the gateway does not serve: fails closed.
    models.entry(RESERVED_CLAUDE.to_string()).or_insert_with(|| {
        json!({ "provider": "openai-compatible", "api": "chat_completions",
                "base_url": format!("{gw}/p/{RESERVED_CLAUDE}"), "api_key": token,
                "model": "disabled-in-philium" })
    });
    let mut default = None;
    if let Some(r) = opts.default_ref.clone().or_else(|| Some(ctx.setting("default_model", ""))) {
        let (pid, mid) = split_ref(&r).map(|(a, b)| (a, Some(b))).unwrap_or((r.as_str(), None));
        if providers.iter().any(|p| p.id == pid) {
            if let Some(mid) = mid {
                models.get_mut(pid).unwrap()["model"] = json!(mid);
            }
            default = Some(pid.to_string());
        }
    }
    let default = default.or_else(|| providers.first().map(|p| p.id.clone())).unwrap_or_else(|| {
        // HAL needs one model; this one has no gateway route, so it fails
        // closed with "unknown provider" instead of auto-detecting logins.
        models.insert(
            "none".into(),
            json!({ "provider": "openai-compatible", "api": "chat_completions",
                    "base_url": format!("{gw}/p/none"), "api_key": token,
                    "model": "add-a-provider-in-philium" }),
        );
        "none".into()
    });
    // HAL never gets a real login: hide both CLI login dirs.
    let no_login = hal_data.parent().unwrap_or(hal_data).join("no-login").to_string_lossy().into_owned();
    let env = vec![("CODEX_HOME".into(), no_login.clone()), ("CLAUDE_CONFIG_DIR".into(), no_login)];
    HalSetup {
        config: json!({ "server": { "data_dir": hal_data }, "default_model": default, "models": models }),
        allow_logins: false,
        env,
    }
}

fn context_window(ctx: &Ctx, pid: &str, mid: &str) -> Option<i64> {
    ctx.db
        .query("SELECT context_window FROM models WHERE provider_id=?1 AND model_id=?2", [pid, mid])
        .ok()?
        .first()?["context_window"]
        .as_i64()
}

// ---------- discovery ----------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffortOption {
    pub value: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Discovered {
    pub id: String,
    pub display: Option<String>,
    pub context_window: Option<i64>,
    pub price_in: Option<f64>,
    pub price_out: Option<f64>,
    /// Explicitly advertised reasoning effort options from upstream `/models`
    /// (`supported_reasoning_levels`, `reasoning_efforts`, `supported_parameters`, etc.).
    /// `None` means upstream `/models` did not include effort metadata for this model.
    /// `Some(vec![])` means upstream explicitly reported that this model accepts no effort parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<EffortOption>>,
}

fn format_effort_label(v: &str) -> String {
    let mut chars = v.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Extract reasoning effort options dynamically from a raw `/models` entry without hardcoding level names.
fn parse_discovered_efforts(m: &Value) -> Option<Vec<EffortOption>> {
    // 1. Explicit array of levels from provider (`/models` `supported_reasoning_levels`, `reasoning_efforts`, `thinking.efforts`, etc.)
    if let Some(arr) = m
        .get("supported_reasoning_levels")
        .or_else(|| m.get("reasoning_efforts"))
        .or_else(|| m.pointer("/capabilities/reasoning_efforts"))
        .or_else(|| m.pointer("/capabilities/supported_reasoning_levels"))
        .or_else(|| m.pointer("/thinking/supported_efforts"))
        .and_then(Value::as_array)
    {
        let mut out: Vec<EffortOption> = Vec::new();
        for item in arr {
            let val = item
                .as_str()
                .or_else(|| item.get("effort").and_then(Value::as_str))
                .or_else(|| item.get("id").and_then(Value::as_str))
                .or_else(|| item.get("value").and_then(Value::as_str))
                .or_else(|| item.get("name").and_then(Value::as_str));
            if let Some(raw) = val {
                let value = raw.trim().to_string();
                if value.is_empty() || out.iter().any(|x| x.value == value) {
                    continue;
                }
                let label = item
                    .get("label")
                    .or_else(|| item.get("display_name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format_effort_label(&value));
                let hint = item
                    .get("description")
                    .or_else(|| item.get("hint"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.push(EffortOption { value, label, hint });
            }
        }
        return Some(out);
    }

    // 2. Provider `/models` `supported_parameters` (e.g. OpenRouter, LiteLLM, vLLM)
    if let Some(params) = m.get("supported_parameters").and_then(Value::as_array) {
        let supports_reasoning = params.iter().filter_map(Value::as_str).any(|p| {
            p.contains("reasoning") || p.contains("thinking") || p == "effort"
        });
        if !supports_reasoning {
            return Some(vec![]);
        }
    }

    // 3. Explicit boolean capability flag in `/models`
    if let Some(thinking) = m
        .pointer("/capabilities/thinking")
        .or_else(|| m.pointer("/capabilities/reasoning"))
        .or_else(|| m.get("thinking_support"))
        .or_else(|| m.get("reasoning"))
        .and_then(Value::as_bool)
    {
        if !thinking {
            return Some(vec![]);
        }
    }
    None
}

/// Record what an upstream provider accepted or rejected at runtime for `(provider_id, model_id)`.
/// Called by the gateway when an upstream API rejects an effort parameter or reports valid enum values.
pub fn record_learned_efforts(ctx: &Ctx, provider_id: &str, model_id: &str, levels: Vec<String>) {
    let mut map = discovered_efforts_map(ctx, provider_id);
    let opts: Vec<EffortOption> = levels
        .into_iter()
        .filter(|v| !v.is_empty() && v != "default")
        .map(|value| EffortOption {
            label: format_effort_label(&value),
            hint: None,
            value,
        })
        .collect();
    map.insert(model_id.to_string(), opts);
    if let Ok(s) = serde_json::to_string(&map) {
        let _ = ctx.db.kv_set(&format!("models_efforts.{provider_id}"), &s);
        ctx.notify("providers");
        ctx.emit("providers:changed", json!({ "id": provider_id }));
    }
}

/// Dynamic effort options for `(kind, model_id)`.
/// Uses the provider's discovered `/models` metadata or gateway-learned capabilities first;
/// if the provider's `/models` endpoint did not include capability metadata yet, falls back
/// only to protocol-level (`Kind`) transport defaults until the gateway observes otherwise.
pub fn model_effort_details(kind: Kind, _model_id: &str, discovered: Option<&[EffortOption]>) -> Vec<EffortOption> {
    if let Some(exp) = discovered {
        if exp.is_empty() {
            return vec![];
        }
        let mut out = vec![EffortOption {
            value: "default".into(),
            label: "Auto".into(),
            hint: Some("Use the provider's default reasoning behavior".into()),
        }];
        for item in exp {
            if item.value != "default" && !out.iter().any(|x| x.value == item.value) {
                out.push(item.clone());
            }
        }
        return out;
    }

    // When the upstream `/models` endpoint only returns bare model IDs without capability
    // metadata, provide the protocol's standard effort levels (which will be refined automatically
    // if the upstream endpoint ever rejects one).
    let levels: &[(&str, &str)] = match kind {
        Kind::Anthropic | Kind::ClaudeOauth => &[
            ("default", "Auto"),
            ("none", "None"),
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("max", "Max"),
        ],
        Kind::ChatgptOauth | Kind::OpenaiResponses | Kind::OpenaiChat => &[
            ("default", "Auto"),
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
        ],
    };
    levels
        .iter()
        .map(|(v, l)| EffortOption {
            value: (*v).into(),
            label: (*l).into(),
            hint: None,
        })
        .collect()
}

pub fn model_efforts(kind: Kind, model_id: &str, discovered: Option<&[EffortOption]>) -> Vec<String> {
    model_effort_details(kind, model_id, discovered)
        .into_iter()
        .map(|o| o.value)
        .collect()
}

/// Validate a requested effort against what the provider advertised or learned for `model_ref`.
/// Never blocks custom provider values unless the provider explicitly advertised a closed list
/// that excludes `requested`.
pub fn validate_effort_for_ref(ctx: &Ctx, model_ref: Option<&str>, requested: &str) -> Result<String, String> {
    let req = requested.trim().to_ascii_lowercase();
    if req.is_empty() || req == "default" || req == "auto" {
        return Ok("default".into());
    }
    if let Ok((p, mid)) = resolve(ctx, model_ref) {
        let disc = discovered_efforts_map(ctx, &p.id);
        if let Some(explicit) = disc.get(&mid) {
            if explicit.is_empty() {
                return Err(format!("Provider `{}` reports that `{}` does not accept reasoning effort", p.id, mid));
            }
            if !explicit.iter().any(|o| o.value.eq_ignore_ascii_case(&req)) {
                let allowed: Vec<&str> = std::iter::once("default")
                    .chain(explicit.iter().map(|o| o.value.as_str()))
                    .collect();
                return Err(format!(
                    "Provider `{}` supports effort [{}] for `{}` (got `{requested}`)",
                    p.id,
                    allowed.join(", "),
                    mid
                ));
            }
        }
    }
    Ok(req)
}

fn discovered_efforts_map(ctx: &Ctx, provider_id: &str) -> std::collections::HashMap<String, Vec<EffortOption>> {
    ctx.db
        .kv_get(&format!("models_efforts.{provider_id}"))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Parse an OpenAI/Anthropic/OpenRouter-style `/models` body.
pub fn parse_models(body: &Value) -> Vec<Discovered> {
    let mut out = parse_models_ordered(body);
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Same, but keeps the provider's own order (flagship first for most APIs).
pub fn parse_models_ordered(body: &Value) -> Vec<Discovered> {
    let list = body.get("data").or_else(|| body.get("models")).unwrap_or(body);
    let Some(arr) = list.as_array() else { return vec![] };
    let mut out: Vec<Discovered> = arr
        .iter()
        .filter_map(|m| {
            let id = m.get("id").or_else(|| m.get("slug")).or_else(|| m.get("name"))?.as_str()?.to_string();
            valid_model_id(&id).ok()?;
            let num = |v: Option<&Value>| v.and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()));
            Some(Discovered {
                display: m.get("display_name").or_else(|| m.get("name")).and_then(Value::as_str).map(str::to_string).filter(|d| *d != id),
                context_window: num(m.get("context_length").or_else(|| m.get("context_window")).or_else(|| m.pointer("/top_provider/context_length"))).map(|x| x as i64),
                // OpenRouter reports $/token as strings; store $/1M tokens.
                price_in: num(m.pointer("/pricing/prompt")).map(|x| x * 1e6),
                price_out: num(m.pointer("/pricing/completion")).map(|x| x * 1e6),
                efforts: parse_discovered_efforts(m),
                id,
            })
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    out.retain(|m| seen.insert(m.id.clone()));
    out
}

/// Short aliases ("opus") duplicate full ids ("claude-opus-5-5"); keep the full ones.
pub fn dedupe_aliases(out: &mut Vec<Discovered>) {
    let mut seen = std::collections::HashSet::new();
    out.retain(|m| seen.insert(m.id.clone()));
    let full: Vec<String> = out.iter().filter(|m| m.id.contains('-')).map(|m| m.id.clone()).collect();
    out.retain(|m| m.id.contains('-') || !full.iter().any(|f| f.split('-').any(|part| part == m.id)));
}

async fn discover(_ctx: &Arc<Ctx>, p: &Provider) -> Result<Vec<Discovered>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let base = p.base_url.trim_end_matches('/');
    let mut url = format!("{base}/models");
    let mut req;
    if p.kind.is_subscription() {
        let c = crate::oauth::access(&p.id, p.kind).await?;
        if p.kind == Kind::ClaudeOauth {
            url.push_str("?limit=1000");
            req = client.get(&url).bearer_auth(&c.access).header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", crate::oauth::claude_beta(None)).header("user-agent", crate::oauth::CLAUDE_CLI_USER_AGENT);
        } else {
            url.push_str("?client_version=1.0.0");
            req = client.get(&url).bearer_auth(&c.access).header("originator", crate::oauth::CHATGPT_ORIGINATOR);
            if let Some(a) = &c.account_id {
                req = req.header("chatgpt-account-id", a);
            }
        }
    } else {
        let key = get_key(&p.id).unwrap_or_default();
        req = client.get(&url);
        if !key.is_empty() {
            req = match p.kind {
                Kind::Anthropic => req.header("x-api-key", &key).header("anthropic-version", "2023-06-01"),
                _ => req.bearer_auth(&key),
            };
        }
    }
    let resp = req.send().await.map_err(|e| format!("{url}: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let hint = if p.kind.is_subscription() && matches!(status.as_u16(), 401 | 403) { " (sign in again)" } else { "" };
        return Err(format!("HTTP {status} from {url}{hint}: {}", text.chars().take(300).collect::<String>()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("bad /models JSON: {e}"))?;
    let mut list = parse_models_ordered(&body);
    if p.kind.is_subscription() {
        dedupe_aliases(&mut list);
    }
    if list.is_empty() {
        return Err("provider returned no models".into());
    }
    Ok(list)
}

/// Refresh one provider's models into SQLite. User edits (enabled, prices,
/// context window) survive; vanished models are removed.
pub async fn refresh_models(ctx: &Arc<Ctx>, id: &str) -> Result<Vec<Value>, String> {
    let p = get(ctx, id).ok_or("unknown provider")?;
    let found = match discover(ctx, &p).await {
        Ok(f) => f,
        Err(e) => {
            ctx.db.kv_set(&format!("models_error.{id}"), &e)?;
            ctx.notify("providers");
            return Err(e);
        }
    };
    let t = now();
    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for m in &found {
            tx.execute(
                "INSERT INTO models(provider_id,model_id,display,context_window,price_in,price_out,discovered_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(provider_id,model_id) DO UPDATE SET discovered_at=?7,
                   display=COALESCE(display,?3), context_window=COALESCE(context_window,?4),
                   price_in=COALESCE(price_in,?5), price_out=COALESCE(price_out,?6)",
                rusqlite::params![id, m.id, m.display, m.context_window, m.price_in, m.price_out, t],
            )?;
        }
        tx.execute("DELETE FROM models WHERE provider_id=?1 AND discovered_at<?2", rusqlite::params![id, t])?;
        tx.execute("UPDATE providers SET models_cached_at=?2 WHERE id=?1", rusqlite::params![id, t])?;
        tx.commit()
    })?;
    let order: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
    ctx.db.kv_set(&format!("models_order.{id}"), &serde_json::to_string(&order).unwrap_or_default())?;
    let mut efforts_map = discovered_efforts_map(ctx, id);
    for m in &found {
        if let Some(e) = &m.efforts {
            efforts_map.insert(m.id.clone(), e.clone());
        }
    }
    if efforts_map.is_empty() {
        let _ = ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("models_efforts.{id}")]);
    } else {
        ctx.db.kv_set(&format!("models_efforts.{id}"), &serde_json::to_string(&efforts_map).unwrap_or_default())?;
    }
    ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("models_error.{id}")])?;
    ctx.notify("providers");
    ctx.emit("providers:changed", json!({ "id": id }));
    models(ctx, Some(id))
}

pub fn models(ctx: &Ctx, provider: Option<&str>) -> Result<Vec<Value>, String> {
    let sql = "SELECT m.*, p.name AS provider_name, p.trust, p.kind, p.provider_id || '/' || m.model_id AS ref
               FROM models m JOIN (SELECT id AS provider_id, name, trust, kind, enabled FROM providers) p USING(provider_id)
               WHERE p.enabled=1 AND (?1 IS NULL OR m.provider_id=?1) ORDER BY p.provider_id, m.model_id";
    let mut rows = ctx.db.query(sql, [provider])?;
    let mut disc_cache: std::collections::HashMap<String, std::collections::HashMap<String, Vec<EffortOption>>> =
        std::collections::HashMap::new();
    for r in &mut rows {
        let pid = r["provider_id"].as_str().unwrap_or("").to_string();
        let mid = r["model_id"].as_str().unwrap_or("").to_string();
        let kind = r["kind"].as_str().and_then(|k| Kind::parse(k).ok()).unwrap_or(Kind::OpenaiChat);
        let disc = disc_cache
            .entry(pid.clone())
            .or_insert_with(|| discovered_efforts_map(ctx, &pid));
        let exp = disc.get(&mid).map(Vec::as_slice);
        r["efforts"] = json!(model_efforts(kind, &mid, exp));
        r["effort_details"] = json!(model_effort_details(kind, &mid, exp));
    }
    Ok(rows)
}

// ---------- module ----------

/// Create or fix the provider row for a signed-in subscription.
pub fn ensure_oauth_provider(ctx: &Ctx, id: &str, kind: Kind) -> Result<(), String> {
    let existing = get(ctx, id);
    let claude = kind == Kind::ClaudeOauth;
    let p = Provider {
        id: id.into(),
        name: existing.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| if claude { "Claude".into() } else { "ChatGPT".into() }),
        kind,
        base_url: if claude { CLAUDE_UPSTREAM } else { CHATGPT_UPSTREAM }.into(),
        trusted: true,
        guard: existing.as_ref().map(|e| e.guard).unwrap_or_default(),
        enabled: true,
        default_model: existing.and_then(|e| e.default_model),
    };
    upsert(ctx, &p)?;
    if ctx.setting("default_model", "").is_empty() {
        ctx.db.kv_set("setting.default_model", &p.id)?;
    }
    Ok(())
}

/// Old CLI-wrapped providers become OAuth kinds that need a fresh sign-in.
pub fn migrate_legacy(ctx: &Ctx) {
    for (old, new, url) in [("claude_code", "claude_oauth", CLAUDE_UPSTREAM), ("chatgpt", "chatgpt_oauth", CHATGPT_UPSTREAM)] {
        let _ = ctx.db.exec("UPDATE providers SET kind=?2, base_url=?3, trust='trusted' WHERE kind=?1", rusqlite::params![old, new, url]);
    }
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    migrate_legacy(ctx);
    sync_routes(ctx);
    // Refresh stale model lists in the background (6h TTL).
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        for p in all(&c).into_iter().filter(|p| p.enabled) {
            let fresh = c
                .db
                .query("SELECT models_cached_at FROM providers WHERE id=?1", [&p.id])
                .ok()
                .and_then(|r| r.first()?["models_cached_at"].as_i64())
                .is_some_and(|t| now() - t < 6 * 3600 * 1000);
            if !fresh {
                let _ = refresh_models(&c, &p.id).await;
            }
        }
    });
    Ok(())
}

fn public(ctx: &Ctx, p: &Provider) -> Value {
    let count = ctx
        .db
        .query("SELECT COUNT(*) AS n FROM models WHERE provider_id=?1", [&p.id])
        .ok()
        .and_then(|r| r.first()?["n"].as_i64())
        .unwrap_or(0);
    let cached = ctx
        .db
        .query("SELECT models_cached_at FROM providers WHERE id=?1", [&p.id])
        .ok()
        .and_then(|r| r.first().map(|x| x["models_cached_at"].clone()))
        .unwrap_or(Value::Null);
    let mut v = serde_json::to_value(p).unwrap();
    if p.kind.is_subscription() {
        let c = crate::oauth::Store::load(&crate::oauth::Keychain, &p.id);
        v["signed_in"] = json!(c.is_some());
        v["account"] = json!(c.as_ref().map(|c| json!({ "email": c.email, "plan": c.plan, "account_id": c.account_id })));
        v["has_key"] = json!(c.is_some());
    } else {
        v["has_key"] = json!(get_key(&p.id).is_some_and(|k| !k.is_empty()));
    }
    v["model_count"] = json!(count);
    v["models_cached_at"] = cached;
    v["models_error"] = json!(ctx.db.kv_get(&format!("models_error.{}", p.id)));
    v["via_gateway"] = json!(true);
    v
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "list" => Ok(json!(all(ctx).iter().map(|p| public(ctx, p)).collect::<Vec<_>>())),
        m if m.starts_with("oauth_") => crate::oauth::call(ctx, m, args).await,
        "save" => {
            let kind = Kind::parse(&arg_str(&args, "kind")?)?;
            let id = arg_str(&args, "id")?;
            let existing = get(ctx, &id);
            if args["create"].as_bool() == Some(true) && existing.is_some() {
                return Err(format!("a provider with id {id} already exists"));
            }
            let p = Provider {
                name: arg_str(&args, "name")?,
                kind,
                base_url: if kind == Kind::ClaudeOauth { CLAUDE_UPSTREAM.into() } else if kind == Kind::ChatgptOauth { CHATGPT_UPSTREAM.into() } else { arg_str(&args, "base_url")?.trim().to_string() },
                trusted: args["trusted"].as_bool().unwrap_or(kind.is_subscription()),
                guard: serde_json::from_value(args["guard"].clone()).unwrap_or(existing.as_ref().map(|e| e.guard).unwrap_or_default()),
                enabled: args["enabled"].as_bool().unwrap_or(true),
                default_model: arg_opt_str(&args, "default_model").filter(|s| !s.is_empty()).or(existing.and_then(|e| e.default_model)),
                id,
            };
            validate(&p)?;
            if let Some(k) = arg_opt_str(&args, "api_key").filter(|k| !k.trim().is_empty()) {
                if k.contains(['\n', '\r']) {
                    return Err("API key contains a line break".into());
                }
                set_key(&p.id, k.trim())?;
            }
            upsert(ctx, &p)?;
            if ctx.setting("default_model", "").is_empty() {
                ctx.db.kv_set("setting.default_model", &p.id)?;
            }
            sync_routes(ctx);
            ctx.log_event("provider_saved", None, &json!({ "id": p.id, "kind": p.kind, "trusted": p.trusted }));
            ctx.notify("providers");
            ctx.emit("providers:changed", json!({ "id": p.id }));
            // Discovery runs after the desk HAL picks up a new subscription.
            let (c, id) = (ctx.clone(), p.id.clone());
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let _ = refresh_models(&c, &id).await;
            });
            Ok(public(ctx, &p))
        }
        "remove" => {
            let id = arg_str(&args, "id")?;
            ctx.db.exec("DELETE FROM providers WHERE id=?1", [&id])?;
            ctx.db.exec("DELETE FROM models WHERE provider_id=?1", [&id])?;
            delete_key(&id);
            crate::oauth::Store::delete(&crate::oauth::Keychain, &id);
            if split_ref(&ctx.setting("default_model", "")).map(|x| x.0).unwrap_or(&ctx.setting("default_model", "")) == id {
                let next = all(ctx).first().map(|p| p.id.clone()).unwrap_or_default();
                ctx.db.kv_set("setting.default_model", &next)?;
            }
            sync_routes(ctx);
            ctx.log_event("provider_removed", None, &json!({ "id": id }));
            ctx.notify("providers");
            ctx.emit("providers:changed", json!({ "id": id }));
            Ok(Value::Null)
        }
        "refresh_models" => Ok(json!(refresh_models(ctx, &arg_str(&args, "id")?).await?)),
        "models" => Ok(json!(models(ctx, arg_opt_str(&args, "provider").as_deref())?)),
        "set_model" => {
            let pid = arg_str(&args, "provider")?;
            let mid = arg_str(&args, "model")?;
            for (col, v) in [("price_in", &args["price_in"]), ("price_out", &args["price_out"]), ("context_window", &args["context_window"])] {
                if !v.is_null() {
                    let sql = format!("UPDATE models SET {col}=?3 WHERE provider_id=?1 AND model_id=?2");
                    ctx.db.exec(&sql, rusqlite::params![pid, mid, v.as_f64()])?;
                }
            }
            if let Some(e) = args["enabled"].as_bool() {
                ctx.db.exec("UPDATE models SET enabled=?3 WHERE provider_id=?1 AND model_id=?2", rusqlite::params![pid, mid, e as i64])?;
            }
            ctx.notify("providers");
            Ok(Value::Null)
        }
        "set_default" => {
            let r = arg_str(&args, "ref")?;
            let (p, m) = resolve(ctx, Some(&r))?;
            ctx.db.kv_set("setting.default_model", &format!("{}/{m}", p.id))?;
            ctx.notify("providers");
            Ok(json!(format!("{}/{m}", p.id)))
        }
        "default" => Ok(json!(resolve(ctx, None).ok().map(|(p, m)| format!("{}/{m}", p.id)))),
        _ => Err(format!("unknown providers.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, url: &str) -> Provider {
        Provider {
            id: id.into(),
            name: "X".into(),
            kind: Kind::OpenaiChat,
            base_url: url.into(),
            trusted: false,
            guard: GuardMode::Redact,
            enabled: true,
            default_model: Some("m".into()),
        }
    }

    /// Touches the real OS keychain; run with `--ignored`.
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        let id = "philium-selftest";
        set_key(id, "k-123").unwrap();
        assert_eq!(get_key(id).as_deref(), Some("k-123"));
        delete_key(id);
        assert_eq!(get_key(id), None);
    }

    #[test]
    fn validation() {
        assert!(validate(&p("cn-1", "https://api.example.cn/v1")).is_ok());
        assert!(validate(&p("x", "http://localhost:8000/v1")).is_ok());
        for (id, url) in [
            ("Bad", "https://a/v1"),
            ("../x", "https://a/v1"),
            ("x", "http://remote.example/v1"),
            ("x", "https://user:pw@a/v1"),
            ("x", "https://a/v1?key=1"),
            ("x", "ftp://a"),
            (RESERVED_CLAUDE, "https://a/v1"),
            ("none", "https://a/v1"),
        ] {
            assert!(validate(&p(id, url)).is_err(), "{id} {url}");
        }
        let mut s = p("sub", "");
        s.kind = Kind::ChatgptOauth;
        assert!(validate(&s).is_err(), "untrusted subscription rejected");
        s.trusted = true;
        assert!(validate(&s).is_ok());
    }

    #[test]
    fn hal_config_never_contains_real_endpoints_or_keys() {
        let ctx = Ctx::test();
        upsert(&ctx, &p("cn", "https://api.example.cn/v1")).unwrap();
        let mut a = p("claude", "https://api.anthropic.com/v1");
        a.kind = Kind::Anthropic;
        upsert(&ctx, &a).unwrap();
        let s = hal_setup(&ctx, Path::new("/d/hal"), &ConfigOpts::default());
        let c = &s.config;
        let text = c.to_string();
        assert!(!text.contains("example.cn") && !text.contains("anthropic.com"));
        assert_eq!(c["default_model"], "cn");
        assert_eq!(c["models"]["cn"]["base_url"], "http://127.0.0.1:9/p/cn");
        assert_eq!(c["models"]["cn"]["api_key"], "test-token");
        assert_eq!(c["models"]["claude"]["api"], "chat_completions");
        assert!(!s.allow_logins);
        assert_eq!(s.env.len(), 2, "both logins hidden");
        assert!(c["models"][RESERVED_CLAUDE]["base_url"].as_str().unwrap().starts_with("http://127.0.0.1:9/"));

        // Per-agent token, restricted provider set, explicit default model.
        let s = hal_setup(&ctx, Path::new("/d/hal"), &ConfigOpts {
            token: Some("AGENT".into()),
            only: Some(vec!["claude".into()]),
            default_ref: Some("claude/opus-x".into()),
            ..Default::default()
        });
        assert!(s.config["models"].get("cn").is_none());
        assert_eq!(s.config["default_model"], "claude");
        assert_eq!(s.config["models"]["claude"]["model"], "opus-x");
        assert_eq!(s.config["models"]["claude"]["api_key"], "AGENT");

        let empty = hal_setup(&Ctx::test(), Path::new("/d/hal"), &ConfigOpts::default());
        assert_eq!(empty.config["default_model"], "none");
    }

    #[test]
    fn subscriptions_go_through_gateway_and_hide_logins() {
        let ctx = Ctx::test();
        let mut s = p("gpt", "https://chatgpt.com/backend-api/codex");
        s.kind = Kind::ChatgptOauth;
        s.trusted = true;
        upsert(&ctx, &s).unwrap();
        let mut c = p("claude", CLAUDE_UPSTREAM);
        c.kind = Kind::ClaudeOauth;
        c.trusted = true;
        upsert(&ctx, &c).unwrap();
        let h = hal_setup(&ctx, Path::new("/d/hal"), &ConfigOpts::default());
        assert!(!h.allow_logins);
        assert_eq!(h.config["models"]["gpt"]["api"], "responses");
        assert_eq!(h.config["models"]["claude"]["api"], "chat_completions");
        assert_eq!(h.config["models"]["claude"]["base_url"], "http://127.0.0.1:9/p/claude");
        assert_eq!(h.config["models"]["claude"]["api_key"], "test-token");
        let text = h.config.to_string();
        assert!(!text.contains("chatgpt.com") && !text.contains("anthropic.com"));
        assert_eq!(h.env.len(), 2);
        let strict = hal_setup(&ctx, Path::new("/d/hal"), &ConfigOpts { no_subscriptions: true, ..Default::default() });
        assert!(strict.config["models"].get("gpt").is_none() && strict.config["models"].get("claude").is_none());
    }

    #[test]
    fn legacy_kinds_migrate() {
        let ctx = Ctx::test();
        ctx.db.exec("INSERT INTO providers(id,name,kind,base_url,trust,guard_mode,enabled,created_at) VALUES('c','C','claude_code','','trusted','redact',1,1)", []).unwrap();
        migrate_legacy(&ctx);
        assert_eq!(get(&ctx, "c").unwrap().kind, Kind::ClaudeOauth);
    }

    #[test]
    fn default_model_skips_minor_variants() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_default(&v(&["codex-auto-review", "gpt-6-mini", "gpt-6"])).unwrap(), "gpt-6");
        assert_eq!(pick_default(&v(&["only-mini"])).unwrap(), "only-mini");
    }

    #[test]
    fn alias_dedupe() {
        let d = |id: &str| Discovered { id: id.into(), display: None, context_window: None, price_in: None, price_out: None, efforts: None };
        let mut v = vec![d("opus"), d("claude-opus-5-5"), d("gpt-6"), d("gpt-6")];
        dedupe_aliases(&mut v);
        assert_eq!(v.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["claude-opus-5-5", "gpt-6"]);
    }

    #[test]
    fn dynamic_model_efforts_match_provider_capabilities() {
        // Explicit /models supported_reasoning_levels preserves provider-defined levels and descriptions
        let codex = json!({"data":[
            {"id":"model-a","supported_reasoning_levels":[
                {"effort":"minimal","description":"Fastest"},
                {"effort":"deep","description":"Deep reasoning"},
                {"effort":"xhigh","description":"Maximum depth"}
            ]},
            {"id":"model-b","supported_reasoning_levels":[]},
            {"id":"model-c","supported_parameters":["temperature","max_tokens"]}
        ]});
        let parsed = parse_models_ordered(&codex);
        assert_eq!(
            model_efforts(Kind::ChatgptOauth, "model-a", parsed[0].efforts.as_deref()),
            ["default", "minimal", "deep", "xhigh"]
        );
        let details = model_effort_details(Kind::ChatgptOauth, "model-a", parsed[0].efforts.as_deref());
        assert_eq!(details[2].hint.as_deref(), Some("Deep reasoning"));
        assert!(model_efforts(Kind::ChatgptOauth, "model-b", parsed[1].efforts.as_deref()).is_empty());
        assert!(model_efforts(Kind::OpenaiChat, "model-c", parsed[2].efforts.as_deref()).is_empty());

        // Runtime learning updates stored model capabilities dynamically
        let ctx = Ctx::test();
        upsert(&ctx, &p("cn", "https://a/v1")).unwrap();
        record_learned_efforts(&ctx, "cn", "m", vec!["low".into(), "high".into()]);
        assert_eq!(validate_effort_for_ref(&ctx, Some("cn/m"), "high").unwrap(), "high");
        assert!(validate_effort_for_ref(&ctx, Some("cn/m"), "xhigh").is_err());
        record_learned_efforts(&ctx, "cn", "m", vec![]);
        assert!(validate_effort_for_ref(&ctx, Some("cn/m"), "low").is_err());
    }

    #[test]
    fn parses_model_lists() {
        let openai = json!({"object":"list","data":[{"id":"b"},{"id":"a"},{"id":"a"},{"id":"bad$"}]});
        let ids: Vec<_> = parse_models(&openai).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["a", "b"]);
        let or = json!({"data":[{"id":"x/y","name":"Y","context_length":200000,"pricing":{"prompt":"0.000003","completion":"0.000015"}}]});
        let m = &parse_models(&or)[0];
        assert_eq!((m.context_window, m.display.as_deref()), (Some(200000), Some("Y")));
        assert!((m.price_in.unwrap() - 3.0).abs() < 1e-9 && (m.price_out.unwrap() - 15.0).abs() < 1e-9);
        let anthropic = json!({"data":[{"id":"claude-x","display_name":"Claude X","type":"model"}]});
        assert_eq!(parse_models(&anthropic)[0].display.as_deref(), Some("Claude X"));
    }

    #[test]
    fn resolve_refs() {
        let ctx = Ctx::test();
        upsert(&ctx, &p("cn", "https://a/v1")).unwrap();
        assert_eq!(resolve(&ctx, Some("cn/glm/4")).unwrap().1, "glm/4");
        assert_eq!(resolve(&ctx, Some("cn")).unwrap().1, "m");
        assert_eq!(resolve(&ctx, None).unwrap().0.id, "cn");
        assert!(resolve(&ctx, Some("nope/x")).is_err());
    }
}
