//! Custom external MCP servers the person adds in Settings.
//!
//! Definitions live in the `mcp_servers` kv row. Secret values (env vars and
//! HTTP headers marked secret) never touch the database: they live in the
//! secrets vault under `mcp.<server>.env.<KEY>` / `mcp.<server>.header.<KEY>`
//! and are only resolved when an agent's HAL config is built.
//!
//! Each server is enabled for all repos or a list of repo ids, and carries an
//! approval rule (`ask`, `allow`, `deny`) that becomes a HAL permission rule.
//! Strict agents never get custom servers: they run untrusted providers and a
//! server could leak its secrets or reach the host outside the sandbox.
use crate::ctx::Ctx;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

const KV: &str = "mcp_servers";
const RESERVED: &[&str] = &["philium"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Server {
    pub name: String,
    /// "stdio" or "http".
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub url: String,
    /// Plain (non-secret) env vars.
    pub env: BTreeMap<String, String>,
    /// Names of env vars whose values are in the vault.
    pub secret_env: Vec<String>,
    /// Plain headers.
    pub headers: BTreeMap<String, String>,
    /// Names of headers whose values are in the vault.
    pub secret_headers: Vec<String>,
    pub enabled: bool,
    pub all_repos: bool,
    pub repos: Vec<String>,
    /// "ask" | "allow" | "deny"
    pub approval: String,
    pub timeout_secs: u64,
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn secret_key(server: &str, kind: &str, k: &str) -> String {
    format!("mcp.{server}.{kind}.{k}")
}

pub fn load(ctx: &Ctx) -> Vec<Server> {
    ctx.db.kv_get(KV).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn store(ctx: &Ctx, list: &[Server]) -> Result<(), String> {
    ctx.db.kv_set(KV, &serde_json::to_string(list).map_err(|e| e.to_string())?)
}

pub fn validate(s: &Server) -> Result<(), String> {
    if !valid_name(&s.name) {
        return Err("Name must be 1-40 letters, digits, '-' or '_'".into());
    }
    if RESERVED.contains(&s.name.as_str()) {
        return Err(format!("'{}' is reserved", s.name));
    }
    match s.transport.as_str() {
        "stdio" if s.command.trim().is_empty() => return Err("stdio servers need a command".into()),
        "http" if !(s.url.starts_with("https://") || s.url.starts_with("http://")) => {
            return Err("http servers need an http(s) URL".into())
        }
        "stdio" | "http" => {}
        _ => return Err("transport must be stdio or http".into()),
    }
    if !matches!(s.approval.as_str(), "ask" | "allow" | "deny") {
        return Err("approval must be ask, allow or deny".into());
    }
    for k in s.env.keys().chain(&s.secret_env).chain(s.headers.keys()).chain(&s.secret_headers) {
        if !valid_key(k) {
            return Err(format!("bad variable/header name '{k}'"));
        }
    }
    Ok(())
}

fn applies(s: &Server, repo_id: Option<&str>) -> bool {
    s.enabled && (s.all_repos || repo_id.is_some_and(|r| s.repos.iter().any(|x| x == r)))
}

/// HAL server config for one entry, with secrets resolved.
fn hal_entry(s: &Server, resolve: &dyn Fn(&str) -> Option<String>) -> Value {
    let mut env = s.env.clone();
    for k in &s.secret_env {
        if let Some(v) = resolve(&secret_key(&s.name, "env", k)) {
            env.insert(k.clone(), v);
        }
    }
    let mut headers = s.headers.clone();
    for k in &s.secret_headers {
        if let Some(v) = resolve(&secret_key(&s.name, "header", k)) {
            headers.insert(k.clone(), v);
        }
    }
    let timeout = if s.timeout_secs == 0 { 300 } else { s.timeout_secs };
    if s.transport == "stdio" {
        json!({ "transport": "stdio", "command": s.command, "args": s.args, "env": env, "timeout_secs": timeout })
    } else {
        json!({ "transport": "streamable_http", "url": s.url, "headers": headers, "timeout_secs": timeout })
    }
}

/// Merge custom servers for this agent into `config["mcp"]["servers"]` and
/// their approval rules into `config["permissions"]["rules"]`.
pub fn merge(ctx: &Ctx, config: &mut Value, repo_id: Option<&str>, strict: bool) {
    merge_with(&load(ctx), config, repo_id, strict, &|k| crate::secrets::get(k));
}

fn merge_with(list: &[Server], config: &mut Value, repo_id: Option<&str>, strict: bool, resolve: &dyn Fn(&str) -> Option<String>) {
    if strict {
        return;
    }
    for s in list.iter().filter(|s| applies(s, repo_id) && validate(s).is_ok()) {
        if !config["mcp"]["servers"].is_object() {
            config["mcp"] = json!({ "servers": {} });
        }
        config["mcp"]["servers"][&s.name] = hal_entry(s, resolve);
        if let Some(rules) = config["permissions"]["rules"].as_array_mut() {
            rules.push(json!({ "action": "mcp", "resource": format!("{}/*", s.name), "effect": s.approval }));
        }
    }
}

/// Public view: secret names with a `set` flag, never values.
fn view(s: &Server) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    let has = |kind: &str, k: &str| crate::secrets::get(&secret_key(&s.name, kind, k)).is_some();
    v["secret_env_set"] = json!(s.secret_env.iter().map(|k| (k.clone(), has("env", k))).collect::<BTreeMap<_, _>>());
    v["secret_headers_set"] = json!(s.secret_headers.iter().map(|k| (k.clone(), has("header", k))).collect::<BTreeMap<_, _>>());
    v
}

fn drop_secrets(s: &Server, keep_env: &[String], keep_headers: &[String]) {
    for k in s.secret_env.iter().filter(|k| !keep_env.contains(k)) {
        crate::secrets::delete(&secret_key(&s.name, "env", k));
    }
    for k in s.secret_headers.iter().filter(|k| !keep_headers.contains(k)) {
        crate::secrets::delete(&secret_key(&s.name, "header", k));
    }
}

/// `mcp.servers_list`, `mcp.servers_save {server, secrets:{env:{K:v}, headers:{K:v}}}`,
/// `mcp.servers_delete {name}`. Empty secret values keep the stored one.
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "servers_list" => Ok(json!(load(ctx).iter().map(view).collect::<Vec<_>>())),
        "servers_save" => {
            let s: Server = serde_json::from_value(args["server"].clone()).map_err(|e| e.to_string())?;
            validate(&s)?;
            let original = args["original"].as_str().unwrap_or(&s.name).to_string();
            let mut list = load(ctx);
            if original != s.name && list.iter().any(|x| x.name == s.name) {
                return Err(format!("a server named '{}' already exists", s.name));
            }
            if let Some(old) = list.iter().find(|x| x.name == original).cloned() {
                if old.name != s.name {
                    // Renamed: move kept secrets to the new name.
                    for (kind, keys) in [("env", &s.secret_env), ("header", &s.secret_headers)] {
                        for k in keys {
                            if let Some(v) = crate::secrets::get(&secret_key(&old.name, kind, k)) {
                                crate::secrets::set(&secret_key(&s.name, kind, k), &v)?;
                            }
                        }
                    }
                    drop_secrets(&old, &[], &[]);
                } else {
                    drop_secrets(&old, &s.secret_env, &s.secret_headers);
                }
            }
            for (kind, field, keys) in [("env", "env", &s.secret_env), ("header", "headers", &s.secret_headers)] {
                if let Some(m) = args["secrets"][field].as_object() {
                    for (k, v) in m {
                        let v = v.as_str().unwrap_or("");
                        if !v.is_empty() && keys.contains(k) {
                            crate::secrets::set(&secret_key(&s.name, kind, k), v)?;
                        }
                    }
                }
            }
            list.retain(|x| x.name != original && x.name != s.name);
            list.push(s.clone());
            list.sort_by(|a, b| a.name.cmp(&b.name));
            store(ctx, &list)?;
            ctx.notify("settings");
            ctx.emit("mcp:changed", json!({}));
            Ok(view(&s))
        }
        "servers_delete" => {
            let name = crate::ctx::arg_str(&args, "name")?;
            let mut list = load(ctx);
            if let Some(old) = list.iter().find(|x| x.name == name) {
                drop_secrets(old, &[], &[]);
            }
            list.retain(|x| x.name != name);
            store(ctx, &list)?;
            ctx.notify("settings");
            ctx.emit("mcp:changed", json!({}));
            Ok(Value::Null)
        }
        _ => Err(format!("unknown mcp.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(name: &str) -> Server {
        Server {
            name: name.into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "x".into()],
            env: [("A".to_string(), "1".to_string())].into(),
            secret_env: vec!["TOKEN".into()],
            enabled: true,
            all_repos: false,
            repos: vec!["r1".into()],
            approval: "ask".into(),
            ..Default::default()
        }
    }

    fn base() -> Value {
        json!({ "mcp": { "servers": { "philium": {} } }, "permissions": { "rules": [] } })
    }

    #[test]
    fn merges_per_repo_with_secrets_and_rule() {
        let mut c = base();
        merge_with(&[srv("gh")], &mut c, Some("r1"), false, &|k| (k == "mcp.gh.env.TOKEN").then(|| "s3cret".into()));
        assert_eq!(c["mcp"]["servers"]["gh"]["env"]["TOKEN"], "s3cret");
        assert_eq!(c["mcp"]["servers"]["gh"]["env"]["A"], "1");
        assert!(c["mcp"]["servers"]["philium"].is_object());
        assert_eq!(c["permissions"]["rules"][0], json!({"action":"mcp","resource":"gh/*","effect":"ask"}));
    }

    #[test]
    fn skips_other_repo_disabled_and_strict() {
        let mut c = base();
        merge_with(&[srv("gh")], &mut c, Some("r2"), false, &|_| None);
        merge_with(&[srv("gh")], &mut c, Some("r1"), true, &|_| None);
        let mut off = srv("off");
        off.enabled = false;
        off.all_repos = true;
        merge_with(&[off], &mut c, Some("r1"), false, &|_| None);
        assert!(c["mcp"]["servers"].get("gh").is_none() && c["mcp"]["servers"].get("off").is_none());
    }

    #[test]
    fn http_entry_and_no_mcp_block() {
        let mut s = srv("web");
        s.transport = "http".into();
        s.url = "https://x.test/mcp".into();
        s.all_repos = true;
        s.secret_env.clear();
        s.secret_headers = vec!["Authorization".into()];
        s.approval = "allow".into();
        let mut c = json!({ "permissions": { "rules": [] } });
        merge_with(&[s], &mut c, None, false, &|_| Some("Bearer t".into()));
        assert_eq!(c["mcp"]["servers"]["web"]["transport"], "streamable_http");
        assert_eq!(c["mcp"]["servers"]["web"]["headers"]["Authorization"], "Bearer t");
    }

    #[test]
    fn validation() {
        assert!(validate(&srv("ok")).is_ok());
        assert!(validate(&srv("philium")).is_err());
        assert!(validate(&srv("bad name")).is_err());
        let mut s = srv("x");
        s.approval = "maybe".into();
        assert!(validate(&s).is_err());
    }
}
