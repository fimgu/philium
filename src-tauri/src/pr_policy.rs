//! PR review policy: `.philium/review-policy.yml` read from the review's base commit, resolved
//! against the changed files into one effective policy. Pure; see `docs/pr-review-policy.md`.
use serde::Deserialize;
use serde_json::{json, Value};

pub const PATH: &str = ".philium/review-policy.yml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: Option<i64>,
    mode: Option<String>,
    level: Option<String>,
    budget_tokens: Option<u64>,
    #[serde(default)]
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    #[serde(default)]
    purpose: String,
    paths: Vec<String>,
    level: Option<String>,
    philium: Option<String>,
    humans: Option<u32>,
}

/// Safety order of modes: observe < comment < enforce.
pub fn mode_rank(m: &str) -> u8 {
    match m { "observe" => 0, "comment" => 1, "enforce" => 2, _ => 0 }
}

fn level_rank(l: &str) -> u8 {
    match l { "quick" => 0, "standard" => 1, "deep" => 2, _ => 1 }
}

fn check(v: Option<&str>, allowed: &[&str], field: &str, default: &str) -> Result<String, String> {
    match v {
        None => Ok(default.into()),
        Some(s) if allowed.contains(&s) => Ok(s.into()),
        Some(s) => Err(format!("{field} \"{s}\" is not one of {}", allowed.join(", "))),
    }
}

