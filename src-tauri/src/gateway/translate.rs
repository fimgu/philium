//! OpenAI Chat Completions <-> Anthropic Messages translation.
//!
//! HAL refuses `api: anthropic` models behind a custom base_url, so Anthropic
//! and Claude subscription providers are configured as `chat_completions`
//! and the gateway converts both directions here.

use serde_json::{json, Map, Value};

/// Default output budget when the client sent none.
fn default_max_tokens(model: &str) -> u64 {
    if model.contains("opus") { 32_000 } else { 64_000 }
}

fn thinking_budget(effort: &str) -> u64 {
    let raw = effort.trim().to_ascii_lowercase();
    if let Some(num_k) = raw.strip_suffix('k').and_then(|s| s.trim().parse::<u64>().ok()) {
        return num_k.saturating_mul(1024).max(1024);
    }
    if let Ok(tokens) = raw.parse::<u64>() {
        return tokens.max(1024);
    }
    match raw.as_str() {
        "minimal" | "low" => 2_048,
        "medium" => 8_192,
        "high" => 16_384,
        _ => 32_000,
    }
}

/// How a Claude model takes reasoning depth. Models up to the 4.5 generation take a thinking
/// budget in tokens. Every later one takes a named effort level and refuses a budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClaudeDepth {
    Budget,
    /// `mycli`: the model also takes `thinking: {type: "adaptive"}`.
    Effort { adaptive: bool },
}

pub fn claude_depth(model: &str) -> ClaudeDepth {
    let m = model.to_ascii_lowercase();
    // Another vendor's model behind an Anthropic-style endpoint keeps the budget form.
    let budget_era = ["claude-2", "claude-3", "haiku-4-5", "sonnet-4-5", "opus-4-1", "opus-4-0", "sonnet-4-0", "opus-4-2025", "sonnet-4-2025"];
    if !m.contains("claude") || budget_era.iter().any(|p| m.contains(p)) {
        return ClaudeDepth::Budget;
    }
    ClaudeDepth::Effort { adaptive: !m.contains("opus-4-5") }
}

/// The levels a Claude model accepts, lowest first, for when the provider has not listed them.
pub fn claude_effort_levels(model: &str) -> &'static [&'static str] {
    let m = model.to_ascii_lowercase();
    match claude_depth(model) {
        ClaudeDepth::Budget => &["none", "low", "medium", "high", "max"],
        ClaudeDepth::Effort { .. } if m.contains("opus-4-5") => &["low", "medium", "high"],
        ClaudeDepth::Effort { .. } if m.contains("-4-6") => &["low", "medium", "high", "max"],
        ClaudeDepth::Effort { .. } => &["low", "medium", "high", "xhigh", "max"],
    }
}

/// The effort level to send for what the person chose. A level this model lacks becomes the
/// nearest lower one it has, and a typed token budget becomes the level of about that size, so
/// a model switch never costs a refused request. A word we do not know is passed on unchanged.
fn named_level(effort: &str, model: &str) -> String {
    const ORDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
    let raw = effort.trim().to_ascii_lowercase();
    let numeric = raw.strip_suffix('k').unwrap_or(&raw).trim().parse::<u64>().is_ok();
    let wanted = if numeric {
        match thinking_budget(&raw) {
            0..=2_048 => "low",
            2_049..=8_192 => "medium",
            8_193..=16_384 => "high",
            16_385..=32_000 => "xhigh",
            _ => "max",
        }
    } else if raw == "minimal" {
        "low"
    } else {
        raw.as_str()
    };
    let Some(rank) = ORDER.iter().position(|l| *l == wanted) else { return raw };
    let have = claude_effort_levels(model);
    ORDER[..=rank].iter().rev().find(|l| have.contains(*l)).or_else(|| have.first()).map(|l| l.to_string()).unwrap_or(raw)
}

/// Apply an active session effort to a request body before translation/forwarding.
pub fn inject_effort(body: &mut Value, effort: Option<&str>, is_responses_api: bool) {
    let Some(obj) = body.as_object_mut() else { return };
    let Some(eff) = effort.map(str::trim).filter(|e| !e.is_empty()) else { return };
    if eff.eq_ignore_ascii_case("default") || eff.eq_ignore_ascii_case("auto") {
        obj.remove("reasoning_effort");
        obj.remove("reasoning");
        obj.remove("thinking");
        return;
    }
    if is_responses_api {
        obj.remove("reasoning_effort");
        if eff.eq_ignore_ascii_case("none") || eff.eq_ignore_ascii_case("off") {
            obj.remove("reasoning");
        } else {
            obj.insert("reasoning".into(), json!({ "effort": eff }));
        }
    } else {
        obj.insert("reasoning_effort".into(), json!(eff));
    }
}

