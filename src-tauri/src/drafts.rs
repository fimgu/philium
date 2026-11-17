//! Private UI composer storage. Never exposed through agent RPC.
//!
//! Layout beneath the private secrets directory (0700, files 0600):
//!   drafts/index.json        small: text + image references per agent
//!   drafts/blobs/<sha256>    raw image bytes, content addressed, written once
//! Text edits rewrite only the small index, never image bytes.
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{collections::{BTreeMap, HashSet}, io::{Read, Write}, path::{Path, PathBuf}, sync::Mutex, time::{Duration, SystemTime}};

pub const MAX_IMAGE: usize = 5 * 1024 * 1024;
const MAX_IMAGES: usize = 8;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_DRAFTS: usize = 256;
const MAX_INDEX: usize = 4 * 1024 * 1024;
const MAX_LEGACY: usize = 64 * 1024 * 1024;
/// Unreferenced blobs younger than this may belong to an in-flight attach.
const BLOB_GRACE: Duration = Duration::from_secs(3600);
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Draft { text: String, images: Vec<ImageRef> }
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct ImageRef { sha256: String, media_type: String, name: String, size: usize }
type Drafts = BTreeMap<String, Draft>;

fn key_valid(key: &str) -> bool {
    key.strip_prefix("agent:").is_some_and(|id| !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}
fn sha_valid(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) }

/// Identify the image by content, never by a caller-supplied label.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("image/png") }
    else if bytes.starts_with(&[0xff, 0xd8, 0xff]) { Some("image/jpeg") }
    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some("image/gif") }
    else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { Some("image/webp") }
    else { None }
}

fn validate(key: &str, draft: &Draft) -> Result<(), String> {
    if !key_valid(key) || draft.text.len() > MAX_TEXT || draft.images.len() > MAX_IMAGES { return Err("Invalid or oversized draft".into()); }
    for im in &draft.images {
        if !sha_valid(&im.sha256) || !matches!(im.media_type.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") || im.name.len() > 512 || im.size == 0 || im.size > MAX_IMAGE {
            return Err("Invalid draft image".into());
        }
    }
    Ok(())
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => return Err("Invalid draft directory".into()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(dir).map_err(|_| "Cannot create private draft directory")?,
        Err(_) => return Err("Cannot inspect draft directory".into()),
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| "Cannot secure draft directory".into())
}
fn check_parent(dir: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => Err("Invalid draft directory".into()),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Cannot inspect draft directory".into()),
    }
}
fn read_bounded(path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max as u64 { return Err("Invalid draft file".into()); }
    }
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read drafts".into()),
    };
    let mut bytes = Vec::new();
    file.take((max + 1) as u64).read_to_end(&mut bytes).map_err(|_| "Cannot read drafts")?;
    if bytes.len() > max { return Err("Draft file too large".into()); }
    Ok(Some(bytes))
}
/// Write-to-temp, fsync, rename, fsync directory. Never leaves a partial file.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let tmp = dir.join(format!(".{name}.tmp"));
    match std::fs::remove_file(&tmp) { Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(_) => return Err("Cannot replace draft temporary file".into()) }
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|_| "Cannot create draft file")?;
        file.write_all(bytes).map_err(|_| "Cannot write drafts")?;
        file.sync_all().map_err(|_| "Cannot sync drafts")?;
        std::fs::rename(&tmp, dir.join(name)).map_err(|_| "Cannot replace drafts")?;
        std::fs::File::open(dir).and_then(|f| f.sync_all()).map_err(|_| "Cannot sync draft directory")?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}

fn store(root: &Path) -> PathBuf { root.join("drafts") }
fn blobs(root: &Path) -> PathBuf { store(root).join("blobs") }

