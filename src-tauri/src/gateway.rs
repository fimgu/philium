//! Local model gateway. HAL's only route to a model provider.
//!
//! HAL is configured with `base_url = http://127.0.0.1:<port>/p/<provider>`
//! and a per-launch gateway token instead of a real API key. Every request is
//! authenticated, scanned by the guard (redact/block), forwarded upstream with
//! the real key injected, streamed back unchanged, and recorded in the activity
//! log that the UI renders live.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

pub(crate) mod gemini;
pub(crate) mod progress;
pub(crate) mod quota;
pub(crate) mod stale;
pub(crate) mod translate;
mod usage;

use crate::guard::{self, GuardMode};
use crate::providers::{Kind, Provider};

const LOG_CAPACITY: usize = 500;
const BODY_CAP: usize = 512 * 1024;
const HEAD_CAP: usize = 64 * 1024;
const TAIL_CAP: usize = 64 * 1024;
/// No streamed bytes for this long is a real stall (normal turns stream continuously).
const STREAM_IDLE: std::time::Duration = std::time::Duration::from_secs(90);

type ByteStream = futures_util::stream::BoxStream<'static, Result<Bytes, String>>;

fn byte_stream(r: reqwest::Response) -> ByteStream {
    r.bytes_stream().map(|x| x.map_err(|e| e.to_string())).boxed()
}

fn err_stream(msg: String) -> ByteStream {
    futures_util::stream::iter([Err(msg)]).boxed()
}

/// End the stream with an error once no chunk arrives within `idle`.
fn idle_guard(s: ByteStream, idle: std::time::Duration) -> ByteStream {
    futures_util::stream::unfold(Some(s), move |st| async move {
        let mut s = st?;
        match tokio::time::timeout(idle, s.next()).await {
            Ok(Some(item)) => Some((item, Some(s))),
            Ok(None) => None,
            Err(_) => Some((Err(format!("upstream stalled: no stream bytes for {}s", idle.as_secs())), None)),
        }
    })
    .boxed()
}

/// A provider with its real key, as the gateway sees it.
#[derive(Clone)]
pub struct Route {
    pub provider: Provider,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    pub id: u64,
    pub started_ms: u64,
    pub provider: String,
    pub model: Option<String>,
    pub method: String,
    pub path: String,
    pub stream: bool,
    /// pending | done | blocked | error
    pub state: &'static str,
    /// pass | redacted | blocked | flagged (guard off but secrets seen)
    pub guard: &'static str,
    pub findings: BTreeMap<String, usize>,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub req_bytes: usize,
    pub resp_bytes: usize,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    /// Time to first response byte.
    pub ttft_ms: Option<u64>,
    /// Agent whose token made the request; None = desk chat.
    pub agent: Option<String>,
    pub error: Option<String>,
}

struct Record {
    summary: Activity,
    /// The request exactly as it left the machine (after redaction).
    request: String,
    response_head: String,
    response_tail: String,
}

type LearnedEffortsFn = dyn Fn(&str, &str, Vec<String>) + Send + Sync;

pub struct Gateway {
    pub token: String,
    client: reqwest::Client,
    routes: RwLock<HashMap<String, Route>>,
    /// Extra tokens (one per agent) -> agent name, for attribution and
    /// per-agent revocation. The desk token is `token`.
    agent_tokens: RwLock<HashMap<String, (String, Option<Vec<String>>)>>,
    /// Dynamic reasoning effort per agent (and desk chat under key `""`), applied at the gateway
    /// so custom provider effort values and token budgets work regardless of HAL's internal enum.
    efforts: RwLock<HashMap<String, String>>,
    learned_efforts_cb: RwLock<Option<Arc<LearnedEffortsFn>>>,
    log: Mutex<VecDeque<Record>>,
    /// Per-provider 1-slot semaphores for untrusted or concurrency-limited keys.
    slots: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    next_id: AtomicU64,
    pub events: broadcast::Sender<Activity>,
    /// Live per-request stream progress (thinking, tool-arg streaming, bytes), throttled.
    pub progress: broadcast::Sender<progress::StreamProgress>,
    log_tx: Option<std::sync::mpsc::SyncSender<Activity>>,
    /// Source of OAuth access tokens for subscription routes.
    pub tokens: TokenSource,
    /// Antigravity: the thought signature of each tool call, sent back on later
    /// turns, and schema keywords the backend refused by name.
    signatures: Mutex<HashMap<String, String>>,
    rejected: Mutex<std::collections::HashSet<String>>,
    /// Antigravity default model learned from this app's last catalog fetch.
    pub antigravity_default: Mutex<Option<String>>,
    /// Latest usage windows each provider reported in its own response headers.
    quotas: Mutex<HashMap<String, quota::Usage>>,
}

type TokenFn = dyn Fn(String, Kind) -> futures_util::future::BoxFuture<'static, Result<crate::oauth::Creds, String>> + Send + Sync;

/// Where the gateway gets a fresh subscription token. Production reads the
/// Keychain and refreshes; tests inject a fake.
pub struct TokenSource(RwLock<Arc<TokenFn>>);

impl TokenSource {
    fn new() -> Self {
        Self(RwLock::new(Arc::new(|id, kind| Box::pin(async move { crate::oauth::access(&id, kind).await }))))
    }
    #[cfg(test)]
    pub fn set(&self, f: Arc<TokenFn>) {
        *self.0.wr() = f;
    }
    async fn get(&self, id: &str, kind: Kind) -> Result<crate::oauth::Creds, String> {
        let f = self.0.rd().clone();
        f(id.to_string(), kind).await
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn cap(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} more bytes]", &s[..end], s.len() - end)
}

