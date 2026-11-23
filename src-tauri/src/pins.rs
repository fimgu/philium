//! Every version, client fingerprint and model ID Philium compiles in.
//!
//! Engineering rule: Philium always uses the latest versions, fingerprints and
//! model IDs. A literal that can go stale lives here, with a written reason and
//! how it is kept current. `tests::no_stale_pins` fails the build when a
//! version-like literal appears anywhere else without an `ALLOWLIST` entry.

/// Claude Code version floor. Resolved at runtime from the npm registry
/// (`@anthropic-ai/claude-code/latest`); this is only the fallback floor and
/// the presented version only ever moves forward.
pub const CLAUDE_CLI_FLOOR: &str = "2.1.330";

/// Anthropic API version header. Pinned: the API contract version, not a
/// client version. Anthropic keeps `2023-06-01` as the current stable version.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta flags Claude Code sends on subscription requests. Fallback floor:
/// caller-supplied `anthropic-beta` flags are merged on top at runtime.
pub const CLAUDE_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,advisor-tool-2026-03-01,advanced-tool-use-2025-11-20,effort-2025-11-24";

/// Minimal beta set for the Claude profile/usage probe. Fallback floor; only
/// gates the endpoint, so it does not need the full Claude Code set.
pub const CLAUDE_PROFILE_BETA: &str = "oauth-2025-04-20,claude-code-20250219";

/// Google OAuth token client fingerprint. Fallback floor: Google does not gate
/// on this version; bump when the Antigravity app bumps its googleapis dep.
pub const GOOGLE_USER_AGENT: &str = "google-api-nodejs-client/9.15.1";

/// Antigravity consumer CLI fingerprint; unlocks the full model catalog.
/// Fallback floor: bump when Antigravity releases; the live catalog
/// (`fetchAvailableModels`) decides which models are offered.
pub const GEMINI_CLI_VERSION: &str = "cli/1.5.6";

/// Antigravity default model before a `fetchAvailableModels` catalog names
/// one. Resolved at runtime per gateway; this is the fallback floor.
pub const GEMINI_FALLBACK_MODEL: &str = "gemini-3-flash";

/// ChatGPT `/models` `client_version` query. Fallback floor: the backend
/// serves the current catalog for any version at or above this.
pub const CHATGPT_CLIENT_VERSION: &str = "1.0.0";

/// Managed sandbox base image. Pinned for security: never pulled implicitly,
/// must already exist locally, and a moving tag would change the sandbox.
pub const ALPINE_IMAGE: &str = "alpine:3.20";

/// Antigravity `x-goog-api-client` fingerprint. Fallback floor: bump with
/// GEMINI_CLI_VERSION when Antigravity releases.
pub const GEMINI_API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";

/// MCP protocol version Philium's server answers with when the client sends
/// none. Resolved at runtime: the client's requested version is echoed back.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Literals allowed outside this file's constants: (literal, reason and how it is kept current).
/// The scanner accepts a match when it is a substring of a constant above or of an entry here.
pub const ALLOWLIST: &[(&str, &str)] = &[
    ("claude-2", "Model-family prefix for the pre-effort thinking-budget era. Pinned: historical families never change."),
    ("claude-3", "Model-family prefix for the pre-effort thinking-budget era. Pinned: historical families never change."),
    ("HTTP/1.1", "Wire protocol version in raw HTTP requests/responses. Pinned: a protocol, not a client fingerprint; it does not go stale."),
    ("localhost/127.0.0", "User-facing text listing local hosts, not a version. Matches the name/1.2.3 shape only by accident."),
    ("gemini-3.1-pro-high", "Catalog alias the backend rejects by name; remapped to gemini-pro-agent. Live catalog supplies all other IDs."),
];

/// Public live price catalog, used only for models whose own provider's `/models`
/// returns no prices. Fetched at runtime (6h cache); never a compiled price table.
/// Kept current by its host; if it moves, prices show as unknown, never as zero.
pub const PRICE_CATALOG_URL: &str = "https://openrouter.ai/api/v1/models";

const CONSTANTS: &[&str] = &[
    CLAUDE_CLI_FLOOR, ANTHROPIC_VERSION, CLAUDE_BETA, CLAUDE_PROFILE_BETA, GOOGLE_USER_AGENT,
    GEMINI_CLI_VERSION, GEMINI_FALLBACK_MODEL, CHATGPT_CLIENT_VERSION, ALPINE_IMAGE, GEMINI_API_CLIENT, MCP_PROTOCOL_VERSION,
];

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::path::{Path, PathBuf};

    fn files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if p.is_dir() {
                if name != "node_modules" && name != "tests" {
                    files(&p, out);
                }
            } else if (name.ends_with(".rs") && name != "pins.rs" && !name.ends_with("_tests.rs"))
                || ((name.ends_with(".js") || name.ends_with(".jsx")) && !name.contains(".test."))
            {
                out.push(p);
            }
        }
    }

    /// Drop `#[cfg(test)]` items by brace counting.
    fn strip_test_blocks(src: &str) -> Vec<(usize, String)> {
        let mut out = vec![];
        let (mut skipping, mut armed, mut depth) = (false, false, 0i64);
        for (i, line) in src.lines().enumerate() {
            if !skipping && line.trim_start().starts_with("#[cfg(test)]") {
                skipping = true;
                armed = false;
                depth = 0;
                continue;
            }
            if skipping {
                for c in line.chars() {
                    match c {
                        '{' => { depth += 1; armed = true; }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if (armed && depth <= 0) || (!armed && line.trim_end().ends_with(';')) {
                    skipping = false;
                }
                continue;
            }
            out.push((i + 1, line.to_string()));
        }
        out
    }

    #[test]
    fn no_stale_pins() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut all = vec![];
        files(&root.join("src"), &mut all);
        files(&root.join("../src"), &mut all);
        assert!(all.len() > 10, "scanner found too few files");
        let strings = Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`[^`]*`"#).unwrap();
        let pins = Regex::new(concat!(
            r"[A-Za-z][\w.-]*/v?\d+\.\d+(?:\.\d+)?", // name/1.2.3 user agents
            r"|client_version=\d[\w.]*",
            r"|\b[a-z][\w-]*:\d+\.\d+", // image:tag
            r"|\b(?:claude|gpt|gemini|o\d)-[\w.-]*\d[\w.-]*", // model IDs
            r"|\b\d{4}-\d{2}-\d{2}\b", // dated API versions / beta flags
        ))
        .unwrap();
        let ok = |m: &str| CONSTANTS.iter().chain(ALLOWLIST.iter().map(|(l, _)| l)).any(|c| c.contains(m) || m.contains(*c) && m.len() == c.len());
        let mut bad = vec![];
        for f in all {
            let src = std::fs::read_to_string(&f).unwrap();
            for (n, line) in strip_test_blocks(&src) {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for s in strings.find_iter(&line) {
                    for m in pins.find_iter(s.as_str()) {
                        let m = m.as_str().trim_end_matches(['.', '-']);
                        if !ok(m) {
                            bad.push(format!("{}:{n}: {m}", f.strip_prefix(root).unwrap_or(&f).display()));
                        }
                    }
                }
            }
        }
        assert!(bad.is_empty(), "version-like literals outside src-tauri/src/pins.rs. Resolve them at runtime, or add a pins:: constant/ALLOWLIST entry with a reason:\n{}", bad.join("\n"));
    }

    #[test]
    fn allowlist_entries_have_reasons() {
        for (lit, why) in ALLOWLIST {
            assert!(why.len() > 30, "{lit} needs a written reason");
        }
    }
}
