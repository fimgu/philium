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
pub const ANTIGRAVITY_UPSTREAM: &str = crate::gateway::gemini::ENDPOINT;
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
    /// Google Antigravity subscription (Gemini, and Claude through Google), signed in through Philium.
    AntigravityOauth,
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
            Kind::AntigravityOauth => "antigravity_oauth",
        }
    }
    pub fn is_subscription(self) -> bool {
        matches!(self, Kind::ChatgptOauth | Kind::ClaudeOauth | Kind::AntigravityOauth)
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
    if !p.trusted && p.guard == GuardMode::Off {
        return Err("Untrusted providers must have secret guard set to Redact or Block".into());
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

pub fn context_window(ctx: &Ctx, pid: &str, mid: &str) -> Option<i64> {
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
    /// USD per 1M cached-input tokens, when the source reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_cache_read: Option<f64>,
    /// Prices came from the runtime price catalog, not the provider's own `/models`.
    #[serde(skip)]
    pub from_catalog: bool,
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
    // 0. Anthropic `/v1/models`: `capabilities.effort` names each level with its own `supported`
    //    flag, and `capabilities.thinking.types` says which thinking styles the model takes.
    if let Some(effort) = m.pointer("/capabilities/effort").filter(|e| e.is_object()) {
        let on = |v: Option<&Value>| v.and_then(|x| x.get("supported")).and_then(Value::as_bool).unwrap_or(false);
        let opt = |value: &str| EffortOption { value: value.to_string(), label: format_effort_label(value), hint: None };
        if effort.get("supported").and_then(Value::as_bool).unwrap_or(false) {
            const KNOWN: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
            let mut out: Vec<EffortOption> = KNOWN.iter().filter(|l| on(effort.get(**l))).map(|l| opt(l)).collect();
            // A level this build has not heard of is still offered, after the known ones.
            for (name, v) in effort.as_object().into_iter().flatten() {
                if name != "supported" && !KNOWN.contains(&name.as_str()) && on(Some(v)) {
                    out.push(opt(name));
                }
            }
            return Some(out);
        }
        // No effort levels. A model that still takes a thinking budget keeps the budget steps.
        if on(m.pointer("/capabilities/thinking/types/enabled")) {
            return Some(["none", "low", "medium", "high", "max"].iter().map(|l| opt(l)).collect());
        }
        return Some(vec![]);
    }

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
    // Claude models differ: the 4.5 generation takes a thinking budget, later ones take named
    // levels, and which levels exist depends on the model.
    if matches!(kind, Kind::Anthropic | Kind::ClaudeOauth) && _model_id.to_ascii_lowercase().contains("claude") {
        let mut out = vec![EffortOption { value: "default".into(), label: "Auto".into(), hint: Some("Use the provider's default for this model".into()) }];
        out.extend(crate::gateway::translate::claude_effort_levels(_model_id).iter().map(|l| EffortOption {
            value: (*l).into(),
            label: format_effort_label(l),
            hint: None,
        }));
        return out;
    }
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
        // Antigravity takes no effort setting; the model id picks the thinking level.
        Kind::AntigravityOauth => &[("default", "Auto")],
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
        // A Claude model's levels are known even before the provider lists them. A typed token
        // budget ("16k") is always allowed; the gateway turns it into the nearest level.
        let typed_budget = req.strip_suffix('k').unwrap_or(&req).trim().parse::<u64>().is_ok();
        if !disc.contains_key(&mid) && !typed_budget && matches!(p.kind, Kind::Anthropic | Kind::ClaudeOauth) && mid.to_ascii_lowercase().contains("claude") {
            let allowed = model_efforts(p.kind, &mid, None);
            if !allowed.iter().any(|l| l.eq_ignore_ascii_case(&req)) {
                return Err(format!("`{}` offers effort [{}] (got `{requested}`)", mid, allowed.join(", ")));
            }
        }
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
                price_cache_read: num(m.pointer("/pricing/input_cache_read")).map(|x| x * 1e6),
                from_catalog: false,
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

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default()
    })
}