impl Gateway {
    pub fn new(token: String, log_file: Option<PathBuf>) -> Arc<Self> {
        let log_tx = log_file.map(|path| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Activity>(512);
            std::thread::Builder::new()
                .name("philium-gateway-log".into())
                .spawn(move || {
                    while let Ok(s) = rx.recv() {
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                            let _ = writeln!(f, "{}", serde_json::to_string(&s).unwrap_or_default());
                        }
                    }
                })
                .ok();
            tx
        });
        Arc::new(Self {
            token,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(20))
                // Per-read idle timeout: a stalled upstream stream ends with an
                // error instead of hanging the agent's turn forever.
                .read_timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            routes: RwLock::new(HashMap::new()),
            agent_tokens: RwLock::new(HashMap::new()),
            efforts: RwLock::new(HashMap::new()),
            learned_efforts_cb: RwLock::new(None),
            log: Mutex::new(VecDeque::new()),
            slots: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events: broadcast::channel(256).0,
            progress: broadcast::channel(256).0,
            log_tx,
            tokens: TokenSource::new(),
            signatures: Mutex::new(HashMap::new()),
            rejected: Mutex::new(Default::default()),
            antigravity_default: Mutex::new(None),
            quotas: Mutex::new(HashMap::new()),
        })
    }

    pub fn set_learned_efforts_cb(&self, cb: Arc<LearnedEffortsFn>) {
        *self.learned_efforts_cb.wr() = Some(cb);
    }

    /// Set the active reasoning effort for an agent (`Some(name)`) or desk chat (`None`).
    pub fn set_effort(&self, agent: Option<&str>, effort: Option<&str>) {
        let key = agent.unwrap_or("").to_string();
        let mut map = self.efforts.wr();
        match effort.map(str::trim).filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case("default") && !e.eq_ignore_ascii_case("auto")) {
            Some(e) => {
                map.insert(key, e.to_string());
            }
            None => {
                map.remove(&key);
            }
        }
    }

    pub fn get_effort(&self, agent: Option<&str>) -> Option<String> {
        let key = agent.unwrap_or("");
        self.efforts.rd().get(key).cloned()
    }

    pub fn set_routes(&self, routes: Vec<Route>) {
        let mut slots = self.slots.lk();
        slots.retain(|id, _| routes.iter().any(|r| &r.provider.id == id));
        *self.routes.wr() =
            routes.into_iter().map(|r| (r.provider.id.clone(), r)).collect();
    }

    fn slot(&self, id: &str, serialize: bool) -> Option<Arc<tokio::sync::Semaphore>> {
        let mut slots = self.slots.lk();
        if let Some(s) = slots.get(id) {
            return Some(s.clone());
        }
        if serialize {
            let s = Arc::new(tokio::sync::Semaphore::new(1));
            slots.insert(id.to_string(), s.clone());
            return Some(s);
        }
        None
    }

    /// Issue a gateway token for an agent. Optional provider allowlist.
    pub fn issue_token(&self, agent: &str, providers: Option<Vec<String>>) -> String {
        let t = crate::hal::random_token();
        let mut tokens = self.agent_tokens.wr();
        tokens.retain(|_, (a, _)| a != agent);
        tokens.insert(t.clone(), (agent.to_string(), providers));
        t
    }

    pub fn revoke_agent(&self, agent: &str) {
        self.agent_tokens.wr().retain(|_, (a, _)| a != agent);
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/p/{provider}/{*rest}", any(proxy))
            .with_state(self.clone())
    }

    /// Remember the usage windows a provider advertised on its latest response.
    pub fn note_quota(&self, provider: &str, h: &reqwest::header::HeaderMap) {
        if let Some(u) = quota::parse(h) {
            self.quotas.lk().insert(provider.to_string(), u);
        }
    }

    /// Live usage per provider, as last reported by each provider itself.
    pub fn quotas(&self) -> HashMap<String, quota::Usage> {
        self.quotas.lk().clone()
    }

    pub fn recent(&self) -> Vec<Activity> {
        self.log.lk().iter().map(|r| r.summary.clone()).collect()
    }

    /// Aggregate KV cache, token, latency, guard, and rate-limit telemetry per agent name.
    pub fn agent_stats(&self) -> HashMap<String, Value> {
        #[derive(Default)]
        struct Acc {
            requests: u64,
            pending_requests: u64,
            input_tokens: Option<u64>,
            output_tokens: Option<u64>,
            cache_requests: u64,
            cache_input_tokens: u64,
            cached_tokens: u64,
            last_input_tokens: Option<u64>,
            last_cached_tokens: Option<u64>,
            guard_alerts: u64,
            rate_limits: u64,
            errors: u64,
            ttft_sum: u64,
            ttft_count: u64,
        }
        let mut map: HashMap<String, Acc> = HashMap::new();
        for r in self.log.lk().iter() {
            let s = &r.summary;
            let Some(agent) = s.agent.as_deref() else { continue };
            let a = map.entry(agent.to_string()).or_default();
            a.requests += 1;
            if s.state == "pending" {
                a.pending_requests += 1;
            }
            if let Some(inp) = s.input_tokens {
                a.input_tokens = Some(a.input_tokens.unwrap_or(0).saturating_add(inp));
            }
            if let Some(out) = s.output_tokens {
                a.output_tokens = Some(a.output_tokens.unwrap_or(0).saturating_add(out));
            }
            // Compare only requests for which the provider reported both
            // counters. Missing cache counts are not implicit misses.
            if let (Some(inp), Some(cached)) = (s.input_tokens, s.cached_tokens) {
                if inp > 0 {
                    a.cache_requests += 1;
                    a.cache_input_tokens = a.cache_input_tokens.saturating_add(inp);
                    a.cached_tokens = a.cached_tokens.saturating_add(cached.min(inp));
                }
            }
            if s.state != "pending" {
                a.last_input_tokens = s.input_tokens;
                a.last_cached_tokens = s.cached_tokens;
            }
            if s.guard != "pass" {
                a.guard_alerts += 1;
            }
            if s.status == Some(429) {
                a.rate_limits += 1;
            }
            if matches!(s.state, "error" | "blocked") {
                a.errors += 1;
            }
            if let Some(t) = s.ttft_ms {
                a.ttft_sum += t;
                a.ttft_count += 1;
            }
        }
        map.into_iter()
            .map(|(k, a)| {
                let pct = |input: u64, cached: u64| {
                    (input > 0).then(|| ((u128::from(cached.min(input)) * 100) / u128::from(input)) as u64)
                };
                let hit_pct = pct(a.cache_input_tokens, a.cached_tokens);
                let last_hit_pct = a.last_input_tokens.zip(a.last_cached_tokens)
                    .and_then(|(inp, cached)| pct(inp, cached));
                let avg_ttft_ms = if a.ttft_count > 0 { Some(a.ttft_sum / a.ttft_count) } else { None };
                (
                    k,
                    json!({
                        "requests": a.requests,
                        "pending_requests": a.pending_requests,
                        "input_tokens": a.input_tokens,
                        "output_tokens": a.output_tokens,
                        "cache_requests": a.cache_requests,
                        "cache_input_tokens": a.cache_input_tokens,
                        "cached_tokens": hit_pct.map(|_| a.cached_tokens),
                        "cache_hit_pct": hit_pct,
                        "last_input_tokens": a.last_input_tokens,
                        "last_cached_tokens": a.last_cached_tokens,
                        "last_cache_hit_pct": last_hit_pct,
                        "guard_alerts": a.guard_alerts,
                        "rate_limits": a.rate_limits,
                        "errors": a.errors,
                        "avg_ttft_ms": avg_ttft_ms,
                    }),
                )
            })
            .collect()
    }

    pub fn detail(&self, id: u64) -> Option<Value> {
        let log = self.log.lk();
        let r = log.iter().find(|r| r.summary.id == id)?;
        Some(json!({
            "summary": r.summary,
            "request": r.request,
            "response_head": r.response_head,
            "response_tail": r.response_tail,
        }))
    }

    pub fn clear(&self) {
        self.log.lk().clear();
    }

    pub fn emit_progress(&self, p: progress::StreamProgress) {
        let _ = self.progress.send(p);
    }

    fn push(&self, rec: Record) {
        let s = rec.summary.clone();
        let mut log = self.log.lk();
        if log.len() >= LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(rec);
        drop(log);
        let _ = self.events.send(s);
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Record)) {
        let mut log = self.log.lk();
        let Some(rec) = log.iter_mut().find(|r| r.summary.id == id) else { return };
        f(rec);
        let s = rec.summary.clone();
        drop(log);
        if s.state != "pending" {
            self.persist(&s);
        }
        let _ = self.events.send(s);
    }

    /// Metadata only. Bodies stay in memory and are never written to disk.
    fn persist(&self, s: &Activity) {
        if let Some(tx) = &self.log_tx {
            let _ = tx.try_send(s.clone());
        }
    }

    /// `Some(None)` = desk token, `Some(Some(agent))` = agent token.
    fn who(&self, headers: &HeaderMap) -> Option<Option<(String, Option<Vec<String>>)>> {
        let presented = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))?;
        let eq = |t: &str| {
            presented.len() == t.len() && presented.bytes().zip(t.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        };
        if eq(&self.token) {
            return Some(None);
        }
        let agents = self.agent_tokens.rd();
        agents.iter().find(|(t, _)| eq(t)).map(|(_, a)| Some(a.clone()))
    }
}

fn error_response(status: StatusCode, kind: &str, message: String) -> Response {
    let body = json!({ "error": { "type": kind, "message": message } });
    (status, [("content-type", "application/json")], body.to_string()).into_response()
}

fn response_is_sse(content_type: Option<&HeaderValue>, fallback: bool) -> bool {
    content_type.and_then(|v| v.to_str().ok()).map_or(fallback, |v| {
        v.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
    })
}

