//! Universal, multimodal, security-hardened external filesystem reader/writer.
//!
//! All operations execute ONLY after `access.rs` has authorized a grant and
//! handed us `(root: &Path, is_single_file: bool, label: &str)`.
//! Every operation enforces strict canonical path containment, symlink escape
//! blocking, and secret-file blocking.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SEARCH_FILES: usize = 5_000;
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
const MAX_TEXT_SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CMD_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const CMD_TIMEOUT: Duration = Duration::from_secs(10);

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    ".next",
    ".venv",
    "venv",
];

const SECRET_DIR_COMPONENTS: &[(&str, &str)] = &[
    (".ssh", "SSH directory (.ssh)"),
    (".aws", "AWS credentials directory (.aws)"),
    (".gnupg", "GnuPG directory (.gnupg)"),
    (".kube", "Kubernetes config directory (.kube)"),
    (".docker", "Docker config directory (.docker)"),
    ("keychains", "Keychains directory"),
];

const SECRET_EXTENSIONS: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".kdbx",
    ".jks",
    ".keystore",
];

const SECRET_EXACT_NAMES: &[&str] = &[
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "credentials.json",
    ".vault-token",
];

const DOCUMENT_EXTENSIONS: &[&str] = &["docx", "doc", "rtf", "odt", "webarchive"];

const KNOWN_BINARY_EXTENSIONS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "exe", "dll", "so", "dylib", "wasm", "o",
    "a", "class", "pyc", "pyo", "sqlite", "sqlite3", "db", "mp3", "mp4", "mov", "wav", "avi",
    "mkv", "flac", "ogg", "m4a", "ttf", "otf", "woff", "woff2", "ico", "icns", "dmg", "pkg", "iso",
    "bin", "dat", "xls", "xlsx", "ppt", "pptx", "bmp", "tiff", "tif", "heic", "avif",
];

fn path_is_inside_dir(path: &Path, dir: &Path) -> bool {
    if path.starts_with(dir) {
        return true;
    }
    if let Ok(canon_dir) = fs::canonicalize(dir) {
        if path.starts_with(&canon_dir) {
            return true;
        }
        if let Ok(canon_path) = fs::canonicalize(path) {
            if canon_path.starts_with(&canon_dir) {
                return true;
            }
        }
    }
    false
}

/// Returns `Some(reason)` if `path` points to a secret file, secret directory,
/// or Philium internal secret/pilot data directory.
pub fn is_secret_path(path: &Path) -> Option<&'static str> {
    if let Some(sec_dir) = crate::secrets::dir() {
        if path_is_inside_dir(path, sec_dir) {
            return Some("Philium secrets directory");
        }
    }
    if let Some(pilot_dir) = crate::pilot::data_dir() {
        if path_is_inside_dir(path, &pilot_dir) {
            return Some("Philium pilot data directory");
        }
    }

    for comp in path.components() {
        if let Component::Normal(os) = comp {
            let lower = os.to_string_lossy().to_ascii_lowercase();
            for &(dir_name, reason) in SECRET_DIR_COMPONENTS {
                if lower == dir_name {
                    return Some(reason);
                }
            }
        }
    }

    if let Some(fname) = path.file_name() {
        let lower = fname.to_string_lossy().to_ascii_lowercase();

        if lower.starts_with(".env") {
            if !matches!(
                lower.as_str(),
                ".env.example" | ".env.sample" | ".env.template"
            ) {
                return Some("environment secrets file (.env*)");
            }
        }

        for &ext in SECRET_EXTENSIONS {
            if lower.len() > ext.len() && lower.ends_with(ext) {
                return Some("private key or keystore file");
            }
        }

        if lower == "id_rsa"
            || lower.starts_with("id_rsa.")
            || lower == "id_ed25519"
            || lower.starts_with("id_ed25519.")
            || lower.starts_with("id_ecdsa")
            || lower.starts_with("id_dsa")
        {
            return Some("SSH private key file");
        }

        if SECRET_EXACT_NAMES.contains(&lower.as_str()) {
            return Some("credential store file");
        }
    }

    None
}

fn secret_err(why: &str) -> String {
    format!("Access denied: secret file ({why})")
}

/// Resolves `rel_path` strictly inside `root`, defeating `..` traversal,
/// symlink escapes, broken symlinks, and secret paths.
pub(crate) fn resolve_inside(
    root: &Path,
    is_single_file: bool,
    rel_path: Option<&str>,
    allow_missing_leaf: bool,
) -> Result<PathBuf, String> {
    if let Some(why) = is_secret_path(root) {
        return Err(secret_err(why));
    }
    let canon_root =
        fs::canonicalize(root).map_err(|e| format!("Invalid grant root {}: {e}", root.display()))?;
    if let Some(why) = is_secret_path(&canon_root) {
        return Err(secret_err(why));
    }

    let raw = rel_path.unwrap_or("").trim();
    if raw.contains('\0') {
        return Err("Access denied: path contains NUL byte".into());
    }

    if is_single_file {
        if !canon_root.is_file() {
            return Err("Invalid single-file grant: root is not a regular file".into());
        }
        if raw.is_empty() || raw == "." || raw == "./" {
            return Ok(canon_root);
        }
        let p = Path::new(raw);
        let mut normals = Vec::new();
        for comp in p.components() {
            match comp {
                Component::CurDir => {}
                Component::Normal(c) => normals.push(c),
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err("Access denied: grant is restricted to a single file".into());
                }
            }
        }
        if normals.len() == 1 {
            let requested = normals[0];
            let matches_canon = canon_root.file_name().is_some_and(|n| n == requested);
            let matches_root = root.file_name().is_some_and(|n| n == requested);
            if matches_canon || matches_root {
                return Ok(canon_root);
            }
        }
        return Err("Access denied: grant is restricted to a single file".into());
    }

    if !canon_root.is_dir() {
        return Err("Invalid folder grant: root is not a directory".into());
    }

    if raw.is_empty() || raw == "." {
        return Ok(canon_root);
    }

    let p = Path::new(raw);
    if p.is_absolute() {
        return Err("Access denied: absolute paths are not allowed".into());
    }

    let mut rel_stack: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(c) => rel_stack.push(c),
            Component::ParentDir => {
                if rel_stack.pop().is_none() {
                    return Err("Access denied: path traversal outside grant root".into());
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("Access denied: absolute paths are not allowed".into());
            }
        }
    }

    let mut candidate = canon_root.clone();
    for c in &rel_stack {
        candidate.push(c);
        if let Some(why) = is_secret_path(&candidate) {
            return Err(secret_err(why));
        }
    }

    if fs::symlink_metadata(&candidate).is_ok() {
        let canon = fs::canonicalize(&candidate).map_err(|_| {
            "Access denied: broken symlink or inaccessible target".to_string()
        })?;
        if !canon.starts_with(&canon_root) {
            return Err("Access denied: path escapes grant root via symlink".into());
        }
        if let Some(why) = is_secret_path(&canon) {
            return Err(secret_err(why));
        }
        return Ok(canon);
    }

    if !allow_missing_leaf {
        return Err(format!("Path not found: {raw}"));
    }

    // Walk prefix-by-prefix to verify the nearest existing ancestor and ensure
    // no intermediate symlink escapes `canon_root`.
    let mut cur = canon_root.clone();
    let mut i = 0;
    while i < rel_stack.len() {
        let next = cur.join(rel_stack[i]);
        if let Some(why) = is_secret_path(&next) {
            return Err(secret_err(why));
        }
        match fs::symlink_metadata(&next) {
            Ok(_) => {
                let canon_next = fs::canonicalize(&next).map_err(|_| {
                    "Access denied: symlink target does not exist or escapes grant root".to_string()
                })?;
                if !canon_next.starts_with(&canon_root) {
                    return Err("Access denied: symlink escapes grant root".into());
                }
                if let Some(why) = is_secret_path(&canon_next) {
                    return Err(secret_err(why));
                }
                cur = canon_next;
                i += 1;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if !cur.is_dir() {
                    return Err(format!(
                        "Cannot create path: parent {} is not a directory",
                        rel_display(&canon_root, &cur)
                    ));
                }
                for rest in &rel_stack[i..] {
                    cur.push(rest);
                    if let Some(why) = is_secret_path(&cur) {
                        return Err(secret_err(why));
                    }
                }
                break;
            }
            Err(e) => return Err(format!("Cannot inspect path: {e}")),
        }
    }

    if !cur.starts_with(&canon_root) {
        return Err("Access denied: resolved path escapes grant root".into());
    }
    if let Some(why) = is_secret_path(&cur) {
        return Err(secret_err(why));
    }

    Ok(cur)
}