/// Strip reasoning/thinking fields from an outgoing JSON body if the upstream model rejected them.
/// Returns `true` if any reasoning field was present and removed.
pub fn strip_effort(body: &mut Value) -> bool {
    let Some(obj) = body.as_object_mut() else { return false };
    let a = obj.remove("reasoning_effort").is_some();
    let b = obj.remove("reasoning").is_some();
    let c = obj.remove("thinking").is_some();
    let d = obj.get_mut("output_config").and_then(Value::as_object_mut).is_some_and(|o| o.remove("effort").is_some());
    if obj.get("output_config").and_then(Value::as_object).is_some_and(|o| o.is_empty()) {
        obj.remove("output_config");
    }
    a || b || c || d
}

/// Inspect an upstream HTTP 400 error body to see if it rejected `reasoning_effort`,
/// `reasoning`, or `thinking`. Returns `Some(allowed_levels)` if it was an effort error:
/// - non-empty Vec when the error message enumerates supported values (e.g. `'low', 'medium', 'high'`)
/// - empty Vec when the model does not support reasoning effort at all.
pub fn parse_effort_rejection(status: u16, body: &str) -> Option<Vec<String>> {
    if status != 400 && status != 422 {
        return None;
    }
    let low = body.to_ascii_lowercase();
    let mentions_effort = low.contains("reasoning_effort")
        || low.contains("reasoning.effort")
        || low.contains("thinking")
        || low.contains("budget_tokens")
        || low.contains("output_config")
        || (low.contains("reasoning") && (low.contains("effort") || low.contains("unsupported") || low.contains("not support")));
    if !mentions_effort {
        return None;
    }
    // Extract single-quoted enum candidates if the error lists supported values
    // e.g. "Supported values are: 'low', 'medium', and 'high'."
    if low.contains("supported values") || low.contains("valid values") || low.contains("permitted") || low.contains("must be one of") || low.contains("should be") {
        let mut levels = Vec::new();
        let tail = body
            .find("upported values")
            .or_else(|| body.find("alid values"))
            .or_else(|| body.find("ermitted"))
            .or_else(|| body.find("one of"))
            .or_else(|| body.find("hould be"))
            .map(|i| &body[i..])
            .unwrap_or(body);
        let mut rest = tail;
        while let Some(start) = rest.find('\'') {
            let after = &rest[start + 1..];
            if let Some(end) = after.find('\'') {
                let candidate = after[..end].trim().to_ascii_lowercase();
                if !candidate.is_empty()
                    && candidate.len() <= 24
                    && candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && !levels.contains(&candidate)
                {
                    levels.push(candidate);
                }
                rest = &after[end + 1..];
            } else {
                break;
            }
        }
        if !levels.is_empty() {
            return Some(levels);
        }
    }
    Some(vec![])
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn image_block(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(',') {
            let media = meta.split(';').next().filter(|m| !m.is_empty()).unwrap_or("image/png");
            return json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}});
        }
    }
    json!({"type":"image","source":{"type":"url","url":url}})
}

fn user_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"type":"text","text":s})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let t = p.get("text").and_then(Value::as_str).unwrap_or("");
                    (!t.is_empty()).then(|| json!({"type":"text","text":t}))
                }
                Some("image_url") => {
                    let u = p.pointer("/image_url/url").or_else(|| p.get("image_url"))?.as_str()?;
                    Some(image_block(u))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn push_msg(out: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if let (true, Some(arr)) = (last["role"] == role, last["content"].as_array_mut()) {
            arr.extend(blocks);
            if role == "user" {
                // tool_result blocks must lead the user turn.
                arr.sort_by_key(|b| b["type"] != "tool_result");
            }
            return;
        }
    }
    out.push(json!({"role": role, "content": blocks}));
}

