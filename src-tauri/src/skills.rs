//! Native skills store. Files on disk are the source of truth:
//! - global: `~/Philium/skills/<name>/SKILL.md`
//! - per repo: `<repo>/.philium/skills/<name>/SKILL.md`
//! - open-standard shared dirs, read in place: `~/.agents/skills`, `<repo>/.agents/skills`
//! Every scan mirrors them into the `skills` table so agents' `skill_list` / `skill_get` see them.
//! Disabled skills stay on disk but are not mirrored. Skills installed from a URL keep a
//! `.philium-source.json` so they can be updated.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ctx::{arg_str, Ctx};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn global_dir() -> PathBuf {
    home().join("Philium/skills")
}

fn repo_dir(ctx: &Ctx, repo_id: &str) -> Result<PathBuf, String> {
    let rows = ctx.db.query("SELECT path FROM repos WHERE id=?1", [repo_id])?;
    let p = rows.first().and_then(|r| r["path"].as_str()).ok_or("unknown repo")?;
    Ok(Path::new(p).join(".philium/skills"))
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.starts_with(['-', '.']) && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
}

/// (name, description) from SKILL.md front matter.
fn front_matter(body: &str) -> (Option<String>, String) {
    let (mut name, mut desc) = (None, String::new());
    let mut lines = body.lines();
    if lines.next().map(str::trim) == Some("---") {
        for l in lines {
            if l.trim() == "---" {
                break;
            }
            if let Some(v) = l.strip_prefix("name:") {
                name = Some(v.trim().trim_matches('"').to_string());
            }
            if let Some(v) = l.strip_prefix("description:") {
                desc = v.trim().trim_matches('"').to_string();
            }
        }
    }
    (name, desc)
}

const DISABLED_KEY: &str = "skills.disabled";
const SOURCE_FILE: &str = ".philium-source.json";

/// Open-standard shared location (any tool may read/write it).
fn shared_dir() -> PathBuf {
    home().join(".agents/skills")
}

fn disabled(ctx: &Ctx) -> Vec<String> {
    ctx.db.kv_get(DISABLED_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn scan(dir: &Path, scope: &str) -> Vec<Value> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let Some(folder) = e.file_name().to_str().map(str::to_string) else { continue };
        // Shared dirs may hold symlinked skills; follow them there, never in our own store.
        let is_dir = if scope.contains("shared") { p.is_dir() } else { std::fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false) };
        if !valid_name(&folder) || !is_dir {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(p.join("SKILL.md")) else { continue };
        let (_, desc) = front_matter(&body);
        let source: Value = std::fs::read_to_string(p.join(SOURCE_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        out.push(json!({ "name": folder, "description": desc, "body": body, "scope": scope, "path": p.join("SKILL.md"), "source": source }));
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

/// Every skill on disk, lowest precedence first: shared, Philium global, repo shared, repo.
fn collect(ctx: &Ctx, repo_id: Option<&str>) -> Vec<Value> {
    let mut all = scan(&shared_dir(), "shared");
    all.extend(scan(&global_dir(), "global"));
    if let Some(r) = repo_id {
        if let Ok(d) = repo_dir(ctx, r) {
            if let Some(root) = d.parent().and_then(Path::parent) {
                all.extend(scan(&root.join(".agents/skills"), &format!("repo-shared:{r}")));
            }
            all.extend(scan(&d, &format!("repo:{r}")));
        }
    }
    let off = disabled(ctx);
    for s in &mut all {
        let key = format!("{}/{}", s["scope"].as_str().unwrap_or(""), s["name"].as_str().unwrap_or(""));
        s["enabled"] = json!(!off.contains(&key));
    }
    all
}

/// Mirror disk skills into the agent-facing table. Repo skills win over global on name clash.
fn sync(ctx: &Ctx, skills: &[Value]) -> Result<(), String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("DELETE FROM skills WHERE source='philium' OR source LIKE 'philium-repo:%'", [])?;
        for s in skills {
            if s["enabled"] == json!(false) {
                continue;
            }
            let source = match s["scope"].as_str() {
                Some("global") => "philium".to_string(),
                Some(o) => format!("philium-{o}"),
                None => continue,
            };
            tx.execute(
                "INSERT INTO skills(name,description,body,files,source,updated_at) VALUES(?1,?2,?3,'{}',?4,?5)
                 ON CONFLICT(name) DO UPDATE SET description=excluded.description, body=excluded.body,
                 files=excluded.files, source=excluded.source, updated_at=excluded.updated_at",
                rusqlite::params![s["name"].as_str(), s["description"].as_str(), s["body"].as_str(), source, now],
            )?;
        }
        tx.commit()
    })?;
    Ok(())
}

fn target_dir(ctx: &Ctx, args: &Value) -> Result<PathBuf, String> {
    match args["scope"].as_str().unwrap_or("global") {
        "global" => Ok(global_dir()),
        s => repo_dir(ctx, s.strip_prefix("repo:").ok_or("Shared skills are read-only here; edit them where they live")?),
    }
}

// ---------- install: upload (zip / .skill / folder / .md) or URL (git) ----------

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let name = e.file_name();
        if name == ".git" || name == "__MACOSX" {
            continue;
        }
        let meta = std::fs::symlink_metadata(e.path())?;
        if meta.is_dir() {
            copy_dir(&e.path(), &to.join(&name))?;
        } else if meta.is_file() {
            std::fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

fn stage_root() -> PathBuf {
    std::env::temp_dir().join("philium-skill-stage")
}

fn stage_dir(token: &str) -> Result<PathBuf, String> {
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("bad token".into());
    }
    Ok(stage_root().join(token))
}

fn run(cmd: &mut std::process::Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("")))
    }
}

