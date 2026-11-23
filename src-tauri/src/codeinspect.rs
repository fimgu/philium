//! Read-only, bounded inspection against the exact snapshot selected by the graph.
use super::*;
use std::path::Component;

const TEXT_CAP: usize = 256 * 1024;
const PATCH_CAP: usize = 512 * 1024;
const META_CAP: usize = 8 * 1024 * 1024;

const GIT_DEADLINE: Duration = Duration::from_secs(15);

// Own a fresh process group, never the application's group. Kill descendants too:
// a helper may retain stdout after the Git parent exits.
struct OwnedProcess(Option<std::process::Child>);
impl OwnedProcess {
    fn terminate(&mut self) {
        if let Some(child) = self.0.as_mut() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                // taskkill targets only the owned PID and its descendants. It is
                // itself bounded, and the direct child is killed below as well.
                if let Ok(mut killer) = Command::new("taskkill")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    let until = Instant::now() + Duration::from_millis(500);
                    while matches!(killer.try_wait(), Ok(None)) && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let _ = killer.kill();
                    std::thread::spawn(move || {
                        let _ = killer.wait();
                    });
                }
            }
            let _ = child.kill();
        }
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.terminate();
        if let Some(mut child) = self.0.take() {
            // Reap without an unbounded wait on the request's blocking worker.
            if !matches!(child.try_wait(), Ok(Some(_))) {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}

fn run_command(
    mut command: Command,
    cap: usize,
    diff: bool,
    timeout: Duration,
) -> Result<(Vec<u8>, bool), String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let deadline = Instant::now() + timeout;
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut owned = OwnedProcess(Some(child));
    let stdout = owned
        .0
        .as_mut()
        .unwrap()
        .stdout
        .take()
        .ok_or("missing git stdout")?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let result = stdout
            .take(cap as u64 + 1)
            .read_to_end(&mut out)
            .map(|_| out);
        let _ = tx.send(result);
    });
    let mut out = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "git inspection exceeded its execution deadline")?
        .map_err(|e| e.to_string())?;
    let truncated = out.len() > cap;
    if truncated {
        owned.terminate();
    }
    let status = loop {
        if let Some(status) = owned
            .0
            .as_mut()
            .unwrap()
            .try_wait()
            .map_err(|e| e.to_string())?
        {
            break status;
        }
        if Instant::now() >= deadline {
            return Err("git inspection exceeded its execution deadline".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    out.truncate(cap);
    if !truncated && !status.success() && !(diff && status.code() == Some(1)) {
        return Err("git inspection failed (repository or revision unavailable)".into());
    }
    Ok((out, truncated))
}

/// Stdout of `command`, at most `cap` bytes, killed with its children after `timeout`.
/// Exit 1 counts as success, as for grep and diff with nothing to report.
pub(super) fn bounded_output(command: Command, cap: usize, timeout: Duration) -> Result<Vec<u8>, String> {
    run_command(command, cap, true, timeout).map(|(out, _)| out)
}

// Never use workspace::git here: it trims output and has no execution deadline.
// All inspection Git entry points, including revision fallback, use this wrapper.
fn run(root: &Path, args: &[&str], cap: usize, diff: bool) -> Result<(Vec<u8>, bool), String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=false",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0");
    run_command(command, cap, diff, GIT_DEADLINE)
}

fn metadata(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let (bytes, truncated) = run(root, args, META_CAP, false)?;
    if truncated {
        return Err("repository change metadata exceeds inspection limit".into());
    }
    Ok(bytes)
}

pub(super) fn inspect_revision(root: &Path, reference: &str) -> Result<String, String> {
    let bytes = metadata(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| "invalid revision output")?
        .trim();
    if !(value.len() == 40 || value.len() == 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid revision output".into());
    }
    Ok(value.to_owned())
}

