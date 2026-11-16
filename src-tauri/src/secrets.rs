//! All secrets (provider keys, OAuth tokens) live in ONE macOS Keychain item,
//! read once per app run and cached in memory. macOS asks "allow access?"
//! per item and per read from an unrecognised build, so one item read once
//! means at most one prompt per install instead of one per agent spawn.
//!
//! Older builds stored one item per secret (`provider:<id>`, `oauth:<id>`).
//! Those are moved into the vault the first time they are asked for.
use crate::ctx::LockExt;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const SERVICE: &str = "dev.gunnarsson.philium";
const VAULT: &str = "vault";

type Map = BTreeMap<String, String>;

fn cache() -> &'static Mutex<Option<Map>> {
    static C: OnceLock<Mutex<Option<Map>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn disabled() -> bool {
    // Improve-Philium previews must never see the user's real keys.
    std::env::var_os("PHILIUM_PREVIEW").is_some()
}

fn item(account: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, account).ok()
}

fn load() -> Map {
    item(VAULT).and_then(|e| e.get_password().ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(m: &Map) -> Result<(), String> {
    let e = item(VAULT).ok_or("keychain unavailable")?;
    e.set_password(&serde_json::to_string(m).unwrap_or_default()).map_err(|e| format!("keychain: {e}"))
}

/// Runs `f` on the cached vault, loading it from the Keychain on first use.
fn with<R>(f: impl FnOnce(&mut Map) -> R) -> R {
    let mut g = cache().lk();
    let m = g.get_or_insert_with(load);
    f(m)
}

pub fn get(key: &str) -> Option<String> {
    if disabled() {
        return None;
    }
    if let Some(v) = with(|m| m.get(key).cloned()) {
        return Some(v);
    }
    // One-time migration of a pre-vault item.
    let legacy = item(key)?;
    let v = legacy.get_password().ok()?;
    let saved = with(|m| {
        m.insert(key.to_string(), v.clone());
        save(m)
    });
    if saved.is_ok() {
        let _ = legacy.delete_credential();
    }
    Some(v)
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    if disabled() {
        return Err("keychain is disabled in preview builds".into());
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
    if let Some(e) = item(key) {
        let _ = e.delete_credential();
    }
}
