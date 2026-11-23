//! First-run checks that need the host: is the GitHub CLI there, and is it signed in.
//! The binary is found on the login PATH at runtime, never at a fixed location.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const GITHUB_CLI: &str = "gh";

fn on_path(bin: &str, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    std::env::split_paths(&path?).map(|d| d.join(bin)).find(|p| executable(p))
}

fn executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    m.is_file()
}

/// `{installed, signed_in, path}` for the GitHub CLI, run as the person with their own config.
async fn github() -> Value {
    let Some(exe) = on_path(GITHUB_CLI, std::env::var_os("PATH")) else {
        return json!({"installed": false, "signed_in": false});
    };
    let mut c = tokio::process::Command::new(&exe);
    c.args(["auth", "status"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    if let Some(home) = crate::pilot::real_home() {
        c.env("HOME", home);
    }
    let ok = matches!(tokio::time::timeout(Duration::from_secs(6), c.status()).await, Ok(Ok(s)) if s.success());
    json!({"installed": true, "signed_in": ok, "path": exe})
}

pub async fn call(method: &str) -> Result<Value, String> {
    match method {
        "github" => Ok(github().await),
        _ => Err(format!("unknown setup method {method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_executables_only_on_the_given_path() {
        let dir = std::env::temp_dir().join(format!("philium-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("tool");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        assert!(on_path("tool", Some(dir.clone().into_os_string())).is_none() || !cfg!(unix));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(on_path("tool", Some(dir.clone().into_os_string())), Some(bin.clone()));
        }
        assert!(on_path("missing", Some(dir.clone().into_os_string())).is_none());
        assert!(on_path("tool", None).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
