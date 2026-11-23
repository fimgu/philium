//! OpenAI Chat Completions <-> Google Cloud Code `generateContent`, for the
//! Antigravity sign-in. HAL speaks chat.completions; Antigravity takes a
//! Gemini body wrapped with the account's project. Calls are unary, so a
//! streaming caller gets the whole answer as one burst of chunks.
//!
//! Modeled on jcode's native Antigravity provider (MIT, github.com/1jehuang/jcode):
//! the daily host, the CLI fingerprint, thought-signature carry-forward and the
//! schema rules all come from failures it documents.
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Consumer accounts are only served on the daily host; the plain host
/// answers 429 RESOURCE_EXHAUSTED to the same token.
pub const ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
/// The consumer CLI fingerprint unlocks the full model catalog.
pub const VERSION: &str = crate::pins::GEMINI_CLI_VERSION;
pub const API_CLIENT: &str = crate::pins::GEMINI_API_CLIENT;
/// OS-specific platform values are rejected.
pub const CLIENT_METADATA: &str = r#"{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;
/// Compiled-in default, used only until a `fetchAvailableModels` catalog names one.
pub const FALLBACK_MODEL: &str = crate::pins::GEMINI_FALLBACK_MODEL;


/// The backend's default model named by a `fetchAvailableModels` catalog.
/// Callers keep it per gateway, never process-global.
pub fn catalog_default(body: &Value) -> Option<String> {
    body["defaultAgentModelId"].as_str().map(resolve_model).filter(|d| !d.is_empty() && d != "default")
}

/// `resolve_model`, with "default" mapped to a learned catalog default.
pub fn resolve_model_with(model: &str, learned: Option<&str>) -> String {
    match (model.trim().trim_start_matches("antigravity:"), learned) {
        ("" | "default", Some(d)) => d.into(),
        _ => resolve_model(model),
    }
}
const TOOL_GUARD: &str = "\n\n## Function calling\n- When you call a tool, emit a native function call, not code. Never write Python (or any language) that calls the tool, and never wrap a call in print(...) or a code block.\n- Use the function name exactly as defined. Do not prepend `default_api.` or any other namespace to the function name.";

pub fn user_agent() -> String {
    let platform = if cfg!(windows) { "windows/amd64" } else if cfg!(target_arch = "aarch64") { "darwin/arm64" } else { "darwin/amd64" };
    format!("antigravity/{VERSION} {platform}")
}

