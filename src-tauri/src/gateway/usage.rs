//! Provider usage, independently of the display preview or stream progress.

use serde_json::Value;

// Limit telemetry buffering to one SSE event (or one non-streaming JSON body).
// Oversized events are ignored until the next event boundary. The limit does
// not affect how much of the upstream response is forwarded to the client.
const CAP: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached: Option<u64>,
}

pub(super) struct Capture {
    sse: bool,
    buf: Vec<u8>,
    event_len: usize,
    dropping: bool,
    after_cr: bool,
    line_has_bytes: bool,
    data: Vec<u8>,
    usage: Usage,
    // Anthropic's input count excludes cache reads and writes; its two usage
    // events may supply different subsets of these counters.
    anthropic_input: Option<u64>,
    anthropic_writes: Option<u64>,
}

impl Capture {
    pub fn new(sse: bool) -> Self {
        Self {
            sse,
            buf: Vec::new(),
            event_len: 0,
            dropping: false,
            after_cr: false,
            line_has_bytes: false,
            data: Vec::new(),
            usage: Usage::default(),
            anthropic_input: None,
            anthropic_writes: None,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        if !self.sse {
            if !self.dropping && chunk.len() <= CAP - self.buf.len() {
                self.buf.extend_from_slice(chunk);
            } else {
                self.buf.clear();
                self.dropping = true;
            }
            return;
        }
        for &b in chunk {
            if self.after_cr && b == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = b == b'\r';
            if !self.dropping {
                if self.event_len == CAP && (self.line_has_bytes || !matches!(b, b'\r' | b'\n')) {
                    self.dropping = true;
                    self.buf.clear();
                    self.data.clear();
                } else if self.event_len < CAP {
                    self.event_len += 1;
                }
            }
            match b {
                b'\r' | b'\n' => self.end_line(),
                _ => {
                    self.line_has_bytes = true;
                    if !self.dropping {
                        self.buf.push(b);
                    }
                }
            }
        }
    }

    fn end_line(&mut self) {
        if !self.line_has_bytes {
            self.end_event();
        } else if !self.dropping {
            let line = self.buf.as_slice();
            if let Some(d) = line.strip_prefix(b"data:") {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data
                    .extend_from_slice(d.strip_prefix(b" ").unwrap_or(d));
            }
            self.buf.clear();
        }
        self.line_has_bytes = false;
    }

    fn end_event(&mut self) {
        if !self.dropping {
            self.parse(true);
        }
        self.buf.clear();
        self.data.clear();
        self.event_len = 0;
        self.dropping = false;
    }

    fn parse(&mut self, sse: bool) {
        let data = if sse { &self.data } else { &self.buf };
        // Avoid allocating a JSON tree for typical deltas or [DONE].
        if data.windows(b"usage".len()).any(|w| w == b"usage") {
            if let Ok(v) = serde_json::from_slice(data) {
                self.observe(&v);
            }
        }
    }

    pub fn observe(&mut self, v: &Value) {
        let kind = v.get("type").and_then(Value::as_str);
        let u = match kind {
            Some("message_start") => v.pointer("/message/usage"),
            Some("response.completed" | "response.incomplete" | "response.failed") => {
                v.pointer("/response/usage")
            }
            _ => v.get("usage"),
        };
        let Some(u) = u else { return };
        let get = |k: &str| u.get(k).and_then(Value::as_u64);
        let cached = get("cache_read_input_tokens")
            .or_else(|| {
                u.pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                u.pointer("/input_tokens_details/cached_tokens")
                    .and_then(Value::as_u64)
            })
            .or_else(|| get("cached_tokens"));
        self.usage.cached = cached.or(self.usage.cached);
        self.usage.output = get("completion_tokens")
            .or_else(|| get("output_tokens"))
            .or(self.usage.output);
        if matches!(kind, Some("message_start" | "message_delta" | "message"))
            || u.get("cache_read_input_tokens").is_some()
            || u.get("cache_creation_input_tokens").is_some()
        {
            self.anthropic_input = get("input_tokens").or(self.anthropic_input);
            self.anthropic_writes = get("cache_creation_input_tokens").or(self.anthropic_writes);
            self.usage.input = self.anthropic_input.map(|raw| {
                raw.saturating_add(self.usage.cached.unwrap_or(0))
                    .saturating_add(self.anthropic_writes.unwrap_or(0))
            });
        } else {
            self.usage.input = get("prompt_tokens")
                .or_else(|| get("input_tokens"))
                .or(self.usage.input);
        }
    }

    pub fn finish(&mut self) -> Usage {
        if self.sse {
            // Some upstreams omit the last separator. Only complete JSON counts.
            if self.line_has_bytes {
                self.end_line();
            }
            self.end_event();
        } else if !self.dropping {
            self.parse(false);
        }
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_multiline_event_and_utf8() {
        let body = concat!(
            ": keepalive\r\nevent: response.completed\r\n",
            "data: {\"type\":\"response.completed\",\r\n",
            "data: \"response\":{\"output\":\"🦉\",\"usage\":{\"input_tokens\":9,\"output_tokens\":1,",
            "\"input_tokens_details\":{\"cached_tokens\":0}}}}\r\n\r\n",
        );
        let mut cap = Capture::new(true);
        for byte in body.as_bytes().chunks(1) {
            cap.feed(byte);
        }
        assert_eq!(
            cap.finish(),
            Usage {
                input: Some(9),
                output: Some(1),
                cached: Some(0)
            }
        );
    }

    #[test]
    fn skips_oversized_events_and_recovers_at_the_next_boundary() {
        let mut cap = Capture::new(true);
        cap.feed(b"data: ");
        cap.feed(&vec![b'x'; CAP * 2]);
        assert!(cap.dropping);
        assert!(cap.buf.capacity() <= CAP && cap.data.capacity() <= CAP);
        cap.feed(b"\rdata: {\"usage\":{\"prompt_tokens\":123}}\r");
        cap.feed(b"\rdata: {\"usage\":{\"completion_tokens\":7}}\r\r");
        assert_eq!(
            cap.finish(),
            Usage {
                output: Some(7),
                ..Usage::default()
            }
        );
    }

    #[test]
    fn partial_anthropic_usage_and_whole_json_keep_missing_distinct_from_zero() {
        let mut cap = Capture::new(true);
        cap.feed(br#"data: {"type":"message_start","message":{"usage":{"input_tokens":40,"cache_creation_input_tokens":8,"output_tokens":1}}}

data: {"type":"message_delta","usage":{"output_tokens":9,"cache_read_input_tokens":10}}
"#);
        assert_eq!(
            cap.finish(),
            Usage {
                input: Some(58),
                output: Some(9),
                cached: Some(10)
            }
        );
        let mut cap = Capture::new(false);
        cap.feed(br#"{"usage":{"prompt_tokens":7,"completion_tokens":0}}"#);
        assert_eq!(
            cap.finish(),
            Usage {
                input: Some(7),
                output: Some(0),
                cached: None
            }
        );
    }

    #[test]
    fn truncated_and_nested_non_usage_do_not_report_counters() {
        let mut cap = Capture::new(true);
        cap.feed(
            br#"data: {"type":"response.output_item.done","item":{"usage":{"input_tokens":99}}}

data: {"type":"response.completed","response":{"usage":{"input_tokens":1"#,
        );
        assert_eq!(cap.finish(), Usage::default());
    }
}
