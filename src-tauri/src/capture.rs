//! UI-only native helpers for the composer and transcript. Never agent RPC.
use crate::ctx::Ctx;
use std::sync::Arc;
use tauri::ipc::Response;

/// Interactive macOS region/window screenshot. Returns raw PNG bytes, or an
/// empty body when the user cancels. The temporary file lives in the private
/// draft directory and is removed before returning.
#[tauri::command]
pub async fn capture_screenshot() -> Result<Response, String> {
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
        let status = std::process::Command::new("/usr/sbin/screencapture")
            .args(["-i", "-x", "-t", "png"])
            .arg(&path)
            .status()
            .map_err(|_| "Screenshot tool unavailable")?;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err("Cannot read screenshot".into()),
        };
        let _ = std::fs::remove_file(&path);
        if !status.success() && bytes.is_empty() { return Ok(Vec::new()); }
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
