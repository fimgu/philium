//! All secrets (provider keys, OAuth tokens) live in one file,
//! `<data dir>/secrets/vault.json`, owned by the user with mode 0600 in a
//! 0700 folder. No macOS Keychain, so no "allow access" prompts.
//!
//! Agents never read it: every agent process (HAL, shells, background
//! commands) runs under a Seatbelt profile that denies this folder, strict
//! and trusted agents alike (see `sandbox::deny_secrets`). Only Philium's own
//! process reads it, and the gateway injects keys into model traffic.
//!
//! Older builds kept one Keychain item per secret. Those are copied into the
//! file the first time they are asked for and then removed from the Keychain.
use crate::ctx::LockExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const LEGACY_SERVICE: &str = "dev.gunnarsson.philium";

type Map = BTreeMap<String, String>;

static DIR: OnceLock<PathBuf> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Map>> {
    static C: OnceLock<Mutex<Option<Map>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Call once at startup with the app data dir.
pub fn init(data_dir: &Path) {
    let _ = DIR.set(data_dir.join("secrets"));
}

/// Folder holding the vault; sandbox profiles deny it to agents.
pub fn dir() -> Option<&'static Path> {
    DIR.get().map(PathBuf::as_path)
}

fn disabled() -> bool {
    // Improve-Philium previews must never see the user's real keys.
    std::env::var_os("PHILIUM_PREVIEW").is_some()
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("vault.json"))
}

fn load() -> Map {
    file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(m: &Map) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let d = dir().ok_or("secrets not initialised")?;
    std::fs::create_dir_all(d).map_err(|e| format!("secrets: {e}"))?;
    let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    let tmp = d.join(".vault.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| format!("secrets: {e}"))?;
    f.write_all(serde_json::to_string_pretty(m).unwrap_or_default().as_bytes()).map_err(|e| format!("secrets: {e}"))?;
    f.sync_all().map_err(|e| format!("secrets: {e}"))?;
    std::fs::rename(&tmp, d.join("vault.json")).map_err(|e| format!("secrets: {e}"))
}

fn with<R>(f: impl FnOnce(&mut Map) -> R) -> R {
    let mut g = cache().lk();
    let m = g.get_or_insert_with(load);
    f(m)
}

/// Pre-file builds: one Keychain item per secret. Read once, move, delete.
#[cfg(target_os = "macos")]
fn take_legacy(key: &str, m: &mut Map) -> Option<String> {
    let e = keyring::Entry::new(LEGACY_SERVICE, key).ok()?;
    let v = e.get_password().ok()?;
    m.insert(key.to_string(), v.clone());
    if save(m).is_ok() {
        let _ = e.delete_credential();
    }
    Some(v)
}
#[cfg(not(target_os = "macos"))]
fn take_legacy(_key: &str, _m: &mut Map) -> Option<String> {
    let _ = LEGACY_SERVICE;
    None
}

pub fn get(key: &str) -> Option<String> {
    if disabled() {
        return None;
    }
    with(|m| {
        if let Some(v) = m.get(key) {
            return Some(v.clone());
        }
        take_legacy(key, m)
    })
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    if disabled() {
        return Err("secret storage is disabled in preview builds".into());
    }
    with(|m| {
        if m.get(key).map(String::as_str) == Some(value) {
            return Ok(());
        }
        m.insert(key.to_string(), value.to_string());
        save(m)
    })
}

pub fn delete(key: &str) {
    if disabled() {
        return;
    }
    with(|m| {
        if m.remove(key).is_some() {
            let _ = save(m);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn vault_is_private_and_hidden_from_agents() {
        let tmp = std::env::temp_dir().join(format!("philium-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        super::init(&tmp);
        let dir = super::dir().unwrap().to_path_buf();
        super::set("provider:t", "k-123").unwrap();
        assert_eq!(super::get("provider:t").as_deref(), Some("k-123"));
        let f = dir.join("vault.json");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // Reload from disk, not the cache.
        *super::cache().lock().unwrap() = None;
        assert_eq!(super::get("provider:t").as_deref(), Some("k-123"));

        let cat = |argv: Vec<String>| std::process::Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
        let plain = vec!["/bin/cat".to_string(), f.to_string_lossy().into()];
        assert!(cat(plain.clone()).status.success());
        // Trusted agents: sandboxed with only the vault denied.
        let out = cat(crate::sandbox::wrap_argv(None, plain));
        assert!(!out.status.success(), "agent process could read the vault");
        assert!(!String::from_utf8_lossy(&out.stdout).contains("k-123"));
        // And they can still read their own stuff next to it.
        std::fs::write(tmp.join("ok.txt"), "hi").unwrap();
        assert!(cat(crate::sandbox::wrap_argv(None, vec!["/bin/cat".into(), tmp.join("ok.txt").to_string_lossy().into()])).status.success());

        super::delete("provider:t");
        assert_eq!(super::cache().lock().unwrap().as_ref().unwrap().get("provider:t"), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