/// Accumulates the streamed response; finalizes the record on drop, so a
/// client disconnect or upstream error still closes the log entry.
struct Finalizer {
    gw: Arc<Gateway>,
    id: u64,
    started: Instant,
    first: Option<u64>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
    usage: usage::Capture,
    error: Option<String>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Finalizer {
    fn feed(&mut self, chunk: &[u8]) {
        self.usage.feed(chunk);
        self.preview(chunk);
    }

    fn preview(&mut self, chunk: &[u8]) {
        if self.first.is_none() && !chunk.is_empty() {
            self.first = Some(self.started.elapsed().as_millis() as u64);
        }
        self.total += chunk.len();
        let room = HEAD_CAP.saturating_sub(self.head.len());
        self.head.extend_from_slice(&chunk[..room.min(chunk.len())]);
        let chunk = &chunk[chunk.len().saturating_sub(TAIL_CAP)..];
        let excess = (self.tail.len() + chunk.len()).saturating_sub(TAIL_CAP);
        self.tail.drain(..excess);
        self.tail.extend(chunk);
    }
}

impl Drop for Finalizer {
    fn drop(&mut self) {
        let head = String::from_utf8_lossy(&self.head).into_owned();
        let tail_bytes: Vec<u8> = self.tail.iter().copied().collect();
        let tail = String::from_utf8_lossy(&tail_bytes).into_owned();
        let usage::Usage { input, output, cached } = self.usage.finish();
        let total = self.total;
        let elapsed = self.started.elapsed().as_millis() as u64;
        let error = self.error.take();
        let ttft = self.first;
        self.gw.update(self.id, |r| {
            let s = &mut r.summary;
            s.ttft_ms = ttft;
            s.duration_ms = Some(elapsed);
            s.resp_bytes = total;
            s.input_tokens = input.or(s.input_tokens);
            s.output_tokens = output.or(s.output_tokens);
            s.cached_tokens = cached.or(s.cached_tokens);
            let ok = s.status.is_some_and(|c| c < 400);
            s.state = if error.is_none() && ok { "done" } else { "error" };
            if error.is_some() {
                s.error = error;
            }
            r.response_head = head.clone();
            r.response_tail = if total > HEAD_CAP { tail } else { String::new() };
        });
    }
}

fn is_concurrency_429(body: &str) -> bool {
    let low = body.to_ascii_lowercase();
    body.contains("同时处理")
        || low.contains("concurrent")
        || low.contains("in flight")
        || low.contains("in-flight")
        || low.contains("already processing")
        || low.contains("simultaneous")
        || low.contains("1 request")
}

const FORWARD_HEADERS: [&str; 6] =
    ["content-type", "accept", "anthropic-version", "anthropic-beta", "openai-beta", "user-agent"];

async fn proxy(
    State(gw): State<Arc<Gateway>>,
    Path((provider, rest)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(credential) = gw.who(&headers) else {
        return error_response(StatusCode::UNAUTHORIZED, "philium_auth", "invalid gateway token".into());
    };
    if let Some((a, Some(allowed))) = &credential {
            if !allowed.contains(&provider) {
                return error_response(StatusCode::FORBIDDEN, "philium_route", format!("agent {a} may not use provider {provider}"));
            }
    }
    let agent = credential.map(|(name, _)| name);
    let Some(route) = gw.routes.rd().get(&provider).cloned() else {
        return error_response(StatusCode::NOT_FOUND, "philium_route", format!("unknown provider {provider}"));
    };
    if rest.split('/').any(|s| s == ".." || s == ".") {
        return error_response(StatusCode::BAD_REQUEST, "philium_route", "invalid path".into());
    }
    let p = &route.provider;
    let id = gw.next_id.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();

    // Guard: scan the exact bytes about to leave.
    // Defense-in-depth (SPEC.md:546): untrusted providers can never disable secret redaction.
    let mode = if !p.trusted && p.guard == GuardMode::Off {
        GuardMode::Redact
    } else {
        p.guard
    };
    let mut findings = guard::Findings::new();
    let mut parsed: Option<Value> = None;
    let mut parse_err: Option<String> = None;
    let mut dirty = false;
    let mut redacted = false;
    let mut outgoing: Bytes = if body.is_empty() {
        body
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(mut v) => {
                guard::scan_json(&mut v, mode == GuardMode::Redact, &mut findings);
                if mode == GuardMode::Redact && !findings.is_empty() {
                    dirty = true;
                    redacted = true;
                }
                parsed = Some(v);
                body
            }
            Err(e) => {
                parse_err = Some(e.to_string());
                let mut text = String::from_utf8_lossy(&body).into_owned();
                guard::scan_text(&mut text, mode == GuardMode::Redact, &mut findings);
                if mode == GuardMode::Redact && !findings.is_empty() {
                    Bytes::from(text)
                } else {
                    body
                }
            }
        }
    };
    let model = parsed.as_ref().and_then(|v| v.get("model")).and_then(Value::as_str).map(str::to_string);
    let stream = parsed.as_ref().and_then(|v| v.get("stream")).and_then(Value::as_bool).unwrap_or(false);
    let guard_state = match (findings.is_empty(), mode) {
        (true, _) => "pass",
        (false, GuardMode::Block) => "blocked",
        (false, GuardMode::Redact) => "redacted",
        (false, GuardMode::Off) => "flagged",
    };
    let blocked = guard_state == "blocked";
    let model_name = model.clone();
    let agent_name = agent.clone();
    let summary = Activity {
        id,
        started_ms: now_ms(),
        provider: p.id.clone(),
        model,
        method: method.to_string(),
        path: format!("/{rest}"),
        stream,
        state: if blocked { "blocked" } else { "pending" },
        guard: guard_state,
        findings: findings.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        status: None,
        duration_ms: None,
        req_bytes: outgoing.len(),
        resp_bytes: 0,
        input_tokens: None,
        output_tokens: None,
        cached_tokens: None,
        ttft_ms: None,
        agent,
        error: None,
    };
    gw.push(Record {
        summary,
        request: if blocked {
            format!("[not sent: blocked by guard ({})]", guard::summarize(&findings))
        } else if redacted {
            String::new()
        } else {
            cap(&String::from_utf8_lossy(&outgoing), BODY_CAP)
        },
        response_head: String::new(),
        response_tail: String::new(),
    });
    if blocked {
        let msg = format!(
            "Philium guard blocked this request: it contains {}. Remove the secret or switch the provider's guard to redact.",
            guard::summarize(&findings)
        );
        gw.update(id, |r| {
            r.summary.status = Some(403);
            r.summary.duration_ms = Some(0);
            r.summary.error = Some(msg.clone());
        });
        return error_response(StatusCode::FORBIDDEN, "philium_guard", msg);
    }

    let mut rest = rest;
    if method == Method::POST && matches!(rest.as_str(), "chat/completions" | "responses") {
        if let Some(eff) = gw.get_effort(agent_name.as_deref()) {
            if let Some(v) = parsed.as_mut() {
                translate::inject_effort(v, Some(&eff), rest == "responses");
                dirty = true;
            }
        }
    }
    // HAL speaks chat.completions to Anthropic-family providers; convert.
    let anthropic = matches!(p.kind, Kind::Anthropic | Kind::ClaudeOauth);
    let translating = anthropic && method == Method::POST && rest == "chat/completions";
    if translating {
        match parsed.as_mut() {
            Some(v) => {
                let mut tr = translate::request(v, true);
                translate::inject_cache_breakpoints(&mut tr, true);
                *v = tr;
                dirty = true;
                rest = "messages".into();
            }
            None => {
                let e = parse_err.unwrap_or_else(|| "EOF while parsing a value at line 1 column 0".into());
                let msg = format!("invalid chat.completions body: {e}");
                gw.update(id, |r| {
                    r.summary.state = "error";
                    r.summary.status = Some(400);
                    r.summary.error = Some(msg.clone());
                });
                return error_response(StatusCode::BAD_REQUEST, "philium_request", msg);
            }
        }
    } else if method == Method::POST && rest == "chat/completions" && p.kind == Kind::OpenaiChat {
        if let Some(v) = parsed.as_mut() {
            if v["model"].as_str().is_some_and(|m| m.to_ascii_lowercase().contains("claude")) {
                translate::inject_cache_breakpoints(v, false);
                dirty = true;
            }
        }
    }
    // A4: native API-key /messages gets the same breakpoints as translated traffic.
    if needs_native_cache_marks(p.kind, &method, &rest, translating) {
        if let Some(v) = parsed.as_mut() {
            translate::inject_cache_breakpoints(v, true);
            dirty = true;
        }
    }
    let mut url = format!("{}/{}", p.base_url.trim_end_matches('/'), rest);
    if let Some(q) = query {
        url = format!("{url}?{q}");
    }
    let sub = p.kind.is_subscription();
    let mut oauth: Option<crate::oauth::Creds> = None;
    if sub {
        match gw.tokens.get(&p.id, p.kind).await {
            Ok(c) => oauth = Some(c),
            Err(e) => {
                gw.update(id, |r| {
                    r.summary.state = "error";
                    r.summary.status = Some(401);
                    r.summary.error = Some(e.clone());
                });
                return error_response(StatusCode::UNAUTHORIZED, "philium_signin", e);
            }
        }
        if p.kind == Kind::AntigravityOauth {
            let permit = if method == Method::POST {
                match gw.slot(&p.id, !p.trusted) {
                    Some(sem) => sem.acquire_owned().await.ok(),
                    None => None,
                }
            } else {
                None
            };
            let creds = oauth.take().unwrap_or_default();
            return antigravity(&gw, p, &rest, id, started, parsed, parse_err, stream, creds, permit).await;
        }
        // Shape after the guard so redaction still covers user content.
        if let Some(v) = parsed.as_mut() {
            if p.kind == Kind::ClaudeOauth {
                crate::oauth::shape_claude_body(v);
                if !translating {
                    translate::inject_cache_breakpoints(v, true);
                }
            } else {
                crate::oauth::shape_chatgpt_body(v);
            }
            dirty = true;
        }
    }
    if dirty {
        if let Some(v) = parsed.as_ref() {
            outgoing = Bytes::from(serde_json::to_vec(v).unwrap_or_default());
            if let Some(rec) = gw.log.lk().iter_mut().find(|r| r.summary.id == id) {
                rec.summary.req_bytes = outgoing.len();
                rec.request = cap(&String::from_utf8_lossy(&outgoing), BODY_CAP);
            }
        }
    }
    let mut permit = if method == Method::POST {
        match gw.slot(&p.id, !p.trusted) {
            Some(sem) => sem.acquire_owned().await.ok(),
            None => None,
        }
    } else {
        None
    };
    // Claude beta flags the backend refused by name on this request; dropped on the one retry.
    let mut dropped_betas: Vec<String> = vec![];
    let build_req = |body_bytes: &Bytes, dropped_betas: &[String]| {
        let mut req = gw.client.request(method.clone(), &url).body(body_bytes.clone());
        for h in FORWARD_HEADERS {
            if sub && matches!(h, "anthropic-beta" | "user-agent") {
                continue;
            }
            if let Some(v) = headers.get(h) {
                req = req.header(h, v);
            }
        }
        if let Some(c) = &oauth {
            req = req.bearer_auth(&c.access);
            if p.kind == Kind::ClaudeOauth {
                let beta = headers.get("anthropic-beta").and_then(|v| v.to_str().ok());
                req = req
                    .header("anthropic-beta", stale::without(&crate::oauth::claude_beta(beta), dropped_betas))
                    .header("user-agent", crate::oauth::claude_cli_user_agent());
                if headers.get("anthropic-version").is_none() {
                    req = req.header("anthropic-version", crate::pins::ANTHROPIC_VERSION);
                }
            } else {
                req = req.header("originator", crate::oauth::CHATGPT_ORIGINATOR);
                if let Some(a) = &c.account_id {
                    req = req.header("chatgpt-account-id", a);
                }
            }
        } else if let Some(key) = route.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = match p.kind {
                Kind::Anthropic => req.header("x-api-key", key),
                _ => req.bearer_auth(key),
            };
        }
        if anthropic && headers.get("anthropic-version").is_none() && p.kind == Kind::Anthropic {
            req = req.header("anthropic-version", crate::pins::ANTHROPIC_VERSION);
        }
        if translating {
            req = req.header("content-type", "application/json");
        }
        req
    };
    let mut buffered_err: Option<(reqwest::StatusCode, Option<HeaderValue>, Option<HeaderValue>, Bytes)> = None;
    let mut upstream_resp = None;
    let mut version_retried = false;
    for attempt in 0..4 {
        let resp = match build_req(&outgoing, &dropped_betas).send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("upstream unreachable: {e}");
                gw.update(id, |r| {
                    r.summary.state = "error";
                    r.summary.error = Some(msg.clone());
                    r.summary.duration_ms = Some(started.elapsed().as_millis() as u64);
                });
                return error_response(StatusCode::BAD_GATEWAY, "philium_upstream", msg);
            }
        };
        if matches!(resp.status().as_u16(), 400 | 422) && attempt < 3 {
            let status = resp.status();
            let ct = resp.headers().get("content-type").cloned();
            let ra = resp.headers().get("retry-after").cloned();
            let bytes = resp.bytes().await.unwrap_or_default();
            if p.kind == Kind::ClaudeOauth && !version_retried {
                if let Some(min) = crate::oauth::parse_version_too_old(&String::from_utf8_lossy(&bytes)) {
                    version_retried = true;
                    if crate::oauth::learn_claude_min_version(&min) {
                        if let Some(v) = parsed.as_mut() {
                            crate::oauth::shape_claude_body(v);
                            outgoing = Bytes::from(serde_json::to_vec(v).unwrap_or_default());
                        }
                        continue;
                    }
                }
            }
            if p.kind == Kind::ClaudeOauth && dropped_betas.is_empty() {
                let sent = crate::oauth::claude_beta(headers.get("anthropic-beta").and_then(|v| v.to_str().ok()));
                if let Some(flag) = stale::rejected_beta(status.as_u16(), &String::from_utf8_lossy(&bytes), &sent) {
                    eprintln!("[gateway] {}; retrying without it", stale::warning("Claude", "beta flag", &flag));
                    dropped_betas.push(flag);
                    continue;
                }
            }
            if let Some(learned) = translate::parse_effort_rejection(status.as_u16(), &String::from_utf8_lossy(&bytes)) {
                if let Some(v) = parsed.as_mut() {
                    // Only a request that carried an effort setting can teach us about effort. A
                    // refusal that merely mentions thinking must not erase a model's levels.
                    if translate::strip_effort(v) {
                        if let (Some(cb), Some(m)) = (gw.learned_efforts_cb.rd().clone(), model_name.as_deref()) {
                            cb(&p.id, m, learned);
                        }
                        gw.set_effort(agent_name.as_deref(), None);
                        outgoing = Bytes::from(serde_json::to_vec(v).unwrap_or_default());
                        continue;
                    }
                }
            }
            buffered_err = Some((status, ct, ra, bytes));
            break;
        }
        if resp.status() == StatusCode::TOO_MANY_REQUESTS && resp.headers().get("retry-after").is_none() {
            let status = resp.status();
            let ct = resp.headers().get("content-type").cloned();
            let ra = resp.headers().get("retry-after").cloned();
            let bytes = resp.bytes().await.unwrap_or_default();
            if attempt < 3 && is_concurrency_429(&String::from_utf8_lossy(&bytes)) {
                if permit.is_none() {
                    if let Some(sem) = gw.slot(&p.id, true) {
                        permit = sem.acquire_owned().await.ok();
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(150 * (attempt as u64 + 1))).await;
                continue;
            }
            buffered_err = Some((status, ct, ra, bytes));
            break;
        }
        upstream_resp = Some(resp);
        break;
    }
    if let Some((status, mut content_type, retry_after, mut bytes)) = buffered_err {
        let text = String::from_utf8_lossy(&bytes).to_string();
        let stale_msg = if p.kind == Kind::ClaudeOauth {
            let sent = crate::oauth::claude_beta(headers.get("anthropic-beta").and_then(|v| v.to_str().ok()));
            stale::rejected_beta(status.as_u16(), &text, &sent).map(|f| stale::warning("Claude", "beta flag", &f))
        } else if p.kind.is_subscription() && stale::outdated(status.as_u16(), &text) {
            Some(if p.kind == Kind::ClaudeOauth {
                stale::warning("Claude", "client version", &crate::oauth::claude_cli_user_agent())
            } else {
                stale::warning("ChatGPT", "client", crate::oauth::CHATGPT_ORIGINATOR)
            })
        } else {
            None
        };
        if let Some(w) = &stale_msg {
            eprintln!("[gateway] {w}");
            let detail = cap(&text, 300);
            bytes = Bytes::from(json!({"error": {"type": "philium_client_outdated", "message": format!("{w}. Provider said: {detail}")}}).to_string());
            content_type = Some(HeaderValue::from_static("application/json"));
        }
        gw.update(id, |r| {
            r.summary.status = Some(status.as_u16());
            if let Some(w) = &stale_msg {
                r.summary.error = Some(w.clone());
            }
        });
        let mut fin = Finalizer {
            gw: gw.clone(),
            id,
            started,
            first: None,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            usage: usage::Capture::new(response_is_sse(content_type.as_ref(), false)),
            error: None,
            _permit: permit,
        };
        fin.feed(&bytes);
        let mut out = Response::new(Body::from(bytes));
        *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
        let h = out.headers_mut();
        h.insert("content-type", content_type.unwrap_or(HeaderValue::from_static("application/json")));
        if let Some(v) = retry_after {
            h.insert("retry-after", v);
        }
        return out;
    }
    let resp = upstream_resp.expect("response or buffered error");
    let status = resp.status();
    gw.note_quota(&provider, resp.headers());
    gw.update(id, |r| r.summary.status = Some(status.as_u16()));
    let content_type = resp.headers().get("content-type").cloned();
    let trace_stream = stream || content_type.as_ref().and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let retry_after = resp.headers().get("retry-after").cloned();
    let mut upstream = byte_stream(resp);
    let mut pre: Option<Result<Bytes, String>> = None;
    if stream && status.is_success() {
        // Stall before the first byte: nothing reached the client yet, so resend once invisibly.
        match tokio::time::timeout(STREAM_IDLE, upstream.next()).await {
            Ok(x) => pre = x,
            Err(_) => {
                eprintln!("[gateway] request {id}: no stream bytes for {}s, retrying once", STREAM_IDLE.as_secs());
                gw.emit_progress(progress::StreamProgress { id, agent: agent_name.clone(), stalled: true, ..Default::default() });
                upstream = match build_req(&outgoing, &dropped_betas).send().await {
                    Ok(r) if r.status().is_success() => byte_stream(r),
                    Ok(r) => err_stream(format!("upstream stalled; retry returned {}", r.status())),
                    Err(e) => err_stream(format!("upstream stalled; retry failed: {e}")),
                };
            }
        }
    }
    let upstream = idle_guard(futures_util::stream::iter(pre).chain(upstream).boxed(), if stream { STREAM_IDLE } else { std::time::Duration::from_secs(3600) });
    let mut sniff = progress::Sniffer::new(id, agent_name.clone());
    let mut last_emit: Option<Instant> = None;
    let gw_p = gw.clone();
    let mut track = move |chunk: &[u8], err: bool| {
        if !trace_stream || !status.is_success() {
            return;
        }
        sniff.feed(chunk);
        if err {
            sniff.p.stalled = true;
        }
        if err || sniff.p.done || last_emit.is_none_or(|t| t.elapsed().as_millis() >= 250) {
            gw_p.emit_progress(sniff.p.clone());
            last_emit = Some(Instant::now());
        }
    };

    let mut fin = Finalizer {
        gw: gw.clone(),
        id,
        started,
        first: None,
        head: Vec::new(),
        tail: VecDeque::new(),
        total: 0,
        usage: usage::Capture::new(response_is_sse(content_type.as_ref(), stream || p.kind == Kind::ChatgptOauth)),
        error: None,
        _permit: permit,
    };
    let convert = translating && status.is_success();
    let mut out = if convert && stream {
        let tr = Arc::new(Mutex::new(translate::StreamTranslator::new()));
        let tr2 = tr.clone();
        let body = upstream.map(move |item| {
            match item {
                Ok(chunk) => {
                    fin.usage.feed(&chunk);
                    let s = tr.lk().feed(&chunk);
                    fin.preview(s.as_bytes());
                    track(s.as_bytes(), false);
                    Ok(Bytes::from(s))
                }
                Err(e) => {
                    fin.error = Some(format!("upstream stream error: {e}"));
                    track(b"", true);
                    Err(e)
                }
            }
        });
        let tail = futures_util::stream::once(async move { Ok::<_, String>(Bytes::from(tr2.lk().end())) });
        Response::new(Body::from_stream(body.chain(tail)))
    } else if convert {
        let mut bytes = Vec::new();
        let mut upstream = upstream;
        while let Some(Ok(c)) = upstream.next().await {
            bytes.extend_from_slice(&c);
        }
        let outb = match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => {
                fin.usage.observe(&v);
                translate::response(&v).to_string().into_bytes()
            }
            Err(_) => bytes.to_vec(),
        };
        fin.preview(&outb);
        Response::new(Body::from(outb))
    } else {
        let body = upstream.map(move |item| {
            match &item {
                Ok(chunk) => {
                    fin.feed(chunk);
                    track(chunk, false);
                }
                Err(e) => {
                    fin.error = Some(format!("upstream stream error: {e}"));
                    track(b"", true);
                }
            }
            item
        });
        Response::new(Body::from_stream(body))
    };
    *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let h = out.headers_mut();
    let ct = match (convert, stream) {
        (true, true) => HeaderValue::from_static("text/event-stream"),
        (true, false) => HeaderValue::from_static("application/json"),
        _ => content_type.unwrap_or(HeaderValue::from_static("application/json")),
    };
    h.insert("content-type", ct);
    if let Some(v) = retry_after {
        h.insert("retry-after", v);
    }
    out
}