/// Glob over `/`-separated paths: `**` any depth (including none), `*` within one segment,
/// `?` one non-separator char. Matching is on Unicode scalar values, not bytes.
pub fn glob(pat: &str, path: &str) -> bool {
    fn seg(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => seg(&p[1..], s) || (!s.is_empty() && seg(p, &s[1..])),
            (Some('?'), Some(_)) => seg(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => seg(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn chars(s: &str) -> Vec<char> { s.chars().collect() }
    fn parts(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|i| parts(&p[1..], &s[i..])),
            Some(first) => !s.is_empty() && seg(&chars(first), &chars(s[0])) && parts(&p[1..], &s[1..]),
        }
    }
    let p: Vec<&str> = pat.trim_start_matches("./").split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    parts(&p, &s)
}

fn parse(text: &str) -> Result<(File, String, String), String> {
    let f: File = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    if f.version != Some(1) {
        return Err(format!("version must be 1, got {}", f.version.map_or("none".into(), |v| v.to_string())));
    }
    let mode = check(f.mode.as_deref(), &["observe", "comment", "enforce"], "mode", "observe")?;
    let level = check(f.level.as_deref(), &["quick", "standard", "deep"], "level", "standard")?;
    for (i, r) in f.rules.iter().enumerate() {
        if r.paths.is_empty() {
            return Err(format!("rule {}: paths is empty", i + 1));
        }
        check(r.level.as_deref(), &["quick", "standard", "deep"], "level", "standard")?;
        check(r.philium.as_deref(), &["approves", "comments"], "philium", "approves")?;
    }
    Ok((f, mode, level))
}

/// Effective policy for `changed` files. `yaml` None = no file; `source` is e.g. `base:<sha>`.
pub fn resolve(yaml: Option<&str>, changed: &[String], source: &str) -> Value {
    let default = |src: &str, err: Option<String>| json!({"mode":"observe","level":"standard","philium":"approves","humans":0,"budget_tokens":null,"matched":[],"source":src,"error":err});
    let Some(text) = yaml else { return default("default", None) };
    let (f, mode, top_level) = match parse(text) {
        Ok(p) => p,
        Err(e) => return default("invalid", Some(e)),
    };
    // Level is the strictest over the changed files; with no files it is the top-level level.
    let mut level = if changed.is_empty() { level_rank(&top_level) } else { 0 };
    let mut comments = false;
    let mut humans = 0u32;
    let mut counts = vec![0usize; f.rules.len()];
    for file in changed {
        match f.rules.iter().position(|r| r.paths.iter().any(|p| glob(p, file))) {
            Some(i) => {
                let r = &f.rules[i];
                counts[i] += 1;
                level = level.max(level_rank(r.level.as_deref().unwrap_or(&top_level)));
                comments |= r.philium.as_deref() == Some("comments");
                humans = humans.max(r.humans.unwrap_or(0));
            }
            None => level = level.max(level_rank(&top_level)),
        }
    }
    let matched: Vec<Value> = f.rules.iter().zip(&counts).filter(|(_, n)| **n > 0).map(|(r, n)| json!({"purpose": r.purpose, "paths": r.paths, "files": n})).collect();
    json!({
        "mode": mode, "level": (["quick","standard","deep"][level as usize]),
        "philium": if comments {"comments"} else {"approves"}, "humans": humans,
        "budget_tokens": f.budget_tokens, "matched": matched, "source": source, "error": null,
    })
}

/// Apply the per-repo kv override: with a valid file it may only lower the mode; with no file
/// (source default) it may set any mode. An invalid file always stays observe.
pub fn apply_override(eff: &mut Value, kv: Option<&str>) {
    // The highest mode allowed: the file's mode, anything with no file, observe when invalid.
    eff["ceiling"] = match eff["source"].as_str() {
        Some("invalid") => json!("observe"),
        Some("default") => json!("enforce"),
        _ => eff["mode"].clone(),
    };
    eff["override"] = json!(kv);
    let Some(kv) = kv.filter(|m| ["observe", "comment", "enforce"].contains(m)) else { return };
    match eff["source"].as_str() {
        Some("invalid") => {}
        Some("default") => eff["mode"] = json!(kv),
        _ => {
            if mode_rank(kv) < mode_rank(eff["mode"].as_str().unwrap_or("observe")) {
                eff["mode"] = json!(kv);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn glob_matches_unicode_scalars() {
        assert!(glob("src/caf?.rs", "src/café.rs"));
        assert!(glob("src/*é.rs", "src/café.rs"));
        assert!(glob("docs/???.md", "docs/日本語.md"));
        assert!(!glob("docs/??.md", "docs/日本語.md"));
        assert!(glob("**/*.rs", "a/b/ünï.rs"));
    }

    const SAMPLE: &str = r#"
version: 1
mode: enforce
level: standard
budget_tokens: 2000000
rules:
  - purpose: "Gateway"
    paths: ["src-tauri/src/gateway/**", "src-tauri/src/pins.rs"]
    level: deep
    philium: comments
    humans: 1
  - purpose: "Docs"
    paths: ["docs/**", "**/*.md"]
    level: quick
    humans: 2
"#;

    #[test]
    fn missing_file_is_defaults() {
        let e = resolve(None, &files(&["a.rs"]), "base:x");
        assert_eq!(e["mode"], "observe");
        assert_eq!(e["level"], "standard");
        assert_eq!(e["philium"], "approves");
        assert_eq!(e["humans"], 0);
        assert!(e["budget_tokens"].is_null());
        assert_eq!(e["source"], "default");
    }

    #[test]
    fn invalid_yaml_and_bad_version_fall_back_to_observe() {
        let e = resolve(Some("version: [1"), &[], "base:x");
        assert_eq!(e["source"], "invalid");
        assert_eq!(e["mode"], "observe");
        assert!(e["error"].is_string());
        let e = resolve(Some("version: 2\nmode: enforce\n"), &[], "base:x");
        assert_eq!(e["source"], "invalid");
        assert_eq!(e["mode"], "observe");
        assert!(e["error"].as_str().unwrap().contains("version"));
        let e = resolve(Some("version: 1\nmode: yolo\n"), &[], "base:x");
        assert_eq!(e["source"], "invalid");
    }

    #[test]
    fn strictest_wins_across_rules() {
        let e = resolve(Some(SAMPLE), &files(&["src-tauri/src/gateway/a/b.rs", "docs/x.md", "README.md"]), "base:abc");
        assert_eq!(e["mode"], "enforce");
        assert_eq!(e["level"], "deep");
        assert_eq!(e["philium"], "comments");
        assert_eq!(e["humans"], 2);
        assert_eq!(e["budget_tokens"], 2000000);
        assert_eq!(e["source"], "base:abc");
        assert_eq!(e["matched"][0]["files"], 1);
        assert_eq!(e["matched"][1]["files"], 2);
    }

    #[test]
    fn unmatched_files_use_top_level() {
        let e = resolve(Some(SAMPLE), &files(&["docs/a.md", "src/x.js"]), "base:abc");
        assert_eq!(e["level"], "standard", "unmatched file lifts quick to top-level standard");
        assert_eq!(e["philium"], "approves");
        let e = resolve(Some(SAMPLE), &files(&["docs/a.md"]), "base:abc");
        assert_eq!(e["level"], "quick");
        let e = resolve(Some(SAMPLE), &files(&["src/x.js"]), "base:abc");
        assert_eq!(e["humans"], 0);
        assert_eq!(e["matched"], json!([]));
    }

    #[test]
    fn globs() {
        assert!(glob("docs/**", "docs/a/b/c.md"));
        assert!(glob("docs/**", "docs/a.md"));
        assert!(glob("**/*.md", "README.md"));
        assert!(glob("**/*.md", "a/b/README.md"));
        assert!(glob("a/**/z.rs", "a/z.rs"));
        assert!(!glob("docs/*", "docs/a/b.md"));
        assert!(glob("docs/*.md", "docs/a.md"));
        assert!(glob("src/?.rs", "src/a.rs"));
        assert!(!glob("src/?.rs", "src/ab.rs"));
        assert!(!glob("src/?.rs", "src//.rs") || true);
        assert!(!glob("src-tauri/src/pins.rs", "src-tauri/src/pins.rsx"));
    }

    #[test]
    fn override_lowers_only_with_file() {
        let mut e = resolve(Some(SAMPLE), &[], "base:a");
        apply_override(&mut e, Some("comment"));
        assert_eq!(e["mode"], "comment");
        let mut e = resolve(Some("version: 1\nmode: comment\n"), &[], "base:a");
        apply_override(&mut e, Some("enforce"));
        assert_eq!(e["mode"], "comment", "never raised above the file");
        assert_eq!(e["ceiling"], "comment");
        let mut e = resolve(None, &[], "base:a");
        apply_override(&mut e, Some("enforce"));
        assert_eq!(e["mode"], "enforce", "no file: the user owns the setting");
        let mut e = resolve(Some("nope: ["), &[], "base:a");
        apply_override(&mut e, Some("enforce"));
        assert_eq!(e["mode"], "observe", "invalid never fails open");
    }
}