async fn discover(ctx: &Arc<Ctx>, p: &Provider) -> Result<Vec<Discovered>, String> {
    let client = http_client();
    let base = p.base_url.trim_end_matches('/');
    if p.kind == Kind::AntigravityOauth {
        return discover_antigravity(ctx, client, p, base).await;
    }
    let mut url = format!("{base}/models");
    let mut req;
    if p.kind.is_subscription() {
        let c = crate::oauth::access(&p.id, p.kind).await?;
        if p.kind == Kind::ClaudeOauth {
            url.push_str("?limit=1000");
            req = client.get(&url).bearer_auth(&c.access).header("anthropic-version", crate::pins::ANTHROPIC_VERSION)
                .header("anthropic-beta", crate::oauth::claude_beta(None)).header("user-agent", crate::oauth::claude_cli_user_agent());
        } else {
            url.push_str(&format!("?client_version={}", crate::pins::CHATGPT_CLIENT_VERSION));
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
                Kind::Anthropic => req.header("x-api-key", &key).header("anthropic-version", crate::pins::ANTHROPIC_VERSION),
                _ => req.bearer_auth(&key),
            };
        }
    }
    let resp = req.send().await.map_err(|e| format!("{url}: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        use crate::gateway::stale;
        let stale_msg = match p.kind {
            Kind::ClaudeOauth => stale::rejected_beta(status.as_u16(), &text, &crate::oauth::claude_beta(None)).map(|f| stale::warning("Claude", "beta flag", &f)),
            Kind::ChatgptOauth if stale::outdated(status.as_u16(), &text) => Some(stale::warning("ChatGPT", "client version", crate::pins::CHATGPT_CLIENT_VERSION)),
            _ => None,
        };
        if let Some(w) = stale_msg {
            eprintln!("[providers] {w}");
            return Err(format!("{w}. HTTP {status} from {url}: {}", text.chars().take(300).collect::<String>()));
        }
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

/// Antigravity lists the account's models, with quota, from `fetchAvailableModels`.
async fn discover_antigravity(ctx: &Arc<Ctx>, client: &reqwest::Client, p: &Provider, base: &str) -> Result<Vec<Discovered>, String> {
    use crate::gateway::gemini;
    let c = crate::oauth::access(&p.id, p.kind).await?;
    let url = format!("{base}/v1internal:fetchAvailableModels");
    let body = c.account_id.as_ref().map(|proj| json!({ "project": proj })).unwrap_or_else(|| json!({}));
    let resp = client
        .post(&url)
        .bearer_auth(&c.access)
        .header("user-agent", gemini::user_agent())
        .header("x-goog-api-client", gemini::API_CLIENT)
        .header("client-metadata", gemini::CLIENT_METADATA)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{url}: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        if crate::gateway::stale::outdated(status.as_u16(), &text) {
            let w = crate::gateway::stale::warning("Antigravity", "client version", gemini::VERSION);
            eprintln!("[providers] {w}");
            return Err(format!("{w}. HTTP {status} from {url}: {}", text.chars().take(300).collect::<String>()));
        }
        let hint = if matches!(status.as_u16(), 401 | 403) { " (sign in again)" } else { "" };
        return Err(format!("HTTP {status} from {url}{hint}: {}", text.chars().take(300).collect::<String>()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("bad model list: {e}"))?;
    if let Some(d) = gemini::catalog_default(&v) {
        *crate::ctx::LockExt::lk(&ctx.gateway.antigravity_default) = Some(d);
    }
    let list: Vec<Discovered> = gemini::models(&v)
        .into_iter()
        .map(|(id, display, _)| {
            let l = id.to_ascii_lowercase();
            let context_window = Some(if l.starts_with("gemini") { 1_000_000 } else if l.contains("claude") { 200_000 } else { 131_072 });
            Discovered { id, display, context_window, price_in: None, price_out: None, price_cache_read: None, from_catalog: false, efforts: Some(vec![]) }
        })
        .collect();
    if list.is_empty() {
        return Err("Antigravity listed no models for this account".into());
    }
    Ok(list)
}

/// Live public price catalog (`pins::PRICE_CATALOG_URL`), cached for 6h in memory.
/// Returns None when unreachable: prices then stay unknown, never zero.
async fn price_catalog() -> Option<Arc<Vec<Discovered>>> {
    static CACHE: std::sync::Mutex<Option<(i64, Arc<Vec<Discovered>>)>> = std::sync::Mutex::new(None);
    if let Some((at, v)) = CACHE.lock().ok()?.as_ref() {
        if now() - at < 6 * 3600 * 1000 {
            return Some(v.clone());
        }
    }
    let resp = http_client().get(crate::pins::PRICE_CATALOG_URL).send().await.ok()?;
    let body: Value = resp.error_for_status().ok()?.json().await.ok()?;
    let v = Arc::new(parse_models(&body).into_iter().filter(|m| m.price_in.is_some()).collect::<Vec<_>>());
    if v.is_empty() {
        return None;
    }
    *CACHE.lock().ok()? = Some((now(), v.clone()));
    Some(v)
}

/// Fill unpriced models from the catalog by exact model id (catalog ids are
/// `vendor/model`; only the part after the slash must match exactly).
pub fn fill_from_catalog(found: &mut [Discovered], cat: &[Discovered]) {
    for m in found.iter_mut().filter(|m| m.price_in.is_none()) {
        let hit = cat.iter().find(|c| c.id == m.id || c.id.rsplit_once('/').is_some_and(|(_, tail)| tail == m.id));
        if let Some(c) = hit {
            m.price_in = c.price_in;
            m.price_out = c.price_out;
            m.price_cache_read = c.price_cache_read;
            m.from_catalog = true;
        }
    }
}

/// Refresh one provider's models into SQLite. User edits (enabled, prices,
/// context window) survive; vanished models are removed.
pub async fn refresh_models(ctx: &Arc<Ctx>, id: &str) -> Result<Vec<Value>, String> {
    let (list, _) = refresh_models_inner(ctx, id, true).await?;
    Ok(list)
}

async fn refresh_models_inner(ctx: &Arc<Ctx>, id: &str, emit: bool) -> Result<(Vec<Value>, bool), String> {
    let p = get(ctx, id).ok_or("unknown provider")?;
    let prev_err = ctx.db.kv_get(&format!("models_error.{id}"));
    let found = match discover(ctx, &p).await {
        Ok(f) => f,
        Err(e) => {
            let err_changed = prev_err.as_deref() != Some(e.as_str());
            ctx.db.kv_set(&format!("models_error.{id}"), &e)?;
            if emit || err_changed {
                ctx.notify("providers");
            }
            return Err(e);
        }
    };
    let mut found = found;
    if !p.kind.is_subscription() && found.iter().any(|m| m.price_in.is_none()) {
        if let Some(cat) = price_catalog().await {
            fill_from_catalog(&mut found, &cat);
        }
    }
    let prev_order = ctx.db.kv_get(&format!("models_order.{id}"));
    let prev_efforts = ctx.db.kv_get(&format!("models_efforts.{id}"));
    let t = now();
    let deleted = ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for m in &found {
            tx.execute(
                // Fresh runtime prices replace older ones; a manual edit is kept.
                "INSERT INTO models(provider_id,model_id,display,context_window,price_in,price_out,price_cache_read,price_source,priced_at,discovered_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?8,CASE WHEN ?5 IS NULL THEN NULL ELSE ?9 END,CASE WHEN ?5 IS NULL THEN NULL ELSE ?7 END,?7)
                 ON CONFLICT(provider_id,model_id) DO UPDATE SET discovered_at=?7,
                   display=COALESCE(display,?3), context_window=COALESCE(context_window,?4),
                   price_in=CASE WHEN price_source='manual' OR ?5 IS NULL THEN price_in ELSE ?5 END,
                   price_out=CASE WHEN price_source='manual' OR ?6 IS NULL THEN price_out ELSE ?6 END,
                   price_cache_read=CASE WHEN price_source='manual' OR ?5 IS NULL THEN price_cache_read ELSE ?8 END,
                   priced_at=CASE WHEN price_source='manual' OR ?5 IS NULL THEN priced_at ELSE ?7 END,
                   price_source=CASE WHEN price_source='manual' OR ?5 IS NULL THEN price_source ELSE ?9 END",
                rusqlite::params![id, m.id, m.display, m.context_window, m.price_in, m.price_out, t, m.price_cache_read, if m.from_catalog { "catalog" } else { "live" }],
            )?;
        }
        let del = tx.execute("DELETE FROM models WHERE provider_id=?1 AND discovered_at<?2", rusqlite::params![id, t])?;
        tx.execute("UPDATE providers SET models_cached_at=?2 WHERE id=?1", rusqlite::params![id, t])?;
        tx.commit()?;
        Ok(del)
    })?;
    let order: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
    let order_json = serde_json::to_string(&order).unwrap_or_default();
    ctx.db.kv_set(&format!("models_order.{id}"), &order_json)?;
    let mut efforts_map = discovered_efforts_map(ctx, id);
    for m in &found {
        if let Some(e) = &m.efforts {
            efforts_map.insert(m.id.clone(), e.clone());
        }
    }
    let efforts_json = if efforts_map.is_empty() {
        let _ = ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("models_efforts.{id}")]);
        None
    } else {
        let s = serde_json::to_string(&efforts_map).unwrap_or_default();
        ctx.db.kv_set(&format!("models_efforts.{id}"), &s)?;
        Some(s)
    };
    ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("models_error.{id}")])?;
    let changed = deleted > 0
        || prev_err.is_some()
        || prev_order.as_deref() != Some(order_json.as_str())
        || prev_efforts != efforts_json;
    if emit {
        ctx.notify("providers");
        ctx.emit("providers:changed", json!({ "id": id }));
    }
    Ok((models(ctx, Some(id))?, changed))
}

