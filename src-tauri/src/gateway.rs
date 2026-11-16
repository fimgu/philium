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

use crate::guard::{self, GuardMode};
use crate::providers::{Kind, Provider};

const LOG_CAPACITY: usize = 500;
const BODY_CAP: usize = 512 * 1024;
const HEAD_CAP: usize = 64 * 1024;
const TAIL_CAP: usize = 64 * 1024;

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
    pub error: Option<String>,
}

struct Record {
    summary: Activity,
    /// The request exactly as it left the machine (after redaction).
    request: String,
    response_head: String,
    response_tail: String,
}

pub struct Gateway {
    pub token: String,
    client: reqwest::Client,
    routes: RwLock<HashMap<String, Route>>,
    log: Mutex<VecDeque<Record>>,
    next_id: AtomicU64,
    pub events: broadcast::Sender<Activity>,
    log_file: Option<PathBuf>,
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
        Arc::new(Self {
            token,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("http client"),
            routes: RwLock::new(HashMap::new()),
            log: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            events: broadcast::channel(256).0,
            log_file,
        })
    }

    pub fn set_routes(&self, routes: Vec<Route>) {
        *self.routes.write().unwrap() =
            routes.into_iter().map(|r| (r.provider.id.clone(), r)).collect();
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/p/{provider}/{*rest}", any(proxy))
            .with_state(self.clone())
    }

    pub fn recent(&self) -> Vec<Activity> {
        self.log.lock().unwrap().iter().map(|r| r.summary.clone()).collect()
    }

    pub fn detail(&self, id: u64) -> Option<Value> {
        let log = self.log.lock().unwrap();
        let r = log.iter().find(|r| r.summary.id == id)?;
        Some(json!({
            "summary": r.summary,
            "request": r.request,
            "response_head": r.response_head,
            "response_tail": r.response_tail,
        }))
    }

    pub fn clear(&self) {
        self.log.lock().unwrap().clear();
    }

    fn push(&self, rec: Record) {
        let s = rec.summary.clone();
        let mut log = self.log.lock().unwrap();
        if log.len() >= LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(rec);
        drop(log);
        let _ = self.events.send(s);
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Record)) {
        let mut log = self.log.lock().unwrap();
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
        let Some(path) = &self.log_file else { return };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{}", serde_json::to_string(s).unwrap_or_default());
        }
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let presented = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()));
        presented.is_some_and(|p| {
            p.len() == self.token.len()
                && p.bytes().zip(self.token.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
        })
    }
}

fn error_response(status: StatusCode, kind: &str, message: String) -> Response {
    let body = json!({ "error": { "type": kind, "message": message } });
    (status, [("content-type", "application/json")], body.to_string()).into_response()
}

/// Accumulates the streamed response; finalizes the record on drop, so a
/// client disconnect or upstream error still closes the log entry.
struct Finalizer {
    gw: Arc<Gateway>,
    id: u64,
    started: Instant,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
    error: Option<String>,
}

impl Finalizer {
    fn feed(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        let room = HEAD_CAP.saturating_sub(self.head.len());
        self.head.extend_from_slice(&chunk[..room.min(chunk.len())]);
        self.tail.extend(chunk);
        while self.tail.len() > TAIL_CAP {
            self.tail.pop_front();
        }
    }
}