/// Translate a chat.completions request body to an Anthropic Messages body.
pub fn request(body: &Value, flatten_system: bool) -> Value {
    let model = body["model"].as_str().unwrap_or("").to_string();
    let mut system: Vec<String> = vec![];
    let mut msgs: Vec<Value> = vec![];
    for m in body["messages"].as_array().cloned().unwrap_or_default() {
        match m["role"].as_str().unwrap_or("") {
            "system" | "developer" => {
                let t = text_of(&m["content"]);
                if !t.is_empty() {
                    system.push(t);
                }
            }
            "assistant" => {
                let mut blocks = vec![];
                let t = text_of(&m["content"]);
                if !t.is_empty() {
                    blocks.push(json!({"type":"text","text":t}));
                }
                for tc in m["tool_calls"].as_array().cloned().unwrap_or_default() {
                    let args = tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("");
                    let input = serde_json::from_str::<Value>(args).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
                    blocks.push(json!({"type":"tool_use","id":tc["id"],"name":tc.pointer("/function/name").cloned().unwrap_or(json!("")),"input":input}));
                }
                push_msg(&mut msgs, "assistant", blocks);
            }
            "tool" => {
                let t = text_of(&m["content"]);
                let block = json!({"type":"tool_result","tool_use_id":m["tool_call_id"],"content":if t.is_empty() { "(no output)".to_string() } else { t }});
                push_msg(&mut msgs, "user", vec![block]);
            }
            _ => push_msg(&mut msgs, "user", user_blocks(&m["content"])),
        }
    }
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), json!(msgs));
    if !system.is_empty() {
        if flatten_system {
            out.insert("system".into(), json!(system.join("\n\n")));
        } else {
            out.insert("system".into(), json!(system.iter().map(|s| json!({"type":"text","text":s})).collect::<Vec<_>>()));
        }
    }
    let mut max = body.get("max_tokens").or(body.get("max_completion_tokens")).and_then(Value::as_u64).unwrap_or_else(|| default_max_tokens(&model));
    let depth = claude_depth(&model);
    let mut thinking = false;
    if let Some(e) = body["reasoning_effort"].as_str().filter(|e| !matches!(*e, "none" | "off")) {
        match depth {
            ClaudeDepth::Budget => {
                let mut budget = thinking_budget(e);
                if budget + 1024 > max {
                    budget = max.saturating_sub(1024).min(budget);
                }
                if budget >= 1024 {
                    out.insert("thinking".into(), json!({"type":"enabled","budget_tokens":budget}));
                    thinking = true;
                }
            }
            ClaudeDepth::Effort { adaptive } => {
                out.insert("output_config".into(), json!({ "effort": named_level(e, &model) }));
                if adaptive {
                    out.insert("thinking".into(), json!({"type": "adaptive"}));
                }
                thinking = true;
            }
        }
    }
    max = max.max(1);
    out.insert("max_tokens".into(), json!(max));
    // A model that takes an effort level refuses sampling settings.
    if !thinking && depth == ClaudeDepth::Budget {
        if let Some(t) = body.get("temperature").filter(|t| t.is_number()) {
            out.insert("temperature".into(), t.clone());
        }
    }
    if let Some(s) = body.get("stop") {
        let seqs: Vec<Value> = match s {
            Value::String(x) => vec![json!(x)],
            Value::Array(a) => a.iter().filter(|x| x.is_string()).cloned().collect(),
            _ => vec![],
        };
        if !seqs.is_empty() {
            out.insert("stop_sequences".into(), json!(seqs));
        }
    }
    if body["stream"].as_bool().unwrap_or(false) {
        out.insert("stream".into(), json!(true));
    }
    let choice = &body["tool_choice"];
    if choice != "none" {
        if let Some(tools) = body["tools"].as_array().filter(|t| !t.is_empty()) {
            let t: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let f = &t["function"];
                    json!({"name":f["name"],"description":f.get("description").cloned().unwrap_or(json!("")),
                           "input_schema":f.get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}}))})
                })
                .collect();
            out.insert("tools".into(), json!(t));
            match choice {
                Value::String(s) if s == "required" => {
                    if !thinking {
                        out.insert("tool_choice".into(), json!({"type":"any"}));
                    }
                }
                Value::Object(_) => {
                    if let (Some(n), false) = (choice.pointer("/function/name"), thinking) {
                        out.insert("tool_choice".into(), json!({"type":"tool","name":n}));
                    }
                }
                _ => {}
            }
        }
    }
    Value::Object(out)
}

