//! Outbound secret guard. Pure functions over request JSON; no I/O.
//!
//! Runs inside Philium's gateway, so it inspects the exact bytes about to leave
//! the machine regardless of which wire format HAL used (Chat Completions,
//! Responses, Anthropic Messages). Only string values are scanned and
//! rewritten, so JSON structure is always preserved.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardMode {
    Off,
    #[default]
    Redact,
    Block,
}

struct Rule {
    name: &'static str,
    re: Regex,
    keep: Option<fn(&str) -> bool>,
}

/// `API_KEY_HEADER = 'x-api-key'` names a header, not a secret.
fn assignment_is_secret(m: &str) -> bool {
    let name: String = m
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_uppercase();
    const META: [&str; 11] = [
        "_HEADER", "_NAME", "_FIELD", "_ENV", "_VAR", "_PARAM", "_PATH", "_FILE", "_URL", "_TYPE",
        "_ID",
    ];
    !META.iter().any(|s| name.ends_with(s))
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let r = |name, pat: &str| Rule { name, re: Regex::new(pat).expect("guard rule"), keep: None };
    vec![
        r("private_key", r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----[\s\S]*?(?:-----END (?:[A-Z0-9]+ )*PRIVATE KEY-----|\z)"),
        r("aws_access_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        r("github_token", r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b"),
        r("anthropic_key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
        r("openai_key", r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{32,}"),
        r("google_api_key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
        r("slack_token", r"\bxox[abposr]-[A-Za-z0-9\-]{10,}"),
        r("stripe_key", r"\b(?:sk|rk)_live_[A-Za-z0-9]{20,}"),
        r("jwt", r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"),
        r("url_credentials", r"\b[a-z][a-z0-9+.\-]*://[^\s:/@]+:[^\s@/]{3,}@"),
        Rule {
            keep: Some(assignment_is_secret),
            ..r(
                "secret_assignment",
                // Quoted literal or unquoted env-style value; `token = lexer.next()` passes.
                r#"(?i)\b[A-Z0-9_]*(?:SECRET|PASSWORD|PASSWD|TOKEN|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY|CREDENTIALS?)[A-Z0-9_]*["']?\s*[:=]\s*(?:"[^"\s]{8,}"|'[^'\s]{8,}'|[A-Za-z0-9_\-+/=]{8,}(?:[\s;,]|$))"#,
            )
        },
    ]
});

/// Rule name -> number of matches.
pub type Findings = BTreeMap<&'static str, usize>;

/// Scan one string; when `redact`, replace matches with `[REDACTED:<rule>]`.
pub fn scan_text(text: &mut String, redact: bool, findings: &mut Findings) {
    for rule in RULES.iter() {
        let keep = |s: &str| rule.keep.is_none_or(|k| k(s));
        let n = rule.re.find_iter(text).filter(|m| keep(m.as_str())).count();
        if n == 0 {
            continue;
        }
        *findings.entry(rule.name).or_default() += n;
        if redact {
            let marker = format!("[REDACTED:{}]", rule.name);
            *text = rule
                .re
                .replace_all(text, |c: &regex::Captures| {
                    if keep(&c[0]) { marker.clone() } else { c[0].to_string() }
                })
                .into_owned();
        }
    }
}

/// Walk every string in a JSON document (including object keys' values and
/// JSON-encoded tool arguments, which are themselves strings).
pub fn scan_json(v: &mut Value, redact: bool, findings: &mut Findings) {
    match v {
        Value::String(s) => scan_text(s, redact, findings),
        Value::Array(a) => a.iter_mut().for_each(|x| scan_json(x, redact, findings)),
        Value::Object(o) => o.values_mut().for_each(|x| scan_json(x, redact, findings)),
        _ => {}
    }
}

pub fn summarize(f: &Findings) -> String {
    f.iter().map(|(r, n)| format!("{r}×{n}")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scan(s: &str) -> (Findings, String) {
        let mut t = s.to_string();
        let mut f = Findings::new();
        scan_text(&mut t, true, &mut f);
        (f, t)
    }

    #[test]
    fn detects_and_redacts_common_secrets() {
        let cases = [
            ("key AKIAABCDEFGHIJKLMNOP here", "aws_access_key"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789AB", "github_token"),
            ("sk-ant-api03-abcdefghijklmnopqrstuvwx", "anthropic_key"),
            ("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789", "openai_key"),
            ("postgres://admin:hunter22@db.local/x", "url_credentials"),
            ("DATABASE_PASSWORD=correcthorsebattery", "secret_assignment"),
            ("\"api_key\": \"abc123def456ghi\"", "secret_assignment"),
            ("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----", "private_key"),
        ];
        for (input, rule) in cases {
            let (f, out) = scan(input);
            assert!(f.contains_key(rule), "{input:?} -> {f:?}");
            assert!(out.contains(&format!("[REDACTED:{rule}]")), "{out}");
        }
    }

    #[test]
    fn ignores_ordinary_code() {
        for s in [
            "fn main() { let token = lexer.next(); }",
            "password_field.focus()",
            "const API_KEY_HEADER = 'x-api-key';",
            "see https://example.com/docs",
        ] {
            assert!(scan(s).0.is_empty(), "false positive on {s:?}");
        }
    }

    #[test]
    fn json_walk_covers_every_wire_format_and_keeps_structure() {
        let mut body = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "use AKIAABCDEFGHIJKLMNOP"},
                {"role": "assistant", "tool_calls": [{"function": {"arguments": "{\"url\":\"https://u:pass1234@h\"}"}}]},
                {"role": "tool", "content": [{"type": "text", "text": "DB_PASSWORD=wJalrXUtnFEMIK7MD"}]}
            ],
            "input": [{"type": "function_call_output", "output": "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB"}],
            "stream": true, "max_tokens": 10
        });
        let mut f = Findings::new();
        scan_json(&mut body, true, &mut f);
        let s = body.to_string();
        for leaked in ["AKIAABCDEFGHIJKLMNOP", "pass1234", "wJalrXUtn", "ghp_abcdef"] {
            assert!(!s.contains(leaked), "{leaked} leaked: {s}");
        }
        assert_eq!(f.values().sum::<usize>(), 4, "{f:?}");
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_tokens"], json!(10));
    }
}
