//! Subscription sign-in owned by Philium (Claude and ChatGPT), modeled on
//! jcode and HAL. Tokens live only in the OS keychain entry `oauth:<id>`;
//! the gateway asks for a fresh access token per request and swaps it in, so
//! HAL, its config and its environment never see them.
//!
//! rpc (via `providers.*`): `oauth_start {kind}`, `oauth_paste {id, input}`,
//! `oauth_cancel {kind}`, `oauth_signout {id}`, `oauth_import_check`,
//! `oauth_import {id}` (Claude Code login, only after explicit consent).
//! Events: `oauth:done {id, ok, error}`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::ctx::{arg_str, Ctx};
use crate::providers::Kind;

pub const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const CLAUDE_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
pub const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CLAUDE_MANUAL_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
pub const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
pub const CLAUDE_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
pub const CLAUDE_REFRESH_SCOPES: &str = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
pub const CLAUDE_CLI_USER_AGENT: &str = "claude-cli/2.1.280 (external, sdk-cli)";
pub const CLAUDE_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,effort-2025-11-24";
const CLAUDE_BILLING: &str = "x-anthropic-billing-header: cc_version=2.1.280; cc_entrypoint=sdk-cli; cch=33f85;";
const CLAUDE_IDENTITY: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

pub const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const OPENAI_ISSUER: &str = "https://auth.openai.com";
pub const OPENAI_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
pub const CHATGPT_ORIGINATOR: &str = "codex_cli_rs";