fn rel_display(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
        _ => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string()),
    }
}

fn lower_ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn image_mime_from_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn image_mime_from_magic(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if header.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn classify_media_type_by_ext(path: &Path) -> &'static str {
    let ext = lower_ext(path);
    if image_mime_from_ext(&ext).is_some() {
        "image"
    } else if ext == "pdf" {
        "pdf"
    } else if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
        "document"
    } else if KNOWN_BINARY_EXTENSIONS.contains(&ext.as_str()) {
        "binary"
    } else {
        "text"
    }
}

fn is_skip_dir_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SKIP_DIRS.contains(&lower.as_str())
}

/// Case-insensitive wildcard glob matcher supporting `*` and `?`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.to_lowercase().chars().collect();
    let txt: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti: usize = 0;

    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }

    pi == pat.len()
}

fn matches_glob(glob: Option<&str>, rel_path: &str, file_name: &str) -> bool {
    match glob.map(str::trim).filter(|g| !g.is_empty()) {
        None => true,
        Some(pat) => glob_match(pat, rel_path) || glob_match(pat, file_name),
    }
}

/// Lists files and directories inside an authorized grant.
pub fn fs_list(
    root: &Path,
    is_single_file: bool,
    rel_path: Option<&str>,
    glob: Option<&str>,
    depth: usize,
    limit: usize,
) -> Result<Value, String> {
    let max_depth = if depth == 0 { 2 } else { depth.clamp(1, 5) };
    let max_limit = if limit == 0 {
        200
    } else {
        limit.clamp(1, 500)
    };

    let canon_root =
        fs::canonicalize(root).map_err(|e| format!("Invalid grant root {}: {e}", root.display()))?;
    let target = resolve_inside(root, is_single_file, rel_path, false)?;

    let root_label = match rel_path.map(str::trim).filter(|s| !s.is_empty() && *s != ".") {
        Some(r) => r.to_string(),
        None => canon_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string()),
    };

    if is_single_file || target.is_file() {
        let meta = fs::metadata(&target).map_err(|e| format!("Cannot read metadata: {e}"))?;
        let fname = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rel = if is_single_file {
            fname.clone()
        } else {
            rel_display(&canon_root, &target)
        };
        let mut entries = Vec::new();
        if matches_glob(glob, &rel, &fname) {
            entries.push(json!({
                "path": rel,
                "kind": "file",
                "size": meta.len(),
                "ext": lower_ext(&target),
                "media_type": classify_media_type_by_ext(&target),
            }));
        }
        return Ok(json!({
            "root": root_label,
            "entries": entries,
            "total": entries.len(),
            "truncated": false,
        }));
    }

    let mut entries: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    visited_dirs.insert(target.clone());

    #[allow(clippy::too_many_arguments)]
    fn walk_list(
        canon_root: &Path,
        dir: &Path,
        cur_depth: usize,
        max_depth: usize,
        max_limit: usize,
        glob: Option<&str>,
        visited_dirs: &mut HashSet<PathBuf>,
        entries: &mut Vec<Value>,
        truncated: &mut bool,
    ) {
        if cur_depth > max_depth || *truncated {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|de| de.path())).collect();
        children.sort();

        for child in children {
            if *truncated {
                return;
            }
            if is_secret_path(&child).is_some() {
                continue;
            }
            let Ok(sym_meta) = fs::symlink_metadata(&child) else {
                continue;
            };
            let fname = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();

            let (resolved, meta) = if sym_meta.file_type().is_symlink() {
                let Ok(canon) = fs::canonicalize(&child) else {
                    continue;
                };
                if !canon.starts_with(canon_root) || is_secret_path(&canon).is_some() {
                    continue;
                }
                let Ok(m) = fs::metadata(&canon) else {
                    continue;
                };
                (canon, m)
            } else {
                (child.clone(), sym_meta)
            };

            let is_dir = meta.is_dir();
            if is_dir && is_skip_dir_name(&fname) {
                continue;
            }
            if !is_dir && !meta.is_file() {
                continue;
            }

            let rel = rel_display(canon_root, &child);
            if matches_glob(glob, &rel, &fname) {
                if entries.len() >= max_limit {
                    *truncated = true;
                    return;
                }
                if is_dir {
                    entries.push(json!({
                        "path": rel,
                        "kind": "dir",
                        "size": 0u64,
                        "ext": "",
                        "media_type": "dir",
                    }));
                } else {
                    entries.push(json!({
                        "path": rel,
                        "kind": "file",
                        "size": meta.len(),
                        "ext": lower_ext(&child),
                        "media_type": classify_media_type_by_ext(&child),
                    }));
                }
            }

            if is_dir && cur_depth < max_depth && visited_dirs.insert(resolved.clone()) {
                walk_list(
                    canon_root,
                    &child,
                    cur_depth + 1,
                    max_depth,
                    max_limit,
                    glob,
                    visited_dirs,
                    entries,
                    truncated,
                );
            }
        }
    }

    walk_list(
        &canon_root,
        &target,
        1,
        max_depth,
        max_limit,
        glob,
        &mut visited_dirs,
        &mut entries,
        &mut truncated,
    );

    let total = entries.len();
    Ok(json!({
        "root": root_label,
        "entries": entries,
        "total": total,
        "truncated": truncated,
    }))
}

fn has_utf16_bom(bytes: &[u8]) -> Option<bool> {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        Some(true) // UTF-16 LE
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        Some(false) // UTF-16 BE
    } else {
        None
    }
}

fn is_binary_header(header: &[u8]) -> bool {
    if has_utf16_bom(header).is_some() {
        return false;
    }
    let check_len = header.len().min(BINARY_SNIFF_BYTES);
    header[..check_len].contains(&0)
}

fn decode_text_bytes(bytes: &[u8]) -> String {
    if let Some(is_le) = has_utf16_bom(bytes) {
        let body = &bytes[2..];
        let mut units = Vec::with_capacity(body.len() / 2);
        for chunk in body.chunks_exact(2) {
            let u = if is_le {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            };
            units.push(u);
        }
        return String::from_utf16_lossy(&units);
    }
    let slice = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(slice).into_owned()
}

fn trim_line_max_240(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() <= 240 {
        trimmed.to_string()
    } else {
        trimmed.chars().take(240).collect()
    }
}