fn put_blob_at(root: &Path, bytes: &[u8]) -> Result<(String, &'static str), String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE { return Err("Images must be 5 MiB or smaller.".into()); }
    let media = sniff(bytes).ok_or("Unsupported image. Use PNG, JPEG, GIF or WebP.")?;
    let sha = hex(&Sha256::digest(bytes));
    ensure_dir(root)?; ensure_dir(&store(root))?; ensure_dir(&blobs(root))?;
    let path = blobs(root).join(&sha);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() && m.len() == bytes.len() as u64 => {
            // Refresh mtime so garbage collection treats it as recently attached.
            let _ = std::fs::File::options().append(true).custom_flags(libc::O_NOFOLLOW).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
            return Ok((sha, media));
        }
        Ok(_) => return Err("Invalid draft blob".into()),
        Err(_) => {}
    }
    write_atomic(&blobs(root), &sha, bytes)?;
    Ok((sha, media))
}
fn get_blob_at(root: &Path, sha: &str) -> Result<Vec<u8>, String> {
    if !sha_valid(sha) { return Err("Invalid image reference".into()); }
    check_parent(&store(root))?; check_parent(&blobs(root))?;
    let bytes = read_bounded(&blobs(root).join(sha), MAX_IMAGE)?.ok_or("Draft image is missing")?;
    if hex(&Sha256::digest(&bytes)) != sha { return Err("Draft image is corrupted".into()); }
    Ok(bytes)
}

fn load_index(root: &Path) -> Result<Drafts, String> {
    if !check_parent(root)? { return Ok(Drafts::new()); }
    migrate_legacy(root)?;
    if !check_parent(&store(root))? { return Ok(Drafts::new()); }
    let Some(bytes) = read_bounded(&store(root).join("index.json"), MAX_INDEX)? else { return Ok(Drafts::new()) };
    let drafts: Drafts = serde_json::from_slice(&bytes).map_err(|_| "Invalid draft file, refusing to overwrite")?;
    if drafts.len() > MAX_DRAFTS { return Err("Too many drafts".into()); }
    for (key, draft) in &drafts { validate(key, draft)?; }
    Ok(drafts)
}
fn write_index(root: &Path, drafts: &Drafts) -> Result<(), String> {
    let bytes = serde_json::to_vec(drafts).map_err(|_| "Cannot encode drafts")?;
    if bytes.len() > MAX_INDEX { return Err("Draft storage full".into()); }
    ensure_dir(root)?; ensure_dir(&store(root))?;
    write_atomic(&store(root), "index.json", &bytes)
}
fn save_at(root: &Path, key: &str, draft: Draft) -> Result<(), String> {
    validate(key, &draft)?;
    for im in &draft.images {
        let meta = std::fs::symlink_metadata(blobs(root).join(&im.sha256)).map_err(|_| "Draft image is missing, attach it again")?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != im.size as u64 { return Err("Draft image is missing, attach it again".into()); }
    }
    let mut drafts = load_index(root)?;
    if draft.text.is_empty() && draft.images.is_empty() { drafts.remove(key); } else { drafts.insert(key.into(), draft); }
    if drafts.len() > MAX_DRAFTS { return Err("Too many drafts".into()); }
    write_index(root, &drafts)?;
    collect(root, &drafts);
    Ok(())
}
/// Remove blobs no draft references, after a grace period for in-flight attaches.
fn collect(root: &Path, drafts: &Drafts) {
    let live: HashSet<&str> = drafts.values().flat_map(|d| d.images.iter().map(|i| i.sha256.as_str())).collect();
    let Ok(entries) = std::fs::read_dir(blobs(root)) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !sha_valid(name) || live.contains(name) { continue; }
        let old = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > BLOB_GRACE);
        if old { let _ = std::fs::remove_file(entry.path()); }
    }
}

/// One-time conversion of the previous single-file base64 format.
fn migrate_legacy(root: &Path) -> Result<(), String> {
    #[derive(Deserialize)] struct OldImage { data: String, media_type: String, name: String }
    #[derive(Deserialize)] struct OldDraft { text: String, images: Vec<OldImage> }
    let old = root.join("composer-drafts.json");
    let Some(bytes) = read_bounded(&old, MAX_LEGACY)? else { return Ok(()) };
    let parsed: BTreeMap<String, OldDraft> = serde_json::from_slice(&bytes).map_err(|_| "Invalid legacy draft file, refusing to overwrite")?;
    let mut drafts = if store(root).join("index.json").exists() { load_index_only(root)? } else { Drafts::new() };
    for (key, d) in parsed.into_iter().take(MAX_DRAFTS) {
        if !key_valid(&key) || drafts.contains_key(&key) { continue; }
        let mut images = Vec::new();
        for im in d.images.into_iter().take(MAX_IMAGES) {
            let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&im.data) else { continue };
            let Ok((sha, media)) = put_blob_at(root, &raw) else { continue };
            let _ = im.media_type;
            images.push(ImageRef { sha256: sha, media_type: media.into(), name: im.name.chars().take(200).collect(), size: raw.len() });
        }
        let text: String = if d.text.len() > MAX_TEXT { String::new() } else { d.text };
        if !text.is_empty() || !images.is_empty() { drafts.insert(key, Draft { text, images }); }
    }
    write_index(root, &drafts)?;
    std::fs::remove_file(&old).map_err(|_| "Cannot retire legacy draft file")?;
    let _ = std::fs::remove_file(root.join(".composer-drafts.tmp"));
    Ok(())
}
fn load_index_only(root: &Path) -> Result<Drafts, String> {
    let Some(bytes) = read_bounded(&store(root).join("index.json"), MAX_INDEX)? else { return Ok(Drafts::new()) };
    serde_json::from_slice(&bytes).map_err(|_| "Invalid draft file, refusing to overwrite".into())
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn directory() -> Result<&'static Path, String> {
    isolated_directory(std::env::var_os("PHILIUM_PREVIEW").is_some(), crate::secrets::dir())
}
fn isolated_directory(preview: bool, dir: Option<&Path>) -> Result<&Path, String> {
    if preview { return Err("Draft storage is disabled in preview builds".into()); }
    dir.ok_or_else(|| "Draft storage not initialized".into())
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        f()
    }).await.map_err(|_| "Draft storage worker failed".to_string())?
}

