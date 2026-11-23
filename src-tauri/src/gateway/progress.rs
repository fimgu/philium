//! Live stream progress: sniff SSE bytes flowing through the gateway so the UI can show
//! thinking and tool-call argument streaming, not only visible text.
//! Metadata only: block kind, tool name, a file path argument, byte and approx token counts.

use serde::Serialize;
use serde_json::Value;

const ARG_CAP: usize = 4096;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct StreamProgress {
    pub id: u64,
    pub agent: Option<String>,
    /// thinking | text | tool | "" (unknown yet)
    pub phase: &'static str,
    pub tool: Option<String>,
    pub file: Option<String>,
    pub bytes: usize,
    /// Approximate output tokens (delta chars / 4).
    pub tokens: u64,
    pub done: bool,
    /// Set when the gateway gave up on an idle upstream.
    pub stalled: bool,
}

#[derive(Default)]
pub struct Sniffer {
    pub p: StreamProgress,
    line: String,
    args: String,
    chars: u64,
}

impl Sniffer {
    pub fn new(id: u64, agent: Option<String>) -> Self {
        Self { p: StreamProgress { id, agent, ..Default::default() }, ..Default::default() }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.p.bytes += chunk.len();
        self.line.push_str(&String::from_utf8_lossy(chunk));
        while let Some(pos) = self.line.find('\n') {
            let l: String = self.line.drain(..=pos).collect();
            if let Some(d) = l.trim_end().strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                    self.event(&v);
                }
            }
        }
        if self.line.len() > 1 << 20 {
            self.line.clear();
        }
        self.p.tokens = self.chars / 4;
    }

    fn start_tool(&mut self, name: Option<&str>) {
        self.p.phase = "tool";
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            self.p.tool = Some(n.to_string());
        }
        self.p.file = None;
        self.args.clear();
    }

    fn delta(&mut self, phase: &'static str, s: &str) {
        if phase != "tool" && self.p.phase == "tool" {
            self.p.tool = None;
            self.p.file = None;
        }
        self.p.phase = phase;
        self.chars += s.chars().count() as u64;
        if phase == "tool" {
            if self.args.len() < ARG_CAP {
                self.args.push_str(s);
            }
            if self.p.file.is_none() {
                self.p.file = file_arg(&self.args);
            }
        }
    }

    fn event(&mut self, v: &Value) {
        let s = |x: &Value| x.as_str().map(str::to_string);
        match v["type"].as_str().unwrap_or("") {
            // Anthropic Messages.
            "content_block_start" => match v["content_block"]["type"].as_str() {
                Some("tool_use" | "server_tool_use") => self.start_tool(v["content_block"]["name"].as_str()),
                Some("thinking" | "redacted_thinking") => self.p.phase = "thinking",
                Some("text") => self.p.phase = "text",
                _ => {}
            },
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("thinking_delta") => self.delta("thinking", &s(&d["thinking"]).unwrap_or_default()),
                    Some("text_delta") => self.delta("text", &s(&d["text"]).unwrap_or_default()),
                    Some("input_json_delta") => self.delta("tool", &s(&d["partial_json"]).unwrap_or_default()),
                    _ => {}
                }
            }
            "message_stop" | "response.completed" => self.p.done = true,
            // OpenAI Responses.
            "response.output_item.added" => match v["item"]["type"].as_str() {
                Some("function_call" | "custom_tool_call") => self.start_tool(v["item"]["name"].as_str()),
                Some("reasoning") => self.p.phase = "thinking",
                _ => {}
            },
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                self.delta("tool", &s(&v["delta"]).unwrap_or_default())
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.delta("thinking", &s(&v["delta"]).unwrap_or_default())
            }
            "response.output_text.delta" => self.delta("text", &s(&v["delta"]).unwrap_or_default()),
            _ => {
                // OpenAI Chat Completions (also the translated Antigravity stream).
                let d = &v["choices"][0]["delta"];
                if let Some(tc) = d["tool_calls"].get(0) {
                    if let Some(n) = tc["function"]["name"].as_str().filter(|n| !n.is_empty()) {
                        self.start_tool(Some(n));
                    }
                    self.delta("tool", &s(&tc["function"]["arguments"]).unwrap_or_default());
                } else if let Some(r) = d["reasoning_content"].as_str().or(d["reasoning"].as_str()) {
                    self.delta("thinking", r);
                } else if let Some(t) = d["content"].as_str() {
                    self.delta("text", t);
                }
                if v["choices"][0]["finish_reason"].is_string() {
                    self.p.done = true;
                }
            }
        }
    }
}

/// First `path`-like string argument from partial JSON tool arguments.
fn file_arg(args: &str) -> Option<String> {
    for key in ["\"file_path\"", "\"path\"", "\"file\"", "\"filename\""] {
        let Some(i) = args.find(key) else { continue };
        let rest = args[i + key.len()..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        let end = rest.find('"')?;
        let p = &rest[..end];
        if !p.is_empty() {
            return Some(p.rsplit('/').next().unwrap_or(p).to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_tool_call_and_thinking() {
        let mut s = Sniffer::new(1, Some("a".into()));
        s.feed(b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"thinking\"}}\n\n");
        s.feed(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmmmmmmm\"}}\n\n");
        assert_eq!(s.p.phase, "thinking");
        assert_eq!(s.p.tokens, 2);
        s.feed(b"data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"name\":\"write\"}}\n\n");
        s.feed(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"file_pa\"}}\n");
        s.feed(b"\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"th\\\": \\\"src/a/b.rs\\\", \\\"content\"}}\n\n");
        assert_eq!((s.p.phase, s.p.tool.as_deref(), s.p.file.as_deref()), ("tool", Some("write"), Some("b.rs")));
        s.feed(b"data: {\"type\":\"message_stop\"}\n\n");
        assert!(s.p.done);
    }

    #[test]
    fn openai_chat_and_responses() {
        let mut s = Sniffer::new(1, None);
        s.feed(b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"edit\",\"arguments\":\"{\\\"path\\\":\\\"x.js\\\"\"}}]}}]}\n\n");
        assert_eq!((s.p.tool.as_deref(), s.p.file.as_deref()), (Some("edit"), Some("x.js")));
        let mut r = Sniffer::new(2, None);
        r.feed(b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"abcd\"}\n\n");
        assert_eq!(r.p.phase, "thinking");
        r.feed(b"data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"name\":\"shell\"}}\n\n");
        assert_eq!((r.p.phase, r.p.tool.as_deref()), ("tool", Some("shell")));
    }
}