pub fn models(ctx: &Ctx, provider: Option<&str>) -> Result<Vec<Value>, String> {
    let sql = "SELECT m.*, p.name AS provider_name, p.trust, p.kind, p.provider_id || '/' || m.model_id AS ref
               FROM models m JOIN (SELECT id AS provider_id, name, trust, kind, enabled FROM providers) p USING(provider_id)
               WHERE p.enabled=1 AND (?1 IS NULL OR m.provider_id=?1) ORDER BY p.provider_id, m.model_id";
    let mut rows = ctx.db.query(sql, [provider])?;
    let mut disc_cache: std::collections::HashMap<String, std::collections::HashMap<String, Vec<EffortOption>>> =
        std::collections::HashMap::new();
    // Each provider's models in the provider's own order (newest first for most APIs), so a
    // picker can show the latest on top. A model the saved order does not know sorts last.
    let mut ranks: std::collections::HashMap<String, std::collections::HashMap<String, i64>> = std::collections::HashMap::new();
    for r in &mut rows {
        let pid = r["provider_id"].as_str().unwrap_or("").to_string();
        let mid = r["model_id"].as_str().unwrap_or("").to_string();
        let kind = r["kind"].as_str().and_then(|k| Kind::parse(k).ok()).unwrap_or(Kind::OpenaiChat);
        let disc = disc_cache
            .entry(pid.clone())
            .or_insert_with(|| discovered_efforts_map(ctx, &pid));
        let exp = disc.get(&mid).map(Vec::as_slice);
        let details = model_effort_details(kind, &mid, exp);
        let efforts: Vec<&str> = details.iter().map(|o| o.value.as_str()).collect();
        r["efforts"] = json!(efforts);
        r["effort_details"] = json!(details);
        let order = ranks.entry(pid.clone()).or_insert_with(|| {
            ctx.db
                .kv_get(&format!("models_order.{pid}"))
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(i, m)| (m, i as i64))
                .collect()
        });
        r["rank"] = json!(order.get(mid.as_str()).copied().unwrap_or(1_000_000));
    }
    rows.sort_by(|a, b| {
        let ka = (
            a["provider_id"].as_str().unwrap_or(""),
            a["rank"].as_i64().unwrap_or(1_000_000),
            a["model_id"].as_str().unwrap_or(""),
        );
        let kb = (
            b["provider_id"].as_str().unwrap_or(""),
            b["rank"].as_i64().unwrap_or(1_000_000),
            b["model_id"].as_str().unwrap_or(""),
        );
        ka.cmp(&kb)
    });
    Ok(rows)
}

