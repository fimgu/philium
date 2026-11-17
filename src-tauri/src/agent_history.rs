//! Replay HAL's persisted event journal without starting or contacting a runtime.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::time::Duration;

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn events(ctx: &Ctx, raw: &str, after: i64, limit: i64) -> Result<Value, String> {
    let Some((path, session)) = locate(ctx, raw)? else { return Ok(json!([])) };
    with_db(&path, |db| read_events(db, &session, after, limit)).map_err(|e| {
        format!("Saved conversation unavailable: {e}. Retry, or restore the agent's saved HAL data.")
    })
}

/// Raw bytes of an image the user sent in this agent's own session. The hash
/// must be referenced by a user message of that session, so this cannot be
/// used to read arbitrary blobs or other sessions.
pub(super) fn attachment(ctx: &Ctx, raw: &str, sha256: &str) -> Result<(String, Vec<u8>), String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err("invalid attachment reference".into());
    }
    let (path, session) = locate(ctx, raw)?.ok_or("agent has no saved session")?;
    with_db(&path, |db| {
        let found: Option<(String, i64)> = db
            .query_row(
                "SELECT b.media_type, length(b.data) FROM blobs b WHERE b.session_id=?1 AND b.sha256=?2 AND EXISTS (
                    SELECT 1 FROM messages m, json_each(m.content, '$.images') i
                    WHERE m.session_id=b.session_id AND m.role='user' AND json_extract(i.value, '$.sha256')=b.sha256)",
                [&session, sha256],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (media, size) = found.ok_or("attachment not found in this conversation")?;
        if size <= 0 || size as usize > MAX_ATTACHMENT_BYTES {
            return Err("attachment exceeds safety limit".into());
        }
        if !matches!(media.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
            return Err("attachment type cannot be previewed".into());
        }
        let bytes: Vec<u8> = db
            .query_row("SELECT data FROM blobs WHERE session_id=?1 AND sha256=?2", [&session, sha256], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        Ok((media, bytes))
    })
}

/// Agent's HAL database path and recorded session, refusing symlinked storage.
fn locate(ctx: &Ctx, raw: &str) -> Result<Option<(PathBuf, String)>, String> {
    // Do not use get_row: it intentionally hides SQL errors for metadata callers.
    let rows = ctx.db.query(
        "SELECT id,name,hal_session_id FROM agents WHERE id=?1 OR name=?1",
        [raw],
    )?;
    let row = rows.first().ok_or_else(|| format!("no agent {raw}"))?;
    let name = row["name"]
        .as_str()
        .filter(|n| valid_name(n))
        .ok_or("saved history has invalid agent name")?;
    let Some(session) = row["hal_session_id"].as_str().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let path = ctx.agent_dir(name).join("hal/hal/hal.db");
    // History must stay in this agent's storage, not follow a replaced symlink
    // into another agent or arbitrary user database.
    let mut current = ctx.agent_dir(name);
    for part in ["", "hal", "hal", "hal.db"] {
        if !part.is_empty() {
            current.push(part);
        }
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(
                    "Saved conversation unavailable: symlinked agent history is not allowed".into(),
                )
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("Saved conversation unavailable: {e}")),
        }
    }
    Ok(Some((path, session.to_string())))
}

/// Open HAL's database strictly read-only with bounded execution time.
fn with_db<T>(path: &Path, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
    let sidecar = |suffix: &str| {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        PathBuf::from(p)
    };
    let wal = sidecar("-wal");
    for side in [&wal, &sidecar("-shm"), &sidecar("-journal")] {
        if std::fs::symlink_metadata(side)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err("symlinked history sidecar is not allowed".into());
        }
    }
    if wal.exists() && !sidecar("-shm").exists() {
        return Err(
            "saved WAL history needs its existing shared-memory sidecar; refusing to create one"
                .into(),
        );
    }
    // A checkpointed WAL-mode DB can otherwise cause SQLite to create -wal/-shm
    // even under READ_ONLY. When no WAL exists, immutable reads the base snapshot
    // without creating sidecars. Existing WAL must remain visible for live reads.
    let mut header = [0u8; 20];
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot open saved history ({e})"))?;
    use std::io::Read;
    let _ = file.read(&mut header).map_err(|e| e.to_string())?;
    let immutable = header[18] == 2 && !wal.exists();
    let encoded = path
        .to_str()
        .ok_or("saved history path is not UTF-8")?
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/.-_~:".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect::<String>();
    let uri = format!(
        "file:{encoded}?mode=ro{}",
        if immutable { "&immutable=1" } else { "" }
    );
    // Never initialize/migrate HAL storage or permit SQLite CREATE.
    let db = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("cannot open saved history ({e})"))?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|e| e.to_string())?;
    db.pragma_update(None, "query_only", true)
        .map_err(|e| e.to_string())?;
    db.pragma_update(None, "trusted_schema", false)
        .map_err(|e| e.to_string())?;
    // Bound a corrupt/malicious database's query execution, not just row count.
    let interrupt = db.get_interrupt_handle();
    let (done, wait) = std::sync::mpsc::channel();
    let timer = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(3)).is_err() {
            interrupt.interrupt();
        }
    });
    let result = f(&db);
    let _ = done.send(());
    let _ = timer.join();
    if immutable && wal.exists() {
        return Err("saved history changed while loading; retry to read the current WAL".into());
    }
    result
}

fn read_events(db: &Connection, session: &str, after: i64, limit: i64) -> Result<Value, String> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [session],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("the recorded session is missing from saved history".into());
    }
    let mut stmt = db.prepare("SELECT seq,length(CAST(payload AS BLOB)),payload FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3").map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query(rusqlite::params![
            session,
            after.max(0),
            limit.clamp(1, 10000)
        ])
        .map_err(|e| e.to_string())?;
    let mut bytes = 0usize;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let seq: i64 = row.get(0).map_err(|e| e.to_string())?;
        let size: i64 = row.get(1).map_err(|e| e.to_string())?;
        if size < 0
            || size as usize > MAX_EVENT_BYTES
            || bytes.saturating_add(size as usize) > MAX_PAGE_BYTES
        {
            return Err(
                "saved history page exceeds safety limit; retry with a smaller limit".into(),
            );
        }
        bytes += size as usize;
        let payload: String = row.get(2).map_err(|e| e.to_string())?;
        let event = decode(&payload, session, seq).ok_or_else(|| {
            format!("saved event at sequence {seq} is malformed or has an invalid identity")
        })?;
        out.push(event);
    }
    Ok(Value::Array(out))
}

fn decode(payload: &str, parent: &str, seq: i64) -> Option<Value> {
    let mut value: Value = serde_json::from_str(payload).ok()?;
    let kind = value["type"].as_str()?;
    // Mirror HAL Event::from_persisted's legacy subagent envelope repair. Preserve
    // all other event fields verbatim, including future event variants.
    if value["session_id"].as_str()? != parent {
        if !matches!(kind, "subagent.started" | "subagent.finished")
            || value.get("child_session_id").is_some()
        {
            return None;
        }
        let child = value["session_id"].as_str()?.to_string();
        if child.is_empty() {
            return None;
        }
        value["child_session_id"] = json!(child);
        value["session_id"] = json!(parent);
    }
    if value["seq"].as_i64()? != seq || seq <= 0 || !value["ts"].is_string() {
        return None;
    }
    Some(value)
}

#[cfg(test)]
#[path = "agent_history_tests.rs"]
mod tests;