/// Searches file contents and filenames inside an authorized grant.
pub fn fs_search(
    root: &Path,
    is_single_file: bool,
    query: &str,
    glob: Option<&str>,
    case_sensitive: bool,
    max_results: usize,
) -> Result<Value, String> {
    if query.trim().is_empty() {
        return Err("Search query cannot be empty".into());
    }
    let max_res = if max_results == 0 {
        50
    } else {
        max_results.clamp(1, 200)
    };

    let canon_root = resolve_inside(root, is_single_file, None, false)?;
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };

    let mut matches: Vec<Value> = Vec::new();
    let mut filename_matches: Vec<String> = Vec::new();
    let mut searched_files: usize = 0;
    let mut truncated = false;

    let mut inspect_file = |file_path: &Path, rel: String, fname: &str| -> bool {
        if !matches_glob(glob, &rel, fname) {
            return true;
        }
        if searched_files >= MAX_SEARCH_FILES {
            truncated = true;
            return false;
        }

        let name_hit = if case_sensitive {
            rel.contains(&needle) || fname.contains(&needle)
        } else {
            rel.to_lowercase().contains(&needle) || fname.to_lowercase().contains(&needle)
        };
        if name_hit && filename_matches.len() < max_res {
            filename_matches.push(rel.clone());
        }

        let Ok(meta) = fs::metadata(file_path) else {
            return true;
        };
        if !meta.is_file() || meta.len() > MAX_SEARCH_FILE_BYTES {
            return true;
        }

        searched_files += 1;

        let Ok(bytes) = fs::read(file_path) else {
            return true;
        };
        if is_binary_header(&bytes) {
            return true;
        }

        let text = decode_text_bytes(&bytes);
        for (idx, line) in text.lines().enumerate() {
            let hit = if case_sensitive {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            };
            if hit {
                if matches.len() >= max_res {
                    truncated = true;
                    return false;
                }
                matches.push(json!({
                    "path": rel,
                    "line": idx + 1,
                    "text": trim_line_max_240(line),
                }));
            }
        }
        true
    };

    if is_single_file {
        let fname = canon_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = inspect_file(&canon_root, fname.clone(), &fname);
    } else {
        let mut stack: Vec<PathBuf> = vec![canon_root.clone()];
        let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
        visited_dirs.insert(canon_root.clone());

        while let Some(dir) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> =
                rd.filter_map(|e| e.ok().map(|de| de.path())).collect();
            children.sort();

            let mut subdirs = Vec::new();
            for child in children {
                if is_secret_path(&child).is_some() {
                    continue;
                }
                let Ok(sym_meta) = fs::symlink_metadata(&child) else {
                    continue;
                };
                let fname = child
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();

                let (resolved, meta) = if sym_meta.file_type().is_symlink() {
                    let Ok(canon) = fs::canonicalize(&child) else {
                        continue;
                    };
                    if !canon.starts_with(&canon_root) || is_secret_path(&canon).is_some() {
                        continue;
                    }
                    let Ok(m) = fs::metadata(&canon) else {
                        continue;
                    };
                    (canon, m)
                } else {
                    (child.clone(), sym_meta)
                };

                if meta.is_dir() {
                    if !is_skip_dir_name(&fname) && visited_dirs.insert(resolved) {
                        subdirs.push(child);
                    }
                } else if meta.is_file() {
                    let rel = rel_display(&canon_root, &child);
                    if !inspect_file(&resolved, rel, &fname) {
                        stack.clear();
                        break;
                    }
                }
            }
            for sd in subdirs.into_iter().rev() {
                stack.push(sd);
            }
        }
    }

    Ok(json!({
        "query": query,
        "matches": matches,
        "filename_matches": filename_matches,
        "searched_files": searched_files,
        "truncated": truncated,
    }))
}

struct PaginatedText {
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    truncated: bool,
    content: String,
}

fn paginate_lines(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: Option<usize>,
) -> PaginatedText {
    let req_start = start_line.unwrap_or(1).max(1);
    let default_end = req_start.saturating_add(249);
    let req_end = end_line
        .unwrap_or(default_end)
        .max(req_start)
        .min(req_start.saturating_add(999));
    let byte_cap = max_bytes.unwrap_or(48_000).clamp(1024, 131_072);

    let mut total_lines: usize = 0;
    let mut out = String::new();
    let mut actual_end: usize = 0;
    let mut hit_cap = false;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        total_lines = line_no;
        if line_no < req_start || line_no > req_end || hit_cap {
            continue;
        }
        let line = raw_line.trim_end_matches('\r');
        let formatted = if out.is_empty() {
            format!("{line_no}: {line}")
        } else {
            format!("\n{line_no}: {line}")
        };
        if out.len() + formatted.len() > byte_cap {
            if out.is_empty() {
                // Ensure we return at least a prefix of the first requested line.
                let prefix = format!("{line_no}: ");
                let avail = byte_cap.saturating_sub(prefix.len());
                let mut slice_end = avail.min(line.len());
                while slice_end > 0 && !line.is_char_boundary(slice_end) {
                    slice_end -= 1;
                }
                out.push_str(&prefix);
                out.push_str(&line[..slice_end]);
                actual_end = line_no;
            }
            hit_cap = true;
            continue;
        }
        out.push_str(&formatted);
        actual_end = line_no;
    }

    if total_lines == 0 {
        return PaginatedText {
            start_line: req_start,
            end_line: 0,
            total_lines: 0,
            truncated: false,
            content: String::new(),
        };
    }

    if req_start > total_lines {
        return PaginatedText {
            start_line: req_start,
            end_line: total_lines,
            total_lines,
            truncated: false,
            content: String::new(),
        };
    }

    let truncated = hit_cap || actual_end < total_lines;
    PaginatedText {
        start_line: req_start,
        end_line: actual_end,
        total_lines,
        truncated,
        content: out,
    }
}