pub fn finish_reason(stop: &str) -> &'static str {
    match stop {
        "max_tokens" | "model_context_window_exceeded" => "length",
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

fn usage_json(u: &Value) -> Value {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cached = g("cache_read_input_tokens");
    let p = g("input_tokens") + cached + g("cache_creation_input_tokens");
    let mut out = json!({"prompt_tokens":p,"completion_tokens":g("output_tokens"),"total_tokens":p + g("output_tokens")});
    if cached > 0 {
        out["prompt_tokens_details"] = json!({"cached_tokens": cached});
    }
    out
}

pub const CACHE_BREAK_MARKER: &str = "<!-- PHILIUM_CACHE_BREAK -->";

fn mark_block_ephemeral(content: &mut Value) {
    let ep = json!({"type": "ephemeral"});
    match content {
        Value::String(s) if !s.is_empty() => {
            if let Some((shared, tail)) = s.split_once(CACHE_BREAK_MARKER) {
                let shared = shared.trim_end();
                let tail = tail.trim_start();
                if !shared.is_empty() && !tail.is_empty() {
                    *content = json!([
                        {"type": "text", "text": shared, "cache_control": ep},
                        {"type": "text", "text": tail}
                    ]);
                    return;
                }
            }
            *content = json!([{"type": "text", "text": s.clone(), "cache_control": ep}]);
        }
        Value::Array(blocks) => {
            if let Some(last) = blocks.iter_mut().rev().find(|b| b.is_object()) {
                last["cache_control"] = ep;
            }
        }
        _ => {}
    }
}

/// Inject up to 4 Anthropic ephemeral cache breakpoints (last tool, last system
/// block, and last 2 messages) for `/messages` or Claude `/chat/completions`.
pub fn inject_cache_breakpoints(v: &mut Value, anthropic: bool) {
    if !anthropic {
        let is_claude = v["model"].as_str().is_some_and(|m| m.to_ascii_lowercase().contains("claude"));
        if !is_claude {
            return;
        }
        if let Some(msgs) = v.get_mut("messages").and_then(Value::as_array_mut) {
            if let Some(sys) = msgs.iter_mut().rev().find(|m| matches!(m["role"].as_str(), Some("system" | "developer"))) {
                mark_block_ephemeral(&mut sys["content"]);
            }
            for m in msgs.iter_mut().rev().filter(|m| !matches!(m["role"].as_str(), Some("system" | "developer"))).take(2) {
                mark_block_ephemeral(&mut m["content"]);
            }
        }
        return;
    }
    if let Some(last_tool) = v.get_mut("tools").and_then(Value::as_array_mut).and_then(|t| t.last_mut()).filter(|t| t.is_object()) {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
    // A subscription body passes through here twice, with identity blocks added in between.
    // The shared prefix marked the first time stays the only system breakpoint.
    let system_marked = v["system"].as_array().is_some_and(|a| a.iter().any(|b| b.get("cache_control").is_some()));
    if !system_marked {
        if let Some(sys) = v.get_mut("system") {
            mark_block_ephemeral(sys);
        }
    }
    if let Some(msgs) = v.get_mut("messages").and_then(Value::as_array_mut) {
        for m in msgs.iter_mut().rev().take(2) {
            mark_block_ephemeral(&mut m["content"]);
        }
    }
    cap_cache_breakpoints(v);
}

/// Anthropic refuses a request with more than four breakpoints. Keep the tool list first, then
/// system blocks in order, then the newest messages; an older mark beyond that is dropped.
fn cap_cache_breakpoints(v: &mut Value) {
    fn keep(b: &mut Value, left: &mut usize) {
        if b.get("cache_control").is_some() {
            if *left > 0 {
                *left -= 1;
            } else if let Some(o) = b.as_object_mut() {
                o.remove("cache_control");
            }
        }
    }
    // `v["key"]` on an object inserts `key: null` when it is missing, and Anthropic refuses
    // `tools: null`. Look keys up with `get_mut` so a request without tools stays without them.
    let mut left = 4usize;
    if let Some(tools) = v.get_mut("tools").and_then(Value::as_array_mut) {
        for b in tools.iter_mut().rev() {
            keep(b, &mut left);
        }
    }
    if let Some(system) = v.get_mut("system").and_then(Value::as_array_mut) {
        for b in system.iter_mut() {
            keep(b, &mut left);
        }
    }
    if let Some(msgs) = v.get_mut("messages").and_then(Value::as_array_mut) {
        for m in msgs.iter_mut().rev() {
            if let Some(blocks) = m["content"].as_array_mut() {
                for b in blocks.iter_mut().rev() {
                    keep(b, &mut left);
                }
            }
        }
    }
}

#[cfg(test)]
fn count_cache_breakpoints(v: &Value) -> usize {
    match v {
        Value::Object(o) => usize::from(o.contains_key("cache_control")) + o.values().map(count_cache_breakpoints).sum::<usize>(),
        Value::Array(a) => a.iter().map(count_cache_breakpoints).sum(),
        _ => 0,
    }
}

/// Translate a whole (non-streaming) Anthropic message to chat.completion.
pub fn response(v: &Value) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = vec![];
    for b in v["content"].as_array().cloned().unwrap_or_default() {
        match b["type"].as_str() {
            Some("text") => text.push_str(b["text"].as_str().unwrap_or("")),
            Some("thinking") => reasoning.push_str(b["thinking"].as_str().unwrap_or("")),
            Some("tool_use") => calls.push(json!({"id":b["id"],"type":"function","function":{"name":b["name"],"arguments":b["input"].to_string()}})),
            _ => {}
        }
    }
    let mut msg = json!({"role":"assistant","content":text});
    if !reasoning.is_empty() {
        msg["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        msg["tool_calls"] = json!(calls);
    }
    json!({"id":v["id"],"object":"chat.completion","model":v["model"],
           "choices":[{"index":0,"message":msg,"finish_reason":finish_reason(v["stop_reason"].as_str().unwrap_or("end_turn"))}],
           "usage":usage_json(&v["usage"])})
}

/// Incremental SSE translator: feed upstream bytes, get downstream bytes.
#[derive(Default)]
pub struct StreamTranslator {
    buf: String,
    id: String,
    model: String,
    usage_in: Value,
    tool_index: i64,
    /// Anthropic block index -> OpenAI tool index.
    tool_of_block: std::collections::HashMap<u64, i64>,
    finish: Option<&'static str>,
    out_tokens: u64,
    done: bool,
}

impl StreamTranslator {
    pub fn new() -> Self {
        Self { tool_index: -1, usage_in: json!({}), ..Default::default() }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        let v = json!({"id":self.id,"object":"chat.completion.chunk","model":self.model,
                       "choices":[{"index":0,"delta":delta,"finish_reason":finish}]});
        format!("data: {v}\n\n")
    }

    pub fn feed(&mut self, bytes: &[u8]) -> String {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = String::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            if let Some(d) = line.trim().strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                    out.push_str(&self.event(&v));
                }
            }
        }
        out
    }

    fn event(&mut self, v: &Value) -> String {
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.id = v.pointer("/message/id").and_then(Value::as_str).unwrap_or("chatcmpl").to_string();
                self.model = v.pointer("/message/model").and_then(Value::as_str).unwrap_or("").to_string();
                self.usage_in = v.pointer("/message/usage").cloned().unwrap_or(json!({}));
                self.chunk(json!({"role":"assistant","content":""}), None)
            }
            "content_block_start" => {
                let b = &v["content_block"];
                if b["type"] == "tool_use" {
                    self.tool_index += 1;
                    self.tool_of_block.insert(v["index"].as_u64().unwrap_or(0), self.tool_index);
                    self.chunk(json!({"tool_calls":[{"index":self.tool_index,"id":b["id"],"type":"function",
                        "function":{"name":b["name"],"arguments":""}}]}), None)
                } else {
                    String::new()
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => self.chunk(json!({"content":d["text"]}), None),
                    Some("thinking_delta") => self.chunk(json!({"reasoning_content":d["thinking"]}), None),
                    Some("input_json_delta") => {
                        let idx = self.tool_of_block.get(&v["index"].as_u64().unwrap_or(0)).copied().unwrap_or(self.tool_index.max(0));
                        self.chunk(json!({"tool_calls":[{"index":idx,"function":{"arguments":d["partial_json"]}}]}), None)
                    }
                    _ => String::new(),
                }
            }
            "message_delta" => {
                self.out_tokens = v.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(self.out_tokens);
                if let Some(cr) = v.pointer("/usage/cache_read_input_tokens").and_then(Value::as_u64) {
                    self.usage_in["cache_read_input_tokens"] = json!(cr);
                }
                let f = finish_reason(v.pointer("/delta/stop_reason").and_then(Value::as_str).unwrap_or("end_turn"));
                self.finish = Some(f);
                self.chunk(json!({}), Some(f))
            }
            "message_stop" => self.end(),
            "error" => {
                self.done = true;
                format!("data: {}\n\ndata: [DONE]\n\n", json!({"error":{"type":v["error"]["type"],"message":v["error"]["message"]}}))
            }
            _ => String::new(),
        }
    }

    /// Final usage chunk + [DONE]. Safe to call twice.
    pub fn end(&mut self) -> String {
        if self.done {
            return String::new();
        }
        self.done = true;
        let mut u = self.usage_in.clone();
        u["output_tokens"] = json!(self.out_tokens);
        let v = json!({"id":self.id,"object":"chat.completion.chunk","model":self.model,"choices":[],"usage":usage_json(&u)});
        format!("data: {v}\n\ndata: [DONE]\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_request() {
        // A 4.5-generation model: the last Claude generation that accepts a sampling setting.
        let b = json!({"model":"claude-sonnet-4-5-20250929","stream":true,"stop":"END","temperature":0.2,
          "messages":[
            {"role":"system","content":"be brief"},{"role":"developer","content":"dev"},
            {"role":"user","content":[{"type":"text","text":"look"},
              {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,AAAA"}},
              {"type":"image_url","image_url":{"url":"https://x/y.png"}}]},
            {"role":"assistant","content":"ok","tool_calls":[{"id":"c1","type":"function","function":{"name":"read","arguments":"{\"p\":1}"}},
              {"id":"c2","type":"function","function":{"name":"glob","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"c1","name":"read","content":"file"},
            {"role":"tool","tool_call_id":"c2","name":"glob","content":"a.rs"},
            {"role":"user","content":"thanks"}],
          "tools":[{"type":"function","function":{"name":"read","description":"d","parameters":{"type":"object","properties":{"p":{"type":"number"}}}}}]});
        let r = request(&b, true);
        assert_eq!(r["system"], "be brief\n\ndev");
        assert_eq!(r["max_tokens"], 64000);
        assert_eq!(r["stop_sequences"], json!(["END"]));
        assert_eq!(r["temperature"], 0.2);
        assert_eq!(r["stream"], true);
        let m = r["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3, "tool results merge with following user text");
        assert_eq!(m[0]["content"][1]["source"], json!({"type":"base64","media_type":"image/jpeg","data":"AAAA"}));
        assert_eq!(m[0]["content"][2]["source"]["type"], "url");
        assert_eq!(m[1]["content"][0], json!({"type":"text","text":"ok"}));
        assert_eq!(m[1]["content"][1], json!({"type":"tool_use","id":"c1","name":"read","input":{"p":1}}));
        let last = m[2]["content"].as_array().unwrap();
        assert_eq!((last[0]["type"].as_str(), last[1]["tool_use_id"].as_str(), last[2]["type"].as_str()), (Some("tool_result"), Some("c2"), Some("text")));
        assert_eq!(r["tools"][0]["input_schema"]["properties"]["p"]["type"], "number");
        let blocks = request(&b, false);
        assert_eq!(blocks["system"][1]["text"], "dev");
    }

    /// Memory observation and dreaming send no tools. Anthropic refuses `tools: null`, so marking
    /// cache breakpoints must not add a key the request did not have.
    #[test]
    fn cache_breakpoints_add_no_missing_keys() {
        let mut v = request(&json!({"model": "claude-haiku-5-5", "stream": false, "temperature": 0, "max_tokens": 50,
            "messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "u"}]}), true);
        crate::oauth::shape_claude_body(&mut v);
        inject_cache_breakpoints(&mut v, true);
        inject_cache_breakpoints(&mut v, true);
        assert!(v.get("tools").is_none(), "{v}");
        assert!(v["system"].is_array() && v["messages"].is_array());
    }

    #[test]
    fn thinking_from_effort() {
        // A model of the 4.5 generation takes a thinking budget.
        let b = json!({"model":"claude-haiku-4-5-20251001","temperature":0.5,"reasoning_effort":"high","messages":[{"role":"user","content":"hi"}]});
        let r = request(&b, true);
        assert_eq!(r["thinking"], json!({"type":"enabled","budget_tokens":16384}));
        assert!(r.get("temperature").is_none());
        assert!(r.get("output_config").is_none());
        assert_eq!(r["max_tokens"], 64000);
    }

    /// Current Claude models refuse a thinking budget and sampling settings. They take a named
    /// effort level, and each model has its own set of levels.
    #[test]
    fn current_claude_models_take_an_effort_level() {
        let ask = |model: &str, effort: &str| request(&json!({"model": model, "temperature": 0.5, "reasoning_effort": effort, "messages": [{"role": "user", "content": "hi"}]}), true);
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-2", "claude-haiku-5-5", "claude-opus-4-8"] {
            let r = ask(model, "xhigh");
            assert_eq!(r["output_config"], json!({"effort": "xhigh"}), "{model}");
            assert_eq!(r["thinking"], json!({"type": "adaptive"}), "{model}");
            assert!(r["thinking"].get("budget_tokens").is_none() && r.get("temperature").is_none(), "{model}");
        }
        // A level the model lacks becomes the nearest lower one it has.
        assert_eq!(ask("claude-sonnet-4-6", "xhigh")["output_config"]["effort"], "high");
        assert_eq!(ask("claude-opus-4-5-20251101", "max")["output_config"]["effort"], "high");
        assert!(ask("claude-opus-4-5-20251101", "high").get("thinking").is_none());
        // A typed budget becomes the level of about that size.
        assert_eq!(ask("claude-opus-5-5", "16k")["output_config"]["effort"], "high");
        assert_eq!(ask("claude-opus-5-5", "2000")["output_config"]["effort"], "low");
        // Auto and None send nothing, so the provider's own default applies; no sampling either.
        for effort in ["none", "off"] {
            let r = ask("claude-opus-5-5", effort);
            assert!(r.get("output_config").is_none() && r.get("thinking").is_none() && r.get("temperature").is_none());
        }
        let plain = request(&json!({"model": "claude-opus-5-5", "temperature": 0.5, "messages": [{"role": "user", "content": "hi"}]}), true);
        assert!(plain.get("output_config").is_none() && plain.get("temperature").is_none());
        // Another vendor's model behind an Anthropic-style endpoint keeps the budget form.
        assert_eq!(ask("kimi-k3", "high")["thinking"]["budget_tokens"], 16_384);

        assert_eq!(claude_effort_levels("claude-opus-5-5"), ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(claude_effort_levels("claude-sonnet-4-6"), ["low", "medium", "high", "max"]);
        assert_eq!(claude_effort_levels("claude-haiku-4-5-20251001"), ["none", "low", "medium", "high", "max"]);

        // Withdrawing a refused effort removes it in this form too and keeps other output settings.
        let mut sent = ask("claude-opus-5-5", "max");
        sent["output_config"]["format"] = json!({"type": "json_schema"});
        assert!(strip_effort(&mut sent));
        assert_eq!(sent["output_config"], json!({"format": {"type": "json_schema"}}));
        assert!(sent.get("thinking").is_none());
        let refusal = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.effort: Input should be 'low', 'medium' or 'high'"}}"#;
        assert_eq!(parse_effort_rejection(400, refusal), Some(vec!["low".into(), "medium".into(), "high".into()]));
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    fn run(events: &[Value], split: usize) -> Vec<Value> {
        let text = sse(events);
        let mut t = StreamTranslator::new();
        let mut out = String::new();
        for c in text.as_bytes().chunks(split) {
            out.push_str(&t.feed(c));
        }
        out.push_str(&t.end());
        assert!(out.ends_with("data: [DONE]\n\n"));
        out.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect()
    }

    #[test]
    fn streams_text_thinking_tool_and_usage() {
        let ev = [
            json!({"type":"message_start","message":{"id":"msg_1","model":"claude-x","usage":{"input_tokens":40,"cache_read_input_tokens":10,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
            json!({"type":"message_stop"}),
        ];
        for split in [7, 1000] {
            let c = run(&ev, split);
            let content: String = c.iter().filter_map(|x| x.pointer("/choices/0/delta/content")?.as_str()).collect();
            assert_eq!(content, "Hello");
            let reasoning: String = c.iter().filter_map(|x| x.pointer("/choices/0/delta/reasoning_content")?.as_str()).collect();
            assert_eq!(reasoning, "hmm");
            let start = c.iter().find_map(|x| x.pointer("/choices/0/delta/tool_calls/0/id")).unwrap();
            assert_eq!(start, "toolu_1");
            let args: String = c.iter().filter_map(|x| x.pointer("/choices/0/delta/tool_calls/0/function/arguments")?.as_str()).collect();
            assert_eq!(args, "{\"path\":\"a.rs\"}");
            assert!(c.iter().all(|x| x.pointer("/choices/0/delta/tool_calls/0/index").is_none_or(|i| i == 0)));
            assert!(c.iter().any(|x| x.pointer("/choices/0/finish_reason") == Some(&json!("tool_calls"))));
            let u = c.last().unwrap();
            assert_eq!(u["choices"], json!([]));
            assert_eq!(u["usage"], json!({"prompt_tokens":50,"completion_tokens":9,"total_tokens":59,"prompt_tokens_details":{"cached_tokens":10}}));
        }
    }

    #[test]
    fn injects_claude_cache_breakpoints() {
        let mut ant = request(&json!({
            "model": "claude-sonnet-5-5",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "u1"},
                {"role": "assistant", "content": "a1"},
                {"role": "user", "content": "u2"}
            ],
            "tools": [{"type": "function", "function": {"name": "t1", "parameters": {"type": "object"}}}]
        }), true);
        inject_cache_breakpoints(&mut ant, true);
        assert_eq!(ant["tools"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(ant["system"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert!(ant["messages"][0]["content"][0].get("cache_control").is_none());
        assert_eq!(ant["messages"][1]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(ant["messages"][2]["content"][0]["cache_control"], json!({"type": "ephemeral"}));

        let mut oai = json!({
            "model": "claude-fable-5-2",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "u1"},
                {"role": "assistant", "content": "a1"},
                {"role": "user", "content": "u2"}
            ]
        });
        inject_cache_breakpoints(&mut oai, false);
        assert_eq!(oai["messages"][0]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(oai["messages"][1]["content"], "u1");
        assert_eq!(oai["messages"][2]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(oai["messages"][3]["content"][0]["cache_control"], json!({"type": "ephemeral"}));

        let mut split_ant = request(&json!({
            "model": "claude-sonnet-5-5",
            "messages": [
                {"role": "system", "content": format!("SHARED_REPO_PREFIX\n{CACHE_BREAK_MARKER}\nAgent: lion (orchestrator)")},
                {"role": "user", "content": "go"}
            ]
        }), true);
        inject_cache_breakpoints(&mut split_ant, true);
        assert_eq!(split_ant["system"][0]["text"], "SHARED_REPO_PREFIX");
        assert_eq!(split_ant["system"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(split_ant["system"][1]["text"], "Agent: lion (orchestrator)");
        assert!(split_ant["system"][1].get("cache_control").is_none());
    }

    /// The subscription path marks, adds its identity blocks, then marks again. An agent prompt
    /// with a shared prefix used to carry five breakpoints from the second message on, and the
    /// provider refused every such request.
    #[test]
    fn a_subscription_body_never_exceeds_four_breakpoints() {
        let mut ant = request(&json!({
            "model": "claude-sonnet-5-5",
            "messages": [
                {"role": "system", "content": format!("SHARED_REPO_PREFIX\n{CACHE_BREAK_MARKER}\nAgent: lead (orchestrator)")},
                {"role": "user", "content": "u1"},
                {"role": "assistant", "content": "a1"},
                {"role": "user", "content": "u2"}
            ],
            "tools": [{"type": "function", "function": {"name": "t1", "parameters": {"type": "object"}}}]
        }), true);
        inject_cache_breakpoints(&mut ant, true);
        crate::oauth::shape_claude_body(&mut ant);
        inject_cache_breakpoints(&mut ant, true);
        assert_eq!(count_cache_breakpoints(&ant), 4, "{ant}");
        let marked: Vec<_> = ant["system"].as_array().unwrap().iter().filter(|b| b.get("cache_control").is_some()).collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0]["text"], "SHARED_REPO_PREFIX");
        assert!(ant["messages"][2]["content"][0].get("cache_control").is_some());

        // A caller that brings its own marks is trimmed from the oldest message, never refused.
        let turns: Vec<Value> = (0..6)
            .map(|i| json!({"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": [{"type": "text", "text": format!("m{i}"), "cache_control": {"type": "ephemeral"}}]}))
            .collect();
        let mut many = json!({"system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}], "messages": turns});
        inject_cache_breakpoints(&mut many, true);
        assert_eq!(count_cache_breakpoints(&many), 4);
        assert!(many["messages"][5]["content"][0].get("cache_control").is_some());
        assert!(many["messages"][0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn stop_reasons_and_whole_response() {
        assert_eq!(finish_reason("end_turn"), "stop");
        assert_eq!(finish_reason("max_tokens"), "length");
        assert_eq!(finish_reason("tool_use"), "tool_calls");
        let r = response(&json!({"id":"m","model":"x","stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":4},
            "content":[{"type":"thinking","thinking":"t"},{"type":"text","text":"hi"},{"type":"tool_use","id":"u","name":"n","input":{"a":1}}]}));
        assert_eq!(r["choices"][0]["message"]["content"], "hi");
        assert_eq!(r["choices"][0]["message"]["reasoning_content"], "t");
        assert_eq!(r["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(r["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(r["usage"]["total_tokens"], 7);
    }

    #[test]
    fn numeric_thinking_budgets_preserve_output_headroom() {
        for (effort, max_tokens, expected) in [
            ("16k", 64_000, Some(16_384)),
            (" 16K ", 64_000, Some(16_384)),
            ("16384", 64_000, Some(16_384)),
            ("high", 64_000, Some(16_384)),
            ("0k", 64_000, Some(1_024)),
            ("16k", 4_096, Some(3_072)),
            ("16k", 2_048, Some(1_024)),
            ("16k", 1_024, None),
        ] {
            let translated = request(&json!({
                "model": "claude-haiku-4-5-20251001",
                "reasoning_effort": effort,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": "hi"}]
            }), true);
            assert_eq!(translated["thinking"]["budget_tokens"].as_u64(), expected, "{effort}, max={max_tokens}");
            assert_eq!(translated["max_tokens"], max_tokens);
        }
    }

    #[test]
    fn unrelated_enum_rejections_do_not_change_effort_capabilities() {
        let unrelated = r#"{"error":{"message":"Invalid value: 'xhigh'. Supported values are: 'low', 'medium', and 'high'."}}"#;
        assert_eq!(parse_effort_rejection(400, unrelated), None);
        let effort = r#"{"error":{"param":"reasoning_effort","message":"Supported values are: 'low', 'high'."}}"#;
        assert_eq!(parse_effort_rejection(422, effort), Some(vec!["low".into(), "high".into()]));
        assert_eq!(parse_effort_rejection(500, effort), None);
    }

    #[test]
    fn dynamic_effort_injection_and_rejection_parsing() {
        // Dynamic token budgets & named levels
        let ant_16k = request(&json!({
            "model": "claude-haiku-4-5-20251001",
            "reasoning_effort": "16k",
            "messages": [{"role": "user", "content": "hi"}]
        }), true);
        // `k` uses binary token units, matching the named high budget (16 * 1024).
        assert_eq!(ant_16k["thinking"]["budget_tokens"], 16_384);
        // Thinking reserves space within the model's normal output budget.
        assert_eq!(ant_16k["max_tokens"], 64_000);

        let ant_num = request(&json!({
            "model": "claude-haiku-4-5-20251001",
            "reasoning_effort": "4096",
            "messages": [{"role": "user", "content": "hi"}]
        }), true);
        assert_eq!(ant_num["thinking"]["budget_tokens"], 4096);

        // Gateway effort injection + strip
        let mut chat = json!({"model": "gpt-6-luna", "messages": []});
        inject_effort(&mut chat, Some("xhigh"), false);
        assert_eq!(chat["reasoning_effort"], "xhigh");
        assert!(strip_effort(&mut chat));
        assert!(chat.get("reasoning_effort").is_none());

        let mut resp = json!({"model": "gpt-6-luna", "input": "hi"});
        inject_effort(&mut resp, Some("high"), true);
        assert_eq!(resp["reasoning"]["effort"], "high");
        assert!(strip_effort(&mut resp));
        assert!(resp.get("reasoning").is_none());

        // Upstream 400 error parsing: enumerated supported values
        let enum_err = r#"{"error":{"param":"reasoning_effort","message":"Invalid value: 'xhigh'. Supported values are: 'low', 'medium', and 'high'."}}"#;
        assert_eq!(
            parse_effort_rejection(400, enum_err),
            Some(vec!["low".into(), "medium".into(), "high".into()])
        );

        // Upstream 400 error parsing: parameter completely unsupported by model
        let unsup_err = r#"{"error":{"message":"Unrecognized request argument supplied: reasoning_effort"}}"#;
        assert_eq!(parse_effort_rejection(400, unsup_err), Some(vec![]));
    }
}