pub(super) fn private_name(path: &Path) -> bool {
    let normalized = path.to_string_lossy().to_ascii_lowercase();
    if [".config/gh", ".config/gcloud", ".config/hub", ".config/op"]
        .iter()
        .any(|p| {
            normalized == *p
                || normalized.starts_with(&format!("{p}/"))
                || normalized.contains(&format!("/{p}/"))
        })
    {
        return true;
    }
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .any(|part| {
            let p = part.to_ascii_lowercase();
            p == ".git"
                || p == ".env"
                || p.starts_with(".env.")
                || p.ends_with(".pem")
                || p.ends_with(".key")
                || matches!(
                    p.as_str(),
                    ".ssh"
                        | ".aws"
                        | ".azure"
                        | ".kube"
                        | ".gnupg"
                        | ".docker"
                        | ".netrc"
                        | ".git-credentials"
                        | ".npmrc"
                        | ".pypirc"
                        | ".password-store"
                        | ".vault-token"
                        | "credentials"
                        | "credentials.json"
                        | "secrets.json"
                        | "id_rsa"
                        | "id_ed25519"
                        | ".jcode"
                        | ".claude"
                        | ".codex"
                        | ".zsh_history"
                        | ".bash_history"
                        | ".python_history"
                        | ".node_repl_history"
                        | ".terraform.d"
                )
        })
}

