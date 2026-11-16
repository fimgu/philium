//! Provider settings. Metadata lives in `providers.json` in the app data dir;
//! API keys live only in the OS keychain (macOS Keychain, Windows Credential
//! Manager, Secret Service on Linux) and are read by the gateway, never by HAL.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::guard::GuardMode;

const KEYCHAIN_SERVICE: &str = "dev.gunnarsson.filium";
pub const RESERVED_CLAUDE: &str = "claude-code";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// OpenAI-compatible `/chat/completions` (most third-party endpoints).
    OpenaiChat,
    /// OpenAI `/responses`.
    OpenaiResponses,
    /// Anthropic Messages `/messages`.
    Anthropic,
}

impl Kind {
    pub fn hal_api(self) -> &'static str {
        match self {
            Kind::OpenaiChat => "chat_completions",
            Kind::OpenaiResponses => "responses",
            Kind::Anthropic => "anthropic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Stable slug; also the HAL model name and the gateway route segment.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Real upstream URL, e.g. `https://api.example.cn/v1`.
    pub base_url: String,
    /// Upstream model id.
    pub model: String,
    #[serde(default = "default_context")]
    pub context_window: usize,
    #[serde(default)]
    pub guard: GuardMode,
}

fn default_context() -> usize {
    128_000
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub default: Option<String>,
}

pub fn validate(p: &Provider) -> Result<(), String> {
    let id_ok = !p.id.is_empty()
        && p.id.len() <= 40
        && p.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !p.id.starts_with('-');
    if !id_ok || matches!(p.id.as_str(), RESERVED_CLAUDE | "none" | "default") {
        return Err("ID must be lowercase letters, digits and dashes".into());
    }
    let url = reqwest::Url::parse(&p.base_url).map_err(|_| "Base URL is not a valid URL")?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err("Base URL must be https (http only for localhost)".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err("Put credentials in the API key field, not the URL".into());
    }
    // `$` would be env-expanded by HAL's config loader.
    if p.model.trim().is_empty() || p.model.contains(['\n', '\r', '$', '"', '\\']) {
        return Err("Model is required".into());
    }
    if p.name.trim().is_empty() {
        return Err("Name is required".into());
    }
    Ok(())
}

impl Store {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("providers.json")
    }

    pub fn load(dir: &Path) -> Store {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join("providers.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, Self::path(dir)).map_err(|e| e.to_string())
    }

    pub fn upsert(&mut self, p: Provider) {
        match self.providers.iter_mut().find(|x| x.id == p.id) {
            Some(x) => *x = p,
            None => self.providers.push(p),
        }
        if self.default.as_ref().is_none_or(|d| !self.providers.iter().any(|p| &p.id == d)) {
            self.default = self.providers.first().map(|p| p.id.clone());
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.providers.retain(|p| p.id != id);
        if self.default.as_deref() == Some(id) {
            self.default = self.providers.first().map(|p| p.id.clone());
        }
    }
}

fn entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &format!("provider:{id}")).map_err(|e| e.to_string())
}

pub fn get_key(id: &str) -> Option<String> {
    entry(id).ok()?.get_password().ok()
}

pub fn set_key(id: &str, key: &str) -> Result<(), String> {
    entry(id)?.set_password(key).map_err(|e| format!("keychain: {e}"))
}

pub fn delete_key(id: &str) {
    if let Ok(e) = entry(id) {
        let _ = e.delete_credential();
    }
}

/// The HAL config Filium launches HAL with. Every model points at the local
/// gateway and carries only the per-launch gateway token, never a real key.
pub fn hal_config(store: &Store, gateway: &str, token: &str, data_dir: &Path) -> serde_json::Value {
    use serde_json::json;
    let mut models = serde_json::Map::new();
    for p in &store.providers {
        let mut m = json!({
            "provider": "openai-compatible",
            "api": p.kind.hal_api(),
            "base_url": format!("{gateway}/p/{}", p.id),
            "api_key": token,
            "model": p.model,
            "context_window": p.context_window,
        });
        if p.kind == Kind::Anthropic {
            m["provider"] = json!("anthropic");
            m["allow_anthropic_custom_endpoint"] = json!(true);
        }
        models.insert(p.id.clone(), m);
    }
    // HAL adds a `claude-code` model (local Claude CLI subscription, which
    // would bypass the gateway) unless that name is already configured.
    // Occupy it with a route the gateway does not serve: fails closed.
    models.entry(RESERVED_CLAUDE.to_string()).or_insert_with(|| {
        json!({
            "provider": "openai-compatible",
            "api": "chat_completions",
            "base_url": format!("{gateway}/p/{RESERVED_CLAUDE}"),
            "api_key": token,
            "model": "disabled-in-filium",
        })
    });
    if store.providers.is_empty() {
        // HAL needs one model; this one has no gateway route, so it fails
        // closed with "unknown provider" instead of auto-detecting logins.
        models.insert(
            "none".into(),
            json!({
                "provider": "openai-compatible",
                "api": "chat_completions",
                "base_url": format!("{gateway}/p/none"),
                "api_key": token,
                "model": "add-a-provider-in-filium",
            }),
        );
    }
    let default = store
        .default
        .clone()
        .filter(|d| models.contains_key(d))
        .or_else(|| store.providers.first().map(|p| p.id.clone()))
        .unwrap_or_else(|| "none".into());
    json!({
        "server": { "data_dir": data_dir },
        "default_model": default,
        "models": models,
    })
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
            model: "m".into(),
            context_window: 1000,
            guard: GuardMode::Redact,
        }
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
        ] {
            assert!(validate(&p(id, url)).is_err(), "{id} {url}");
        }
    }

    #[test]
    fn hal_config_never_contains_real_endpoints_or_keys() {
        let mut s = Store::default();
        s.upsert(p("cn", "https://api.example.cn/v1"));
        let mut a = p("claude", "https://api.anthropic.com/v1");
        a.kind = Kind::Anthropic;
        s.upsert(a);
        let c = hal_config(&s, "http://127.0.0.1:9", "TOKEN", Path::new("/d"));
        let text = c.to_string();
        assert!(!text.contains("example.cn") && !text.contains("anthropic.com"));
        assert_eq!(c["default_model"], "cn");
        assert_eq!(c["models"]["cn"]["base_url"], "http://127.0.0.1:9/p/cn");
        assert_eq!(c["models"]["claude"]["api"], "anthropic");
        assert_eq!(c["models"]["claude"]["allow_anthropic_custom_endpoint"], true);
        let empty = hal_config(&Store::default(), "http://127.0.0.1:9", "T", Path::new("/d"));
        assert_eq!(empty["default_model"], "none");
        for c in [&c, &empty] {
            assert!(c["models"][RESERVED_CLAUDE]["base_url"].as_str().unwrap().starts_with("http://127.0.0.1:9/"));
        }
        let mut bad = p(RESERVED_CLAUDE, "https://a/v1");
        assert!(validate(&bad).is_err());
        bad.id = "none".into();
        assert!(validate(&bad).is_err());
    }
}