fn read_text_file_paginated(
    path: &Path,
    header: &[u8],
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: Option<usize>,
) -> Result<PaginatedText, String> {
    if has_utf16_bom(header).is_some() {
        let meta = fs::metadata(path).map_err(|e| format!("Cannot read file: {e}"))?;
        if meta.len() > MAX_TEXT_SCAN_BYTES {
            return Err(format!(
                "UTF-16 text file exceeds {} MiB limit",
                MAX_TEXT_SCAN_BYTES / (1024 * 1024)
            ));
        }
        let bytes = fs::read(path).map_err(|e| format!("Cannot read file: {e}"))?;
        let text = decode_text_bytes(&bytes);
        return Ok(paginate_lines(&text, start_line, end_line, max_bytes));
    }

    let req_start = start_line.unwrap_or(1).max(1);
    let default_end = req_start.saturating_add(249);
    let req_end = end_line
        .unwrap_or(default_end)
        .max(req_start)
        .min(req_start.saturating_add(999));
    let byte_cap = max_bytes.unwrap_or(48_000).clamp(1024, 131_072);

    let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let mut total_lines: usize = 0;
    let mut scanned_bytes: u64 = 0;
    let mut scan_capped = false;
    let mut out = String::new();
    let mut actual_end: usize = 0;
    let mut hit_cap = false;

    loop {
        buf.clear();
        let n = reader
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("Cannot read file: {e}"))?;
        if n == 0 {
            break;
        }
        scanned_bytes += n as u64;
        total_lines += 1;

        if total_lines >= req_start && total_lines <= req_end && !hit_cap {
            let mut slice = buf.as_slice();
            if total_lines == 1 {
                slice = slice.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(slice);
            }
            if slice.ends_with(b"\n") {
                slice = &slice[..slice.len() - 1];
            }
            if slice.ends_with(b"\r") {
                slice = &slice[..slice.len() - 1];
            }
            let line = String::from_utf8_lossy(slice);
            let formatted = if out.is_empty() {
                format!("{total_lines}: {line}")
            } else {
                format!("\n{total_lines}: {line}")
            };
            if out.len() + formatted.len() > byte_cap {
                if out.is_empty() {
                    let prefix = format!("{total_lines}: ");
                    let avail = byte_cap.saturating_sub(prefix.len());
                    let mut slice_end = avail.min(line.len());
                    while slice_end > 0 && !line.is_char_boundary(slice_end) {
                        slice_end -= 1;
                    }
                    out.push_str(&prefix);
                    out.push_str(&line[..slice_end]);
                    actual_end = total_lines;
                }
                hit_cap = true;
            } else {
                out.push_str(&formatted);
                actual_end = total_lines;
            }
        }

        if scanned_bytes >= MAX_TEXT_SCAN_BYTES && (total_lines >= req_end || hit_cap) {
            scan_capped = true;
            break;
        }
    }

    if total_lines == 0 {
        return Ok(PaginatedText {
            start_line: req_start,
            end_line: 0,
            total_lines: 0,
            truncated: false,
            content: String::new(),
        });
    }

    if req_start > total_lines {
        return Ok(PaginatedText {
            start_line: req_start,
            end_line: total_lines,
            total_lines,
            truncated: scan_capped,
            content: String::new(),
        });
    }

    let truncated = hit_cap || scan_capped || actual_end < total_lines;
    Ok(PaginatedText {
        start_line: req_start,
        end_line: actual_end,
        total_lines,
        truncated,
        content: out,
    })
}

fn run_command_with_timeout(
    cmd: &mut Command,
    stdin_data: Option<&[u8]>,
    timeout: Duration,
) -> Option<Vec<u8>> {
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if let Some(input) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            let owned = input.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&owned);
            });
        }
    }

    let stdout = child.stdout.take()?;
    let reader_handle = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.take(MAX_CMD_OUTPUT_BYTES).read_to_end(&mut out);
        out
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let bytes = reader_handle.join().ok()?;
    let st = status?;
    if st.success() {
        Some(bytes)
    } else {
        None
    }
}