impl Drop for Finalizer {
    fn drop(&mut self) {
        let head = String::from_utf8_lossy(&self.head).into_owned();
        let tail_bytes: Vec<u8> = self.tail.iter().copied().collect();
        let tail = String::from_utf8_lossy(&tail_bytes).into_owned();
        let (input, output) = extract_usage(&head, &tail);
        let total = self.total;
        let elapsed = self.started.elapsed().as_millis() as u64;
        let error = self.error.take();
        self.gw.update(self.id, |r| {
            let s = &mut r.summary;
            s.duration_ms = Some(elapsed);
            s.resp_bytes = total;
            s.input_tokens = input.or(s.input_tokens);
            s.output_tokens = output.or(s.output_tokens);
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

/// Token usage from any of the wire formats, streamed (SSE) or whole.
/// OpenAI: prompt/completion_tokens. Responses + Anthropic: input/output_tokens
/// (Anthropic reports input in message_start and output in message_delta).
fn extract_usage(head: &str, tail: &str) -> (Option<u64>, Option<u64>) {
    let mut input = None;
    let mut output = None;
    let mut visit = |v: &Value| {
        fn walk(v: &Value, f: &mut dyn FnMut(&Value)) {
            match v {
                Value::Object(o) => {
                    if let Some(u) = o.get("usage") {
                        f(u);
                    }
                    o.values().for_each(|x| walk(x, f));
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
                _ => {}
            }
        }
        walk(v, &mut |u| {
            let get = |k: &str| u.get(k).and_then(Value::as_u64);
            if let Some(i) = get("prompt_tokens").or(get("input_tokens")) {
                input = Some(input.unwrap_or(0).max(i));
            }
            if let Some(o) = get("completion_tokens").or(get("output_tokens")) {
                output = Some(output.unwrap_or(0).max(o));
            }
        });
    };
    for text in [head, tail] {
        if let Ok(v) = serde_json::from_str::<Value>(text.trim()) {
            visit(&v);
            continue;
        }
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    visit(&v);
                }
            }
        }
    }
    (input, output)
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
    if !gw.authorized(&headers) {
        return error_response(StatusCode::UNAUTHORIZED, "filium_auth", "invalid gateway token".into());
    }
    let Some(route) = gw.routes.read().unwrap().get(&provider).cloned() else {
        return error_response(StatusCode::NOT_FOUND, "filium_route", format!("unknown provider {provider}"));
    };
    if rest.split('/').any(|s| s == ".." || s == ".") {
        return error_response(StatusCode::BAD_REQUEST, "filium_route", "invalid path".into());
    }
    let p = &route.provider;
    let id = gw.next_id.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();

    // Guard: scan the exact bytes about to leave.
    let mode = p.guard;
    let mut findings = guard::Findings::new();
    let mut parsed: Option<Value> = None;
    let outgoing: Bytes = if body.is_empty() {
        body
    } else if let Ok(mut v) = serde_json::from_slice::<Value>(&body) {
        guard::scan_json(&mut v, mode == GuardMode::Redact, &mut findings);
        let out = if mode == GuardMode::Redact && !findings.is_empty() {
            Bytes::from(serde_json::to_vec(&v).unwrap_or_default())
        } else {
            body
        };
        parsed = Some(v);
        out
    } else {
        let mut text = String::from_utf8_lossy(&body).into_owned();
        guard::scan_text(&mut text, mode == GuardMode::Redact, &mut findings);
        if mode == GuardMode::Redact && !findings.is_empty() { Bytes::from(text) } else { body }
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
        error: None,
    };
    gw.push(Record {
        summary,
        request: if blocked {
            format!("[not sent: blocked by guard ({})]", guard::summarize(&findings))
        } else {
            cap(&String::from_utf8_lossy(&outgoing), BODY_CAP)
        },
        response_head: String::new(),
        response_tail: String::new(),
    });
    if blocked {
        let msg = format!(
            "Filium guard blocked this request: it contains {}. Remove the secret or switch the provider's guard to redact.",
            guard::summarize(&findings)
        );
        gw.update(id, |r| {
            r.summary.status = Some(403);
            r.summary.duration_ms = Some(0);
            r.summary.error = Some(msg.clone());
        });
        return error_response(StatusCode::FORBIDDEN, "filium_guard", msg);
    }

    let mut url = format!("{}/{}", p.base_url.trim_end_matches('/'), rest);
    if let Some(q) = query {
        url = format!("{url}?{q}");
    }
    let mut req = gw.client.request(method, &url).body(outgoing);
    for h in FORWARD_HEADERS {
        if let Some(v) = headers.get(h) {
            req = req.header(h, v);
        }
    }
    if let Some(key) = route.api_key.as_deref().filter(|k| !k.is_empty()) {
        req = match p.kind {
            Kind::Anthropic => req.header("x-api-key", key),
            _ => req.bearer_auth(key),
        };
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("upstream unreachable: {e}");
            gw.update(id, |r| {
                r.summary.state = "error";
                r.summary.error = Some(msg.clone());
                r.summary.duration_ms = Some(started.elapsed().as_millis() as u64);
            });
            return error_response(StatusCode::BAD_GATEWAY, "filium_upstream", msg);
        }
    };
    let status = resp.status();
    gw.update(id, |r| r.summary.status = Some(status.as_u16()));
    let content_type = resp.headers().get("content-type").cloned();
    let retry_after = resp.headers().get("retry-after").cloned();

    let mut fin = Finalizer {
        gw: gw.clone(),
        id,
        started,
        head: Vec::new(),
        tail: VecDeque::new(),
        total: 0,
        error: None,
    };
    let body = resp.bytes_stream().map(move |item| {
        match &item {
            Ok(chunk) => fin.feed(chunk),
            Err(e) => fin.error = Some(format!("upstream stream error: {e}")),
        }
        item
    });
    let mut out = Response::new(Body::from_stream(body));
    *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let h = out.headers_mut();
    h.insert("content-type", content_type.unwrap_or(HeaderValue::from_static("application/json")));
    if let Some(v) = retry_after {
        h.insert("retry-after", v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    type Seen = Arc<StdMutex<Vec<(HeaderMap, String)>>>;

    /// Fake provider: records what it receives, replies with SSE + usage.
    async fn upstream() -> (String, Seen) {
        let seen: Seen = Default::default();
        let s = seen.clone();
        let app = Router::new().route(
            "/v1/{*rest}",
            any(move |headers: HeaderMap, body: String| {
                let s = s.clone();
                async move {
                    s.lock().unwrap().push((headers, body));
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
                model: "m".into(),
                context_window: 128000,
                guard,
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
        assert!(seen.lock().unwrap().is_empty());
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
        let (headers, sent) = seen.lock().unwrap()[0].clone();
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
        assert!(seen.lock().unwrap().is_empty());
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
        let (headers, _) = seen.lock().unwrap()[0].clone();
        assert_eq!(headers["x-api-key"], "REAL-UPSTREAM-KEY");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert!(headers.get("authorization").is_none());
    }

    #[test]
    fn usage_from_each_wire_format() {
        let anthropic = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":40,\"output_tokens\":1}}}\n\n\
                         data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n";
        assert_eq!(extract_usage(anthropic, ""), (Some(40), Some(9)));
        let responses = "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":6}}}\n";
        assert_eq!(extract_usage("", responses), (Some(5), Some(6)));
        let whole = "{\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":8}}";
        assert_eq!(extract_usage(whole, ""), (Some(7), Some(8)));
    }
}