// ---------- module ----------

/// Create or fix the provider row for a signed-in subscription.
/// The fixed upstream of a subscription kind.
pub fn upstream(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::ClaudeOauth => Some(CLAUDE_UPSTREAM),
        Kind::ChatgptOauth => Some(CHATGPT_UPSTREAM),
        Kind::AntigravityOauth => Some(ANTIGRAVITY_UPSTREAM),
        _ => None,
    }
}

pub fn ensure_oauth_provider(ctx: &Ctx, id: &str, kind: Kind) -> Result<(), String> {
    let existing = get(ctx, id);
    let label = match kind {
        Kind::ClaudeOauth => "Claude",
        Kind::AntigravityOauth => "Google (Antigravity)",
        _ => "ChatGPT",
    };
    let p = Provider {
        id: id.into(),
        name: existing.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| label.into()),
        kind,
        base_url: upstream(kind).unwrap_or(CHATGPT_UPSTREAM).into(),
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
    // Price provenance: 'live' (provider /models), 'catalog' (runtime catalog) or 'manual'.
    for ddl in ["price_cache_read REAL", "price_source TEXT", "priced_at INTEGER"] {
        let _ = ctx.db.exec(&format!("ALTER TABLE models ADD COLUMN {ddl}"), []);
    }
    migrate_legacy(ctx);
    sync_routes(ctx);
    // Refresh stale model lists in the background (6h TTL) concurrently.
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        let cutoff = now() - 6 * 3600 * 1000;
        let stale_ids: Vec<String> = c
            .db
            .query("SELECT id, models_cached_at FROM providers WHERE enabled=1 ORDER BY created_at", [])
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r["models_cached_at"].as_i64().is_none_or(|t| t <= cutoff))
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        if stale_ids.is_empty() {
            return;
        }
        let results = futures_util::future::join_all(
            stale_ids.iter().map(|id| {
                let c = c.clone();
                let id = id.clone();
                async move {
                    let changed = refresh_models_inner(&c, &id, false).await.map(|(_, ch)| ch).unwrap_or(false);
                    (id, changed)
                }
            }),
        )
        .await;
        let changed_ids: Vec<String> = results.into_iter().filter_map(|(id, ch)| ch.then_some(id)).collect();
        if !changed_ids.is_empty() {
            c.notify("providers");
            c.emit("providers:changed", json!({ "ids": changed_ids }));
        }
    });
    Ok(())
}