/// Antigravity answers one whole turn per call. Translate, retry what its
/// refusals ask for, keep each call's signature, and answer as chat.completions.
#[allow(clippy::too_many_arguments)]
/// Claude OAuth marks after shaping; translated bodies were marked already.
pub(crate) fn needs_native_cache_marks(kind: Kind, method: &Method, rest: &str, translating: bool) -> bool {
    kind == Kind::Anthropic && *method == Method::POST && rest == "messages" && !translating
}

async fn antigravity(
    gw: &Arc<Gateway>,
    p: &crate::providers::Provider,
    rest: &str,
    id: u64,
    started: Instant,
    parsed: Option<Value>,
    parse_err: Option<String>,
    stream: bool,
    creds: crate::oauth::Creds,
    mut permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Response {
    let fail = |status: StatusCode, kind: &str, msg: String| {
        gw.update(id, |r| {
            r.summary.state = "error";
            r.summary.status = Some(status.as_u16());
            r.summary.error = Some(msg.clone());
            r.summary.duration_ms = Some(started.elapsed().as_millis() as u64);
        });
        error_response(status, kind, msg)
    };
    if rest != "chat/completions" {
        return fail(StatusCode::NOT_FOUND, "philium_route", "Antigravity answers chat requests only".into());
    }
    let Some(body) = parsed else {
        let e = parse_err.unwrap_or_else(|| "EOF while parsing a value at line 1 column 0".into());
        return fail(StatusCode::BAD_REQUEST, "philium_request", format!("invalid chat.completions body: {e}"));
    };
    let Some(project) = creds.account_id.clone().filter(|p| !p.is_empty()) else {
        return fail(StatusCode::UNAUTHORIZED, "philium_signin", "No Antigravity project for this Google account. Sign in again on the Providers screen".into());
    };
    let mut body = body;
    let model = gemini::resolve_model_with(body["model"].as_str().unwrap_or(""), gw.antigravity_default.lk().as_deref());
    body["model"] = Value::String(model.clone());
    let url = format!("{}/v1internal:generateContent", p.base_url.trim_end_matches('/'));
    let mut shaping = gemini::Shaping { rejected: gw.rejected.lk().clone(), ..Default::default() };
    let (mut schema_fixed, mut forced) = (false, 0);
    let mut attempt = 0;
    let answer = loop {
        attempt += 1;
        if attempt > 8 {
            return fail(StatusCode::BAD_GATEWAY, "philium_upstream", "Antigravity kept refusing this turn; start a new turn or choose another model".into());
        }
        let wrapped = {
            let sigs = gw.signatures.lk();
            gemini::request(&body, &project, &crate::oauth::random_string(32), &|k| sigs.get(k).cloned(), &shaping)
        };
        let payload = wrapped.to_string();
        if attempt == 1 {
            if let Some(rec) = gw.log.lk().iter_mut().find(|r| r.summary.id == id) {
                if rec.request.is_empty() {
                    rec.summary.req_bytes = payload.len();
                    rec.request = cap(&payload, BODY_CAP);
                }
            }
        }
        let sent = gw
            .client
            .post(&url)
            .bearer_auth(&creds.access)
            .header("content-type", "application/json")
            .header("user-agent", gemini::user_agent())
            .header("x-goog-api-client", gemini::API_CLIENT)
            .header("x-goog-request-params", format!("project={project}"))
            .header("x-goog-client-metadata", gemini::CLIENT_METADATA)
            .body(payload)
            .send()
            .await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) => return fail(StatusCode::BAD_GATEWAY, "philium_upstream", format!("upstream unreachable: {e}")),
        };
        let status = resp.status();
        gw.note_quota(&p.id, resp.headers());
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            match gemini::fix_for(status.as_u16(), &text) {
                Some(gemini::Fix::ToolsAsText) if !shaping.tools_as_text => {
                    shaping.tools_as_text = true;
                    continue;
                }
                Some(gemini::Fix::NoCombiners) if !shaping.no_combiners => {
                    shaping.no_combiners = true;
                    continue;
                }
                Some(gemini::Fix::DropKeywords(k)) if !schema_fixed => {
                    schema_fixed = true;
                    gw.rejected.lk().extend(k.iter().cloned());
                    shaping.rejected.extend(k);
                    continue;
                }
                _ => {}
            }
            gw.update(id, |r| r.summary.status = Some(status.as_u16()));
            let mut fin = Finalizer { gw: gw.clone(), id, started, first: None, head: Vec::new(), tail: VecDeque::new(), total: 0, usage: usage::Capture::new(false), error: None, _permit: permit.take() };
            fin.feed(text.as_bytes());
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| cap(&text, 500));
            let stale_msg = stale::outdated(status.as_u16(), &text).then(|| stale::warning("Antigravity", "client version", gemini::VERSION));
            if let Some(w) = &stale_msg {
                eprintln!("[gateway] {w}");
                gw.update(id, |r| r.summary.error = Some(w.clone()));
            }
            let hint = match &stale_msg {
                Some(w) => format!(" ({w})"),
                None if matches!(status.as_u16(), 401 | 403) => " (sign in again on the Providers screen)".into(),
                None => String::new(),
            };
            let mut out = Response::new(Body::from(json!({"error": {"type": "antigravity", "code": status.as_u16(), "message": format!("Antigravity: {msg}{hint}")}}).to_string()));
            *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            out.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
            if let Some(secs) = gemini::retry_after(&text) {
                if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                    out.headers_mut().insert("retry-after", v);
                }
            }
            return out;
        }
        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return fail(StatusCode::BAD_GATEWAY, "philium_upstream", format!("Antigravity sent an unreadable answer: {e}")),
        };
        let a = match gemini::response(&v, &model, &mut || format!("call_{}", crate::oauth::random_string(16))) {
            Ok(a) => a,
            Err(e) => return fail(StatusCode::BAD_GATEWAY, "philium_upstream", e),
        };
        // Gemini 3 sometimes writes a call as code or text: ask again for a native call.
        if (a.empty || a.pseudo_call) && forced < 2 {
            forced += 1;
            shaping.force_call = true;
            continue;
        }
        if a.empty {
            return fail(StatusCode::BAD_GATEWAY, "philium_upstream", format!("Antigravity returned no usable output: {}", a.finish_message));
        }
        break a;
    };
    {
        let mut sigs = gw.signatures.lk();
        if sigs.len() > 20_000 {
            sigs.clear();
        }
        sigs.extend(answer.signatures.iter().cloned());
    }
    gw.update(id, |r| r.summary.status = Some(200));
    let bytes = if stream { gemini::to_sse(&answer.completion) } else { answer.completion.to_string() };
    let mut fin = Finalizer { gw: gw.clone(), id, started, first: None, head: Vec::new(), tail: VecDeque::new(), total: 0, usage: usage::Capture::new(false), error: None, _permit: permit.take() };
    fin.usage.observe(&answer.completion);
    fin.preview(bytes.as_bytes());
    drop(fin);
    let mut out = Response::new(Body::from(bytes));
    out.headers_mut().insert("content-type", HeaderValue::from_static(if stream { "text/event-stream" } else { "application/json" }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    type Seen = Arc<StdMutex<Vec<(HeaderMap, String)>>>;

    #[test]
    fn native_api_key_messages_get_cache_marks_once() {
        assert!(needs_native_cache_marks(Kind::Anthropic, &Method::POST, "messages", false));
        assert!(!needs_native_cache_marks(Kind::Anthropic, &Method::POST, "messages", true));
        assert!(!needs_native_cache_marks(Kind::ClaudeOauth, &Method::POST, "messages", false));
        assert!(!needs_native_cache_marks(Kind::Anthropic, &Method::POST, "messages/count_tokens", false));
        assert!(!needs_native_cache_marks(Kind::Anthropic, &Method::GET, "messages", false));
    }

    /// Fake provider: records what it receives, replies with SSE + usage.
    async fn upstream() -> (String, Seen) {
        let seen: Seen = Default::default();
        let s = seen.clone();
        let app = Router::new().route(
            "/v1/{*rest}",
            any(move |headers: HeaderMap, body: String| {
                let s = s.clone();
                async move {
                    s.lk().push((headers, body));
                    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                               data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\n\
                               data: [DONE]\n\n";
                    ([("content-type", "text/event-stream")], sse)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("http://{addr}/v1"), seen)
    }

    async fn gateway(base: &str, kind: Kind, guard: GuardMode) -> (Arc<Gateway>, String) {
        let gw = Gateway::new("t".repeat(48), None);
        gw.set_routes(vec![Route {
            provider: Provider {
                id: "cn".into(),
                name: "CN".into(),
                kind,
                base_url: base.into(),
                trusted: false,
                guard,
                enabled: true,
                default_model: None,
            },
            api_key: Some("REAL-UPSTREAM-KEY".into()),
        }]);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let r = gw.router();
        tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
        (gw, format!("http://{addr}/p/cn"))
    }

    fn body() -> Value {
        json!({"model":"m","stream":true,"messages":[{"role":"user","content":"deploy with AKIAABCDEFGHIJKLMNOP"}]})
    }

    async fn settle(gw: &Gateway) -> Activity {
        for _ in 0..50 {
            let a = gw.recent().pop().unwrap();
            if a.state != "pending" {
                return a;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("activity never finished");
    }

    #[tokio::test]
    async fn responses_usage_survives_truncated_previews() {
        let final_event = format!(
            "event: response.completed\r\ndata: {}\r\n\r\n",
            json!({"type":"response.completed","response":{
                "output":[{"type":"message","text":"x".repeat(HEAD_CAP + TAIL_CAP)}],
                "usage":{"input_tokens":25,"output_tokens":31,"input_tokens_details":{"cached_tokens":0}}
            }})
        );
        let chunks: Vec<_> = final_event.as_bytes().chunks(8001)
            .map(|c| Ok::<_, std::convert::Infallible>(Bytes::copy_from_slice(c)))
            .collect();
        let app = Router::new().route("/v1/responses", any(move || {
            let chunks = chunks.clone();
            async move { ([("content-type", "text/event-stream; charset=utf-8")], Body::from_stream(futures_util::stream::iter(chunks))) }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (gw, url) = gateway(&base, Kind::ChatgptOauth, GuardMode::Off).await;
        gw.tokens.set(Arc::new(|_, _| {
            Box::pin(async { Ok(crate::oauth::Creds { access: "synthetic".into(), ..Default::default() }) })
        }));
        let response = reqwest::Client::new().post(format!("{url}/responses"))
            .bearer_auth(gw.issue_token("agent", None)).json(&json!({"model":"m","input":"hi"}))
            .send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), final_event);
        let a = settle(&gw).await;
        assert_eq!((a.input_tokens, a.output_tokens, a.cached_tokens), (Some(25), Some(31), Some(0)));
        let detail = gw.detail(a.id).unwrap();
        assert_eq!(detail["response_head"].as_str().unwrap().len(), HEAD_CAP);
        assert_eq!(detail["response_tail"].as_str().unwrap().len(), TAIL_CAP);
        assert_eq!(gw.agent_stats()["agent"]["cache_hit_pct"], 0);

        // Unknown data neither becomes a known 0% nor dilutes the aggregate.
        let mut next = gw.log.lk().back().unwrap().summary.clone();
        next.id += 1;
        next.state = "pending";
        next.agent = Some("other".into());
        next.input_tokens = None;
        next.output_tokens = None;
        next.cached_tokens = None;
        gw.push(Record { summary: next.clone(), request: String::new(), response_head: String::new(), response_tail: String::new() });
        let stats = gw.agent_stats();
        assert_eq!(stats["other"]["pending_requests"], 1);
        assert!(stats["other"]["cache_hit_pct"].is_null());
        gw.update(next.id, |r| {
            r.summary.state = "done";
            r.summary.input_tokens = Some(100);
        });
        let stats = gw.agent_stats();
        assert_eq!(stats["other"]["pending_requests"], 0);
        assert!(stats["other"]["cache_hit_pct"].is_null());
        gw.update(next.id, |r| r.summary.agent = Some("agent".into()));
        let stats = gw.agent_stats();
        assert_eq!(stats["agent"]["input_tokens"], 125);
        assert_eq!(stats["agent"]["cache_input_tokens"], 25);
        assert!(stats["agent"]["last_cache_hit_pct"].is_null());
    }

    /// Raw TCP upstream: answers every connection with `reply`, then either
    /// closes (reset mid-stream) or holds the socket open (`hold`).
    async fn raw_upstream(reply: &'static str, hold: bool) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 8192];
                    let _ = s.read(&mut buf).await;
                    let _ = s.write_all(reply.as_bytes()).await;
                    if hold {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                });
            }
        });
        format!("http://{addr}/v1")
    }

    fn post(url: &str) -> reqwest::RequestBuilder {
        reqwest::Client::new().post(format!("{url}/chat/completions")).bearer_auth("t".repeat(48)).json(&json!({"model":"m","stream":true,"messages":[]}))
    }

    #[tokio::test]
    async fn rate_limit_and_5xx_pass_through_with_retry_after() {
        for (code, reply) in [
            (429, "HTTP/1.1 429 Too Many Requests\r\nretry-after: 7\r\ncontent-type: application/json\r\ncontent-length: 27\r\n\r\n{\"error\":\"slow down pls\"}"),
            (503, "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\n\r\nbusy"),
        ] {
            let base = raw_upstream(reply, false).await;
            let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
            let r = post(&url).send().await.unwrap();
            assert_eq!(r.status().as_u16(), code);
            if code == 429 {
                assert_eq!(r.headers()["retry-after"], "7");
            }
            let _ = r.text().await;
            let a = settle(&gw).await;
            assert_eq!((a.state, a.status), ("error", Some(code)));
        }
    }

    #[tokio::test]
    async fn upstream_unreachable_is_502() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        drop(l);
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
        let r = post(&url).send().await.unwrap();
        assert_eq!(r.status(), 502);
        assert!(r.text().await.unwrap().contains("upstream unreachable"));
        assert_eq!(gw.recent().pop().unwrap().state, "error");
    }

    #[tokio::test]
    async fn connection_reset_mid_stream_is_an_error_not_a_hang() {
        // Promise 1000 bytes, send a few, close.
        let base = raw_upstream("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 1000\r\n\r\ndata: {\"choices\":[]}\n\n", false).await;
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
        let r = post(&url).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.text().await.is_err(), "client sees the broken stream");
        let a = settle(&gw).await;
        assert_eq!(a.state, "error");
        assert!(a.error.unwrap_or_default().contains("upstream stream error"));
    }

    #[tokio::test]
    async fn malformed_anthropic_sse_does_not_panic() {
        let base = raw_upstream("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {not json\n\ndata: {\"type\":\"content_block_delta\",\"index\":99}\n\ndata: \u{0}\u{ff}\n\nevent: x\n\n", false).await;
        let (gw, url) = gateway(&base, Kind::Anthropic, GuardMode::Redact).await;
        let r = post(&url).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let _ = r.text().await;
        assert_ne!(settle(&gw).await.state, "pending");
    }

    #[tokio::test]
    async fn client_disconnect_mid_stream_finalizes_activity() {
        let base = raw_upstream("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n10\r\ndata: {\"a\":1}\n\n\r\n", true).await;
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
        let mut r = post(&url).send().await.unwrap();
        assert!(r.chunk().await.unwrap().is_some());
        drop(r); // client goes away while upstream is still open
        let a = settle(&gw).await;
        assert_ne!(a.state, "pending");
        assert!(a.duration_ms.is_some());
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_token() {
        let (base, seen) = upstream().await;
        let (_gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
        let c = reqwest::Client::new();
        for auth in [None, Some("Bearer wrong")] {
            let mut r = c.post(format!("{url}/chat/completions")).json(&body());
            if let Some(a) = auth {
                r = r.header("authorization", a);
            }
            assert_eq!(r.send().await.unwrap().status(), 401);
        }
        assert!(seen.lk().is_empty());
    }

    #[tokio::test]
    async fn redacts_injects_real_key_streams_and_logs_usage() {
        let (base, seen) = upstream().await;
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Redact).await;
        let resp = reqwest::Client::new()
            .post(format!("{url}/chat/completions"))
            .bearer_auth("t".repeat(48))
            .json(&body())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let text = resp.text().await.unwrap();
        assert!(text.contains("[DONE]"), "stream passed through: {text}");
        let (headers, sent) = seen.lk()[0].clone();
        assert_eq!(headers["authorization"], "Bearer REAL-UPSTREAM-KEY");
        assert!(!sent.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(sent.contains("[REDACTED:aws_access_key]"));
        let a = settle(&gw).await;
        assert_eq!((a.state, a.guard, a.status), ("done", "redacted", Some(200)));
        assert_eq!((a.input_tokens, a.output_tokens), (Some(12), Some(3)));
        assert_eq!(a.findings["aws_access_key"], 1);
        let d = gw.detail(a.id).unwrap();
        assert!(!d.to_string().contains("AKIAABCDEFGHIJKLMNOP"), "log never holds the secret");
        assert!(!d.to_string().contains("REAL-UPSTREAM-KEY"));
    }

    #[tokio::test]
    async fn block_mode_sends_nothing() {
        let (base, seen) = upstream().await;
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Block).await;
        let resp = reqwest::Client::new()
            .post(format!("{url}/chat/completions"))
            .bearer_auth("t".repeat(48))
            .json(&body())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        assert!(resp.text().await.unwrap().contains("aws_access_key"));
        assert!(seen.lk().is_empty());
        let a = gw.recent().pop().unwrap();
        assert_eq!((a.state, a.guard), ("blocked", "blocked"));
        assert!(!gw.detail(a.id).unwrap().to_string().contains("AKIA"));
    }

    #[tokio::test]
    async fn anthropic_uses_x_api_key_and_hides_gateway_token() {
        let (base, seen) = upstream().await;
        let (_gw, url) = gateway(&base, Kind::Anthropic, GuardMode::Off).await;
        let resp = reqwest::Client::new()
            .post(format!("{url}/messages"))
            .header("x-api-key", "t".repeat(48))
            .header("anthropic-version", "2023-06-01")
            .json(&json!({"model":"m","messages":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let (headers, _) = seen.lk()[0].clone();
        assert_eq!(headers["x-api-key"], "REAL-UPSTREAM-KEY");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert!(headers.get("authorization").is_none());
    }

    /// Fake Cloud Code: a first turn calls a tool with a signature; a later turn
    /// needs that signature back (or the history as text); "RATE" hits a quota.
    async fn antigravity_upstream() -> (String, Seen) {
        let seen: Seen = Default::default();
        let s = seen.clone();
        let app = Router::new().route(
            "/{*rest}",
            any(move |headers: HeaderMap, body: String| {
                let s = s.clone();
                async move {
                    s.lk().push((headers, body.clone()));
                    let ok = |v: Value| (StatusCode::OK, [("content-type", "application/json")], v.to_string());
                    if body.contains("RATE") {
                        let e = json!({"error": {"code": 429, "message": "Resource has been exhausted", "status": "RESOURCE_EXHAUSTED",
                            "details": [{"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "6.2s"}]}});
                        return (StatusCode::TOO_MANY_REQUESTS, [("content-type", "application/json")], e.to_string());
                    }
                    if body.contains("functionResponse") && !body.contains("thoughtSignature") {
                        return (StatusCode::BAD_REQUEST, [("content-type", "application/json")], json!({"error": {"message": "Function call is missing a thought_signature in functionCall parts"}}).to_string());
                    }
                    if body.contains("functionResponse") || body.contains("[previous tool result]") {
                        return ok(json!({"traceId": "t2", "response": {"candidates": [{"content": {"role": "model", "parts": [{"text": "done"}]}, "finishReason": "STOP"}],
                            "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 2}}}));
                    }
                    ok(json!({"traceId": "t1", "response": {"candidates": [{"content": {"role": "model", "parts": [
                        {"functionCall": {"name": "read", "args": {"path": "a"}, "id": "call.1"}, "thoughtSignature": "SIG-A"}]}, "finishReason": "STOP"}],
                        "usageMetadata": {"promptTokenCount": 20, "candidatesTokenCount": 5}}}))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    async fn signed_in(gw: &Gateway, project: Option<&'static str>) {
        gw.tokens.set(Arc::new(move |_, _| {
            Box::pin(async move { Ok(crate::oauth::Creds { access: "GOOGLE-ACCESS".into(), account_id: project.map(str::to_string), ..Default::default() }) })
        }));
    }

    #[tokio::test]
    async fn antigravity_turns_go_out_as_cloud_code_calls_and_keep_their_signatures() {
        let (base, seen) = antigravity_upstream().await;
        let (gw, url) = gateway(&base, Kind::AntigravityOauth, GuardMode::Redact).await;
        signed_in(&gw, Some("proj-7")).await;
        let c = reqwest::Client::new();
        let tools = json!([{"type": "function", "function": {"name": "read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "$schema": "x"}}}]);
        let first = json!({"model": "default", "stream": true, "tools": tools, "messages": [{"role": "user", "content": "read a, key AKIAABCDEFGHIJKLMNOP"}]});
        let r = c.post(format!("{url}/chat/completions")).bearer_auth("t".repeat(48)).json(&first).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/event-stream");
        let sse = r.text().await.unwrap();
        assert!(sse.contains("\"id\":\"call_1\"") && sse.contains("\"finish_reason\":\"tool_calls\"") && sse.ends_with("data: [DONE]\n\n"), "{sse}");
        {
            let (h, sent) = seen.lk()[0].clone();
            assert_eq!(h["authorization"], "Bearer GOOGLE-ACCESS");
            assert!(h["user-agent"].to_str().unwrap().starts_with("antigravity/cli/1.5.6 "));
            assert_eq!(h["x-goog-request-params"], "project=proj-7");
            assert!(h["x-goog-client-metadata"].to_str().unwrap().contains("PLATFORM_UNSPECIFIED"));
            assert!(!format!("{h:?}").contains(&"t".repeat(48)), "gateway token never forwarded");
            let v: Value = serde_json::from_str(&sent).unwrap();
            assert_eq!((v["model"].as_str(), v["project"].as_str()), (Some("gemini-3-flash"), Some("proj-7")));
            assert!(!sent.contains("AKIAABCDEFGHIJKLMNOP"), "guard still applies");
            assert!(!sent.contains("$schema"));
        }
        let a = settle(&gw).await;
        assert_eq!((a.state, a.status), ("done", Some(200)));
        // The next turn sends the call's signature back.
        let second = json!({"model": "gemini-3-flash", "tools": tools, "messages": [
            {"role": "user", "content": "read a"},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": "A"}]});
        let r = c.post(format!("{url}/chat/completions")).bearer_auth("t".repeat(48)).json(&second).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "done");
        assert_eq!(v["usage"]["prompt_tokens"], 30);
        assert!(seen.lk()[1].1.contains("SIG-A"));
        assert_eq!(seen.lk().len(), 2);
        // After a restart the signature is gone: the refusal is answered by sending the history as text.
        let (gw2, url2) = gateway(&base, Kind::AntigravityOauth, GuardMode::Redact).await;
        signed_in(&gw2, Some("proj-7")).await;
        let r = c.post(format!("{url2}/chat/completions")).bearer_auth("t".repeat(48)).json(&second).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "done");
        assert_eq!(seen.lk().len(), 4);
        assert!(seen.lk()[3].1.contains("[previous tool result] read: A"));
        // A quota refusal passes through with when to try again.
        let rate = json!({"model": "gemini-3-flash", "messages": [{"role": "user", "content": "RATE"}]});
        let r = c.post(format!("{url}/chat/completions")).bearer_auth("t".repeat(48)).json(&rate).send().await.unwrap();
        assert_eq!(r.status(), 429);
        assert_eq!(r.headers()["retry-after"], "7");
        assert!(r.text().await.unwrap().contains("Antigravity: Resource has been exhausted"));
    }

    #[tokio::test]
    async fn antigravity_without_a_project_asks_to_sign_in_again_and_sends_nothing() {
        let (base, seen) = antigravity_upstream().await;
        let (gw, url) = gateway(&base, Kind::AntigravityOauth, GuardMode::Redact).await;
        signed_in(&gw, None).await;
        let r = post(&url).send().await.unwrap();
        assert_eq!(r.status(), 401);
        assert!(r.text().await.unwrap().contains("Sign in again"));
        assert!(seen.lk().is_empty());
    }

    #[tokio::test]
    async fn subscription_swaps_in_oauth_and_hides_gateway_token() {
        for (kind, expect_id) in [(Kind::ClaudeOauth, false), (Kind::ChatgptOauth, true)] {
            let (base, seen) = upstream().await;
            let (gw, url) = gateway(&base, kind, GuardMode::Redact).await;
            gw.tokens.set(Arc::new(|_, _| {
                Box::pin(async { Ok(crate::oauth::Creds { access: "OAUTH-ACCESS".into(), account_id: Some("acct-9".into()), ..Default::default() }) })
            }));
            let path = if kind == Kind::ClaudeOauth { "messages" } else { "responses" };
            let tok = "t".repeat(48);
            let resp = reqwest::Client::new()
                .post(format!("{url}/{path}"))
                .header("x-api-key", &tok)
                .header("anthropic-beta", "custom-beta")
                .json(&json!({"model":"m","stream":true,"system":"sys AKIAABCDEFGHIJKLMNOP","messages":[]}))
                .send().await.unwrap();
            assert_eq!(resp.status(), 200);
            let (h, sent) = seen.lk()[0].clone();
            assert_eq!(h["authorization"], "Bearer OAUTH-ACCESS");
            assert!(h.get("x-api-key").is_none(), "gateway token never forwarded");
            assert!(!format!("{h:?}{sent}").contains(&tok));
            assert!(!sent.contains("AKIAABCDEFGHIJKLMNOP"), "guard still applies");
            if expect_id {
                assert_eq!(h["chatgpt-account-id"], "acct-9");
                assert_eq!(h["originator"], "codex_cli_rs");
                assert!(sent.contains("\"store\":false"));
            } else {
                assert!(h["anthropic-beta"].to_str().unwrap().contains("oauth-2025-04-20"));
                assert!(h["anthropic-beta"].to_str().unwrap().contains("custom-beta"));
                assert!(h["user-agent"].to_str().unwrap().starts_with("claude-cli/"));
                assert!(sent.contains("Claude Agent SDK"));
            }
            let a = settle(&gw).await;
            assert_eq!((a.state, a.input_tokens), ("done", Some(12)));
            assert!(!gw.detail(a.id).unwrap().to_string().contains("OAUTH-ACCESS"));
        }
    }

    #[tokio::test]
    async fn emits_progress_for_chatgpt_when_oauth_enables_streaming() {
        let (base, _) = upstream().await;
        let (gw, url) = gateway(&base, Kind::ChatgptOauth, GuardMode::Off).await;
        gw.tokens.set(Arc::new(|_, _| {
            Box::pin(async { Ok(crate::oauth::Creds { access: "test".into(), ..Default::default() }) })
        }));
        let mut progress = gw.progress.subscribe();
        let token = gw.issue_token("fixture-agent", None);
        let response = reqwest::Client::new().post(format!("{url}/responses"))
            .bearer_auth(token).json(&json!({"model":"fixture"})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.text().await.unwrap().contains("[DONE]"));
        let p = progress.try_recv().expect("streaming progress for forced SSE");
        assert_eq!(p.agent.as_deref(), Some("fixture-agent"));
        assert_eq!(p.phase, "text");
    }

    /// The effort a person chose reaches each Claude model in the form that model takes: a named
    /// level for current models, a thinking budget for the 4.5 generation.
    #[tokio::test]
    async fn effort_reaches_a_claude_model_in_the_form_it_takes() {
        let (base, seen) = upstream().await;
        let (gw, url) = gateway(&base, Kind::ClaudeOauth, GuardMode::Redact).await;
        gw.tokens.set(Arc::new(|_, _| Box::pin(async { Ok(crate::oauth::Creds { access: "OAUTH-ACCESS".into(), ..Default::default() }) })));
        gw.set_effort(None, Some("high"));
        let ask = |model: &'static str| {
            let url = url.clone();
            async move {
                let body = json!({"model": model, "stream": true, "temperature": 0.3, "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]});
                let resp = reqwest::Client::new().post(format!("{url}/chat/completions")).header("authorization", format!("Bearer {}", "t".repeat(48))).json(&body).send().await.unwrap();
                assert_eq!(resp.status(), 200);
                let _ = resp.bytes().await;
            }
        };
        ask("claude-opus-5-5").await;
        ask("claude-haiku-4-5-20251001").await;
        let sent: Vec<Value> = seen.lk().iter().map(|(_, b)| serde_json::from_str(b).unwrap()).collect();
        assert_eq!(sent[0]["output_config"], json!({"effort": "high"}), "{}", sent[0]);
        assert_eq!(sent[0]["thinking"], json!({"type": "adaptive"}));
        assert!(sent[0].get("temperature").is_none());
        assert_eq!(sent[1]["thinking"], json!({"type": "enabled", "budget_tokens": 16384}), "{}", sent[1]);
        assert!(sent[1].get("output_config").is_none());
    }

    #[tokio::test]
    async fn signed_out_subscription_is_a_clear_401() {
        let (base, seen) = upstream().await;
        let (gw, url) = gateway(&base, Kind::ClaudeOauth, GuardMode::Off).await;
        gw.tokens.set(Arc::new(|_, _| Box::pin(async { Err("Signed out. Sign in again".into()) })));
        let r = reqwest::Client::new().post(format!("{url}/messages")).bearer_auth("t".repeat(48)).json(&json!({})).send().await.unwrap();
        assert_eq!(r.status(), 401);
        assert!(r.text().await.unwrap().contains("Sign in again"));
        assert!(seen.lk().is_empty());
    }

    #[tokio::test]
    async fn serializes_concurrent_requests_and_retries_busy_slot_429() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (a2, m2, c2) = (active.clone(), max_active.clone(), calls.clone());
        let app = Router::new().route(
            "/v1/{*rest}",
            any(move || {
                let (a2, m2, c2) = (a2.clone(), m2.clone(), c2.clone());
                async move {
                    let n = c2.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        let body = "{\"error\":{\"message\":\"请求被拒绝（429）：本卡最多同时处理 1 个请求\"}}";
                        return (StatusCode::TOO_MANY_REQUESTS, [("content-type", "application/json")], body.to_string());
                    }
                    let cur = a2.fetch_add(1, Ordering::SeqCst) + 1;
                    m2.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    a2.fetch_sub(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        [("content-type", "text/event-stream")],
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":6}}}\n\ndata: [DONE]\n\n".to_string(),
                    )
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (gw, url) = gateway(&base, Kind::OpenaiChat, GuardMode::Off).await;
        let (r1, r2) = tokio::join!(post(&url).send(), post(&url).send());
        let (r1, r2) = (r1.unwrap(), r2.unwrap());
        assert_eq!((r1.status(), r2.status()), (StatusCode::OK, StatusCode::OK));
        let _ = (r1.text().await, r2.text().await);
        assert_eq!(max_active.load(Ordering::SeqCst), 1, "untrusted provider streams never overlap");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "busy-slot 429 retried transparently");
        let a = settle(&gw).await;
        assert_eq!((a.input_tokens, a.output_tokens, a.cached_tokens), (Some(10), Some(2), Some(6)));
    }

    #[tokio::test]
    async fn untrusted_provider_enforces_redact_even_if_guard_off() {
        let gw = Gateway::new("t".repeat(48), None);
        gw.set_routes(vec![Route {
            provider: Provider {
                id: "cn".into(),
                name: "CN".into(),
                kind: Kind::OpenaiChat,
                base_url: "http://127.0.0.1:1/v1".into(),
                trusted: false,
                guard: GuardMode::Off,
                enabled: true,
                default_model: None,
            },
            api_key: Some("REAL-UPSTREAM-KEY".into()),
        }]);
        gw.set_effort(None, Some("high"));
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", "t".repeat(48))).unwrap());
        let payload = Bytes::from(serde_json::to_vec(&body()).unwrap());
        let _ = proxy(
            State(gw.clone()),
            Path(("cn".into(), "chat/completions".into())),
            RawQuery(None),
            Method::POST,
            headers,
            payload,
        )
        .await;
        let a = gw.recent().pop().unwrap();
        assert_eq!(a.guard, "redacted", "untrusted provider must enforce Redact even when configured Off");
        let d = gw.detail(a.id).unwrap();
        let req_text = d["request"].as_str().unwrap();
        assert!(!req_text.contains("AKIAABCDEFGHIJKLMNOP"), "secret must be redacted from request: {req_text}");
        assert!(req_text.contains("[REDACTED:aws_access_key]"), "{req_text}");
        assert!(req_text.contains("\"reasoning_effort\":\"high\""), "single-pass effort injection preserved: {req_text}");
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
