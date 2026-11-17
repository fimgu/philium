//! UI-only native helpers for the composer and transcript. Never agent RPC.
use crate::ctx::Ctx;
use std::sync::Arc;
use tauri::ipc::Response;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

pub const SCREEN_PERMISSION: &str = "Philium needs Screen Recording permission to take screenshots. Turn on Philium in System Settings > Privacy & Security > Screen & System Audio Recording, then quit and reopen Philium. You can still paste, drop or choose images.";

/// macOS only grants capture to the responsible app (Philium), not to the
/// screencapture child. Without it the tool exits as if cancelled, so check
/// first and say what to do instead of failing silently.
fn screen_access() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    unsafe {
        if !CGPreflightScreenCaptureAccess() {
            // First call shows the system prompt and lists Philium in Settings.
            CGRequestScreenCaptureAccess();
            return Err(SCREEN_PERMISSION.into());
        }
    }
    Ok(())
}

/// Interactive macOS region/window screenshot. Returns raw PNG bytes, or an
/// empty body when the user cancels. The temporary file lives in the private
/// draft directory and is removed before returning.
#[tauri::command]
pub async fn capture_screenshot() -> Result<Response, String> {
    screen_access()?;
    let dir = crate::secrets::dir().ok_or("Screenshot storage not initialized")?.join("drafts");
    tauri::async_runtime::spawn_blocking(move || {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&dir).map_err(|_| "Cannot create private screenshot directory")?;
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| "Cannot inspect screenshot directory")?;
        if meta.file_type().is_symlink() || !meta.is_dir() { return Err("Invalid screenshot directory".to_string()); }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| "Cannot secure screenshot directory")?;
        let path = dir.join(format!(".capture-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // -i interactive (drag a region, Space toggles window mode, Esc cancels), -x silent.
        let out = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-i", "-x", "-t", "png"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|_| "Screenshot tool unavailable")?;
        let status = out.status;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err("Cannot read screenshot".into()),
        };
        let _ = std::fs::remove_file(&path);
        if bytes.is_empty() {
            // Esc/cancel leaves no file and no error text. A capture error
            // (e.g. permission revoked mid-session) must not look like a cancel.
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("could not create image") || err.contains("declined") { return Err(SCREEN_PERMISSION.into()); }
            if !status.success() && !err.trim().is_empty() { return Err("Screenshot failed. Try again or paste an image instead.".into()); }
            return Ok(Vec::new());
        }
        if bytes.len() > 64 * 1024 * 1024 { return Err("Screenshot is too large".into()); }
        Ok(bytes)
    })
    .await
    .map_err(|_| "Screenshot worker failed".to_string())?
    .map(Response::new)
}

/// Image the user sent earlier in this agent's own conversation, for display.
#[tauri::command]
pub async fn agent_attachment(app: tauri::State<'_, crate::App>, id: String, sha256: String) -> Result<Response, String> {
    let ctx: Arc<Ctx> = app.ctx.clone();
    tauri::async_runtime::spawn_blocking(move || crate::agents::sent_attachment(&ctx, &id, &sha256))
        .await
        .map_err(|_| "Attachment worker failed".to_string())?
        .map(|(_, bytes)| Response::new(bytes))
}