fn public_from(p: &Provider, count: i64, cached: Value, error: Option<String>) -> Value {
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
    v["models_error"] = json!(error);
    v["via_gateway"] = json!(true);
    v
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
    let error = ctx.db.kv_get(&format!("models_error.{}", p.id));
    public_from(p, count, cached, error)
}

fn list_public(ctx: &Ctx) -> Vec<Value> {
    let rows = ctx
        .db
        .query("SELECT * FROM providers ORDER BY created_at", [])
        .unwrap_or_default();
    if rows.is_empty() {
        return Vec::new();
    }
    let counts: std::collections::HashMap<String, i64> = ctx
        .db
        .query("SELECT provider_id, COUNT(*) AS n FROM models GROUP BY provider_id", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| Some((r["provider_id"].as_str()?.to_string(), r["n"].as_i64()?)))
        .collect();
    let errors: std::collections::HashMap<String, String> = ctx
        .db
        .query("SELECT k, v FROM kv WHERE k LIKE 'models_error.%'", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| {
            let k = r["k"].as_str()?.strip_prefix("models_error.")?.to_string();
            let v = r["v"].as_str()?.to_string();
            Some((k, v))
        })
        .collect();
    rows.iter()
        .filter_map(|r| {
            let p = from_row(r)?;
            let count = counts.get(&p.id).copied().unwrap_or(0);
            let cached = r.get("models_cached_at").cloned().unwrap_or(Value::Null);
            let error = errors.get(&p.id).cloned();
            Some(public_from(&p, count, cached, error))
        })
        .collect()
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "list" => Ok(json!(list_public(ctx))),
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
                base_url: if let Some(u) = upstream(kind) { u.into() } else { arg_str(&args, "base_url")?.trim().to_string() },
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
            let (c, id) = (ctx.clone(), p.id.clone());
            tauri::async_runtime::spawn(async move {
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
                    if col != "context_window" {
                        ctx.db.exec("UPDATE models SET price_source='manual', priced_at=?3 WHERE provider_id=?1 AND model_id=?2", rusqlite::params![pid, mid, now()])?;
                    }
                }
            }
            if let Some(e) = args["enabled"].as_bool() {
                ctx.db.exec("UPDATE models SET enabled=?3 WHERE provider_id=?1 AND model_id=?2", rusqlite::params![pid, mid, e as i64])?;
            }
            ctx.notify("providers");
            ctx.emit("providers:changed", json!({ "id": pid, "model": mid }));
            Ok(Value::Null)
        }
        "set_default" => {
            let r = arg_str(&args, "ref")?;
            let (p, m) = resolve(ctx, Some(&r))?;
            let default_ref = format!("{}/{m}", p.id);
            ctx.db.kv_set("setting.default_model", &default_ref)?;
            ctx.notify("providers");
            ctx.emit("providers:changed", json!({ "id": p.id, "default_model": default_ref }));
            Ok(json!(default_ref))
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

        let mut untrusted_off = p("cn-off", "https://api.example.cn/v1");
        untrusted_off.guard = GuardMode::Off;
        assert_eq!(
            validate(&untrusted_off).unwrap_err(),
            "Untrusted providers must have secret guard set to Redact or Block"
        );
        untrusted_off.trusted = true;
        assert!(validate(&untrusted_off).is_ok());
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
    fn catalog_fills_only_unpriced_exact_ids() {
        let cat = parse_models(&json!({"data": [
            {"id": "vendor/model-x", "pricing": {"prompt": "0.000002", "completion": "0.00001", "input_cache_read": "0.0000005"}},
            {"id": "vendor/model-xl", "pricing": {"prompt": "0.000009", "completion": "0.00009"}}]}));
        let d = |id: &str, p: Option<f64>| Discovered { id: id.into(), display: None, context_window: None, price_in: p, price_out: p, price_cache_read: None, from_catalog: false, efforts: None };
        let mut v = vec![d("model-x", None), d("model-xl", Some(1.0)), d("model", None)];
        fill_from_catalog(&mut v, &cat);
        assert!((v[0].price_in.unwrap() - 2.0).abs() < 1e-9 && (v[0].price_cache_read.unwrap() - 0.5).abs() < 1e-9 && v[0].from_catalog);
        assert_eq!((v[1].price_in, v[1].from_catalog), (Some(1.0), false));
        assert_eq!(v[2].price_in, None);
    }

    #[test]
    fn alias_dedupe() {
        let d = |id: &str| Discovered { id: id.into(), display: None, context_window: None, price_in: None, price_out: None, price_cache_read: None, from_catalog: false, efforts: None };
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

    /// Effort differs by model. The provider's own list wins; without it a Claude model still
    /// gets the levels of its generation, and a level the model lacks is refused before sending.
    #[test]
    fn claude_effort_levels_follow_the_model() {
        let listed = json!({"data":[
            {"id":"claude-opus-5-5","capabilities":{"thinking":{"supported":true,"types":{"enabled":{"supported":false},"mycli":{"supported":true}}},
                "effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}}}},
            {"id":"claude-sonnet-4-6","capabilities":{"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"xhigh":{"supported":false},"max":{"supported":true}}}},
            {"id":"claude-haiku-4-5","capabilities":{"thinking":{"supported":true,"types":{"enabled":{"supported":true},"mycli":{"supported":false}}},"effort":{"supported":false}}},
            {"id":"claude-plain","capabilities":{"thinking":{"supported":false},"effort":{"supported":false}}}
        ]});
        let parsed = parse_models_ordered(&listed);
        let of = |i: usize| model_efforts(Kind::ClaudeOauth, &parsed[i].id, parsed[i].efforts.as_deref());
        assert_eq!(of(0), ["default", "low", "medium", "high", "xhigh", "max"]);
        assert_eq!(of(1), ["default", "low", "medium", "high", "max"]);
        assert_eq!(of(2), ["default", "none", "low", "medium", "high", "max"]);
        assert!(of(3).is_empty());

        // Before the provider has listed anything, the model's generation decides.
        assert_eq!(model_efforts(Kind::ClaudeOauth, "claude-fable-5-2", None), ["default", "low", "medium", "high", "xhigh", "max"]);
        assert_eq!(model_efforts(Kind::Anthropic, "claude-opus-4-5-20251101", None), ["default", "low", "medium", "high"]);
        assert_eq!(model_efforts(Kind::Anthropic, "claude-haiku-4-5-20251001", None), ["default", "none", "low", "medium", "high", "max"]);

        let ctx = Ctx::test();
        let mut claude = p("claude", "https://api.anthropic.com/v1");
        claude.kind = Kind::ClaudeOauth;
        upsert(&ctx, &claude).unwrap();
        assert_eq!(validate_effort_for_ref(&ctx, Some("claude/claude-opus-5-5"), "xhigh").unwrap(), "xhigh");
        assert!(validate_effort_for_ref(&ctx, Some("claude/claude-opus-5-5"), "none").is_err());
        assert!(validate_effort_for_ref(&ctx, Some("claude/claude-sonnet-4-6"), "xhigh").is_err());
        assert_eq!(validate_effort_for_ref(&ctx, Some("claude/claude-opus-5-5"), "16k").unwrap(), "16k");
        assert_eq!(validate_effort_for_ref(&ctx, Some("claude/claude-opus-5-5"), "auto").unwrap(), "default");
    }

    /// A picker shows the latest models first, so the list keeps the provider's own order
    /// instead of sorting names; a model the saved order does not know comes last.
    #[test]
    fn models_come_in_the_providers_order() {
        let ctx = Ctx::test();
        upsert(&ctx, &p("cn", "https://a/v1")).unwrap();
        for id in ["alpha-1", "zeta-9", "mid-5", "unlisted"] {
            ctx.db.exec("INSERT INTO models(provider_id,model_id,discovered_at) VALUES('cn',?1,1)", [id]).unwrap();
        }
        ctx.db.kv_set("models_order.cn", r#"["zeta-9","mid-5","alpha-1"]"#).unwrap();
        let ids: Vec<String> = models(&ctx, Some("cn")).unwrap().iter().map(|m| m["model_id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["zeta-9", "mid-5", "alpha-1", "unlisted"]);
    }
}