fn vault_hit(path: &Path) -> bool {
    crate::secrets::dir()
        .map(|v| {
            path.starts_with(v)
                || v.canonicalize()
                    .map(|v| path.starts_with(v))
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Validate even missing paths by resolving every existing ancestor. Reject symlinks
/// altogether, including in-tree links, so a diff can never expose a link target.
pub(super) fn checked(root: &Path, path: &str) -> Result<PathBuf, String> {
    if path.is_empty()
        || path.contains('\0')
        || path.contains('\\')
        || path.contains(':')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || private_name(Path::new(path))
    {
        return Err("invalid or private inspection path".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if vault_hit(&root) {
        return Err("private vault is not inspectable".into());
    }
    let mut full = root.clone();
    for part in Path::new(path).components() {
        full.push(part);
        match std::fs::symlink_metadata(&full) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Err("symlink inspection is not allowed".into());
                }
                let resolved = full.canonicalize().map_err(|e| e.to_string())?;
                if !resolved.starts_with(&root)
                    || vault_hit(&resolved)
                    || private_name(&resolved.strip_prefix(&root).unwrap_or(&resolved))
                {
                    return Err("inspection path escapes repository or is private".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if std::fs::symlink_metadata(&full)
        .map(|m| !m.is_file())
        .unwrap_or(false)
    {
        return Err("inspection path is not a regular file".into());
    }
    Ok(full)
}

fn bounded_text(bytes: &[u8], cap: usize) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() > cap {
        let mut end = cap;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn base_for(ctx: &Ctx, t: &Target) -> Result<String, String> {
    if let Some(w) = &t.wt {
        if let Some(b) = ctx
            .db
            .query("SELECT base_commit FROM ci_wt WHERE wt_id=?1", [&w.id])?
            .first()
            .and_then(|r| r["base_commit"].as_str())
            .map(str::to_owned)
        {
            return Ok(b);
        }
    }
    if let Some(b) = ctx
        .db
        .query(
            "SELECT commit_id FROM ci_snap WHERE repo_id=?1",
            [&t.repo_id],
        )?
        .first()
        .and_then(|r| r["commit_id"].as_str())
        .map(str::to_owned)
    {
        return Ok(b);
    }
    inspect_revision(&t.repo_path, &t.branch).or_else(|_| inspect_revision(&t.repo_path, "HEAD"))
}

fn revision(t: &Target, base: &str) -> Result<String, String> {
    match &t.wt {
        None => Ok(base.to_owned()),
        Some(w) => Ok(format!(
            "{}:working-tree",
            inspect_revision(&w.path, "HEAD")?
        )),
    }
}

fn language(path: &str) -> Option<&'static str> {
    match Path::new(path).extension()?.to_str()? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "go" => Some("go"),
        "json" => Some("json"),
        "md" => Some("markdown"),
        "css" => Some("css"),
        "html" => Some("html"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

fn blob(root: &Path, base: &str, path: &str) -> Result<Option<(Vec<u8>, bool)>, String> {
    let listing = metadata(root, &["ls-tree", "-z", base, "--", path])?;
    let Some(entry) = listing.split(|b| *b == 0).find(|e| !e.is_empty()) else {
        return Ok(None);
    };
    let entry = std::str::from_utf8(entry).map_err(|_| "non-UTF8 git entry")?;
    let (meta, name) = entry.split_once('\t').ok_or("invalid git tree entry")?;
    let fields: Vec<_> = meta.split_whitespace().collect();
    if name != path || fields.len() != 3 || fields[1] != "blob" {
        return Ok(None);
    }
    if fields[0] == "120000" {
        return Err("symlink inspection is not allowed".into());
    }
    run(root, &["cat-file", "blob", fields[2]], TEXT_CAP, false).map(Some)
}

#[derive(Clone, Debug)]
struct Change {
    path: String,
    old: Option<String>,
    status: String,
    added: Option<u64>,
    removed: Option<u64>,
}
impl Change {
    fn json(&self) -> Value {
        json!({"path":self.path,"old_path":self.old,"status":self.status,"added":self.added,"removed":self.removed})
    }
}

fn changes(root: &Path, base: &str) -> Result<(Vec<Change>, bool), String> {
    // -z numstat preserves tabs/newlines and reports rename pairs without quoting.
    let bytes = metadata(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            "--find-renames",
            base,
            "--",
        ],
    )?;
    let mut fields = bytes.split(|b| *b == 0);
    let mut result = Vec::new();
    let mut omitted = false;
    while let Some(field) = fields.next() {
        if result.len() >= 10000 {
            omitted = true;
            break;
        }
        if field.is_empty() {
            continue;
        }
        let mut parts = field.splitn(3, |b| *b == b'\t');
        let add = std::str::from_utf8(parts.next().ok_or("bad numstat")?)
            .ok()
            .and_then(|s| s.parse().ok());
        let remove = std::str::from_utf8(parts.next().ok_or("bad numstat")?)
            .ok()
            .and_then(|s| s.parse().ok());
        let name = parts.next().ok_or("bad numstat path")?;
        let (old, name) = if name.is_empty() {
            (
                Some(fields.next().ok_or("bad rename source")?),
                fields.next().ok_or("bad rename destination")?,
            )
        } else {
            (None, name)
        };
        let Ok(path) = std::str::from_utf8(name) else {
            omitted = true;
            continue;
        };
        let old = match old {
            Some(v) => match std::str::from_utf8(v) {
                Ok(v) => Some(v.to_string()),
                Err(_) => {
                    omitted = true;
                    continue;
                }
            },
            None => None,
        };
        if checked(root, path).is_err()
            || old
                .as_ref()
                .map(|p| checked(root, p).is_err())
                .unwrap_or(false)
        {
            omitted = true;
            continue;
        }
        // Name-status gives additions/deletions correctly, including empty files.
        result.push(Change {
            path: path.into(),
            old,
            status: "M".into(),
            added: add,
            removed: remove,
        });
    }
    let statuses = metadata(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "-z",
            "--find-renames",
            base,
            "--",
        ],
    )?;
    let mut fields = statuses.split(|b| *b == 0);
    while let Some(s) = fields.next() {
        if s.is_empty() {
            continue;
        }
        let first = fields.next().ok_or("bad diff status")?;
        let name = if s[0] == b'R' || s[0] == b'C' {
            fields.next().ok_or("bad rename status")?
        } else {
            first
        };
        if let Ok(path) = std::str::from_utf8(name) {
            if let Some(c) = result.iter_mut().find(|c| c.path == path) {
                c.status = (s[0] as char).to_string();
            }
        }
    }
    let untracked = metadata(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for name in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        if result.len() >= 10000 {
            omitted = true;
            break;
        }
        let Ok(path) = std::str::from_utf8(name) else {
            omitted = true;
            continue;
        };
        if checked(root, path).is_err() {
            omitted = true;
            continue;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(path))
            .map_err(|e| e.to_string())?
            .take(TEXT_CAP as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let added = if bytes.len() <= TEXT_CAP
            && !bytes.contains(&0)
            && std::str::from_utf8(&bytes).is_ok()
        {
            Some(
                bytes.iter().filter(|b| **b == b'\n').count() as u64
                    + u64::from(!bytes.is_empty() && bytes.last() != Some(&b'\n')),
            )
        } else {
            None
        };
        result.push(Change {
            path: path.into(),
            old: None,
            status: "?".into(),
            added,
            removed: Some(0),
        });
    }
    let unmerged = metadata(root, &["diff", "--name-only", "--diff-filter=U", "-z", "--"])?;
    for path in unmerged.split(|b| *b == 0).filter_map(|path| std::str::from_utf8(path).ok()) {
        if path.is_empty() || checked(root, path).is_err() {
            continue;
        }
        if let Some(change) = result.iter_mut().find(|change| change.path == path) {
            change.status = "U".into();
        } else {
            result.push(Change { path: path.into(), old: None, status: "U".into(), added: None, removed: None });
        }
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    if result.len() > 10000 {
        result.truncate(10000);
        omitted = true;
    }
    Ok((result, omitted))
}

pub(super) fn call(ctx: &Ctx, t: &Target, method: &str, args: &Value) -> Result<Value, String> {
    let root =
        t.wt.as_ref()
            .map(|w| w.path.as_path())
            .unwrap_or(&t.repo_path);
    let base = if method != "source" && t.wt.is_some() && args["comparison"] == "working" {
        inspect_revision(root, "HEAD")?
    } else {
        base_for(ctx, t)?
    };
    let rev = revision(t, &base)?;
    let id = t.wt.as_ref().map(|w| w.id.as_str());
    if method == "changes" {
        let (files, truncated, note) = if t.wt.is_some() {
            let (files, omitted) = changes(root, &base)?;
            (
                files.iter().map(Change::json).collect::<Vec<_>>(),
                omitted,
                if omitted {
                    Some("Some private, unsafe, non-UTF8 or excess paths were omitted.")
                } else {
                    None
                },
            )
        } else {
            (
                vec![],
                false,
                Some("Base snapshot selected. Select a working tree to inspect changes."),
            )
        };
        return Ok(
            json!({"base_commit":base,"revision":rev,"worktree_id":id,"files":files,"truncated":truncated,"note":note}),
        );
    }
    let path = args["path"].as_str().ok_or("missing `path`")?;
    let full = checked(root, path)?;
    if method == "source" {
        let bytes = if t.wt.is_none() {
            blob(root, &base, path)?
        } else {
            match std::fs::File::open(&full) {
                Ok(f) => {
                    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
                        return Err("inspection path is not a regular file".into());
                    }
                    let mut bytes = Vec::new();
                    f.take(TEXT_CAP as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    let truncated = bytes.len() > TEXT_CAP;
                    bytes.truncate(TEXT_CAP);
                    Some((bytes, truncated))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.to_string()),
            }
        };
        let exists = bytes.is_some();
        let (bytes, truncated) = bytes.unwrap_or_default();
        let binary = bytes.contains(&0) || (!truncated && std::str::from_utf8(&bytes).is_err());
        let text = if binary {
            None
        } else {
            Some(bounded_text(&bytes, TEXT_CAP))
        };
        let note = if !exists {
            Some("File does not exist in the selected revision.")
        } else if binary {
            Some("Binary content is not displayed.")
        } else if truncated {
            Some("Source truncated at 256 KiB.")
        } else {
            None
        };
        return Ok(
            json!({"path":path,"text":text,"language":language(path),"revision":rev,"base_commit":base,"worktree_id":id,"exists":exists,"binary":binary,"truncated":truncated,"note":note}),
        );
    }
    if t.wt.is_none() {
        return Ok(
            json!({"path":path,"old_path":null,"patch":"","base_commit":base,"revision":rev,"worktree_id":id,"binary":false,"truncated":false,"note":"Base snapshot selected. Select a working tree to inspect changes."}),
        );
    }
    let (files, _) = changes(root, &base)?;
    let change = files
        .iter()
        .find(|c| c.path == path || c.old.as_deref() == Some(path));
    let Some(c) = change else {
        return Ok(
            json!({"path":path,"old_path":null,"patch":"","base_commit":base,"revision":rev,"worktree_id":id,"binary":false,"truncated":false,"note":"No changes for this path relative to the selected graph base."}),
        );
    };
    let mut cmd = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
    ];
    cmd.push(if args["more_context"] == true { "--unified=20" } else { "--unified=3" });
    if c.status == "?" {
        cmd.extend(["--no-index", "--", "/dev/null", &c.path]);
    } else {
        cmd.extend([&base, "--", &c.path]);
        if let Some(old) = &c.old {
            cmd.push(old);
        }
    }
    let (bytes, truncated) = run(root, &cmd, PATCH_CAP, true)?;
    let patch = bounded_text(&bytes, PATCH_CAP);
    let binary = (!matches!(c.status.as_str(), "?" | "U") && c.added.is_none()) || patch.contains("Binary files ");
    Ok(
        json!({"path":c.path,"old_path":c.old,"patch":patch,"base_commit":base,"revision":rev,"worktree_id":id,"binary":binary,"truncated":truncated,"note":if truncated {Some("Patch truncated at 512 KiB.")} else if binary {Some("Binary content is not displayed.")} else {None}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        wt: PathBuf,
        ctx: Arc<Ctx>,
        base: Target,
        disk: Target,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("inspect-{}", crate::db::new_id()));
            let wt = root.join("agent");
            std::fs::create_dir_all(&root).unwrap();
            sh(&root, &["init", "-q", "-b", "main"]);
            for (name, text) in [
                ("a.py", "def initial():\n    return 1\n"),
                ("delete.py", "def gone():\n    pass\n"),
                (
                    "rename.py",
                    "def moved():\n    return 'long enough unique rename content'\n",
                ),
            ] {
                std::fs::write(root.join(name), text).unwrap();
            }
            std::fs::write(root.join("binary.bin"), [0, 1, 2]).unwrap();
            sh(&root, &["add", "."]);
            sh(&root, &["commit", "-qm", "base"]);
            sh(
                &root,
                &["worktree", "add", "-qb", "agent", wt.to_str().unwrap()],
            );
            let ctx = Ctx::test();
            ctx.db
                .with(|c| c.execute_batch(super::super::SCHEMA))
                .unwrap();
            let base = Target {
                repo_id: "fixture".into(),
                repo_path: root.clone(),
                branch: "main".into(),
                wt: None,
            };
            let disk = Target {
                wt: Some(Wt {
                    id: "agent".into(),
                    path: wt.clone(),
                    label: "Agent".into(),
                }),
                ..base.clone()
            };
            ensure_base(&ctx, "fixture", &root, "main").unwrap();
            Self {
                root,
                wt,
                ctx,
                base,
                disk,
            }
        }
        fn get(&self, t: &Target, method: &str, path: &str) -> Value {
            call(&self.ctx, t, method, &json!({"path":path})).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn sh(root: &Path, args: &[&str]) {
        let o = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    #[test]
    fn immutable_base_and_current_worktree_revisions() {
        let f = Fixture::new();
        let base = f.get(&f.base, "source", "a.py");
        assert_eq!(base["text"], "def initial():\n    return 1\n");
        assert_eq!(base["language"], "python");
        assert_eq!(base["revision"], base["base_commit"]);
        std::fs::write(f.root.join("a.py"), "def local():\n    pass\n").unwrap();
        std::fs::write(f.wt.join("a.py"), "def committed():\n    pass\n").unwrap();
        sh(&f.wt, &["add", "a.py"]);
        sh(&f.wt, &["commit", "-qm", "agent commit"]);
        std::fs::write(f.wt.join("a.py"), "def staged():\n    pass\n").unwrap();
        sh(&f.wt, &["add", "a.py"]);
        std::fs::write(f.wt.join("a.py"), "def current():\n    pass\n").unwrap();
        assert_eq!(f.get(&f.base, "source", "a.py")["text"], base["text"]);
        let current = f.get(&f.disk, "source", "a.py");
        assert_eq!(current["text"], "def current():\n    pass\n");
        assert_eq!(current["base_commit"], base["base_commit"]);
        assert!(current["revision"]
            .as_str()
            .unwrap()
            .ends_with(":working-tree"));
        let diff = f.get(&f.disk, "file_diff", "a.py");
        assert!(diff["patch"].as_str().unwrap().contains("-def initial():"));
        assert!(diff["patch"].as_str().unwrap().contains("+def current():"));
        assert_eq!(diff["base_commit"], base["base_commit"]);
        let working = call(&f.ctx, &f.disk, "file_diff", &json!({"path":"a.py","comparison":"working"})).unwrap();
        assert_ne!(working["base_commit"], base["base_commit"]);
        assert!(working["patch"].as_str().unwrap().contains("-def committed():"));
        assert!(working["patch"].as_str().unwrap().contains("+def current():"));
        assert_eq!(f.get(&f.base, "changes", "")["files"], json!([]));
        assert!(f.get(&f.base, "changes", "")["note"].is_string());
        assert_eq!(f.get(&f.base, "file_diff", "a.py")["patch"], "");
    }

    #[test]
    fn changes_cover_rename_delete_added_untracked_and_binary() {
        let f = Fixture::new();
        sh(&f.wt, &["mv", "rename.py", "moved.py"]);
        std::fs::remove_file(f.wt.join("delete.py")).unwrap();
        std::fs::write(f.wt.join("added.py"), "def added():\n    pass\n").unwrap();
        sh(&f.wt, &["add", "added.py"]);
        std::fs::write(f.wt.join("new.py"), "def new():\n    pass\n").unwrap();
        std::fs::write(f.wt.join("binary.bin"), [0, 3, 4]).unwrap();
        let changes = f.get(&f.disk, "changes", "");
        let files = changes["files"].as_array().unwrap();
        for (path, status) in [
            ("moved.py", "R"),
            ("delete.py", "D"),
            ("added.py", "A"),
            ("new.py", "?"),
            ("binary.bin", "M"),
        ] {
            assert_eq!(
                files.iter().find(|c| c["path"] == path).unwrap()["status"],
                status
            );
        }
        assert_eq!(
            files.iter().find(|c| c["path"] == "moved.py").unwrap()["old_path"],
            "rename.py"
        );
        assert_eq!(f.get(&f.disk, "source", "delete.py")["exists"], false);
        assert_eq!(f.get(&f.base, "source", "delete.py")["exists"], true);
        assert!(f.get(&f.disk, "file_diff", "delete.py")["patch"]
            .as_str()
            .unwrap()
            .contains("deleted file"));
        assert!(f.get(&f.disk, "file_diff", "new.py")["patch"]
            .as_str()
            .unwrap()
            .contains("+def new():"));
        assert_eq!(
            f.get(&f.disk, "file_diff", "rename.py")["old_path"],
            "rename.py"
        );
        assert_eq!(f.get(&f.disk, "file_diff", "binary.bin")["binary"], true);
        assert_eq!(f.get(&f.disk, "source", "binary.bin")["text"], Value::Null);
        for t in [&f.base, &f.disk] {
            assert_eq!(f.get(t, "source", "missing.py")["exists"], false);
        }
        assert_eq!(f.get(&f.disk, "file_diff", "missing.py")["patch"], "");
    }

    #[test]
    fn paths_are_literal_private_and_escape_safe() {
        let f = Fixture::new();
        for path in [
            "",
            "/etc/passwd",
            "../a.py",
            "a/../a.py",
            "a\0.py",
            ".git/config",
            ".GiT/config",
            ".env",
            ".env.local",
            ".ssh/id_rsa",
            "key.pem",
            "a\\b",
            "C:/file",
        ] {
            for method in ["source", "file_diff"] {
                assert!(
                    call(&f.ctx, &f.disk, method, &json!({"path":path})).is_err(),
                    "{method} accepted {path:?}"
                );
            }
        }
        std::fs::write(f.wt.join("[x].py"), "literal\n").unwrap();
        std::fs::write(f.wt.join("x.py"), "other\n").unwrap();
        assert_eq!(f.get(&f.disk, "source", "[x].py")["text"], "literal\n");
        let patch = f.get(&f.disk, "file_diff", "[x].py");
        assert!(patch["patch"].as_str().unwrap().contains("+literal"));
        assert!(!patch["patch"].as_str().unwrap().contains("+other"));
        std::fs::write(f.wt.join(".env"), "secret\n").unwrap();
        let changes = f.get(&f.disk, "changes", "");
        assert_eq!(changes["truncated"], true);
        assert!(!changes.to_string().contains(".env"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&f.root, f.wt.join("escape")).unwrap();
            std::os::unix::fs::symlink(f.root.join("a.py"), f.wt.join("link.py")).unwrap();
            assert!(checked(&f.wt, "escape/a.py").is_err());
            assert!(checked(&f.wt, "escape/missing.py").is_err());
            assert!(checked(&f.wt, "link.py").is_err());
        }
    }

    #[test]
    fn source_and_patch_are_bounded_in_both_modes() {
        let f = Fixture::new();
        let text = "print('some content')\n".repeat(40000);
        std::fs::write(f.wt.join("large.py"), &text).unwrap();
        let source = f.get(&f.disk, "source", "large.py");
        assert_eq!(source["truncated"], true);
        assert_eq!(source["text"].as_str().unwrap().len(), TEXT_CAP);
        let patch = f.get(&f.disk, "file_diff", "large.py");
        assert_eq!(patch["truncated"], true);
        assert!(patch["patch"].as_str().unwrap().len() <= PATCH_CAP);
        sh(&f.wt, &["add", "large.py"]);
        assert_eq!(f.get(&f.disk, "file_diff", "large.py")["truncated"], true);
        std::fs::write(f.root.join("large.py"), &text).unwrap();
        sh(&f.root, &["add", "large.py"]);
        sh(&f.root, &["commit", "-qm", "large base"]);
        ensure_base(&f.ctx, "fixture", &f.root, "main").unwrap();
        assert_eq!(f.get(&f.base, "source", "large.py")["truncated"], true);
    }

    #[test]
    fn cached_graph_base_survives_branch_movement_without_reindex() {
        let f = Fixture::new();
        let original = f.get(&f.base, "source", "a.py");
        std::fs::write(f.root.join("a.py"), "def advanced():\n    pass\n").unwrap();
        sh(&f.root, &["add", "a.py"]);
        sh(&f.root, &["commit", "-qm", "branch advances after graph"]);
        assert_eq!(f.get(&f.base, "source", "a.py"), original);
        let changes = f.get(&f.disk, "changes", "");
        assert_eq!(changes["base_commit"], original["base_commit"]);
        assert_eq!(changes["files"], json!([]));
    }

    #[test]
    fn external_diff_and_textconv_are_never_invoked() {
        let f = Fixture::new();
        sh(&f.root, &["config", "diff.external", "/not-a-real-command"]);
        sh(
            &f.root,
            &["config", "diff.inspector.textconv", "/not-a-real-command"],
        );
        std::fs::write(f.wt.join(".gitattributes"), "*.py diff=inspector\n").unwrap();
        std::fs::write(f.wt.join("a.py"), "def safe():\n    pass\n").unwrap();
        assert!(f.get(&f.disk, "file_diff", "a.py")["patch"]
            .as_str()
            .unwrap()
            .contains("+def safe():"));
        assert_eq!(
            f.get(&f.disk, "changes", "")["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["path"] == "a.py")
                .unwrap()["status"],
            "M"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fsmonitor_helper_is_disabled_for_inspection_and_revision_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let helper = f.root.join("fsmonitor-helper");
        std::fs::write(
            &helper,
            "#!/bin/sh\nprintf called > \"$0.marker\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        sh(
            &f.root,
            &["config", "core.fsmonitor", helper.to_str().unwrap()],
        );
        // Positive control: ordinary Git really invokes this repository helper.
        assert!(Command::new("git")
            .arg("-C")
            .arg(&f.wt)
            .args(["ls-files", "--others", "--exclude-standard"])
            .output()
            .unwrap()
            .status
            .success());
        let actual_marker = helper.with_file_name("fsmonitor-helper.marker");
        assert!(actual_marker.exists());
        std::fs::remove_file(&actual_marker).unwrap();
        f.get(&f.disk, "changes", "");
        f.get(&f.disk, "file_diff", "a.py");
        f.ctx
            .db
            .with(|c| c.execute("DELETE FROM ci_snap", []))
            .unwrap();
        assert!(f.get(&f.base, "source", "a.py")["exists"]
            .as_bool()
            .unwrap());
        assert_eq!(
            super::super::rev_parse(&f.root, "missing-branch").unwrap(),
            inspect_revision(&f.root, "HEAD").unwrap()
        );
        assert!(!actual_marker.exists());
    }

    #[cfg(unix)]
    fn assert_stopped(pid: i32) {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            let output = Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            // A killed orphan can briefly await the OS init process's reap.
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                return;
            }
            assert!(
                Instant::now() < until,
                "owned process {pid} remains live: {state}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_deadline_covers_silent_reads_wait_and_inherited_stdout() {
        let f = Fixture::new();
        for (name, script) in [
            ("silent", "echo $$ > \"$1\"; exec sleep 30"),
            ("closed", "echo $$ > \"$1\"; exec 1>&-; exec sleep 30"),
            ("inherited", "sleep 30 & echo $! > \"$1\"; exit 0"),
        ] {
            let pidfile = f.root.join(name);
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script, "fixture", pidfile.to_str().unwrap()]);
            let start = Instant::now();
            let err = run_command(command, 1024, false, Duration::from_millis(200)).unwrap_err();
            assert!(err.contains("deadline"), "{name}: {err}");
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "{name} exceeded bound"
            );
            let pid = std::fs::read_to_string(pidfile)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_stopped(pid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_cap_kills_and_reaps_owned_writer() {
        let f = Fixture::new();
        let pidfile = f.root.join("writer-pid");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "echo $$ > \"$1\"; while :; do printf '0123456789'; done",
            "fixture",
            pidfile.to_str().unwrap(),
        ]);
        let start = Instant::now();
        let (bytes, truncated) = run_command(command, 100, false, Duration::from_secs(2)).unwrap();
        assert_eq!(bytes.len(), 100);
        assert!(truncated);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_stopped(
            std::fs::read_to_string(pidfile)
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn historical_symlink_blobs_and_nonregular_files_are_rejected() {
        let f = Fixture::new();
        std::os::unix::fs::symlink("a.py", f.root.join("historical.py")).unwrap();
        sh(&f.root, &["add", "historical.py"]);
        sh(&f.root, &["commit", "-qm", "symlink"]);
        ensure_base(&f.ctx, "fixture", &f.root, "main").unwrap();
        std::fs::remove_file(f.root.join("historical.py")).unwrap();
        assert!(call(&f.ctx, &f.base, "source", &json!({"path":"historical.py"})).is_err());
        assert!(checked(&f.root, "agent").is_err());
        let fifo = f.wt.join("fifo.py");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(checked(&f.wt, "fifo.py").is_err());
    }

    #[tokio::test]
    async fn public_dispatch_and_local_checkout_graph_share_target() {
        let f = Fixture::new();
        f.ctx.db.with(|c| c.execute("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES(?1,'Fixture',?2,'main',0)", params!["fixture", f.root.to_str().unwrap()])).unwrap();
        std::fs::write(f.root.join("a.py"), "def local_only():\n    pass\n").unwrap();
        let args = json!({"repo_id":"fixture","working_tree":true});
        let t = resolve(&f.ctx, &args).unwrap();
        assert_eq!(t.wt.as_ref().unwrap().id, "local:fixture");
        assert_eq!(
            resolve(&f.ctx, &json!({"worktree_id":"local:fixture"}))
                .unwrap()
                .wt
                .unwrap()
                .path,
            f.root
        );
        let tree = super::super::call(&f.ctx, "tree", args.clone())
            .await
            .unwrap();
        assert!(tree.to_string().contains("local_only"));
        let source = super::super::call(
            &f.ctx,
            "source",
            json!({"repo_id":"fixture","working_tree":true,"path":"a.py"}),
        )
        .await
        .unwrap();
        assert_eq!(source["base_commit"], tree["base_commit"]);
        assert_eq!(source["worktree_id"], "local:fixture");
        assert!(source["text"].as_str().unwrap().contains("local_only"));
    }
}