#[tauri::command]
pub async fn drafts_load(key: String) -> Result<Draft, String> {
    if !key_valid(&key) { return Err("Invalid draft key".into()); }
    blocking(move || Ok(load_index(directory()?)?.remove(&key).unwrap_or_default())).await
}
#[tauri::command]
pub async fn drafts_save(key: String, draft: Draft) -> Result<(), String> {
    // Awaited on purpose: the UI shows "saved" only after the atomic write is durable.
    blocking(move || save_at(directory()?, &key, draft)).await
}
/// Body is the raw image bytes. Returns the content hash and sniffed type.
#[tauri::command]
pub async fn drafts_put_blob(request: tauri::ipc::Request<'_>) -> Result<serde_json::Value, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else { return Err("Expected raw image bytes".into()) };
    if bytes.len() > MAX_IMAGE { return Err("Images must be 5 MiB or smaller.".into()); }
    let bytes = bytes.clone();
    let size = bytes.len();
    let (sha, media) = blocking(move || put_blob_at(directory()?, &bytes)).await?;
    Ok(serde_json::json!({ "sha256": sha, "media_type": media, "size": size }))
}
#[tauri::command]
pub async fn drafts_blob(sha256: String) -> Result<tauri::ipc::Response, String> {
    let bytes = blocking(move || get_blob_at(directory()?, &sha256)).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-image";
    fn temp() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("philium-drafts-{}-{}-{sequence}", std::process::id(), SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&p).unwrap(); p
    }
    fn image(root: &Path, bytes: &[u8]) -> ImageRef {
        let (sha, media) = put_blob_at(root, bytes).unwrap();
        ImageRef { sha256: sha, media_type: media.into(), name: "shot.png".into(), size: bytes.len() }
    }
    #[tokio::test]
    async fn async_load_rejects_invalid_keys_before_storage_access() {
        assert!(matches!(drafts_load("../vault.json".into()).await, Err(e) if e == "Invalid draft key"));
    }
    #[test]
    fn preview_cannot_resolve_private_storage() {
        assert!(isolated_directory(true, Some(Path::new("/unused"))).is_err());
        assert!(isolated_directory(false, None).is_err());
    }
    #[test]
    fn roundtrip_isolation_clear_and_permissions() {
        let root = temp();
        let im = image(&root, PNG);
        save_at(&root, "agent:a", Draft { text: "unsent".into(), images: vec![im.clone()] }).unwrap();
        save_at(&root, "agent:b", Draft { text: "peer".into(), images: vec![] }).unwrap();
        let loaded = load_index(&root).unwrap();
        assert_eq!(loaded["agent:a"].images[0], im);
        assert_eq!(get_blob_at(&root, &im.sha256).unwrap(), PNG);
        for p in [store(&root), blobs(&root)] { assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o700); }
        for p in [store(&root).join("index.json"), blobs(&root).join(&im.sha256)] { assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600); }
        save_at(&root, "agent:a", Draft::default()).unwrap();
        let restored = load_index(&root).unwrap();
        assert!(!restored.contains_key("agent:a")); assert_eq!(restored["agent:b"].text, "peer");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn text_edits_do_not_rewrite_image_bytes_and_index_stays_small() {
        let root = temp();
        let big = [PNG, &vec![7u8; 3 * 1024 * 1024]].concat();
        let im = image(&root, &big);
        let blob = blobs(&root).join(&im.sha256);
        let before = std::fs::metadata(&blob).unwrap().modified().unwrap();
        for i in 0..20 { save_at(&root, "agent:a", Draft { text: format!("edit {i}"), images: vec![im.clone()] }).unwrap(); }
        assert_eq!(std::fs::metadata(&blob).unwrap().modified().unwrap(), before);
        assert!(std::fs::metadata(store(&root).join("index.json")).unwrap().len() < 1024);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn content_sniffing_rejects_disguised_and_oversized_images() {
        let root = temp();
        assert!(put_blob_at(&root, b"<svg onload=alert(1)>").is_err());
        assert!(put_blob_at(&root, &[]).is_err());
        assert!(put_blob_at(&root, &[PNG, &vec![0u8; MAX_IMAGE]].concat()).is_err());
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_or_tampered_blobs_are_reported_not_trusted() {
        let root = temp();
        let im = image(&root, PNG);
        std::fs::write(blobs(&root).join(&im.sha256), b"\x89PNG\r\n\x1a\ntampered-bytes").unwrap();
        assert!(get_blob_at(&root, &im.sha256).unwrap_err().contains("corrupted"));
        let ghost = ImageRef { sha256: "a".repeat(64), ..im };
        assert!(save_at(&root, "agent:a", Draft { text: String::new(), images: vec![ghost] }).is_err());
        assert!(get_blob_at(&root, "../index.json").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn garbage_collection_keeps_referenced_and_recent_blobs() {
        let root = temp();
        let kept = image(&root, PNG);
        let fresh = image(&root, b"GIF89a-fresh");
        let stale = image(&root, b"GIF89a-stale");
        let old = SystemTime::now() - BLOB_GRACE - Duration::from_secs(60);
        std::fs::File::options().append(true).open(blobs(&root).join(&stale.sha256)).unwrap().set_modified(old).unwrap();
        std::fs::File::options().append(true).open(blobs(&root).join(&kept.sha256)).unwrap().set_modified(old).unwrap();
        save_at(&root, "agent:a", Draft { text: String::new(), images: vec![kept.clone()] }).unwrap();
        assert!(blobs(&root).join(&kept.sha256).exists());
        assert!(blobs(&root).join(&fresh.sha256).exists());
        assert!(!blobs(&root).join(&stale.sha256).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn symlinks_and_corruption_are_rejected_without_overwrite() {
        let root = temp();
        let outside = temp();
        std::fs::create_dir_all(store(&root)).unwrap();
        std::fs::write(outside.join("target"), "untouched").unwrap();
        std::os::unix::fs::symlink(outside.join("target"), store(&root).join("index.json")).unwrap();
        assert!(load_index(&root).is_err());
        assert!(save_at(&root, "agent:a", Draft::default()).is_err());
        assert_eq!(std::fs::read_to_string(outside.join("target")).unwrap(), "untouched");
        std::fs::remove_file(store(&root).join("index.json")).unwrap();
        std::fs::write(store(&root).join("index.json"), "broken").unwrap();
        assert!(save_at(&root, "agent:a", Draft::default()).is_err());
        assert_eq!(std::fs::read_to_string(store(&root).join("index.json")).unwrap(), "broken");
        assert!(serde_json::from_str::<Draft>(r#"{"text":"","images":[],"secret":true}"#).is_err());
        assert!(save_at(&root, "../vault.json", Draft::default()).is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn legacy_single_file_drafts_migrate_once() {
        let root = temp();
        let data = base64::engine::general_purpose::STANDARD.encode(PNG);
        std::fs::write(root.join("composer-drafts.json"), format!(r#"{{"agent:a":{{"text":"old","images":[{{"data":"{data}","media_type":"image/png","name":"x.png"}}]}}}}"#)).unwrap();
        let loaded = load_index(&root).unwrap();
        assert_eq!(loaded["agent:a"].text, "old");
        assert_eq!(get_blob_at(&root, &loaded["agent:a"].images[0].sha256).unwrap(), PNG);
        assert!(!root.join("composer-drafts.json").exists());
        assert_eq!(load_index(&root).unwrap(), loaded);
        std::fs::remove_dir_all(root).unwrap();
    }
}