/// Parse a repo URL, optionally a GitHub-style `/tree/<ref>/<subdir>` URL, into (clone url, ref, subdir).
pub fn parse_url(url: &str) -> Result<(String, Option<String>, String), String> {
    let url = url.trim().trim_end_matches('/');
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("git@") || url.starts_with("ssh://")) {
        return Err("Enter an https:// or git@ repository URL".into());
    }
    for marker in ["/tree/", "/blob/", "/-/tree/"] {
        if let Some((repo, rest)) = url.split_once(marker) {
            let mut parts = rest.splitn(2, '/');
            let r = parts.next().unwrap_or("").to_string();
            let mut sub = parts.next().unwrap_or("").to_string();
            if marker == "/blob/" {
                sub = Path::new(&sub).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            }
            return Ok((repo.trim_end_matches("/-").to_string(), Some(r).filter(|r| !r.is_empty()), sub));
        }
    }
    Ok((url.to_string(), None, String::new()))
}

/// Fetch `url` into `dest` and return the directory to search (the subdir if one was given).
fn fetch_url(url: &str, dest: &Path) -> Result<PathBuf, String> {
    let (repo, rref, sub) = parse_url(url)?;
    if sub.split('/').any(|p| p == "..") {
        return Err("bad path in URL".into());
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet", "--depth", "1"]).env("GIT_TERMINAL_PROMPT", "0");
    if let Some(r) = &rref {
        cmd.args(["--branch", r]);
    }
    cmd.arg(&repo).arg(dest);
    run(&mut cmd, "git clone")?;
    let root = dest.join(&sub);
    if !root.exists() {
        return Err(format!("{sub} not found in repository"));
    }
    Ok(root)
}

/// Put an uploaded path into `dest`; returns the directory (or file) to search.
fn stage_upload(path: &Path, dest: &Path) -> Result<PathBuf, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if path.is_dir() {
        copy_dir(path, dest).map_err(|e| e.to_string())?;
        Ok(dest.to_path_buf())
    } else if ext == "zip" || ext == "skill" {
        std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        run(std::process::Command::new("unzip").args(["-qq", "-o"]).arg(path).arg("-d").arg(dest), "unzip")?;
        let _ = std::fs::remove_dir_all(dest.join("__MACOSX"));
        Ok(dest.to_path_buf())
    } else if ext == "md" {
        std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        let body = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (name, _) = front_matter(&body);
        let folder = name.filter(|n| valid_name(n)).unwrap_or_else(|| "skill".into());
        std::fs::create_dir_all(dest.join(&folder)).map_err(|e| e.to_string())?;
        std::fs::write(dest.join(&folder).join("SKILL.md"), body).map_err(|e| e.to_string())?;
        Ok(dest.to_path_buf())
    } else {
        Err("Upload a .zip, .skill, a skill folder, or a SKILL.md file".into())
    }
}

/// Skill folders (containing SKILL.md) under `root`, relative paths, bounded walk.
pub fn find_skills(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0;
    while let Some((d, depth)) = stack.pop() {
        seen += 1;
        if seen > 5000 {
            break;
        }
        if d.join("SKILL.md").is_file() {
            out.push(d.strip_prefix(root).unwrap_or(&d).to_path_buf());
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let n = e.file_name();
            if depth < 8 && n != ".git" && n != "node_modules" && std::fs::symlink_metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false) {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    out.sort();
    out
}

fn is_script(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let exec = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
        }
        #[cfg(not(unix))]
        false
    };
    exec || ["sh", "bash", "zsh", "py", "js", "mjs", "ts", "rb", "pl", "ps1", "bat", "cmd", "exe"].contains(&ext)
}

