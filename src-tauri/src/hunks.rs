//! Per-hunk review actions on a worktree: list hunks for a layer and stage, unstage or
//! revert one hunk (or a whole file). The client only sends a backend-issued hunk id.
//! The patch is regenerated server-side and the id must still match, so a hunk that
//! changed since it was shown is rejected instead of applied to the wrong lines.
//!
//! Layers: "unstaged" (index -> disk) and "staged" (HEAD -> index).
//! Ops: stage (unstaged only), unstage (staged only), revert (unstaged only, discards
//! the hunk from disk).
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const MAX_PATCH: usize = 2_000_000;

#[derive(Debug, Clone)]
pub(crate) struct FileDiff {
    pub path: String,
    pub header: String,
    pub hunks: Vec<String>,
    pub binary: bool,
}

fn layer_args(layer: &str) -> Result<Vec<&'static str>, String> {
    let mut a = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-renames", "-U3"];
    match layer {
        "unstaged" => {}
        "staged" => a.push("--cached"),
        _ => return Err("Unknown layer. Use unstaged or staged.".into()),
    }
    a.push("--");
    Ok(a)
}

fn run_git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    if let Some(input) = stdin {
        child.stdin.take().ok_or("git: no stdin")?.write_all(input.as_bytes()).map_err(|e| format!("git: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Split a unified diff into files and hunks, keeping exact text so a hunk can be re-applied.
pub(crate) fn parse(patch: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            let path = line.trim_end().rsplit_once(" b/").map(|(_, p)| p.to_string()).unwrap_or_default();
            files.push(FileDiff { path, header: line.into(), hunks: vec![], binary: false });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if line.starts_with("@@") {
            f.hunks.push(line.into());
        } else if let Some(h) = f.hunks.last_mut() {
            h.push_str(line);
        } else {
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                f.binary = true;
            }
            f.header.push_str(line);
        }
    }
    files
}

pub(crate) fn hunk_id(file: &FileDiff, hunk: &str) -> String {
    let mut h = Sha256::new();
    h.update(file.header.as_bytes());
    h.update(hunk.as_bytes());
    h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn file_id(file: &FileDiff) -> String {
    let mut h = Sha256::new();
    h.update(file.header.as_bytes());
    for x in &file.hunks {
        h.update(x.as_bytes());
    }
    h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn load(dir: &Path, layer: &str) -> Result<Vec<FileDiff>, String> {
    let patch = run_git(dir, &layer_args(layer)?, None)?;
    if patch.len() > MAX_PATCH {
        return Err("These changes are too large to review hunk by hunk.".into());
    }
    Ok(parse(&patch))
}

/// `{layer, files:[{path, id, binary, hunks:[{id, header, text}]}]}`
pub(crate) fn list(dir: &Path, layer: &str) -> Result<Value, String> {
    let files = load(dir, layer)?;
    let rows: Vec<Value> = files
        .iter()
        .map(|f| {
            let hunks: Vec<Value> = f
                .hunks
                .iter()
                .map(|h| json!({"id": hunk_id(f, h), "header": h.lines().next().unwrap_or(""), "text": h}))
                .collect();
            json!({"path": f.path, "id": file_id(f), "binary": f.binary, "hunks": hunks})
        })
        .collect();
    Ok(json!({"layer": layer, "files": rows}))
}

/// Apply `op` to one hunk (`id` is a hunk id) or one whole file (`id` is a file id).
pub(crate) fn apply(dir: &Path, layer: &str, op: &str, id: &str) -> Result<Value, String> {
    let (index, reverse) = match (layer, op) {
        ("unstaged", "stage") => (true, false),
        ("staged", "unstage") => (true, true),
        ("unstaged", "revert") => (false, true),
        _ => return Err(format!("Cannot {op} a {layer} change.")),
    };
    let files = load(dir, layer)?;
    let patch = files
        .iter()
        .find_map(|f| {
            if file_id(f) == id {
                return Some(Ok(format!("{}{}", f.header, f.hunks.concat())));
            }
            f.hunks.iter().find(|h| hunk_id(f, h) == id).map(|h| {
                if f.binary {
                    Err("Binary files can only be changed as a whole file.".to_string())
                } else {
                    Ok(format!("{}{}", f.header, h))
                }
            })
        })
        .ok_or("This change is no longer there. Refresh to see the current state.")??;
    let mut args = vec!["apply", "--whitespace=nowarn"];
    if index {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    let mut check = args.clone();
    check.push("--check");
    check.push("-");
    run_git(dir, &check, Some(&patch)).map_err(|e| format!("Not applied: {e}"))?;
    args.push("-");
    run_git(dir, &args, Some(&patch))?;
    Ok(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn repo() -> Tmp {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let t = Tmp(std::env::temp_dir().join(format!("philium-hunks-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst))));
        std::fs::create_dir_all(&t.0).unwrap();
        let p = t.path();
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.test"]] {
            run_git(p, a, None).unwrap();
        }
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("f.txt"), body).unwrap();
        run_git(p, &["add", "."], None).unwrap();
        run_git(p, &["commit", "-qm", "init"], None).unwrap();
        let edited: String = (1..=30)
            .map(|i| if i == 2 || i == 28 { format!("changed {i}\n") } else { format!("line {i}\n") })
            .collect();
        std::fs::write(p.join("f.txt"), edited).unwrap();
        t
    }

    fn hunks(p: &Path, layer: &str) -> Vec<String> {
        list(p, layer).unwrap()["files"][0]["hunks"]
            .as_array()
            .map(|a| a.iter().map(|h| h["id"].as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn stage_unstage_revert_single_hunk() {
        let t = repo();
        let p = t.path();
        let ids = hunks(p, "unstaged");
        assert_eq!(ids.len(), 2);
        apply(p, "unstaged", "stage", &ids[0]).unwrap();
        assert_eq!(hunks(p, "unstaged").len(), 1);
        let staged = hunks(p, "staged");
        assert_eq!(staged.len(), 1);
        apply(p, "staged", "unstage", &staged[0]).unwrap();
        assert!(hunks(p, "staged").is_empty());
        let ids = hunks(p, "unstaged");
        apply(p, "unstaged", "revert", &ids[1]).unwrap();
        let disk = std::fs::read_to_string(p.join("f.txt")).unwrap();
        assert!(disk.contains("changed 2") && !disk.contains("changed 28"));
    }

    #[test]
    fn stale_or_wrong_layer_is_rejected() {
        let t = repo();
        let p = t.path();
        let id = hunks(p, "unstaged")[0].clone();
        std::fs::write(p.join("f.txt"), "rewritten\n").unwrap();
        assert!(apply(p, "unstaged", "stage", &id).unwrap_err().contains("no longer there"));
        assert!(apply(p, "staged", "revert", "x").is_err());
        assert!(list(p, "bogus").is_err());
    }
}