/// `default` answers 404 and `gemini-3.1-pro-high` answers 400; send what works.
pub fn resolve_model(model: &str) -> String {
    let m = model.trim().trim_start_matches("antigravity:");
    match m {
        "" | "default" => FALLBACK_MODEL.into(),
        "gemini-3.1-pro-high" => "gemini-pro-agent".into(),
        _ => m.into(),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    Gemini,
    /// Claude behind Cloud Code: no combiners at any depth.
    Claude,
    /// Other models (gpt-oss): numeric bounds on lengths and items break.
    Bridge,
}

pub fn family(model: &str) -> Family {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("gemini") {
        Family::Gemini
    } else if m.contains("claude") {
        Family::Claude
    } else {
        Family::Bridge
    }
}

/// What to change on a retry after a refusal.
#[derive(Default, Clone)]
pub struct Shaping {
    /// Send earlier tool calls and results as plain text (no signatures needed).
    pub tools_as_text: bool,
    /// Force a native function call after an empty or malformed turn.
    pub force_call: bool,
    /// Flatten `anyOf` everywhere, as for Claude.
    pub no_combiners: bool,
    /// Schema keywords the backend refused by name.
    pub rejected: HashSet<String>,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn user_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"text": s})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()).map(|t| json!({"text": t})),
                Some("image_url") => {
                    let u = p.pointer("/image_url/url").or_else(|| p.get("image_url"))?.as_str()?;
                    match u.strip_prefix("data:").and_then(|r| r.split_once(',')) {
                        Some((meta, data)) => {
                            let mime = meta.split(';').next().filter(|m| !m.is_empty()).unwrap_or("image/png");
                            Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
                        }
                        None => Some(json!({"text": format!("[image: {u}]")})),
                    }
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn push(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut() {
        if last["role"] == role {
            if let Some(a) = last["parts"].as_array_mut() {
                a.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({"role": role, "parts": parts}));
}

/// Translate a chat.completions body into the wrapped `generateContent` body.
/// `signature` returns the thought signature stored for a tool call id.
pub fn request(body: &Value, project: &str, request_id: &str, signature: &dyn Fn(&str) -> Option<String>, s: &Shaping) -> Value {
    let model = resolve_model(body["model"].as_str().unwrap_or(""));
    let fam = if s.no_combiners && family(&model) == Family::Gemini { Family::Claude } else { family(&model) };
    let mut system: Vec<String> = vec![];
    let mut contents: Vec<Value> = vec![];
    let mut names: HashMap<String, String> = HashMap::new();
    // Each call carries its own signature, or the latest one seen earlier: the
    // backend accepts a model turn when at least one call in it is signed.
    let mut last_sig: Option<String> = None;
    for m in body["messages"].as_array().cloned().unwrap_or_default() {
        match m["role"].as_str().unwrap_or("") {
            "system" | "developer" => {
                let t = text_of(&m["content"]);
                if !t.trim().is_empty() {
                    system.push(t);
                }
            }
            "assistant" => {
                let mut parts = vec![];
                let t = text_of(&m["content"]);
                if !t.is_empty() {
                    parts.push(json!({"text": t}));
                }
                for tc in m["tool_calls"].as_array().cloned().unwrap_or_default() {
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    let name = tc.pointer("/function/name").and_then(Value::as_str).unwrap_or("tool").to_string();
                    let raw = tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("");
                    let args = serde_json::from_str::<Value>(raw).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
                    names.insert(id.clone(), name.clone());
                    if s.tools_as_text {
                        parts.push(json!({"text": format!("[previous tool call] {name}({args})")}));
                        continue;
                    }
                    if let Some(sig) = signature(&id) {
                        last_sig = Some(sig);
                    }
                    let mut part = json!({"functionCall": {"name": name, "args": args, "id": id}});
                    if let Some(sig) = &last_sig {
                        part["thoughtSignature"] = json!(sig);
                    }
                    parts.push(part);
                }
                push(&mut contents, "model", parts);
            }
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap_or("").to_string();
                let name = names.get(&id).cloned().unwrap_or_else(|| "tool".into());
                let t = text_of(&m["content"]);
                let t = if t.is_empty() { "(no output)".to_string() } else { t };
                let part = if s.tools_as_text {
                    json!({"text": format!("[previous tool result] {name}: {t}")})
                } else {
                    json!({"functionResponse": {"name": name, "response": {"content": t}, "id": id}})
                };
                push(&mut contents, "user", vec![part]);
            }
            _ => push(&mut contents, "user", user_parts(&m["content"])),
        }
    }
    // Gemini refuses a function call turn that does not follow a user turn or a function
    // response. A compacted history can start with the model's call (its earlier turns are now
    // in the system summary), so give it a user turn to follow.
    if contents.first().is_some_and(|c| c["role"] == "model") {
        contents.insert(0, json!({"role": "user", "parts": [{"text": "Continue from the summary above."}]}));
    }
    let mut req = Map::new();
    req.insert("contents".into(), json!(contents));
    let tools: Vec<Value> = if body["tool_choice"] == "none" {
        vec![]
    } else {
        body["tools"].as_array().cloned().unwrap_or_default()
    };
    let mut sys = system.join("\n\n");
    if !tools.is_empty() {
        sys.push_str(TOOL_GUARD);
    }
    if !sys.trim().is_empty() {
        req.insert("systemInstruction".into(), json!({"role": "user", "parts": [{"text": sys.trim()}]}));
    }
    if !tools.is_empty() {
        let decls: Vec<Value> = tools
            .iter()
            .map(|t| {
                let f = &t["function"];
                let params = f.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                json!({"name": f["name"], "description": f.get("description").cloned().unwrap_or(json!("")), "parameters": clean_schema(&params, fam, &s.rejected)})
            })
            .collect();
        req.insert("tools".into(), json!([{"functionDeclarations": decls}]));
        let any = s.force_call || body["tool_choice"] == "required" || body["tool_choice"].is_object();
        req.insert("toolConfig".into(), json!({"functionCallingConfig": {"mode": if any { "ANY" } else { "AUTO" }}}));
    }
    json!({"model": model, "project": project, "user_prompt_id": request_id, "request": Value::Object(req)})
}

const ALWAYS: [&str; 6] = ["type", "properties", "items", "required", "enum", "title"];
const EXTRA: [&str; 13] = ["description", "default", "format", "anyOf", "nullable", "minimum", "maximum", "minItems", "maxItems", "minLength", "maxLength", "pattern", "example"];
const BRIDGE_DROPS: [&str; 4] = ["minItems", "maxItems", "minLength", "maxLength"];

fn allowed(k: &str, fam: Family, rejected: &HashSet<String>) -> bool {
    if rejected.contains(k) && k != "type" && k != "properties" {
        return false;
    }
    ALWAYS.contains(&k) || (EXTRA.contains(&k) && !(fam == Family::Bridge && BRIDGE_DROPS.contains(&k)))
}

/// Keep what Cloud Code's proto accepts. Unknown keywords fail the whole
/// request ("Unknown name ... Cannot find field") before any model sees it.
pub fn clean_schema(v: &Value, fam: Family, rejected: &HashSet<String>) -> Value {
    let mut out = walk(v, fam, rejected);
    if out.get("type").is_none() {
        out["type"] = json!("object");
    }
    if fam == Family::Gemini {
        flatten_root(&mut out);
    }
    out
}

fn walk(v: &Value, fam: Family, rejected: &HashSet<String>) -> Value {
    let Some(o) = v.as_object() else { return v.clone() };
    let mut o = o.clone();
    // allOf: fold every branch into this node, so its properties survive.
    if let Some(all) = o.remove("allOf").and_then(|a| a.as_array().cloned()) {
        for b in all {
            if let Some(bo) = b.as_object() {
                for (k, bv) in bo {
                    match (k.as_str(), o.get_mut(k)) {
                        ("properties", Some(Value::Object(p))) => {
                            for (pk, pv) in bv.as_object().cloned().unwrap_or_default() {
                                p.entry(pk).or_insert(pv);
                            }
                        }
                        ("required", Some(Value::Array(r))) => r.extend(bv.as_array().cloned().unwrap_or_default()),
                        (_, None) => {
                            o.insert(k.clone(), bv.clone());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    if let Some(one) = o.remove("oneOf") {
        let mut any = o.remove("anyOf").and_then(|a| a.as_array().cloned()).unwrap_or_default();
        any.extend(one.as_array().cloned().unwrap_or_default());
        o.insert("anyOf".into(), json!(any));
    }
    if let Some(c) = o.remove("const") {
        o.insert("enum".into(), json!([c]));
    }
    // `type: ["string", "null"]` becomes one type plus nullable.
    if let Some(Value::Array(types)) = o.get("type").cloned() {
        let real: Vec<&Value> = types.iter().filter(|t| *t != "null").collect();
        o.insert("type".into(), real.first().map(|t| (*t).clone()).unwrap_or(json!("string")));
        if real.len() < types.len() {
            o.insert("nullable".into(), json!(true));
        }
    }
    let mut out = Map::new();
    for (k, val) in o {
        // A refused anyOf is still needed below, to flatten its first branch.
        if !allowed(&k, fam, rejected) && k != "anyOf" {
            continue;
        }
        let val = match k.as_str() {
            "properties" => Value::Object(val.as_object().cloned().unwrap_or_default().into_iter().map(|(pk, pv)| (pk, walk(&pv, fam, rejected))).collect()),
            "items" => match &val {
                Value::Array(a) => a.first().map(|i| walk(i, fam, rejected)).unwrap_or_else(|| json!({"type": "string"})),
                _ => walk(&val, fam, rejected),
            },
            "anyOf" => Value::Array(val.as_array().cloned().unwrap_or_default().iter().map(|b| walk(b, fam, rejected)).collect()),
            _ => val,
        };
        out.insert(k, val);
    }
    // A null branch becomes `nullable`; one branch left is no combiner at all.
    if let Some(Value::Array(branches)) = out.get("anyOf").cloned() {
        let real: Vec<Value> = branches.iter().filter(|b| b["type"] != "null").cloned().collect();
        if real.len() < branches.len() {
            out.insert("nullable".into(), json!(true));
        }
        out.insert("anyOf".into(), json!(real));
        if real.len() <= 1 {
            flatten_here(&mut out);
        }
    }
    if fam != Family::Gemini || rejected.contains("anyOf") {
        flatten_here(&mut out);
    }
    // Enums are for strings only, and every node says its type.
    if out.contains_key("enum") {
        match out.get("type") {
            None => {
                out.insert("type".into(), json!("string"));
            }
            Some(t) if t != "string" => {
                out.remove("enum");
            }
            _ => {}
        }
    }
    if out.contains_key("properties") && !out.contains_key("type") {
        out.insert("type".into(), json!("object"));
    }
    if out.get("type") == Some(&json!("array")) && !out.contains_key("items") {
        out.insert("items".into(), json!({"type": "string"}));
    }
    prune_required(&mut out);
    Value::Object(out)
}

/// Take the first branch of a combiner and lay this node's own keys over it.
fn flatten_here(o: &mut Map<String, Value>) {
    let Some(Value::Array(branches)) = o.remove("anyOf") else { return };
    let Some(Value::Object(first)) = branches.into_iter().find(Value::is_object) else { return };
    for (k, v) in first {
        match (k.as_str(), o.get_mut(&k)) {
            ("properties", Some(Value::Object(p))) => {
                for (pk, pv) in v.as_object().cloned().unwrap_or_default() {
                    p.entry(pk).or_insert(pv);
                }
            }
            (_, None) => {
                o.insert(k, v);
            }
            _ => {}
        }
    }
    prune_required(o);
}

/// Cloud Code refuses a root `anyOf` beside `type` or `properties`: merge every
/// branch's properties into the root instead.
fn flatten_root(v: &mut Value) {
    let Some(o) = v.as_object_mut() else { return };
    let Some(Value::Array(branches)) = o.remove("anyOf") else { return };
    let props = o.entry("properties").or_insert_with(|| json!({}));
    for b in branches {
        for (pk, pv) in b["properties"].as_object().cloned().unwrap_or_default() {
            if let Some(p) = props.as_object_mut() {
                p.entry(pk).or_insert(pv);
            }
        }
    }
    o.insert("type".into(), json!("object"));
    prune_required(o);
}

fn prune_required(o: &mut Map<String, Value>) {
    let Some(Value::Array(req)) = o.get("required").cloned() else { return };
    let props = o.get("properties").and_then(Value::as_object);
    let kept: Vec<Value> = req.into_iter().filter(|r| r.as_str().is_some_and(|n| props.is_some_and(|p| p.contains_key(n)))).collect();
    if kept.is_empty() {
        o.remove("required");
    } else {
        o.insert("required".into(), json!(kept));
    }
}

/// Characters other than `[A-Za-z0-9_-]` in a call id break the next turn.
pub fn sanitize_id(id: &str) -> String {
    let s: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if s.is_empty() { "unknown".into() } else { s }
}

fn normal_finish(r: &str) -> bool {
    matches!(r.to_ascii_uppercase().as_str(), "STOP" | "MAX_TOKENS" | "FINISH_REASON_UNSPECIFIED" | "")
}

/// A converted answer, plus the signature of each tool call to keep for the next turn.
pub struct Answer {
    pub completion: Value,
    pub signatures: Vec<(String, String)>,
    /// No text and no call, with an abnormal finish (for example MALFORMED_FUNCTION_CALL).
    pub empty: bool,
    /// The model wrote a call as text instead of calling.
    pub pseudo_call: bool,
    pub finish_message: String,
}

pub fn response(v: &Value, model: &str, new_id: &mut dyn FnMut() -> String) -> Result<Answer, String> {
    let r = v.get("response").unwrap_or(v);
    let cand = r["candidates"].as_array().and_then(|c| c.first()).ok_or("Antigravity returned no answer")?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = vec![];
    let mut signatures = vec![];
    let mut pending: Option<String> = None;
    for p in cand.pointer("/content/parts").and_then(Value::as_array).cloned().unwrap_or_default() {
        let sig = p["thoughtSignature"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        if let Some(fc) = p.get("functionCall") {
            let id = sanitize_id(&fc["id"].as_str().map(str::to_string).unwrap_or_else(|| new_id()));
            let args = if fc["args"].is_object() { fc["args"].clone() } else { json!({}) };
            calls.push(json!({"id": id, "type": "function", "function": {"name": fc["name"], "arguments": args.to_string()}}));
            if let Some(s) = sig.or_else(|| pending.take()) {
                signatures.push((id, s));
            }
        } else if let Some(t) = p["text"].as_str() {
            if p["thought"] == true { reasoning.push_str(t) } else { text.push_str(t) }
            if let Some(s) = sig {
                pending = Some(s);
            }
        } else if let Some(s) = sig {
            pending = Some(s);
        }
    }
    let finish = cand["finishReason"].as_str().unwrap_or("");
    let empty = text.trim().is_empty() && calls.is_empty() && !normal_finish(finish);
    let pseudo_call = calls.is_empty() && {
        let t = text.trim_start();
        t.strip_prefix("[previous tool call] ").is_some_and(|rest| {
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            !name.is_empty() && rest[name.len()..].starts_with('(')
        })
    };
    let reason = if !calls.is_empty() {
        "tool_calls"
    } else {
        match finish.to_ascii_uppercase().as_str() {
            "MAX_TOKENS" => "length",
            "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "IMAGE_SAFETY" => "content_filter",
            _ => "stop",
        }
    };
    let mut msg = json!({"role": "assistant", "content": text});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        msg["tool_calls"] = json!(calls);
    }
    let u = &r["usageMetadata"];
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let prompt = g("promptTokenCount") + g("toolUsePromptTokenCount");
    let out = g("candidatesTokenCount") + g("thoughtsTokenCount");
    let mut usage = json!({"prompt_tokens": prompt, "completion_tokens": out, "total_tokens": prompt + out});
    if g("cachedContentTokenCount") > 0 {
        usage["prompt_tokens_details"] = json!({"cached_tokens": g("cachedContentTokenCount")});
    }
    let id = format!("chatcmpl-{}", v["traceId"].as_str().map(str::to_string).unwrap_or_else(|| new_id()));
    Ok(Answer {
        completion: json!({"id": id, "object": "chat.completion", "model": model, "choices": [{"index": 0, "message": msg, "finish_reason": reason}], "usage": usage}),
        signatures,
        empty,
        pseudo_call,
        finish_message: cand["finishMessage"].as_str().unwrap_or(finish).chars().take(300).collect(),
    })
}

/// A whole answer as the chunks a streaming caller expects.
pub fn to_sse(c: &Value) -> String {
    let id = &c["id"];
    let model = &c["model"];
    let chunk = |delta: Value, finish: Value| format!("data: {}\n\n", json!({"id": id, "object": "chat.completion.chunk", "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}));
    let msg = &c["choices"][0]["message"];
    let mut out = chunk(json!({"role": "assistant", "content": ""}), Value::Null);
    if let Some(r) = msg["reasoning_content"].as_str() {
        out.push_str(&chunk(json!({"reasoning_content": r}), Value::Null));
    }
    if let Some(t) = msg["content"].as_str().filter(|t| !t.is_empty()) {
        out.push_str(&chunk(json!({"content": t}), Value::Null));
    }
    for (i, tc) in msg["tool_calls"].as_array().cloned().unwrap_or_default().into_iter().enumerate() {
        out.push_str(&chunk(json!({"tool_calls": [{"index": i, "id": tc["id"], "type": "function", "function": tc["function"]}]}), Value::Null));
    }
    out.push_str(&chunk(json!({}), c["choices"][0]["finish_reason"].clone()));
    out.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", json!({"id": id, "object": "chat.completion.chunk", "model": model, "choices": [], "usage": c["usage"]})));
    out
}

/// What a 400 asks us to change, if anything.
pub enum Fix {
    ToolsAsText,
    DropKeywords(Vec<String>),
    NoCombiners,
}

pub fn fix_for(status: u16, body: &str) -> Option<Fix> {
    if status != 400 {
        return None;
    }
    let low = body.to_ascii_lowercase();
    if low.contains("thought_signature") || low.contains("thoughtsignature") {
        return Some(Fix::ToolsAsText);
    }
    if low.contains("draft 2020-12") || low.contains("does not support oneof, allof, or anyof") {
        return Some(Fix::NoCombiners);
    }
    if body.contains("Cannot find field") {
        let mut names = vec![];
        let mut rest = body;
        while let Some(i) = rest.find("Unknown name ") {
            rest = &rest[i + 13..];
            let q = rest.trim_start_matches(['\\', '"']);
            let n: String = q.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$').collect();
            if !n.is_empty() && !names.contains(&n) {
                names.push(n);
            }
        }
        if !names.is_empty() {
            return Some(Fix::DropKeywords(names));
        }
    }
    None
}

/// Seconds from a google.rpc.RetryInfo detail, such as `"retryDelay": "31s"`.
pub fn retry_after(body: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.pointer("/error/details")?.as_array()?.iter().find_map(|d| {
        let s = d["retryDelay"].as_str()?.trim_end_matches('s');
        s.parse::<f64>().ok().map(|f| f.ceil().max(1.0) as u64)
    })
}

/// The catalog from `fetchAvailableModels`: a map keyed by model id. Models with
/// quota come first; the backend's default leads.
pub fn models(body: &Value) -> Vec<(String, Option<String>, bool)> {
    let map = body["models"].as_object().cloned().unwrap_or_default();
    let mut ids: Vec<String> = vec![];
    for id in std::iter::once(body["defaultAgentModelId"].clone())
        .chain(body["commandModelIds"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(str::to_string))
        .chain({
            let mut k: Vec<String> = map.keys().cloned().collect();
            k.sort();
            k
        })
    {
        let id = resolve_model(&id);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut out: Vec<(String, Option<String>, bool)> = ids
        .into_iter()
        .map(|id| {
            let m = map.get(&id).or_else(|| if id == "gemini-pro-agent" { map.get("gemini-3.1-pro-high") } else { None });
            let display = m.and_then(|m| m["displayName"].as_str()).map(str::to_string);
            let available = m.and_then(|m| m.pointer("/quotaInfo/remainingFraction")).and_then(Value::as_f64).is_none_or(|f| f > 0.0);
            (id, display, available)
        })
        .collect();
    out.sort_by_key(|m| !m.2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    /// History cut by compaction can start with the model's tool call. Gemini needs a user
    /// turn before it.
    #[test]
    fn history_starting_with_a_call_gets_a_user_turn_first() {
        let body = json!({"model": "gemini-3-flash", "messages": [
            {"role": "system", "content": "Summary of earlier work."},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "ls", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "x"}],
            "tools": [{"type": "function", "function": {"name": "ls", "parameters": {"type": "object"}}}]});
        let w = request(&body, "p", "r", &none, &Shaping::default());
        let c = w["request"]["contents"].as_array().unwrap();
        assert_eq!(c.iter().map(|x| x["role"].as_str().unwrap()).collect::<Vec<_>>(), ["user", "model", "user"]);
        assert!(c[1]["parts"][0].get("functionCall").is_some());
    }

    #[test]
    fn chat_turns_become_gemini_contents_with_signed_calls() {
        let body = json!({"model": "gemini-3-flash", "messages": [
            {"role": "system", "content": "Be brief."},
            {"role": "user", "content": [{"type": "text", "text": "Read a"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "read", "arguments": "not json"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "A"},
            {"role": "tool", "tool_call_id": "c2", "content": ""},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "c3", "type": "function", "function": {"name": "ls", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c3", "content": "x"}],
            "tools": [{"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path", "gone"], "additionalProperties": false}}}]});
        let sig = |id: &str| (id == "c1").then(|| "SIG1".to_string());
        let w = request(&body, "proj-1", "rid", &sig, &Shaping::default());
        assert_eq!((w["model"].as_str(), w["project"].as_str(), w["user_prompt_id"].as_str()), (Some("gemini-3-flash"), Some("proj-1"), Some("rid")));
        let r = &w["request"];
        assert!(r["systemInstruction"]["parts"][0]["text"].as_str().unwrap().starts_with("Be brief.\n\n## Function calling"));
        let c = r["contents"].as_array().unwrap();
        assert_eq!(c.iter().map(|x| x["role"].as_str().unwrap()).collect::<Vec<_>>(), ["user", "model", "user", "model", "user"]);
        assert_eq!(c[0]["parts"][1]["inlineData"], json!({"mimeType": "image/png", "data": "AAAA"}));
        // Both calls of the first turn are signed; the later turn carries the last signature forward.
        assert_eq!((c[1]["parts"][0]["thoughtSignature"].as_str(), c[1]["parts"][1]["thoughtSignature"].as_str()), (Some("SIG1"), Some("SIG1")));
        assert_eq!(c[1]["parts"][1]["functionCall"]["args"], json!({}));
        assert_eq!(c[3]["parts"][0]["thoughtSignature"].as_str(), Some("SIG1"));
        // Results of parallel calls share one user turn, and carry the call's name.
        assert_eq!(c[2]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(c[2]["parts"][0]["functionResponse"], json!({"name": "read", "response": {"content": "A"}, "id": "c1"}));
        assert_eq!(c[2]["parts"][1]["functionResponse"]["response"]["content"], "(no output)");
        let params = &r["tools"][0]["functionDeclarations"][0]["parameters"];
        assert_eq!(params, &json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}));
        assert_eq!(r["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        // Without any stored signature, calls go unsigned; as text they need none.
        let t = request(&body, "p", "r", &none, &Shaping { tools_as_text: true, force_call: true, ..Default::default() });
        assert_eq!(t["request"]["contents"][1]["parts"][0]["text"], "[previous tool call] read({\"path\":\"a\"})");
        assert_eq!(t["request"]["contents"][2]["parts"][0]["text"], "[previous tool result] read: A");
        assert_eq!(t["request"]["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn models_that_fail_by_name_are_sent_as_ones_that_work() {
        assert_eq!(resolve_model("default"), FALLBACK_MODEL);
        assert_eq!(resolve_model(""), FALLBACK_MODEL);
        assert_eq!(resolve_model_with("default", Some("gemini-pro-agent")), "gemini-pro-agent");
        assert_eq!(resolve_model_with("claude-sonnet-4-6", Some("x")), "claude-sonnet-4-6");
        assert_eq!(catalog_default(&json!({"defaultAgentModelId": "gemini-3.1-pro-high"})).as_deref(), Some("gemini-pro-agent"));
        assert_eq!(resolve_model("antigravity:gemini-3.1-pro-high"), "gemini-pro-agent");
        assert_eq!(resolve_model("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!((family("Gemini-3-flash"), family("claude-opus-4-6-thinking"), family("gpt-oss-120b-medium")), (Family::Gemini, Family::Claude, Family::Bridge));
        assert!(user_agent().starts_with("antigravity/cli/1.5.6 "));
    }

    #[test]
    fn schemas_keep_only_what_each_backend_accepts() {
        let s = json!({"$schema": "x", "type": "object", "additionalProperties": false,
            "properties": {
                "mode": {"oneOf": [{"type": "string", "enum": ["a", "b"]}, {"type": "null"}], "description": "how"},
                "n": {"type": ["integer", "null"], "minimum": 1, "exclusiveMaximum": 9, "enum": [1, 2]},
                "tags": {"type": "array", "minItems": 1},
                "kind": {"const": "x"},
                "both": {"allOf": [{"properties": {"a": {"type": "string"}}, "required": ["a"]}, {"properties": {"b": {"type": "number"}}}]}},
            "anyOf": [{"properties": {"extra": {"type": "boolean"}}}]});
        let none = HashSet::new();
        let g = clean_schema(&s, Family::Gemini, &none);
        assert!(g.get("$schema").is_none() && g.get("additionalProperties").is_none() && g.get("anyOf").is_none());
        assert_eq!(g["type"], "object");
        assert_eq!(g["properties"]["extra"]["type"], "boolean", "root branches merge into the root");
        assert_eq!(g["properties"]["mode"], json!({"type": "string", "enum": ["a", "b"], "description": "how", "nullable": true}), "a null branch becomes nullable");
        let two = clean_schema(&json!({"type": "object", "properties": {"v": {"anyOf": [{"type": "string"}, {"type": "number"}]}}}), Family::Gemini, &none);
        assert_eq!(two["properties"]["v"]["anyOf"].as_array().unwrap().len(), 2, "Gemini keeps nested anyOf");
        assert_eq!(g["properties"]["n"], json!({"type": "integer", "nullable": true, "minimum": 1}));
        assert_eq!(g["properties"]["tags"], json!({"type": "array", "minItems": 1, "items": {"type": "string"}}));
        assert_eq!(g["properties"]["kind"]["enum"], json!(["x"]));
        assert_eq!(g["properties"]["both"]["properties"]["b"]["type"], "number");
        assert_eq!(g["properties"]["both"]["required"], json!(["a"]));
        let c = clean_schema(&s, Family::Claude, &none);
        assert!(c["properties"]["mode"].get("anyOf").is_none(), "Claude takes no combiners at any depth");
        assert_eq!(c["properties"]["mode"]["description"], "how");
        assert_eq!(c["properties"]["mode"]["enum"], json!(["a", "b"]));
        let b = clean_schema(&s, Family::Bridge, &none);
        assert!(b["properties"]["tags"].get("minItems").is_none());
        let learned: HashSet<String> = ["pattern".to_string(), "format".to_string()].into();
        let l = clean_schema(&json!({"type": "object", "properties": {"x": {"type": "string", "pattern": "a+", "format": "uri"}}}), Family::Gemini, &learned);
        assert_eq!(l["properties"]["x"], json!({"type": "string"}));
    }

    #[test]
    fn answers_become_chat_completions_and_keep_call_signatures() {
        let v = json!({"traceId": "t1", "response": {"candidates": [{"content": {"role": "model", "parts": [
            {"text": "thinking", "thought": true},
            {"text": "Reading.", "thoughtSignature": "S0"},
            {"functionCall": {"name": "read", "args": {"path": "a"}, "id": "id.1"}},
            {"functionCall": {"name": "ls", "args": "bad"}, "thoughtSignature": "S2"}]},
            "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 10, "toolUsePromptTokenCount": 2, "candidatesTokenCount": 7, "thoughtsTokenCount": 3, "cachedContentTokenCount": 4}}});
        let mut n = 0;
        let a = response(&v, "gemini-3-flash", &mut || { n += 1; format!("gen{n}") }).unwrap();
        let m = &a.completion["choices"][0]["message"];
        assert_eq!((m["content"].as_str(), m["reasoning_content"].as_str()), (Some("Reading."), Some("thinking")));
        assert_eq!(m["tool_calls"][0]["id"], "id_1");
        assert_eq!(m["tool_calls"][1]["id"], "gen1");
        assert_eq!(m["tool_calls"][1]["function"]["arguments"], "{}");
        assert_eq!(a.signatures, vec![("id_1".to_string(), "S0".to_string()), ("gen1".to_string(), "S2".to_string())]);
        assert_eq!(a.completion["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(a.completion["usage"], json!({"prompt_tokens": 12, "completion_tokens": 10, "total_tokens": 22, "prompt_tokens_details": {"cached_tokens": 4}}));
        assert_eq!(a.completion["id"], "chatcmpl-t1");
        assert!(!a.empty && !a.pseudo_call);
        let sse = to_sse(&a.completion);
        assert!(sse.contains("\"reasoning_content\":\"thinking\"") && sse.contains("\"finish_reason\":\"tool_calls\"") && sse.ends_with("data: [DONE]\n\n"));
        assert_eq!(sse.matches("\"tool_calls\":[{").count(), 2);
        // An empty malformed turn, a pseudo call, and no candidates at all.
        let e = response(&json!({"response": {"candidates": [{"content": {}, "finishReason": "MALFORMED_FUNCTION_CALL", "finishMessage": "Malformed function call: print(x)"}]}}), "m", &mut || "x".into()).unwrap();
        assert!(e.empty && e.finish_message.starts_with("Malformed"));
        let p = response(&json!({"response": {"candidates": [{"content": {"parts": [{"text": "  [previous tool call] read({})"}]}, "finishReason": "STOP"}]}}), "m", &mut || "x".into()).unwrap();
        assert!(p.pseudo_call && !p.empty);
        assert!(response(&json!({"response": {}}), "m", &mut || "x".into()).is_err());
    }

    #[test]
    fn refusals_say_what_to_change() {
        assert!(matches!(fix_for(400, "Function call is missing a thought_signature in functionCall parts"), Some(Fix::ToolsAsText)));
        assert!(matches!(fix_for(400, "input_schema: JSON schema is invalid. It must match JSON Schema draft 2020-12"), Some(Fix::NoCombiners)));
        let body = r#"{"error":{"message":"Invalid JSON payload received. Unknown name \"propertyNames\" at 'request.tools[0]': Cannot find field.\nInvalid JSON payload received. Unknown name \"$ref\" at 'x': Cannot find field."}}"#;
        match fix_for(400, body) {
            Some(Fix::DropKeywords(k)) => assert_eq!(k, ["propertyNames", "$ref"]),
            _ => panic!("expected keywords"),
        }
        assert!(fix_for(429, "thought_signature").is_none());
        assert_eq!(retry_after(r#"{"error":{"code":429,"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"30.5s"}]}}"#), Some(31));
    }

    #[test]
    fn the_catalog_lists_available_models_first_with_the_default_leading() {
        let body = json!({"defaultAgentModelId": "gemini-3.1-pro-high", "commandModelIds": ["gemini-3-flash"], "models": {
            "gemini-3.1-pro-high": {"displayName": "Gemini 3.1 Pro (High)", "quotaInfo": {"remainingFraction": 0.5}},
            "gemini-3-flash": {"displayName": "Gemini 3 Flash"},
            "claude-opus-4-6-thinking": {"displayName": "Claude Opus 4.6 (Thinking)", "quotaInfo": {"remainingFraction": 0}}}});
        let m = models(&body);
        assert_eq!(m.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["gemini-pro-agent", "gemini-3-flash", "claude-opus-4-6-thinking"]);
        assert_eq!(m[0].1.as_deref(), Some("Gemini 3.1 Pro (High)"));
        assert!(m[0].2 && m[1].2 && !m[2].2);
    }
}
