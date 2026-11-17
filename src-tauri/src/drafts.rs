//! Private UI composer storage. Never exposed through agent RPC.
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::{Read, Write}, path::Path, sync::Mutex};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const MAX_FILE: usize = 64 * 1024 * 1024;
const MAX_IMAGE: usize = 5 * 1024 * 1024;
static LOCK: Mutex<()> = Mutex::new(());
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft { text: String, images: Vec<Image> }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image { data: String, media_type: String, name: String }
type Drafts = BTreeMap<String, Draft>;
fn key_valid(key: &str) -> bool {
    key.strip_prefix("agent:").is_some_and(|id| !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}
fn validate(key: &str, draft: &Draft) -> Result<(), String> {
    if !key_valid(key) || draft.text.len() > 1024 * 1024 || draft.images.len() > 8 { return Err("Invalid or oversized draft".into()); }
    for image in &draft.images {
        if !matches!(image.media_type.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") || image.name.len() > 1024 || image.data.len() > MAX_IMAGE.div_ceil(3) * 4 {
            return Err("Invalid or oversized draft image".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data).map_err(|_| "Invalid draft image encoding")?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE { return Err("Invalid or oversized draft image".into()); }
    }
    Ok(())
}
fn load(dir: &Path) -> Result<Drafts, String> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => return Err("Invalid draft directory".into()),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Drafts::new()),
        Err(_) => return Err("Cannot inspect draft directory".into()),
    }
    let path = dir.join("composer-drafts.json");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE as u64 { return Err("Invalid draft file".into()); }
    }
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Drafts::new()),
        Err(_) => return Err("Cannot read drafts".into()),
    };
    let mut bytes = Vec::new();
    file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes).map_err(|_| "Cannot read drafts")?;
    if bytes.len() > MAX_FILE { return Err("Draft file too large".into()); }
    let drafts: Drafts = serde_json::from_slice(&bytes).map_err(|_| "Invalid draft file, refusing to overwrite")?;
    if drafts.len() > 256 { return Err("Too many drafts".into()); }
    for (key, draft) in &drafts { validate(key, draft)?; }
    Ok(drafts)
}
fn save(dir: &Path, key: &str, draft: Draft) -> Result<(), String> {
    validate(key, &draft)?;
    let mut drafts = load(dir)?;
    if draft.text.is_empty() && draft.images.is_empty() { drafts.remove(key); } else { drafts.insert(key.into(), draft); }
    if drafts.len() > 256 { return Err("Too many drafts".into()); }
    let bytes = serde_json::to_vec(&drafts).map_err(|_| "Cannot encode drafts")?;
    if bytes.len() > MAX_FILE { return Err("Draft storage full".into()); }
    std::fs::create_dir_all(dir).map_err(|_| "Cannot create private draft directory")?;
    let meta = std::fs::symlink_metadata(dir).map_err(|_| "Cannot inspect draft directory")?;
    if !meta.is_dir() || meta.file_type().is_symlink() { return Err("Invalid draft directory".into()); }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| "Cannot secure draft directory")?;
    let tmp = dir.join(".composer-drafts.tmp");
    // A stale temporary file after interruption contains only drafts, never vault data.
    match std::fs::remove_file(&tmp) { Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(_) => return Err("Cannot replace draft temporary file".into()) }
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|_| "Cannot create draft file")?;
        file.write_all(&bytes).map_err(|_| "Cannot write drafts")?;
        file.sync_all().map_err(|_| "Cannot sync drafts")?;
        std::fs::rename(&tmp, dir.join("composer-drafts.json")).map_err(|_| "Cannot replace drafts")?;
        std::fs::File::open(dir).and_then(|f| f.sync_all()).map_err(|_| "Cannot sync draft directory")?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(tmp); }
    result
}
fn directory() -> Result<&'static Path, String> {
    isolated_directory(std::env::var_os("PHILIUM_PREVIEW").is_some(), crate::secrets::dir())
}
fn isolated_directory(preview: bool, dir: Option<&Path>) -> Result<&Path, String> {
    if preview { return Err("Draft storage is disabled in preview builds".into()); }
    dir.ok_or_else(|| "Draft storage not initialized".into())
}
#[tauri::command]
pub fn drafts_load(key: String) -> Result<Draft, String> {
    if !key_valid(&key) { return Err("Invalid draft key".into()); }
    let _guard = LOCK.lock().map_err(|_| "Draft storage unavailable")?;
    Ok(load(directory()?)?.remove(&key).unwrap_or_default())
}
#[tauri::command]
pub fn drafts_save(key: String, draft: Draft) -> Result<(), String> {
    let _guard = LOCK.lock().map_err(|_| "Draft storage unavailable")?;
    save(directory()?, &key, draft)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("philium-drafts-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&p).unwrap(); p
    }
    #[test]
    fn preview_cannot_resolve_private_storage() {
        assert!(isolated_directory(true, Some(Path::new("/unused"))).is_err());
        assert!(isolated_directory(false, None).is_err());
        assert_eq!(isolated_directory(false, Some(Path::new("/unused"))).unwrap(), Path::new("/unused"));
    }
    #[test]
    fn roundtrip_isolation_clear_and_permissions() {
        let dir = temp();
        let draft = Draft { text: "unsent".into(), images: vec![Image { data: "aGk=".into(), media_type: "image/png".into(), name: "test.png".into() }] };
        save(&dir, "agent:a", draft.clone()).unwrap();
        save(&dir, "agent:b", draft).unwrap();
        assert_eq!(load(&dir).unwrap()["agent:a"].images[0].data, "aGk=");
        assert_eq!(std::fs::metadata(dir.join("composer-drafts.json")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!dir.join("vault.json").exists());
        save(&dir, "agent:a", Draft::default()).unwrap();
        let restored = load(&dir).unwrap();
        assert!(!restored.contains_key("agent:a")); assert_eq!(restored["agent:b"].text, "unsent");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_atomic_write_preserves_previous_draft() {
        let dir = temp();
        save(&dir, "agent:a", Draft { text: "before".into(), images: vec![] }).unwrap();
        std::fs::create_dir(dir.join(".composer-drafts.tmp")).unwrap();
        assert!(save(&dir, "agent:a", Draft { text: "after".into(), images: vec![] }).is_err());
        assert_eq!(load(&dir).unwrap()["agent:a"].text, "before");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn symlinks_and_storage_bounds_are_rejected() {
        let dir = temp();
        let outside = temp();
        std::fs::write(outside.join("not-a-draft"), "untouched").unwrap();
        std::os::unix::fs::symlink(outside.join("not-a-draft"), dir.join("composer-drafts.json")).unwrap();
        assert!(load(&dir).is_err());
        assert!(save(&dir, "agent:a", Draft::default()).is_err());
        assert_eq!(std::fs::read_to_string(outside.join("not-a-draft")).unwrap(), "untouched");
        std::fs::remove_file(dir.join("composer-drafts.json")).unwrap();
        let file = std::fs::File::create(dir.join("composer-drafts.json")).unwrap();
        file.set_len((MAX_FILE + 1) as u64).unwrap();
        assert!(load(&dir).is_err());
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        assert!(load(&linked).is_err());
        let mut draft = Draft { text: String::new(), images: vec![Image { data: "aGk=".into(), media_type: "image/png".into(), name: "x".into() }; 9] };
        assert!(validate("agent:a", &draft).is_err());
        draft.images.truncate(1);
        draft.images[0].data = "A".repeat(MAX_IMAGE.div_ceil(3) * 4 + 1);
        assert!(validate("agent:a", &draft).is_err());
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn rejects_invalid_boundaries_and_preserves_corruption() {
        let dir = temp();
        assert!(save(&dir, "../vault.json", Draft::default()).is_err());
        assert!(save(&dir, "agent:a", Draft { text: "x".repeat(1024 * 1024 + 1), images: vec![] }).is_err());
        assert!(serde_json::from_str::<Draft>(r#"{"text":"","images":[],"secret":true}"#).is_err());
        let mut draft = Draft { text: "".into(), images: vec![Image { data: "not base64".into(), media_type: "image/png".into(), name: "x".into() }] };
        assert!(validate("agent:a", &draft).is_err());
        draft.images[0].data = "aGk=".into(); draft.images[0].media_type = "image/svg+xml".into();
        assert!(validate("agent:a", &draft).is_err());
        std::fs::write(dir.join("composer-drafts.json"), "broken").unwrap();
        assert!(save(&dir, "agent:a", Draft::default()).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("composer-drafts.json")).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
