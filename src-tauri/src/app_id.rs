//! The app identifier, defined once. `build.rs` reads `identifier` from
//! `tauri.conf.json` and exports it as `PHILIUM_APP_ID`, so this const can
//! never drift from the bundle. Every data path is derived from it.
//!
//! Also holds the one-time move from the previous identifier.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Bundle identifier of the main app, e.g. `app.philium`.
pub const ID: &str = env!("PHILIUM_APP_ID");
/// Side-by-side Pilot build.
pub const PILOT: &str = concat!(env!("PHILIUM_APP_ID"), ".pilot");
/// Improve-Philium preview build.
pub const PREVIEW: &str = concat!(env!("PHILIUM_APP_ID"), ".preview");
/// App data dir relative to the macOS home.
pub const MAC_DATA_REL: &str = concat!("Library/Application Support/", env!("PHILIUM_APP_ID"));

/// The identifier used before the rename. Split so the tracked-file scan for
/// the old owner name stays meaningful.
const LEGACY_ID: &str = concat!("dev.", "gunnar", "sson.philium");

/// (old, new) directory pairs to move for this platform, given the user's dirs.
pub fn legacy_pairs(home: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut bases: Vec<PathBuf> = vec![];
    if cfg!(target_os = "macos") {
        for d in ["Library/Application Support", "Library/Caches", "Library/WebKit"] {
            bases.push(home.join(d));
        }
    } else if cfg!(windows) {
        for v in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(p) = std::env::var_os(v) {
                bases.push(PathBuf::from(p));
            }
        }
    } else {
        let xdg = |v: &str, d: &str| std::env::var_os(v).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(d));
        bases.push(xdg("XDG_DATA_HOME", ".local/share"));
        bases.push(xdg("XDG_CACHE_HOME", ".cache"));
        bases.push(xdg("XDG_CONFIG_HOME", ".config"));
    }
    bases.dedup();
    bases.into_iter().map(|b| (b.join(LEGACY_ID), b.join(ID))).collect()
}

/// Move every legacy dir for the real user. Called once at startup, before
/// any window or data dir exists. Errors are reported, never fatal.
pub fn migrate_legacy() {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from) else { return };
    for (old, new) in legacy_pairs(&home) {
        if let Err(e) = migrate_dir(&old, &new) {
            eprintln!("philium: could not move {} to {}: {e}", old.display(), new.display());
        }
    }
    #[cfg(target_os = "macos")]
    keychain::migrate(LEGACY_ID, ID);
}