fn list_files(dir: &Path, base: &Path, out: &mut Vec<Value>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<_> = rd.flatten().collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        if e.file_name() == ".git" || e.file_name() == SOURCE_FILE {
            continue;
        }
        if m.is_dir() {
            list_files(&p, base, out);
        } else if out.len() < 500 {
            out.push(json!({ "path": p.strip_prefix(base).unwrap_or(&p), "size": m.len(), "script": is_script(&p) }));
        }
    }
}

/// Validated preview of every skill in a staged tree.
fn preview(token: &str, root: &Path, dest: &Path) -> Value {
    let skills: Vec<Value> = find_skills(root)
        .into_iter()
        .map(|rel| {
            let dir = root.join(&rel);
            let body = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
            let (name, description) = front_matter(&body);
            let folder = dir.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string();
            let name = name.filter(|n| !n.is_empty()).unwrap_or(folder);
            let mut problems = vec![];
            if !valid_name(&name) {
                problems.push(format!("name `{name}` must use letters, digits, - and _ (max 64)"));
            }
            if description.is_empty() {
                problems.push("front matter is missing `description`".to_string());
            }
            let mut files = vec![];
            list_files(&dir, &dir, &mut files);
            json!({ "id": rel, "name": name, "description": description, "files": files,
                    "exists": valid_name(&name) && dest.join(&name).exists(), "problems": problems })
        })
        .collect();
    json!({ "token": token, "skills": skills })
}

fn free_name(dest: &Path, base: &str) -> String {
    (2..).map(|i| format!("{base}-{i}")).find(|n| !dest.join(n).exists()).unwrap_or_else(|| base.to_string())
}

/// Copy a staged skill folder into `dest`. action: skip | rename | replace. Returns the final name.
pub fn install_one(src: &Path, name: &str, action: &str, dest: &Path, source: Option<&Value>) -> Result<Option<String>, String> {
    if !valid_name(name) {
        return Err(format!("bad skill name {name}"));
    }
    let mut name = name.to_string();
    if dest.join(&name).exists() {
        match action {
            "replace" => std::fs::remove_dir_all(dest.join(&name)).map_err(|e| e.to_string())?,
            "rename" => name = free_name(dest, &name),
            _ => return Ok(None),
        }
    }
    let to = dest.join(&name);
    copy_dir(src, &to).map_err(|e| format!("copy {name}: {e}"))?;
    if let Some(s) = source {
        std::fs::write(to.join(SOURCE_FILE), serde_json::to_string_pretty(s).unwrap_or_default()).map_err(|e| e.to_string())?;
    }
    Ok(Some(name))
}

fn new_token() -> String {
    use rand::Rng;
    rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(16).map(char::from).collect()
}