fn extract_pdf_pdftotext(path: &Path) -> Option<String> {
    let candidates = [
        "pdftotext",
        "/opt/homebrew/bin/pdftotext",
        "/usr/local/bin/pdftotext",
    ];
    for bin in candidates {
        let mut cmd = Command::new(bin);
        cmd.arg("-layout").arg("-nopgbrk").arg(path).arg("-");
        if let Some(bytes) = run_command_with_timeout(&mut cmd, None, CMD_TIMEOUT) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn extract_pdf_pdfkit(path: &Path) -> Option<(String, Option<usize>)> {
    if !Path::new("/usr/bin/swift").is_file() {
        return None;
    }
    const SCRIPT: &str = r#"
import Foundation
import PDFKit
let args = CommandLine.arguments
guard args.count >= 2 else { exit(2) }
let url = URL(fileURLWithPath: args[1])
guard let doc = PDFDocument(url: url) else { exit(3) }
print("__PHILIUM_PDF_PAGES__:\(doc.pageCount)")
var out: [String] = []
for i in 0..<doc.pageCount {
    if let page = doc.page(at: i), let s = page.string {
        out.append(s)
    }
}
print(out.joined(separator: "\n"))
"#;
    let mod_cache = std::env::temp_dir().join("philium-swift-modcache");
    let _ = fs::create_dir_all(&mod_cache);
    let mut cmd = Command::new("/usr/bin/swift");
    cmd.arg("-module-cache-path")
        .arg(&mod_cache)
        .arg("-")
        .arg(path);
    let bytes = run_command_with_timeout(&mut cmd, Some(SCRIPT.as_bytes()), CMD_TIMEOUT)?;
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let (pages, body) = if let Some(rest) = raw.strip_prefix("__PHILIUM_PDF_PAGES__:") {
        if let Some((first, tail)) = rest.split_once('\n') {
            (first.trim().parse::<usize>().ok(), tail.to_string())
        } else {
            (rest.trim().parse::<usize>().ok(), String::new())
        }
    } else {
        (None, raw)
    };
    if body.trim().is_empty() {
        None
    } else {
        Some((body, pages))
    }
}

#[cfg(not(target_os = "macos"))]
fn extract_pdf_pdfkit(_path: &Path) -> Option<(String, Option<usize>)> {
    None
}

fn parse_pdf_literal_string(bytes: &[u8], start: usize) -> (String, usize) {
    // Expects bytes[start] == b'('
    let mut i = start + 1;
    let mut depth: usize = 1;
    let mut out: Vec<u8> = Vec::new();

    while i < bytes.len() && depth > 0 {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 1;
            match bytes[i] {
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                b'b' => out.push(0x08),
                b'f' => out.push(0x0C),
                b'(' => out.push(b'('),
                b')' => out.push(b')'),
                b'\\' => out.push(b'\\'),
                b'\r' => {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                        i += 1;
                    }
                }
                b'\n' => {}
                c @ b'0'..=b'7' => {
                    let mut val = (c - b'0') as u16;
                    for _ in 0..2 {
                        if i + 1 < bytes.len() && (b'0'..=b'7').contains(&bytes[i + 1]) {
                            i += 1;
                            val = val * 8 + (bytes[i] - b'0') as u16;
                        } else {
                            break;
                        }
                    }
                    out.push((val & 0xFF) as u8);
                }
                other => out.push(other),
            }
            i += 1;
        } else if b == b'(' {
            depth += 1;
            out.push(b);
            i += 1;
        } else if b == b')' {
            depth -= 1;
            if depth > 0 {
                out.push(b);
            }
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }

    (decode_pdf_bytes(&out), i)
}

fn parse_pdf_hex_string(bytes: &[u8], start: usize) -> (String, usize) {
    // Expects bytes[start] == b'<'
    let mut i = start + 1;
    let mut hex = Vec::new();
    while i < bytes.len() && bytes[i] != b'>' {
        let b = bytes[i];
        if b.is_ascii_hexdigit() {
            hex.push(b);
        }
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'>' {
        i += 1;
    }
    if hex.len() % 2 == 1 {
        hex.push(b'0');
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((hi << 4) | lo);
    }
    (decode_pdf_bytes(&out), i)
}

fn decode_pdf_bytes(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let mut units = Vec::with_capacity((raw.len() - 2) / 2);
        for pair in raw[2..].chunks_exact(2) {
            units.push(u16::from_be_bytes([pair[0], pair[1]]));
        }
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    }
}

fn extract_pdf_text_from_content(content: &[u8], out: &mut String) {
    #[derive(Clone)]
    enum PdfArg {
        Str(String),
        Array(Vec<PdfArrItem>),
    }
    #[derive(Clone)]
    enum PdfArrItem {
        Str(String),
        Num(f64),
    }

    let mut i = 0usize;
    let mut in_text = false;
    let mut last_arg: Option<PdfArg> = None;

    while i < content.len() {
        let b = content[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'%' {
            while i < content.len() && content[i] != b'\n' && content[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if b == b'(' {
            let (s, next) = parse_pdf_literal_string(content, i);
            last_arg = Some(PdfArg::Str(s));
            i = next;
            continue;
        }
        if b == b'<' {
            if i + 1 < content.len() && content[i + 1] == b'<' {
                i += 2;
                continue;
            }
            let (s, next) = parse_pdf_hex_string(content, i);
            last_arg = Some(PdfArg::Str(s));
            i = next;
            continue;
        }
        if b == b'[' {
            i += 1;
            let mut items = Vec::new();
            while i < content.len() && content[i] != b']' {
                if content[i].is_ascii_whitespace() {
                    i += 1;
                } else if content[i] == b'(' {
                    let (s, next) = parse_pdf_literal_string(content, i);
                    items.push(PdfArrItem::Str(s));
                    i = next;
                } else if content[i] == b'<' && !(i + 1 < content.len() && content[i + 1] == b'<') {
                    let (s, next) = parse_pdf_hex_string(content, i);
                    items.push(PdfArrItem::Str(s));
                    i = next;
                } else {
                    let start = i;
                    while i < content.len()
                        && !content[i].is_ascii_whitespace()
                        && content[i] != b']'
                        && content[i] != b'('
                        && content[i] != b'<'
                    {
                        i += 1;
                    }
                    if let Ok(num) = std::str::from_utf8(&content[start..i])
                        .unwrap_or("")
                        .parse::<f64>()
                    {
                        items.push(PdfArrItem::Num(num));
                    }
                }
            }
            if i < content.len() && content[i] == b']' {
                i += 1;
            }
            last_arg = Some(PdfArg::Array(items));
            continue;
        }

        // Read operator or token
        let start = i;
        while i < content.len()
            && !content[i].is_ascii_whitespace()
            && !matches!(content[i], b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'%')
        {
            i += 1;
        }
        if i == start {
            i += 1;
            continue;
        }
        let tok = &content[start..i];
        match tok {
            b"BT" => {
                in_text = true;
                last_arg = None;
            }
            b"ET" => {
                if in_text && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                in_text = false;
                last_arg = None;
            }
            b"T*" | b"Td" | b"TD" | b"Tm" => {
                if in_text && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            b"Tj" | b"'" | b"\"" => {
                if in_text {
                    if tok == b"'" || tok == b"\"" {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    if let Some(PdfArg::Str(s)) = last_arg.take() {
                        out.push_str(&s);
                    }
                }
            }
            b"TJ" => {
                if in_text {
                    if let Some(PdfArg::Array(items)) = last_arg.take() {
                        for item in items {
                            match item {
                                PdfArrItem::Str(s) => out.push_str(&s),
                                PdfArrItem::Num(n) if n < -100.0 => {
                                    if !out.ends_with(' ') && !out.ends_with('\n') {
                                        out.push(' ');
                                    }
                                }
                                PdfArrItem::Num(_) => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Pure-Rust fallback extractor for uncompressed PDF text streams.
pub(crate) fn extract_pdf_text_fallback(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut pos = 0usize;
    let mut found_stream = false;

    while let Some(rel) = find_subslice(&bytes[pos..], b"stream") {
        let mut s_start = pos + rel + 6;
        // PDF streams are followed by \r\n or \n
        if s_start < bytes.len() && bytes[s_start] == b'\r' {
            s_start += 1;
        }
        if s_start < bytes.len() && bytes[s_start] == b'\n' {
            s_start += 1;
        } else {
            pos = s_start;
            continue;
        }
        let Some(end_rel) = find_subslice(&bytes[s_start..], b"endstream") else {
            break;
        };
        found_stream = true;
        let stream_bytes = &bytes[s_start..s_start + end_rel];
        extract_pdf_text_from_content(stream_bytes, &mut out);
        pos = s_start + end_rel + 9;
    }

    if !found_stream || out.trim().is_empty() {
        let mut direct = String::new();
        extract_pdf_text_from_content(bytes, &mut direct);
        if !direct.trim().is_empty() {
            out = direct;
        }
    }

    out.lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_pdf_text(path: &Path) -> Result<(String, &'static str, Option<usize>), String> {
    if let Some(t) = extract_pdf_pdftotext(path) {
        return Ok((t, "pdftotext", None));
    }
    if let Some((t, pages)) = extract_pdf_pdfkit(path) {
        return Ok((t, "pdfkit", pages));
    }
    let meta = fs::metadata(path).map_err(|e| format!("Cannot read PDF metadata: {e}"))?;
    if meta.len() > MAX_TEXT_SCAN_BYTES {
        return Ok((
            "(PDF exceeds fallback extraction size limit and no external PDF extractor succeeded.)"
                .to_string(),
            "none",
            None,
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read PDF file: {e}"))?;
    let fallback = extract_pdf_text_fallback(&bytes);
    if !fallback.trim().is_empty() {
        Ok((fallback, "rust-fallback", None))
    } else {
        Ok((
            "(No extractable text found in PDF; it may be scanned or use compressed streams without an external PDF extractor.)"
                .to_string(),
            "none",
            None,
        ))
    }
}

#[cfg(target_os = "macos")]
fn extract_document_text(path: &Path) -> Result<(String, &'static str), String> {
    if !Path::new("/usr/bin/textutil").is_file() {
        return Err("Document extractor (/usr/bin/textutil) is not available".into());
    }
    let mut cmd = Command::new("/usr/bin/textutil");
    cmd.arg("-convert").arg("txt").arg("-stdout").arg(path);
    let bytes = run_command_with_timeout(&mut cmd, None, CMD_TIMEOUT)
        .ok_or_else(|| "Failed to extract text from document via textutil".to_string())?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), "textutil"))
}

#[cfg(not(target_os = "macos"))]
fn extract_document_text(_path: &Path) -> Result<(String, &'static str), String> {
    Err("Rich document extraction requires macOS textutil".into())
}

fn sha256_prefix_file(path: &Path) -> String {
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut remaining = MAX_HASH_BYTES;
    while remaining > 0 {
        let to_read = (buf.len() as u64).min(remaining) as usize;
        match f.read(&mut buf[..to_read]) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                remaining -= n as u64;
            }
            Err(_) => break,
        }
    }
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Universal multimodal file reader inside an authorized grant.
pub fn fs_read(
    root: &Path,
    is_single_file: bool,
    label: &str,
    rel_path: Option<&str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    max_bytes: Option<usize>,
) -> Result<Value, String> {
    let canon_root =
        fs::canonicalize(root).map_err(|e| format!("Invalid grant root {}: {e}", root.display()))?;
    let target = resolve_inside(root, is_single_file, rel_path, false)?;

    if target.is_dir() {
        return Err("Path is a directory; use fs_list to inspect directories".into());
    }

    let meta = fs::metadata(&target).map_err(|e| format!("Cannot read file metadata: {e}"))?;
    let file_len = meta.len();
    let display_rel = if is_single_file {
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        rel_display(&canon_root, &target)
    };

    let mut header = Vec::new();
    {
        let f = File::open(&target).map_err(|e| format!("Cannot open file: {e}"))?;
        let _ = f
            .take(BINARY_SNIFF_BYTES as u64)
            .read_to_end(&mut header);
    }

    let ext = lower_ext(&target);

    // 1. Images (magic bytes or extension)
    if let Some(mime) = image_mime_from_magic(&header).or_else(|| image_mime_from_ext(&ext)) {
        if file_len > MAX_IMAGE_BYTES {
            return Err(format!(
                "Image file too large ({file_len} bytes; maximum is 10 MiB)"
            ));
        }
        let bytes = fs::read(&target).map_err(|e| format!("Cannot read image file: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(json!({
            "__mcp_content": [
                {
                    "type": "text",
                    "text": format!("[external image: {label}/{display_rel} ({mime}, {} bytes)]", bytes.len())
                },
                {
                    "type": "image",
                    "data": b64,
                    "mimeType": mime
                }
            ]
        }));
    }

    // 2. PDF (%PDF- magic or .pdf extension)
    if header.starts_with(b"%PDF-") || ext == "pdf" {
        let (text, extractor, page_count) = extract_pdf_text(&target)?;
        let page = paginate_lines(&text, start_line, end_line, max_bytes);
        let mut obj = json!({
            "source": format!("[external pdf: {label}/{display_rel}]"),
            "path": display_rel,
            "media_type": "pdf",
            "extractor": extractor,
            "start_line": page.start_line,
            "end_line": page.end_line,
            "total_lines": page.total_lines,
            "truncated": page.truncated,
            "content": page.content,
        });
        if let Some(pages) = page_count {
            obj["page_count"] = json!(pages);
        }
        return Ok(obj);
    }

    // 3. Rich documents (.docx, .doc, .rtf, .odt, .webarchive)
    if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
        let (text, extractor) = extract_document_text(&target)?;
        let page = paginate_lines(&text, start_line, end_line, max_bytes);
        return Ok(json!({
            "source": format!("[external document: {label}/{display_rel}]"),
            "path": display_rel,
            "media_type": "document",
            "extractor": extractor,
            "start_line": page.start_line,
            "end_line": page.end_line,
            "total_lines": page.total_lines,
            "truncated": page.truncated,
            "content": page.content,
        }));
    }

    // 4. Unknown binary files (NUL byte in first 8 KiB, excluding UTF-16 BOM)
    if is_binary_header(&header) {
        let sha_prefix = sha256_prefix_file(&target);
        return Ok(json!({
            "source": format!("[external binary: {label}/{display_rel}]"),
            "path": display_rel,
            "media_type": "binary",
            "size": file_len,
            "sha256_prefix": sha_prefix,
            "note": "Binary file cannot be rendered as text or image.",
        }));
    }

    // 5. Text / Code / Markdown / JSON / YAML / CSV / HTML / SVG / Logs
    let page = read_text_file_paginated(&target, &header, start_line, end_line, max_bytes)?;
    Ok(json!({
        "source": format!("[external file: {label}/{display_rel}]"),
        "path": display_rel,
        "media_type": "text",
        "start_line": page.start_line,
        "end_line": page.end_line,
        "total_lines": page.total_lines,
        "truncated": page.truncated,
        "content": page.content,
    }))
}

/// Safely writes or appends UTF-8 content inside an authorized grant.
pub fn fs_write(
    root: &Path,
    is_single_file: bool,
    rel_path: &str,
    content: &str,
    append: bool,
) -> Result<Value, String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Write content exceeds 2 MiB limit ({} bytes)",
            content.len()
        ));
    }
    if !is_single_file && rel_path.trim().is_empty() {
        return Err("Target path cannot be empty".into());
    }

    let canon_root =
        fs::canonicalize(root).map_err(|e| format!("Invalid grant root {}: {e}", root.display()))?;
    let target = resolve_inside(root, is_single_file, Some(rel_path), !is_single_file)?;

    if target.is_dir() {
        return Err("Cannot write to a directory".into());
    }

    let existed = fs::symlink_metadata(&target).is_ok();

    let parent = target
        .parent()
        .ok_or_else(|| "Target path has no parent directory".to_string())?;
    if !is_single_file {
        if !parent.starts_with(&canon_root) {
            return Err("Access denied: target parent escapes grant root".into());
        }
        if !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create parent directories: {e}"))?;
        }
        let canon_parent =
            fs::canonicalize(parent).map_err(|e| format!("Cannot verify parent directory: {e}"))?;
        if !canon_parent.starts_with(&canon_root) {
            return Err("Access denied: parent directory escapes grant root".into());
        }
    }

    if append {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|e| format!("Cannot open file for append: {e}"))?;
        f.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to append to file: {e}"))?;
        f.sync_data()
            .map_err(|e| format!("Failed to flush file: {e}"))?;
    } else {
        let rand_id: u64 = rand::random();
        let tmp_path = parent.join(format!(".philium-tmp-{rand_id:016x}"));
        let write_res = (|| -> io::Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to write temporary file: {e}"));
        }
        if let Err(e) = fs::rename(&tmp_path, &target) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to atomically replace target file: {e}"));
        }
    }

    let display_rel = if is_single_file {
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        rel_display(&canon_root, &target)
    };

    Ok(json!({
        "path": display_rel,
        "bytes": content.len(),
        "created": !existed,
        "append": append,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "philium-access-fs-{tag}-{}-{:016x}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn minimal_png() -> Vec<u8> {
        // Valid 1x1 transparent PNG
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
            .unwrap()
    }

    fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let pdf = format!(
            "%PDF-1.4\n\
             1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length {} >> stream\n{}\nendstream endobj\n\
             5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
             trailer << /Root 1 0 R >>\n%%EOF\n",
            stream.len(),
            stream
        );
        pdf.into_bytes()
    }

    #[test]
    fn test_fs_list_depth_glob_skips_noise_and_secrets() {
        let dir = TempDir::new("list");
        let root = dir.path();
        fs::create_dir_all(root.join("src/nested/deep")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join(".ssh")).unwrap();

        fs::write(root.join("README.md"), "# Hello").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn hi() {}").unwrap();
        fs::write(root.join("src/nested/util.rs"), "pub fn u() {}").unwrap();
        fs::write(root.join("src/nested/deep/far.rs"), "pub fn f() {}").unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "module.exports = {}").unwrap();
        fs::write(root.join(".env"), "API_KEY=supersecret").unwrap();
        fs::write(root.join(".env.example"), "API_KEY=replace_me").unwrap();
        fs::write(root.join("secret.pem"), "-----BEGIN PRIVATE KEY-----").unwrap();
        fs::write(root.join(".ssh/id_ed25519"), "ssh-key").unwrap();

        // Depth 2 should include src/lib.rs and src/nested (dir), NOT src/nested/util.rs
        let res = fs_list(root, false, None, None, 2, 200).unwrap();
        let paths: Vec<&str> = res["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();

        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&"src/lib.rs"));
        assert!(paths.contains(&".env.example"));
        assert!(!paths.contains(&"src/nested/util.rs"));
        assert!(!paths.iter().any(|p| p.contains(".git")));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        assert!(!paths.iter().any(|p| *p == ".env"));
        assert!(!paths.iter().any(|p| p.contains("secret.pem")));
        assert!(!paths.iter().any(|p| p.contains(".ssh")));

        // Glob filter with depth 4 finds all .rs files including deep ones
        let rs_res = fs_list(root, false, None, Some("*.rs"), 4, 200).unwrap();
        let rs_paths: Vec<&str> = rs_res["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            rs_paths,
            vec!["src/lib.rs", "src/nested/deep/far.rs", "src/nested/util.rs"]
        );
    }

    #[test]
    fn test_fs_search_content_filenames_skips_binary_and_secrets() {
        let dir = TempDir::new("search");
        let root = dir.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(
            root.join("docs/architecture.md"),
            "Philium uses Capability Grants.\nSecond line.\n",
        )
        .unwrap();
        fs::write(root.join("capability_notes.txt"), "Unrelated body text.\n").unwrap();
        fs::write(root.join(".env"), "CAPABILITY_TOKEN=secret_should_never_match").unwrap();
        fs::write(root.join("blob.bin"), b"capability\x00binary\x00data").unwrap();

        let res = fs_search(root, false, "capability", None, false, 50).unwrap();
        let matches = res["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "docs/architecture.md");
        assert_eq!(matches[0]["line"], 1);
        assert!(matches[0]["text"]
            .as_str()
            .unwrap()
            .contains("Capability Grants"));

        let fname_matches: Vec<&str> = res["filename_matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(fname_matches.contains(&"capability_notes.txt"));
        assert!(!fname_matches.iter().any(|p| p.contains(".env")));
    }

    #[test]
    fn test_fs_read_text_pagination() {
        let dir = TempDir::new("text");
        let root = dir.path();
        let body: String = (1..=30).map(|i| format!("line-{i}\n")).collect();
        fs::write(root.join("log.txt"), body).unwrap();

        let page1 = fs_read(root, false, "notes", Some("log.txt"), Some(5), Some(10), None).unwrap();
        assert_eq!(page1["media_type"], "text");
        assert_eq!(page1["source"], "[external file: notes/log.txt]");
        assert_eq!(page1["start_line"], 5);
        assert_eq!(page1["end_line"], 10);
        assert_eq!(page1["total_lines"], 30);
        assert_eq!(page1["truncated"], true);
        let content = page1["content"].as_str().unwrap();
        assert!(content.starts_with("5: line-5\n"));
        assert!(content.ends_with("10: line-10"));

        let last_page =
            fs_read(root, false, "notes", Some("log.txt"), Some(28), Some(50), None).unwrap();
        assert_eq!(last_page["start_line"], 28);
        assert_eq!(last_page["end_line"], 30);
        assert_eq!(last_page["total_lines"], 30);
        assert_eq!(last_page["truncated"], false);
    }

    #[test]
    fn test_fs_read_png_image_mcp_content() {
        let dir = TempDir::new("png");
        let root = dir.path();
        let png = minimal_png();
        fs::write(root.join("pixel.png"), &png).unwrap();

        let res = fs_read(root, false, "assets", Some("pixel.png"), None, None, None).unwrap();
        let blocks = res["__mcp_content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert!(blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains("[external image: assets/pixel.png (image/png,"));
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["mimeType"], "image/png");

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(blocks[1]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, png);
    }

    #[test]
    fn test_fs_read_minimal_pdf_fixture() {
        let dir = TempDir::new("pdf");
        let root = dir.path();
        let pdf_bytes = minimal_pdf("Quarterly Revenue Report 2026");
        fs::write(root.join("report.pdf"), &pdf_bytes).unwrap();

        // Pure-Rust fallback itself extracts cleanly
        let fallback = extract_pdf_text_fallback(&pdf_bytes);
        assert!(fallback.contains("Quarterly Revenue Report 2026"));

        let res = fs_read(root, false, "docs", Some("report.pdf"), None, None, None).unwrap();
        assert_eq!(res["media_type"], "pdf");
        assert_eq!(res["source"], "[external pdf: docs/report.pdf]");
        assert!(res["content"]
            .as_str()
            .unwrap()
            .contains("Quarterly Revenue Report 2026"));
    }

    #[test]
    fn test_fs_read_binary_returns_safe_metadata() {
        let dir = TempDir::new("bin");
        let root = dir.path();
        let payload = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00some-binary-bytes";
        fs::write(root.join("program.bin"), payload).unwrap();

        let res = fs_read(root, false, "out", Some("program.bin"), None, None, None).unwrap();
        assert_eq!(res["media_type"], "binary");
        assert_eq!(res["source"], "[external binary: out/program.bin]");
        assert_eq!(res["size"], payload.len() as u64);
        assert_eq!(res["sha256_prefix"].as_str().unwrap().len(), 16);
        assert!(res["note"].as_str().unwrap().contains("Binary file"));
    }

    #[test]
    fn test_security_traversal_symlinks_and_secrets() {
        let dir = TempDir::new("sec");
        let outside = TempDir::new("sec-outside");
        let root = dir.path();
        fs::write(outside.path().join("secret_outside.txt"), "top-secret").unwrap();
        fs::write(root.join(".env"), "TOKEN=1").unwrap();
        fs::write(root.join(".env.local"), "TOKEN=2").unwrap();
        fs::write(root.join(".env.example"), "TOKEN=example").unwrap();
        fs::write(root.join("id_ed25519"), "private-key").unwrap();
        fs::write(root.join("server.key"), "key").unwrap();
        fs::write(root.join("cert.pem"), "pem").unwrap();

        // Secret path checks
        assert!(is_secret_path(Path::new("/a/b/.env")).is_some());
        assert!(is_secret_path(Path::new("/a/b/.env.production")).is_some());
        assert!(is_secret_path(Path::new("/a/b/.env.example")).is_none());
        assert!(is_secret_path(Path::new("/a/b/.env.sample")).is_none());
        assert!(is_secret_path(Path::new("/a/b/.env.template")).is_none());
        assert!(is_secret_path(Path::new("/a/b/id_ed25519")).is_some());
        assert!(is_secret_path(Path::new("/a/b/id_rsa.pub")).is_some());
        assert!(is_secret_path(Path::new("/a/b/server.key")).is_some());
        assert!(is_secret_path(Path::new("/a/b/cert.pem")).is_some());
        assert!(is_secret_path(Path::new("/Users/me/.aws/config")).is_some());
        assert!(is_secret_path(Path::new("/Users/me/Library/Keychains/login.keychain")).is_some());

        // Secret files rejected by fs_read, .env.example allowed
        for bad in [".env", ".env.local", "id_ed25519", "server.key", "cert.pem"] {
            let err = fs_read(root, false, "r", Some(bad), None, None, None).unwrap_err();
            assert!(err.contains("secret file"), "expected secret error for {bad}, got: {err}");
        }
        let ok = fs_read(root, false, "r", Some(".env.example"), None, None, None).unwrap();
        assert!(ok["content"].as_str().unwrap().contains("TOKEN=example"));

        // ../ traversal rejected
        let err = fs_read(root, false, "r", Some("../etc/passwd"), None, None, None).unwrap_err();
        assert!(err.contains("traversal") || err.contains("Access denied"));

        #[cfg(unix)]
        {
            symlink(outside.path().join("secret_outside.txt"), root.join("link_file")).unwrap();
            symlink(outside.path(), root.join("link_dir")).unwrap();
            symlink(outside.path().join("dangling_new.txt"), root.join("dangling_link")).unwrap();

            // Symlink read escape rejected
            let err = fs_read(root, false, "r", Some("link_file"), None, None, None).unwrap_err();
            assert!(err.contains("symlink") || err.contains("escapes"), "got: {err}");

            // Symlink write escape via existing file, intermediate dir, and dangling link rejected
            assert!(fs_write(root, false, "link_file", "pwn", false).is_err());
            assert!(fs_write(root, false, "link_dir/pwn.txt", "pwn", false).is_err());
            assert!(fs_write(root, false, "dangling_link", "pwn", false).is_err());
            assert!(!outside.path().join("pwn.txt").exists());
            assert!(!outside.path().join("dangling_new.txt").exists());
        }
    }

    #[test]
    fn test_fs_write_atomic_append_and_security() {
        let dir = TempDir::new("write");
        let root = dir.path();

        // Atomic create in nested new dir
        let r1 = fs_write(root, false, "notes/sub/hello.md", "Line 1\n", false).unwrap();
        assert_eq!(r1["path"], "notes/sub/hello.md");
        assert_eq!(r1["bytes"], 7);
        assert_eq!(r1["created"], true);
        assert_eq!(r1["append"], false);
        assert_eq!(
            fs::read_to_string(root.join("notes/sub/hello.md")).unwrap(),
            "Line 1\n"
        );

        // Append mode
        let r2 = fs_write(root, false, "notes/sub/hello.md", "Line 2\n", true).unwrap();
        assert_eq!(r2["created"], false);
        assert_eq!(r2["append"], true);
        assert_eq!(
            fs::read_to_string(root.join("notes/sub/hello.md")).unwrap(),
            "Line 1\nLine 2\n"
        );

        // Overwrite atomically
        let r3 = fs_write(root, false, "notes/sub/hello.md", "Replaced", false).unwrap();
        assert_eq!(r3["created"], false);
        assert_eq!(
            fs::read_to_string(root.join("notes/sub/hello.md")).unwrap(),
            "Replaced"
        );

        // Reject secret filenames & traversal
        assert!(fs_write(root, false, ".env", "SECRET=1", false).is_err());
        assert!(fs_write(root, false, "keys/prod.pem", "KEY", false).is_err());
        assert!(fs_write(root, false, "../escape.txt", "NOPE", false).is_err());

        // Single-file grant write & restriction
        let single = root.join("notes/sub/hello.md");
        let r4 = fs_write(&single, true, "hello.md", "Single OK", false).unwrap();
        assert_eq!(r4["path"], "hello.md");
        assert_eq!(fs::read_to_string(&single).unwrap(), "Single OK");
        assert!(fs_write(&single, true, "other.md", "NOPE", false).is_err());

        // Enforce 2 MiB write cap
        let huge = "a".repeat(MAX_WRITE_BYTES + 1);
        let err = fs_write(root, false, "huge.txt", &huge, false).unwrap_err();
        assert!(err.contains("2 MiB"));
    }

    #[test]
    fn test_multimodal_edge_cases_magic_utf16_html_rtf_and_caps() {
        let dir = TempDir::new("mm");
        let root = dir.path();

        // PNG detected via magic bytes even with non-image extension
        let png = minimal_png();
        fs::write(root.join("mystery.dat"), &png).unwrap();
        let img = fs_read(root, false, "assets", Some("mystery.dat"), None, None, None).unwrap();
        assert_eq!(img["__mcp_content"][1]["mimeType"], "image/png");

        // Oversized image (>10 MiB) rejected cleanly
        let mut big_png = png.clone();
        big_png.resize((MAX_IMAGE_BYTES as usize) + 16, 0);
        fs::write(root.join("huge.png"), &big_png).unwrap();
        let err = fs_read(root, false, "assets", Some("huge.png"), None, None, None).unwrap_err();
        assert!(err.contains("10 MiB"));

        // UTF-16LE BOM text file decodes cleanly without binary misclassification
        let mut utf16 = vec![0xFFu8, 0xFEu8];
        for u in "Hello UTF16\nSecond line".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        fs::write(root.join("wide.txt"), &utf16).unwrap();
        let wide = fs_read(root, false, "docs", Some("wide.txt"), None, None, None).unwrap();
        assert_eq!(wide["media_type"], "text");
        assert!(wide["content"].as_str().unwrap().contains("1: Hello UTF16"));
        assert!(wide["content"].as_str().unwrap().contains("2: Second line"));

        // HTML stays raw readable markup ("text")
        fs::write(root.join("page.html"), "<html><body><h1>Hi</h1></body></html>").unwrap();
        let html = fs_read(root, false, "web", Some("page.html"), None, None, None).unwrap();
        assert_eq!(html["media_type"], "text");
        assert!(html["content"].as_str().unwrap().contains("<h1>Hi</h1>"));

        // PDF TJ kerning array + escaped parens in pure-Rust fallback
        let tj_pdf = b"%PDF-1.4\nstream\nBT [(Quarterly ) -150 (Report \\(2026\\))] TJ ET\nendstream\n";
        let extracted = extract_pdf_text_fallback(tj_pdf);
        assert!(extracted.contains("Quarterly"), "got: {extracted}");
        assert!(extracted.contains("Report (2026)"), "got: {extracted}");

        // macOS RTF extraction via textutil
        #[cfg(target_os = "macos")]
        {
            fs::write(root.join("memo.rtf"), r"{\rtf1\ansi Philium Rich Memo 2026}").unwrap();
            let doc = fs_read(root, false, "docs", Some("memo.rtf"), None, None, None).unwrap();
            assert_eq!(doc["media_type"], "document");
            assert_eq!(doc["extractor"], "textutil");
            assert!(doc["content"]
                .as_str()
                .unwrap()
                .contains("Philium Rich Memo 2026"));
        }
    }

    #[test]
    fn test_single_file_grants_limits_and_full_secret_matrix() {
        let dir = TempDir::new("single");
        let root = dir.path();
        let file = root.join("spec.md");
        let lines: String = (1..=100).map(|i| format!("Spec item number {i} with padding text\n")).collect();
        fs::write(&file, &lines).unwrap();

        // Single-file fs_list, fs_search, fs_read
        let listed = fs_list(&file, true, None, None, 2, 200).unwrap();
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["entries"][0]["path"], "spec.md");

        let found = fs_search(&file, true, "item number 42", None, false, 10).unwrap();
        assert_eq!(found["matches"].as_array().unwrap().len(), 1);
        assert_eq!(found["matches"][0]["line"], 42);

        for ok_rel in [None, Some(""), Some("."), Some("spec.md")] {
            let r = fs_read(&file, true, "spec", ok_rel, Some(1), Some(3), None).unwrap();
            assert_eq!(r["end_line"], 3);
            assert_eq!(r["truncated"], true);
        }
        assert!(fs_read(&file, true, "spec", Some("other.md"), None, None, None).is_err());

        // Byte-cap truncation
        let capped = fs_read(&file, true, "spec", None, Some(1), Some(100), Some(1024)).unwrap();
        assert_eq!(capped["truncated"], true);
        assert!(capped["content"].as_str().unwrap().len() <= 1024);

        // NUL byte & absolute path rejected
        assert!(fs_read(root, false, "r", Some("spec.md\0.txt"), None, None, None).is_err());
        assert!(fs_read(root, false, "r", Some("/etc/passwd"), None, None, None).is_err());

        // Empty search query rejected; max_results truncation works
        assert!(fs_search(root, false, "   ", None, false, 10).is_err());
        let trunc = fs_search(root, false, "Spec item", None, false, 5).unwrap();
        assert_eq!(trunc["matches"].as_array().unwrap().len(), 5);
        assert_eq!(trunc["truncated"], true);

        // Full secret path coverage
        for secret in [
            "/x/.git-credentials",
            "/x/.netrc",
            "/x/.npmrc",
            "/x/.pypirc",
            "/x/credentials.json",
            "/x/.vault-token",
            "/x/bundle.p12",
            "/x/cert.pfx",
            "/x/passwords.kdbx",
            "/x/trust.jks",
            "/x/app.keystore",
            "/x/id_ecdsa",
            "/x/id_dsa.pub",
            "/x/.gnupg/secring.gpg",
            "/x/.kube/config",
            "/x/.docker/config.json",
            "/x/.ssh/config",
        ] {
            assert!(
                is_secret_path(Path::new(secret)).is_some(),
                "expected {secret} to be blocked as secret"
            );
        }
    }
}