/// Move `old` to `new` when `new` does not exist yet. Rename first (atomic);
/// across volumes copy into a staging dir, verify byte for byte, rename the
/// staging dir into place and only then delete `old`.
/// Returns whether anything moved.
pub fn migrate_dir(old: &Path, new: &Path) -> io::Result<bool> {
    if new.symlink_metadata().is_ok() || !old.is_dir() {
        return Ok(false);
    }
    if let Some(p) = new.parent() {
        fs::create_dir_all(p)?;
    }
    if fs::rename(old, new).is_ok() {
        return Ok(true);
    }
    let staging = new.with_file_name(format!(".{}.migrating", new.file_name().and_then(|n| n.to_str()).unwrap_or("app")));
    let _ = fs::remove_dir_all(&staging);
    let copied = copy_tree(old, &staging).and_then(|_| {
        if same_tree(old, &staging)? {
            Ok(())
        } else {
            Err(io::Error::other("copy did not verify"))
        }
    });
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    if let Err(e) = fs::rename(&staging, new) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    // Verified copy in place; the old tree may now go.
    fs::remove_dir_all(old)?;
    Ok(true)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    fs::set_permissions(to, fs::metadata(from)?.permissions())?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_dir() {
            copy_tree(&src, &dst)?;
        } else if t.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?;
            #[cfg(not(unix))]
            fs::copy(&src, &dst).map(|_| ())?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn same_tree(a: &Path, b: &Path) -> io::Result<bool> {
    let mut ea: Vec<_> = fs::read_dir(a)?.collect::<Result<_, _>>()?;
    let eb = fs::read_dir(b)?.count();
    if ea.len() != eb {
        return Ok(false);
    }
    ea.sort_by_key(|e| e.file_name());
    for e in ea {
        let (pa, pb) = (e.path(), b.join(e.file_name()));
        let t = e.file_type()?;
        let ok = if t.is_dir() {
            pb.is_dir() && same_tree(&pa, &pb)?
        } else if t.is_symlink() {
            fs::read_link(&pa)? == fs::read_link(&pb)?
        } else {
            fs::read(&pa)? == fs::read(&pb)?
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Generic-password items under the old service move to the new one. The old
/// item is deleted only after the new one reads back identical.
#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
    use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

    fn accounts(service: &str) -> Vec<String> {
        let Ok(found) = ItemSearchOptions::new().class(ItemClass::generic_password()).service(service).load_attributes(true).limit(Limit::All).search() else { return vec![] };
        found
            .iter()
            .filter_map(|r| match r {
                SearchResult::Dict(_) => r.simplify_dict()?.get("acct").cloned(),
                _ => None,
            })
            .collect()
    }

    pub fn migrate(old: &str, new: &str) {
        for acct in accounts(old) {
            let Ok(v) = get_generic_password(old, &acct) else { continue };
            let moved = match get_generic_password(new, &acct) {
                Ok(cur) => cur == v,
                Err(_) => set_generic_password(new, &acct, &v).is_ok() && get_generic_password(new, &acct).ok().as_deref() == Some(&v[..]),
            };
            if moved {
                let _ = delete_generic_password(old, &acct);
            } else {
                eprintln!("philium: Keychain item {acct} kept under the old service");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_matches_bundle_configs() {
        let conf = |f: &str| -> String {
            let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(f)).unwrap()).unwrap();
            v["identifier"].as_str().unwrap().to_string()
        };
        assert_eq!(conf("tauri.conf.json"), ID);
        assert_eq!(conf("tauri.pilot.conf.json"), PILOT);
        assert_eq!(PREVIEW, format!("{ID}.preview"));
        assert!(MAC_DATA_REL.ends_with(ID));
        assert_ne!(LEGACY_ID, ID);
    }

    /// The old identifier must not survive in any tracked file. Case-sensitive:
    /// the LICENSE copyright holder's capitalised name is a legal notice, not an id.
    #[test]
    fn no_legacy_owner_name_in_tracked_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let out = std::process::Command::new("git").arg("-C").arg(&root).args(["ls-files", "-z"]).output().unwrap();
        assert!(out.status.success());
        let needle = concat!("gunnar", "sson");
        let mut bad = vec![];
        for f in out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let rel = String::from_utf8_lossy(f).to_string();
            if rel.contains(needle) {
                bad.push(rel.clone());
            }
            if let Ok(bytes) = fs::read(root.join(&rel)) {
                if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    bad.push(rel);
                }
            }
        }
        assert!(bad.is_empty(), "old identifier still present in: {bad:?}");
    }

    fn fixture(root: &Path) {
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::write(root.join("philium.db"), b"db bytes").unwrap();
        fs::write(root.join("secrets/vault.json"), b"{}").unwrap();
    }

    #[test]
    fn migrate_moves_when_new_missing() {
        let t = crate::ctx::test_dir("appid-move");
        let (old, new) = (t.join(LEGACY_ID), t.join(ID));
        fixture(&old);
        assert!(migrate_dir(&old, &new).unwrap());
        assert!(!old.exists());
        assert_eq!(fs::read(new.join("philium.db")).unwrap(), b"db bytes");
        assert_eq!(fs::read(new.join("secrets/vault.json")).unwrap(), b"{}");
        // Second run is a no-op.
        assert!(!migrate_dir(&old, &new).unwrap());
    }

    #[test]
    fn migrate_never_overwrites_existing_new_dir() {
        let t = crate::ctx::test_dir("appid-keep");
        let (old, new) = (t.join(LEGACY_ID), t.join(ID));
        fixture(&old);
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("philium.db"), b"newer").unwrap();
        assert!(!migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read(new.join("philium.db")).unwrap(), b"newer");
        assert_eq!(fs::read(old.join("philium.db")).unwrap(), b"db bytes", "old data untouched");
    }

    #[test]
    fn copy_path_verifies_before_old_is_removed() {
        let t = crate::ctx::test_dir("appid-copy");
        let old = t.join("old");
        fixture(&old);
        let staged = t.join("staged");
        copy_tree(&old, &staged).unwrap();
        assert!(same_tree(&old, &staged).unwrap());
        fs::write(staged.join("philium.db"), b"corrupt").unwrap();
        assert!(!same_tree(&old, &staged).unwrap(), "a differing copy must not verify");
        fs::write(staged.join("extra"), b"x").unwrap();
        assert!(!same_tree(&old, &staged).unwrap());
    }

    #[test]
    fn pairs_derive_from_ids() {
        let home = crate::ctx::test_dir("appid-pairs");
        let pairs = legacy_pairs(&home);
        assert!(!pairs.is_empty());
        for (o, n) in pairs {
            assert_eq!(o.file_name().unwrap(), LEGACY_ID);
            assert_eq!(n.file_name().unwrap(), ID);
            assert_eq!(o.parent(), n.parent());
        }
        if cfg!(target_os = "macos") {
            assert!(legacy_pairs(&home).iter().any(|(_, n)| n.ends_with(MAC_DATA_REL)));
        }
    }
}