/// Stage a source (upload path or URL) and return its preview.
fn stage(args: &Value) -> Result<Value, String> {
    let token = new_token();
    let dir = stage_dir(&token)?;
    let _ = std::fs::remove_dir_all(&dir);
    let res = (|| {
        let root = if let Some(url) = args["url"].as_str() {
            let r = fetch_url(url, &dir.join("repo"))?;
            std::fs::write(dir.join("url"), url.trim()).map_err(|e| e.to_string())?;
            r
        } else if let Some(files) = args["files"].as_array() {
            // Dropped items: [{path: "rel/path", b64}] written out, then staged like an upload.
            use base64::Engine;
            let drop = dir.join("drop");
            for f in files {
                let rel = f["path"].as_str().unwrap_or("");
                if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|p| p == ".." || p.is_empty()) {
                    return Err(format!("bad dropped path {rel}"));
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(f["b64"].as_str().unwrap_or("")).map_err(|e| e.to_string())?;
                let to = drop.join(rel);
                std::fs::create_dir_all(to.parent().unwrap_or(&drop)).map_err(|e| e.to_string())?;
                std::fs::write(&to, bytes).map_err(|e| e.to_string())?;
            }
            // A single dropped archive or markdown file is unpacked like an upload.
            match files.as_slice() {
                [one] if !one["path"].as_str().unwrap_or("").contains('/') => {
                    stage_upload(&drop.join(one["path"].as_str().unwrap_or("")), &dir.join("upload"))?
                }
                _ => drop,
            }
        } else {
            stage_upload(Path::new(&arg_str(args, "path")?), &dir.join("upload"))?
        };
        std::fs::write(dir.join("root"), root.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let p = preview(&token, &root, &global_dir());
        if p["skills"].as_array().map_or(true, |a| a.is_empty()) {
            return Err("No SKILL.md found".to_string());
        }
        Ok(p)
    })();
    if res.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    res
}

/// Install chosen staged skills. items: [{id, action}] (ids from the preview).
fn install(token: &str, items: &[Value], dest: &Path) -> Result<Vec<String>, String> {
    let dir = stage_dir(token)?;
    let root = PathBuf::from(std::fs::read_to_string(dir.join("root")).map_err(|_| "Preview expired; start again")?);
    let url = std::fs::read_to_string(dir.join("url")).ok();
    let prev = preview(token, &root, dest);
    let mut done = vec![];
    for it in items {
        let id = it["id"].as_str().unwrap_or("");
        let Some(s) = prev["skills"].as_array().into_iter().flatten().find(|s| s["id"].as_str() == Some(id)) else { continue };
        if s["problems"].as_array().is_some_and(|p| !p.is_empty()) {
            continue;
        }
        let source = url.as_ref().map(|u| json!({ "url": u, "path": id, "installed_at": crate::db::now() }));
        if let Some(n) = install_one(&root.join(id), s["name"].as_str().unwrap_or(""), it["action"].as_str().unwrap_or("skip"), dest, source.as_ref())? {
            done.push(n);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(done)
}

/// Re-fetch a URL-installed skill and replace it in place.
fn update(dest: &Path, name: &str) -> Result<(), String> {
    let to = dest.join(name);
    let src: Value = std::fs::read_to_string(to.join(SOURCE_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).ok_or("This skill was not installed from a URL")?;
    let url = src["url"].as_str().ok_or("no url")?;
    let id = src["path"].as_str().unwrap_or("");
    let tmp = stage_dir(&new_token())?;
    let r = (|| {
        let root = fetch_url(url, &tmp)?;
        let from = root.join(id);
        if !from.join("SKILL.md").is_file() {
            return Err(format!("{id}/SKILL.md no longer exists upstream"));
        }
        let mut s = src.clone();
        s["installed_at"] = json!(crate::db::now());
        install_one(&from, name, "replace", dest, Some(&s)).map(|_| ())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

/// Open panel accepting archives, folders, and markdown. {path} or {path: null}.
fn pick_upload() -> Result<Value, String> {
    if !cfg!(target_os = "macos") {
        return Err("File picker is only available on macOS for now; drag the file onto the Skills screen instead.".to_string());
    }
    let script = r#"ObjC.import('AppKit');
var p = $.NSOpenPanel.openPanel;
p.canChooseFiles = true; p.canChooseDirectories = true; p.allowsMultipleSelection = false;
p.allowedFileTypes = $(['zip', 'skill', 'md', 'public.folder']);
p.message = 'Upload a skill: a .zip or .skill file, a skill folder, or a SKILL.md';
$.NSApplication.sharedApplication.activateIgnoringOtherApps(true);
p.runModal == 1 ? p.URL.path.js : '';"#;
    let out = std::process::Command::new("osascript").args(["-l", "JavaScript", "-e", script]).output().map_err(|e| e.to_string())?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(json!({ "path": if out.status.success() && !p.is_empty() { Some(p) } else { None } }))
}

fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> impl std::future::Future<Output = Result<T, String>> {
    async move { tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())? }
}

pub async fn call(ctx: &Ctx, m: &str, args: Value) -> Result<Value, String> {
    let repo = args["repo_id"].as_str();
    match m {
        "list" => {
            let all = collect(ctx, repo);
            sync(ctx, &all)?;
            Ok(json!({
                "skills": all,
                "global_dir": global_dir(),
                "shared_dir": shared_dir(),
            }))
        }
        "get" => {
            let name = arg_str(&args, "name")?;
            collect(ctx, repo).into_iter().rev().find(|s| s["name"] == name.as_str()).ok_or_else(|| format!("no skill named {name}"))
        }
        "save" => {
            let name = arg_str(&args, "name")?;
            if !valid_name(&name) {
                return Err("Name may use letters, digits, - and _ (max 64)".into());
            }
            let body = arg_str(&args, "body")?;
            let dir = target_dir(ctx, &args)?.join(&name);
            if args["create"].as_bool() == Some(true) && dir.exists() {
                return Err(format!("A skill named {name} already exists"));
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("SKILL.md"), body).map_err(|e| e.to_string())?;
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        "delete" => {
            let name = arg_str(&args, "name")?;
            if !valid_name(&name) {
                return Err("bad name".into());
            }
            let dir = target_dir(ctx, &args)?.join(&name);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        "set_enabled" => {
            let key = format!("{}/{}", arg_str(&args, "scope")?, arg_str(&args, "name")?);
            let mut off = disabled(ctx);
            off.retain(|k| *k != key);
            if args["enabled"].as_bool() == Some(false) {
                off.push(key);
            }
            ctx.db.kv_set(DISABLED_KEY, &serde_json::to_string(&off).unwrap_or_default())?;
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        "pick_upload" => blocking(pick_upload).await,
        // {path} or {url} -> {token, skills:[{id,name,description,files,exists,problems}]}
        "stage" => blocking(move || stage(&args)).await,
        "install" => {
            let token = arg_str(&args, "token")?;
            let items = args["items"].as_array().cloned().unwrap_or_default();
            let done = blocking(move || install(&token, &items, &global_dir())).await?;
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(json!({ "installed": done }))
        }
        "discard" => {
            let _ = std::fs::remove_dir_all(stage_dir(&arg_str(&args, "token")?)?);
            Ok(Value::Null)
        }
        "update" => {
            let name = arg_str(&args, "name")?;
            if !valid_name(&name) {
                return Err("bad name".into());
            }
            blocking(move || update(&global_dir(), &name)).await?;
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        _ => Err(format!("unknown skills.{m}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, a: &[&str]) {
        assert!(std::process::Command::new("git").current_dir(dir).args(a).output().unwrap().status.success(), "git {a:?}");
    }

    #[test]
    fn upload_zip_folder_md_and_git_url() {
        let t = std::env::temp_dir().join(format!("philium-skills-t{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let src = t.join("src");
        std::fs::create_dir_all(src.join("pack/a/scripts")).unwrap();
        std::fs::write(src.join("pack/a/SKILL.md"), "---\nname: a\ndescription: A\n---\nbody").unwrap();
        std::fs::write(src.join("pack/a/scripts/run.sh"), "echo").unwrap();
        std::fs::create_dir_all(src.join("pack/b")).unwrap();
        std::fs::write(src.join("pack/b/SKILL.md"), "---\nname: b\n---\nno desc").unwrap();
        std::fs::write(src.join("one.md"), "---\nname: one\ndescription: One\n---\nhi").unwrap();
        let dest = t.join("dest");
        let st = t.join("st");

        // folder: finds both, flags script, flags invalid one
        let root = stage_upload(&src.join("pack"), &st.join("f")).unwrap();
        let p = preview("x", &root, &dest);
        let sk = p["skills"].as_array().unwrap();
        assert_eq!(sk.len(), 2);
        assert_eq!(sk[0]["files"].as_array().unwrap().iter().filter(|f| f["script"] == true).count(), 1);
        assert!(!sk[1]["problems"].as_array().unwrap().is_empty());
        assert_eq!(install_one(&root.join("a"), "a", "skip", &dest, None).unwrap(), Some("a".into()));
        assert!(dest.join("a/scripts/run.sh").is_file());
        // conflicts
        std::fs::write(dest.join("a/SKILL.md"), "mine").unwrap();
        assert_eq!(install_one(&root.join("a"), "a", "skip", &dest, None).unwrap(), None);
        assert_eq!(install_one(&root.join("a"), "a", "rename", &dest, None).unwrap(), Some("a-2".into()));
        install_one(&root.join("a"), "a", "replace", &dest, None).unwrap();
        assert!(std::fs::read_to_string(dest.join("a/SKILL.md")).unwrap().contains("name: a"));

        // zip and .skill
        git(&src, &["init", "-q"]);
        assert!(std::process::Command::new("zip").current_dir(src.join("pack")).args(["-qr", "../../a.skill", "a"]).status().unwrap().success());
        let z = stage_upload(&t.join("a.skill"), &st.join("z")).unwrap();
        assert_eq!(find_skills(&z), vec![PathBuf::from("a")]);
        // single md
        let m = stage_upload(&src.join("one.md"), &st.join("m")).unwrap();
        assert_eq!(find_skills(&m), vec![PathBuf::from("one")]);

        // git URL (local file:// is a real clone) + source record + update
        git(&src, &["add", "."]);
        git(&src, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]);
        let url = format!("file://{}", src.display());
        assert!(parse_url(&url).is_err());
        let (r, b, s) = parse_url("https://github.com/o/r/tree/main/skills/x").unwrap();
        assert_eq!((r.as_str(), b.as_deref(), s.as_str()), ("https://github.com/o/r", Some("main"), "skills/x"));
        let mut cmd = std::process::Command::new("git");
        cmd.args(["clone", "-q", "--depth", "1", &url]).arg(st.join("g"));
        run(&mut cmd, "clone").unwrap();
        let srcv = json!({ "url": url, "path": "pack/a" });
        install_one(&st.join("g/pack/a"), "ua", "skip", &dest, Some(&srcv)).unwrap();
        assert!(dest.join("ua").join(SOURCE_FILE).is_file());
        assert!(scan(&dest, "global").iter().any(|s| s["name"] == "ua" && s["source"]["path"] == "pack/a"));
        std::fs::remove_dir_all(&t).unwrap();
    }

    /// Network: real public repo, tree URL and whole repo, then update. Run with --ignored.
    #[test]
    #[ignore]
    fn public_git_url_end_to_end() {
        let dest = std::env::temp_dir().join(format!("philium-skills-net1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let p = stage(&json!({ "url": "https://github.com/anthropics/skills/tree/main/skills/pdf" })).unwrap();
        let sk = p["skills"].as_array().unwrap();
        assert_eq!(sk.len(), 1);
        assert_eq!(sk[0]["name"], "pdf");
        assert!(sk[0]["files"].as_array().unwrap().iter().any(|f| f["script"] == true));
        let done = install(p["token"].as_str().unwrap(), &[json!({ "id": sk[0]["id"], "action": "skip" })], &dest).unwrap();
        assert_eq!(done, vec!["pdf".to_string()]);
        assert!(dest.join("pdf").join(SOURCE_FILE).is_file());
        update(&dest, "pdf").unwrap();
        let many = stage(&json!({ "url": "https://github.com/anthropics/skills" })).unwrap();
        let n = many["skills"].as_array().unwrap().len();
        assert!(n > 3, "multi-skill repo found {n}");
        assert!(many["skills"].as_array().unwrap().iter().any(|s| s["name"] == "pdf" && s["exists"] == false));
        let _ = std::fs::remove_dir_all(stage_dir(many["token"].as_str().unwrap()).unwrap());
        std::fs::remove_dir_all(&dest).unwrap();
    }

    /// Real public git URL + dropped files, through stage/install/update. Run with --ignored.
    #[test]
    #[ignore]
    fn stage_install_update_public_url_and_drop() {
        let dest = std::env::temp_dir().join(format!("philium-skills-net{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let url = std::env::var("SKILL_URL").unwrap_or("https://github.com/anthropics/skills/tree/main/skills".into());
        let p = stage(&json!({ "url": url })).unwrap();
        let sk = p["skills"].as_array().unwrap();
        assert!(sk.len() > 1, "expected several skills");
        let first = &sk[0];
        let tok = p["token"].as_str().unwrap();
        let done = install(tok, &[json!({ "id": first["id"], "action": "skip" })], &dest).unwrap();
        assert_eq!(done.len(), 1);
        assert!(dest.join(&done[0]).join(SOURCE_FILE).is_file());
        std::fs::write(dest.join(&done[0]).join("SKILL.md"), "stale").unwrap();
        update(&dest, &done[0]).unwrap();
        assert_ne!(std::fs::read_to_string(dest.join(&done[0]).join("SKILL.md")).unwrap(), "stale");
        assert!(!stage_dir(tok).unwrap().exists());
        // Dropped folder as base64 files.
        use base64::Engine;
        let e = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let p = stage(&json!({ "files": [
            { "path": "dropped/SKILL.md", "b64": e("---\nname: dropped\ndescription: D\n---\n") },
            { "path": "dropped/run.py", "b64": e("print(1)") }] })).unwrap();
        assert_eq!(p["skills"][0]["name"], "dropped");
        assert!(stage(&json!({ "files": [{ "path": "../x", "b64": "" }] })).is_err());
        let _ = std::fs::remove_dir_all(stage_dir(p["token"].as_str().unwrap()).unwrap());
        std::fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn names_are_safe() {
        assert!(valid_name("review-pr"));
        assert!(!valid_name("../x") && !valid_name("") && !valid_name(".hidden") && !valid_name("a b"));
    }
}