/// Refresh this long before expiry.
const SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Creds {
    pub access: String,
    pub refresh: String,
    /// Unix ms.
    pub expires_at: i64,
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// ---------- storage ----------

pub trait Store: Send + Sync {
    fn load(&self, id: &str) -> Option<Creds>;
    fn save(&self, id: &str, c: &Creds) -> Result<(), String>;
    fn delete(&self, id: &str);
}

pub struct Keychain;

impl Store for Keychain {
    fn load(&self, id: &str) -> Option<Creds> {
        serde_json::from_str(&crate::secrets::get(&format!("oauth:{id}"))?).ok()
    }
    fn save(&self, id: &str, c: &Creds) -> Result<(), String> {
        crate::secrets::set(&format!("oauth:{id}"), &serde_json::to_string(c).unwrap_or_default())
    }
    fn delete(&self, id: &str) {
        crate::secrets::delete(&format!("oauth:{id}"))
    }
}

#[derive(Default)]
pub struct MemStore(pub Mutex<HashMap<String, Creds>>);
impl Store for MemStore {
    fn load(&self, id: &str) -> Option<Creds> {
        self.0.lk().get(id).cloned()
    }
    fn save(&self, id: &str, c: &Creds) -> Result<(), String> {
        self.0.lk().insert(id.into(), c.clone());
        Ok(())
    }
    fn delete(&self, id: &str) {
        self.0.lk().remove(id);
    }
}

// ---------- PKCE and URLs ----------

pub fn random_string(n: usize) -> String {
    const CS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut r = rand::thread_rng();
    (0..n).map(|_| CS[r.gen_range(0..CS.len())] as char).collect()
}

/// (verifier, challenge)
pub fn pkce() -> (String, String) {
    let v = random_string(64);
    let c = B64URL.encode(Sha256::digest(v.as_bytes()));
    (v, c)
}

pub fn claude_auth_url(redirect: &str, challenge: &str, state: &str) -> String {
    let mut u = reqwest::Url::parse(CLAUDE_AUTHORIZE_URL).unwrap();
    u.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLAUDE_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", CLAUDE_SCOPES)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    u.to_string()
}

/// Accepts a bare code, `code#state`, or a URL/query containing `code=`.
pub fn parse_code_input(input: &str) -> Result<(String, Option<String>), String> {
    let t = input.trim();
    let (raw, state) = if t.contains("code=") {
        let u = reqwest::Url::parse(t)
            .or_else(|_| reqwest::Url::parse(&format!("https://x.invalid/?{}", t.trim_start_matches('?'))))
            .map_err(|_| "could not read that as a callback URL")?;
        let q = |k: &str| u.query_pairs().find(|(a, _)| a == k).map(|(_, v)| v.to_string());
        (q("code").ok_or("no code in that URL")?, q("state"))
    } else {
        (t.to_string(), None)
    };
    let (code, state) = match raw.split_once('#') {
        Some((c, s)) => (c.to_string(), Some(s.to_string())),
        None => (raw, state),
    };
    if code.trim().is_empty() {
        return Err("no authorization code provided".into());
    }
    Ok((code, state.filter(|s| !s.is_empty())))
}

fn scopes_of(s: Option<&str>) -> Vec<String> {
    s.unwrap_or("").split_whitespace().map(str::to_string).collect()
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    id_token: Option<String>,
    scope: Option<String>,
}

async fn post_token(req: reqwest::RequestBuilder) -> Result<TokenResp, String> {
    let resp = req.send().await.map_err(|e| format!("token endpoint unreachable: {}", e.without_url()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let hint = if status.as_u16() == 403 && text.contains("Just a moment") {
            " (blocked by Cloudflare for this network; try another network and use the paste-code option)"
        } else {
            ""
        };
        return Err(format!("sign-in failed (HTTP {status}){hint}: {}", text.chars().take(200).collect::<String>()));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad token response: {e}"))
}

fn claude_creds(t: TokenResp, prev: Option<&Creds>) -> Result<Creds, String> {
    let access = t.access_token.filter(|s| !s.is_empty()).ok_or("no access token returned")?;
    let refresh = t.refresh_token.filter(|s| !s.is_empty()).or_else(|| prev.map(|p| p.refresh.clone())).ok_or("no refresh token returned")?;
    let scopes = if t.scope.is_some() { scopes_of(t.scope.as_deref()) } else { prev.map(|p| p.scopes.clone()).unwrap_or_default() };
    if !scopes.iter().any(|s| s == "user:inference") {
        return Err("this sign-in lacks the user:inference scope. Sign in again with a Claude subscription account".into());
    }
    Ok(Creds {
        access,
        refresh,
        expires_at: now_ms() + t.expires_in.unwrap_or(3600) * 1000,
        account_id: None,
        email: prev.and_then(|p| p.email.clone()),
        plan: prev.and_then(|p| p.plan.clone()),
        scopes,
    })
}

pub async fn claude_exchange(token_url: &str, verifier: &str, code: &str, state: &str, redirect: &str) -> Result<Creds, String> {
    let body = json!({ "grant_type": "authorization_code", "code": code, "redirect_uri": redirect,
        "client_id": CLAUDE_CLIENT_ID, "code_verifier": verifier, "state": state });
    claude_creds(post_token(client()?.post(token_url).json(&body)).await?, None)
}

pub async fn claude_refresh(token_url: &str, c: &Creds) -> Result<Creds, String> {
    let body = json!({ "grant_type": "refresh_token", "refresh_token": c.refresh,
        "client_id": CLAUDE_CLIENT_ID, "scope": CLAUDE_REFRESH_SCOPES });
    claude_creds(post_token(client()?.post(token_url).json(&body)).await?, Some(c))
}

fn jwt_claims(token: &str) -> Option<Value> {
    let p = token.split('.').nth(1)?;
    let bytes = B64URL.decode(p.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn chatgpt_creds(t: TokenResp, prev: Option<&Creds>) -> Result<Creds, String> {
    let access = t.access_token.filter(|s| !s.is_empty()).ok_or("no access token returned")?;
    let refresh = t.refresh_token.filter(|s| !s.is_empty()).or_else(|| prev.map(|p| p.refresh.clone())).ok_or("no refresh token returned")?;
    let auth = |tok: &str| jwt_claims(tok).map(|c| c["https://api.openai.com/auth"].clone());
    let from = |v: &Option<Value>, k: &str| v.as_ref().and_then(|a| a[k].as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let idc = t.id_token.as_deref().and_then(|t| auth(t));
    let acc = auth(&access);
    let account_id = from(&idc, "chatgpt_account_id").or_else(|| from(&acc, "chatgpt_account_id")).or_else(|| prev.and_then(|p| p.account_id.clone())).ok_or("no ChatGPT account in the sign-in response")?;
    let plan = from(&idc, "chatgpt_plan_type").or_else(|| from(&acc, "chatgpt_plan_type")).or_else(|| prev.and_then(|p| p.plan.clone()));
    let email = t.id_token.as_deref().and_then(jwt_claims).and_then(|c| c["email"].as_str().map(str::to_string)).or_else(|| prev.and_then(|p| p.email.clone()));
    let jwt_exp = jwt_claims(&access).and_then(|c| c["exp"].as_i64()).map(|s| s * 1000);
    let ttl = t.expires_in.map(|s| now_ms() + s * 1000);
    let expires_at = match (jwt_exp, ttl) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        _ => now_ms() + 300_000,
    };
    Ok(Creds { access, refresh, expires_at, account_id: Some(account_id), email, plan, scopes: vec![] })
}

pub async fn chatgpt_refresh(issuer: &str, c: &Creds) -> Result<Creds, String> {
    let body = json!({ "client_id": OPENAI_CLIENT_ID, "grant_type": "refresh_token",
        "refresh_token": c.refresh, "scope": "openid profile email" });
    chatgpt_creds(post_token(client()?.post(format!("{issuer}/oauth/token")).json(&body)).await?, Some(c))
}

// ---------- token access with lock ----------

fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// A usable credential, refreshed first when near expiry. Serialized by one
/// lock; the stored copy is re-read after acquiring it so a concurrent
/// refresh (rotating refresh tokens) is never repeated.
pub async fn fresh(store: &dyn Store, id: &str, kind: Kind, claude_token_url: &str, openai_issuer: &str) -> Result<Creds, String> {
    let need = || "Signed out. Sign in again on the Providers screen".to_string();
    let c = store.load(id).ok_or_else(need)?;
    if c.expires_at - now_ms() > SKEW_MS {
        return Ok(c);
    }
    let _g = refresh_lock().lock().await;
    let c = store.load(id).ok_or_else(need)?;
    if c.expires_at - now_ms() > SKEW_MS {
        return Ok(c);
    }
    let n = match kind {
        Kind::ClaudeOauth => claude_refresh(claude_token_url, &c).await,
        Kind::ChatgptOauth => chatgpt_refresh(openai_issuer, &c).await,
        _ => return Err("not a subscription provider".into()),
    }
    .map_err(|e| format!("{e}. Sign in again on the Providers screen"))?;
    store.save(id, &n)?;
    Ok(n)
}

pub async fn access(id: &str, kind: Kind) -> Result<Creds, String> {
    fresh(&Keychain, id, kind, CLAUDE_TOKEN_URL, OPENAI_ISSUER).await
}

// ---------- request shaping (used by the gateway) ----------

/// Merge the Claude Code beta set with whatever HAL asked for.
pub fn claude_beta(incoming: Option<&str>) -> String {
    let mut out: Vec<&str> = CLAUDE_BETA.split(',').collect();
    for b in incoming.unwrap_or("").split(',').map(str::trim).filter(|b| !b.is_empty()) {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out.join(",")
}

/// Claude subscription tokens require the Claude Code identity as the first
/// system blocks. The caller's own system prompt follows.
pub fn shape_claude_body(v: &mut Value) {
    let block = |t: &str| json!({ "type": "text", "text": t });
    let mut system = vec![block(CLAUDE_BILLING), block(CLAUDE_IDENTITY)];
    match v.get("system").cloned() {
        Some(Value::String(s)) if !s.is_empty() => system.push(block(&s)),
        Some(Value::Array(a)) => system.extend(a.into_iter().filter(|b| b["text"] != CLAUDE_IDENTITY && b["text"] != CLAUDE_BILLING)),
        _ => {}
    }
    v["system"] = json!(system);
}

/// The ChatGPT backend is stream-only, stateless and requires instructions.
pub fn shape_chatgpt_body(v: &mut Value) {
    v["store"] = json!(false);
    v["stream"] = json!(true);
    if v.get("instructions").and_then(Value::as_str).is_none_or(str::is_empty) {
        v["instructions"] = json!("You are a helpful assistant.");
    }
    if let Some(o) = v.as_object_mut() {
        o.remove("max_output_tokens");
        o.remove("temperature");
    }
}

// ---------- profile ----------

async fn claude_profile(access: &str, url: &str) -> (Option<String>, Option<String>) {
    let Ok(c) = client() else { return (None, None) };
    let Ok(r) = c.get(url).bearer_auth(access).header("user-agent", CLAUDE_CLI_USER_AGENT)
        .header("anthropic-beta", "oauth-2025-04-20,claude-code-20250219").send().await else { return (None, None) };
    let Ok(v) = r.json::<Value>().await else { return (None, None) };
    let email = v["account"]["email"].as_str().map(str::to_string);
    let plan = if v["account"]["has_claude_max"] == true { Some("Max".to_string()) } else if v["account"]["has_claude_pro"] == true { Some("Pro".to_string()) } else { None };
    (email, plan)
}

// ---------- login flows ----------

struct Pending {
    verifier: String,
    state: String,
    cancel: tokio::sync::watch::Sender<bool>,
}

fn pending() -> &'static Mutex<HashMap<String, Pending>> {
    static P: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn default_id(kind: Kind) -> &'static str {
    if kind == Kind::ClaudeOauth { "claude" } else { "gpt" }
}

/// Existing provider of this kind, else the default slug.
fn target_id(ctx: &Ctx, kind: Kind) -> String {
    crate::providers::all(ctx).into_iter().find(|p| p.kind == kind).map(|p| p.id).unwrap_or_else(|| default_id(kind).to_string())
}

fn open_browser(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

async fn finish(ctx: &Arc<Ctx>, id: &str, kind: Kind, mut creds: Creds) -> Result<(), String> {
    if kind == Kind::ClaudeOauth && creds.email.is_none() {
        let (e, p) = claude_profile(&creds.access, CLAUDE_PROFILE_URL).await;
        creds.email = e;
        creds.plan = p.or(creds.plan);
    }
    Keychain.save(id, &creds)?;
    crate::providers::ensure_oauth_provider(ctx, id, kind)?;
    crate::providers::sync_routes(ctx);
    ctx.log_event("provider_signin", None, &json!({ "id": id, "kind": kind }));
    ctx.notify("providers");
    ctx.emit("providers:changed", json!({ "id": id }));
    let (c, id2) = (ctx.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let _ = crate::providers::refresh_models(&c, &id2).await;
    });
    Ok(())
}

fn done(ctx: &Ctx, id: &str, r: Result<(), String>) {
    ctx.emit("oauth:done", json!({ "id": id, "ok": r.is_ok(), "error": r.err() }));
}

/// Read one HTTP request line from a callback connection, answer it, and
/// return the query string of `GET /callback?...`.
async fn accept_callback(l: &tokio::net::TcpListener) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let (mut s, _) = l.accept().await.ok()?;
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut buf)).await.ok()?.ok()?;
        let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
        let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let ok = target.starts_with("/callback?");
        let body = if ok { "<html><body style=\"font-family:sans-serif\"><h3>Signed in</h3>You can close this tab and return to Philium.</body></html>" } else { "Not found" };
        let _ = s.write_all(format!("HTTP/1.1 {} X\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", if ok { 200 } else { 404 }, body.len()).as_bytes()).await;
        if ok {
            return target.split_once('?').map(|x| x.1.to_string());
        }
    }
}

async fn start_claude(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let id = target_id(ctx, Kind::ClaudeOauth);
    let (verifier, challenge) = pkce();
    let state = random_string(43);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let local = format!("http://localhost:{port}/callback");
    let auto_url = claude_auth_url(&local, &challenge, &state);
    let manual_url = claude_auth_url(CLAUDE_MANUAL_REDIRECT, &challenge, &state);
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    if let Some(old) = pending().lk().insert(id.clone(), Pending { verifier: verifier.clone(), state: state.clone(), cancel: tx }) {
        let _ = old.cancel.send(true);
    }
    open_browser(&auto_url);
    let (c, id2) = (ctx.clone(), id.clone());
    tauri::async_runtime::spawn(async move {
        let q = tokio::select! {
            q = accept_callback(&listener) => q,
            _ = rx.changed() => None,
            _ = tokio::time::sleep(std::time::Duration::from_secs(600)) => None,
        };
        let Some(q) = q else { return };
        let r = async {
            let (code, st) = parse_code_input(&format!("?{q}"))?;
            if st.as_deref() != Some(state.as_str()) {
                return Err("sign-in state mismatch; start again".to_string());
            }
            let creds = claude_exchange(CLAUDE_TOKEN_URL, &verifier, &code, &state, &local).await?;
            finish(&c, &id2, Kind::ClaudeOauth, creds).await
        }
        .await;
        pending().lk().remove(&id2);
        done(&c, &id2, r);
    });
    Ok(json!({ "id": id, "kind": "claude_oauth", "auto_url": auto_url, "manual_url": manual_url }))
}

async fn paste_claude(ctx: &Arc<Ctx>, id: &str, input: &str) -> Result<Value, String> {
    let (verifier, state) = {
        let p = pending().lk();
        let p = p.get(id).ok_or("no sign-in in progress. Press Sign in with Claude first")?;
        (p.verifier.clone(), p.state.clone())
    };
    let (code, st) = parse_code_input(input)?;
    if st.as_deref().is_some_and(|s| s != state) {
        return Err("that code belongs to a different sign-in attempt; start again".into());
    }
    let creds = claude_exchange(CLAUDE_TOKEN_URL, &verifier, &code, &state, CLAUDE_MANUAL_REDIRECT).await?;
    if let Some(p) = pending().lk().remove(id) {
        let _ = p.cancel.send(true);
    }
    finish(ctx, id, Kind::ClaudeOauth, creds).await?;
    done(ctx, id, Ok(()));
    Ok(Value::Null)
}

fn valid_user_code(c: &str) -> bool {
    !c.is_empty() && c.len() <= 64 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

async fn start_chatgpt(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let id = target_id(ctx, Kind::ChatgptOauth);
    let cl = client()?;
    let v: Value = cl.post(format!("{OPENAI_ISSUER}/api/accounts/deviceauth/usercode")).json(&json!({ "client_id": OPENAI_CLIENT_ID }))
        .send().await.map_err(|e| e.without_url().to_string())?.json().await.map_err(|e| format!("bad device response: {e}"))?;
    let dev_id = v["device_auth_id"].as_str().unwrap_or("").to_string();
    let code = v["user_code"].as_str().or_else(|| v["usercode"].as_str()).unwrap_or("").to_string();
    if dev_id.is_empty() || !valid_user_code(&code) {
        return Err("OpenAI did not return a valid device code".into());
    }
    let interval = v["interval"].as_u64().or_else(|| v["interval"].as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(5).clamp(1, 60);
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    if let Some(old) = pending().lk().insert(id.clone(), Pending { verifier: String::new(), state: String::new(), cancel: tx }) {
        let _ = old.cancel.send(true);
    }
    open_browser(OPENAI_VERIFY_URL);
    let (c, id2, code2) = (ctx.clone(), id.clone(), code.clone());
    tauri::async_runtime::spawn(async move {
        let poll = async {
            loop {
                let r = cl.post(format!("{OPENAI_ISSUER}/api/accounts/deviceauth/token")).json(&json!({ "device_auth_id": dev_id, "user_code": code2 })).send().await.map_err(|e| e.without_url().to_string())?;
                if matches!(r.status().as_u16(), 403 | 404) {
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                    continue;
                }
                if !r.status().is_success() {
                    return Err(format!("device sign-in failed (HTTP {})", r.status()));
                }
                break r.json::<Value>().await.map_err(|e| e.to_string());
            }
        };
        let auth = tokio::select! {
            a = tokio::time::timeout(std::time::Duration::from_secs(900), poll) => a.unwrap_or_else(|_| Err("the code expired; start again".into())),
            _ = rx.changed() => return,
        };
        let r = async {
            let a = auth?;
            let form = [("grant_type", "authorization_code"), ("client_id", OPENAI_CLIENT_ID),
                ("code", a["authorization_code"].as_str().unwrap_or("")), ("code_verifier", a["code_verifier"].as_str().unwrap_or("")),
                ("redirect_uri", "https://auth.openai.com/deviceauth/callback")];
            let t = post_token(client()?.post(format!("{OPENAI_ISSUER}/oauth/token")).form(&form)).await?;
            finish(&c, &id2, Kind::ChatgptOauth, chatgpt_creds(t, None)?).await
        }
        .await;
        pending().lk().remove(&id2);
        done(&c, &id2, r);
    });
    Ok(json!({ "id": id, "kind": "chatgpt_oauth", "user_code": code, "verify_url": OPENAI_VERIFY_URL }))
}

// ---------- optional import of a Claude Code login ----------

pub fn parse_claude_code_creds(raw: &str) -> Result<Creds, String> {
    let v: Value = serde_json::from_str(raw).map_err(|_| "unreadable Claude Code credentials")?;
    let o = v.get("claudeAiOauth").unwrap_or(&v);
    let access = o["accessToken"].as_str().ok_or("no access token in the Claude Code login")?.to_string();
    let refresh = o["refreshToken"].as_str().ok_or("no refresh token in the Claude Code login")?.to_string();
    let scopes: Vec<String> = o["scopes"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if !scopes.iter().any(|s| s == "user:inference") {
        return Err("the Claude Code login lacks the user:inference scope. Use Sign in with Claude instead".into());
    }
    Ok(Creds { access, refresh, expires_at: o["expiresAt"].as_i64().unwrap_or(0), scopes, plan: o["subscriptionType"].as_str().map(str::to_string), ..Default::default() })
}

fn read_claude_code_login() -> Option<String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    if let Ok(s) = std::fs::read_to_string(home.join(".claude/.credentials.json")) {
        return Some(s);
    }
    #[cfg(target_os = "macos")]
    {
        let o = std::process::Command::new("security").args(["find-generic-password", "-s", "Claude Code-credentials", "-w"]).output().ok()?;
        if o.status.success() {
            return Some(String::from_utf8_lossy(&o.stdout).trim().to_string());
        }
    }
    None
}

/// Presence check that never reads the secret (no keychain prompt).
fn claude_code_login_exists() -> bool {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return false };
    if home.join(".claude/.credentials.json").exists() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("security").args(["find-generic-password", "-s", "Claude Code-credentials"]).output().is_ok_and(|o| o.status.success());
    }
    #[allow(unreachable_code)]
    false
}

// ---------- rpc ----------

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "oauth_start" => match Kind::parse(&arg_str(&args, "kind")?)? {
            Kind::ClaudeOauth => start_claude(ctx).await,
            Kind::ChatgptOauth => start_chatgpt(ctx).await,
            _ => Err("not a subscription kind".into()),
        },
        "oauth_paste" => paste_claude(ctx, &arg_str(&args, "id")?, &arg_str(&args, "input")?).await,
        "oauth_cancel" => {
            let id = target_id(ctx, Kind::parse(&arg_str(&args, "kind")?)?);
            if let Some(p) = pending().lk().remove(&id) {
                let _ = p.cancel.send(true);
            }
            Ok(Value::Null)
        }
        "oauth_signout" => {
            let id = arg_str(&args, "id")?;
            Keychain.delete(&id);
            crate::providers::sync_routes(ctx);
            ctx.log_event("provider_signout", None, &json!({ "id": id }));
            ctx.notify("providers");
            ctx.emit("providers:changed", json!({ "id": id }));
            Ok(Value::Null)
        }
        "oauth_import_check" => Ok(json!({ "available": claude_code_login_exists() })),
        "oauth_import" => {
            let id = target_id(ctx, Kind::ClaudeOauth);
            let creds = parse_claude_code_creds(&read_claude_code_login().ok_or("no Claude Code login found on this Mac")?)?;
            finish(ctx, &id, Kind::ClaudeOauth, creds).await?;
            Ok(json!({ "id": id }))
        }
        _ => Err(format!("unknown providers.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};

    async fn fake_token(counter: Arc<Mutex<Vec<Value>>>) -> String {
        let app = Router::new().route(
            "/token",
            post(move |Json(b): Json<Value>| {
                let c = counter.clone();
                async move {
                    c.lk().push(b);
                    Json(json!({ "access_token": "NEW-ACCESS", "refresh_token": "NEW-REFRESH", "expires_in": 3600, "scope": "user:profile user:inference" }))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{a}/token")
    }

    #[test]
    fn pkce_and_url() {
        let (v, c) = pkce();
        assert_eq!(v.len(), 64);
        assert_eq!(c, B64URL.encode(Sha256::digest(v.as_bytes())));
        assert!(!c.contains(['=', '+', '/']));
        let u = reqwest::Url::parse(&claude_auth_url("http://localhost:5/callback", &c, "st")).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], CLAUDE_CLIENT_ID);
        assert_eq!(q["code_challenge"], c);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], "http://localhost:5/callback");
        assert!(q["scope"].contains("user:inference"));
        assert_eq!(u.host_str(), Some("claude.com"));
    }

    #[test]
    fn code_inputs() {
        assert_eq!(parse_code_input("abc").unwrap(), ("abc".into(), None));
        assert_eq!(parse_code_input("abc#st").unwrap(), ("abc".into(), Some("st".into())));
        let r = parse_code_input("https://platform.claude.com/oauth/code/callback?code=abc&state=st").unwrap();
        assert_eq!(r, ("abc".into(), Some("st".into())));
        assert_eq!(parse_code_input("?code=z&state=q").unwrap().1.as_deref(), Some("q"));
        assert!(parse_code_input("  ").is_err());
    }

    #[tokio::test]
    async fn refresh_only_when_needed_and_keeps_account() {
        let seen = Arc::new(Mutex::new(vec![]));
        let url = fake_token(seen.clone()).await;
        let store = MemStore::default();
        let fresh_c = Creds { access: "OLD".into(), refresh: "R".into(), expires_at: now_ms() + 3_600_000, scopes: vec!["user:inference".into()], ..Default::default() };
        store.save("claude", &fresh_c).unwrap();
        assert_eq!(fresh(&store, "claude", Kind::ClaudeOauth, &url, "").await.unwrap().access, "OLD");
        assert!(seen.lk().is_empty());
        let stale = Creds { expires_at: now_ms() + 1000, email: Some("a@b.c".into()), ..fresh_c };
        store.save("claude", &stale).unwrap();
        let n = fresh(&store, "claude", Kind::ClaudeOauth, &url, "").await.unwrap();
        assert_eq!((n.access.as_str(), n.refresh.as_str(), n.email.as_deref()), ("NEW-ACCESS", "NEW-REFRESH", Some("a@b.c")));
        assert_eq!(store.load("claude").unwrap().access, "NEW-ACCESS", "persisted");
        let body = seen.lk()[0].clone();
        assert_eq!((body["grant_type"].as_str(), body["refresh_token"].as_str()), (Some("refresh_token"), Some("R")));
        // Concurrent callers refresh once.
        store.save("claude", &Creds { expires_at: 0, ..n }).unwrap();
        let (a, b) = tokio::join!(fresh(&store, "claude", Kind::ClaudeOauth, &url, ""), fresh(&store, "claude", Kind::ClaudeOauth, &url, ""));
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(seen.lk().len(), 2, "one extra refresh, not two");
        assert!(fresh(&store, "none", Kind::ClaudeOauth, &url, "").await.is_err());
    }

    #[test]
    fn chatgpt_claims_and_claude_import() {
        let payload = B64URL.encode(json!({ "exp": 4102444800i64, "email": "me@x.y",
            "https://api.openai.com/auth": { "chatgpt_account_id": "acc-1", "chatgpt_plan_type": "plus" } }).to_string());
        let jwt = format!("h.{payload}.s");
        let c = chatgpt_creds(TokenResp { access_token: Some(jwt.clone()), refresh_token: Some("r".into()), expires_in: Some(3600), id_token: Some(jwt), scope: None }, None).unwrap();
        assert_eq!((c.account_id.as_deref(), c.plan.as_deref(), c.email.as_deref()), (Some("acc-1"), Some("plus"), Some("me@x.y")));
        let raw = r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":5,"scopes":["user:inference"],"subscriptionType":"max"}}"#;
        assert_eq!(parse_claude_code_creds(raw).unwrap().plan.as_deref(), Some("max"));
        assert!(parse_claude_code_creds(r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","scopes":["user:profile"]}}"#).is_err());
    }

    #[test]
    fn shaping() {
        let mut v = json!({ "system": "be brief", "messages": [] });
        shape_claude_body(&mut v);
        assert_eq!(v["system"][1]["text"], CLAUDE_IDENTITY);
        assert_eq!(v["system"][2]["text"], "be brief");
        shape_claude_body(&mut v);
        assert_eq!(v["system"].as_array().unwrap().len(), 3, "idempotent");
        assert!(claude_beta(Some("foo-1,oauth-2025-04-20")).ends_with(",foo-1"));
        let mut g = json!({ "model": "m", "max_output_tokens": 5 });
        shape_chatgpt_body(&mut g);
        assert_eq!((g["store"].clone(), g["stream"].clone()), (json!(false), json!(true)));
        assert!(g.get("max_output_tokens").is_none() && g["instructions"].as_str().is_some());
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
