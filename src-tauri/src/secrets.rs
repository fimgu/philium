//! All secrets (provider keys, OAuth tokens) live in one file,
//! `<data dir>/secrets/vault.json`, owned by the user with mode 0600 in a
//! 0700 folder. No macOS Keychain, so no "allow access" prompts.
//!
//! Agents never read it: every agent process (HAL, shells, background
//! commands) runs under a Seatbelt profile that denies this folder, strict
//! and trusted agents alike (see `sandbox::deny_secrets`). Only Philium's own
//! process reads it, and the gateway injects keys into model traffic.
//!
//! Existing Keychain entries are left untouched. Missing file credentials
//! require signing in again rather than triggering an automatic Keychain read.
use crate::ctx::LockExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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

fn load() -> Result<Map, String> {
    let path = file().ok_or("secrets not initialised")?;
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| "secret file is invalid; refusing to overwrite it".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("cannot read secret file: {e}")),
    }
}

fn save(m: &Map) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let d = dir().ok_or("secrets not initialised")?;
    std::fs::create_dir_all(d).map_err(|e| format!("secrets: {e}"))?;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("secrets permissions: {e}"))?;
    let tmp = d.join(".vault.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| format!("secrets: {e}"))?;
    f.write_all(serde_json::to_string_pretty(m).unwrap_or_default().as_bytes()).map_err(|e| format!("secrets: {e}"))?;
    f.sync_all().map_err(|e| format!("secrets: {e}"))?;
    std::fs::rename(&tmp, d.join("vault.json")).map_err(|e| format!("secrets: {e}"))
}

fn with<R>(f: impl FnOnce(&mut Map) -> R) -> Result<R, String> {
    let mut g = cache().lk();
    if g.is_none() {
        *g = Some(load()?);
    }
    Ok(f(g.as_mut().expect("loaded secret cache")))
}

pub fn get(key: &str) -> Option<String> {
    if disabled() {
        return None;
    }
    with(|m| m.get(key).cloned()).ok().flatten()
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    if disabled() {
        return Err("secret storage is disabled in preview builds".into());
    }
    with(|m| {
        if m.get(key).map(String::as_str) == Some(value) {
            return Ok(());
        }
        let mut next = m.clone();
        next.insert(key.to_string(), value.to_string());
        save(&next)?;
        *m = next;
        Ok(())
    })?
}

pub fn delete(key: &str) {
    if disabled() {
        return;
    }
    let _ = with(|m| {
        let mut next = m.clone();
        if next.remove(key).is_some() && save(&next).is_ok() {
            *m = next;
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
        assert_eq!(super::get("provider:t"), None);
        assert_eq!(super::get("oauth:missing"), None);
        assert_eq!(super::get("oauth:missing"), None);
        // Failed writes must not poison the cache or masquerade as success.
        std::fs::create_dir(dir.join(".vault.tmp")).unwrap();
        assert!(super::set("provider:t", "new-value").is_err());
        assert_eq!(super::get("provider:t"), None);
        assert!(super::set("provider:t", "new-value").is_err());
        std::fs::remove_dir(dir.join(".vault.tmp")).unwrap();
        // Invalid storage must not be treated as empty and overwritten.
        std::fs::write(&f, "invalid-json").unwrap();
        *super::cache().lock().unwrap() = None;
        assert!(super::set("provider:t", "new-value").is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "invalid-json");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
