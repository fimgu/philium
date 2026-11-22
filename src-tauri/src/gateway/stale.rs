//! Client fingerprints with no public "latest" source (Antigravity's CLI version,
//! ChatGPT's `client_version`, Claude beta flags). When a provider refuses one as
//! outdated or unsupported, name the stale value instead of passing on an opaque 400.

const OUTDATED: &[&str] = &[
    "outdated",
    "too old",
    "no longer supported",
    "not supported anymore",
    "unsupported client",
    "unsupported version",
    "upgrade your client",
    "update your client",
    "please upgrade",
    "please update",
    "minimum version",
    "minimum supported version",
    "client version",
    "client_version",
    "deprecated client",
];

/// A refusal that blames the client's version.
pub fn outdated(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 403 | 412 | 422 | 426) {
        return false;
    }
    let b = body.to_ascii_lowercase();
    status == 426 || OUTDATED.iter().any(|p| b.contains(p))
}

/// User-visible warning naming the stale value.
pub fn warning(provider: &str, what: &str, value: &str) -> String {
    format!("Philium's {provider} {what} ({value}) is outdated, update needed")
}

/// The beta flag, among those sent, that a refusal names as unknown or unsupported.
pub fn rejected_beta(status: u16, body: &str, sent: &str) -> Option<String> {
    if !matches!(status, 400 | 422) {
        return None;
    }
    let b = body.to_ascii_lowercase();
    if !b.contains("beta") {
        return None;
    }
    sent.split(',').map(str::trim).filter(|f| !f.is_empty()).find(|f| b.contains(&f.to_ascii_lowercase())).map(str::to_string)
}

/// The beta header with refused flags removed.
pub fn without(beta: &str, dropped: &[String]) -> String {
    beta.split(',').map(str::trim).filter(|f| !f.is_empty() && !dropped.iter().any(|d| d == f)).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_refusals_are_named_and_others_are_not() {
        assert!(outdated(400, r#"{"error":{"message":"Your client is outdated. Please upgrade."}}"#));
        assert!(outdated(400, r#"{"detail":"Unsupported client_version 1.0.0"}"#));
        assert!(outdated(426, ""));
        assert!(!outdated(400, r#"{"error":{"message":"messages: field required"}}"#));
        assert!(!outdated(500, "client version"));
        assert_eq!(warning("ChatGPT", "client version", "1.0.0"), "Philium's ChatGPT client version (1.0.0) is outdated, update needed");
    }

    #[test]
    fn a_refused_beta_flag_is_found_and_dropped() {
        let sent = "claude-code-20250219,oauth-2025-04-20,effort-2025-11-24";
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Unexpected value(s) `effort-2025-11-24` for the `anthropic-beta` header."}}"#;
        assert_eq!(rejected_beta(400, body, sent).as_deref(), Some("effort-2025-11-24"));
        assert_eq!(rejected_beta(400, r#"{"error":"bad max_tokens"}"#, sent), None);
        assert_eq!(without(sent, &["effort-2025-11-24".into()]), "claude-code-20250219,oauth-2025-04-20");
    }
}
