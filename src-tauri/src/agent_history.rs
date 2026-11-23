//! Replay HAL's persisted event journal without starting or contacting a runtime.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::time::Duration;

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn events(ctx: &Ctx, raw: &str, after: i64, limit: i64) -> Result<Value, String> {
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
    attachment_at(&path, &session, sha256)
}

/// Scoped read against one HAL database: only blobs referenced by a user
/// message's `$.images` in `session`. Shared with the real-HAL e2e test.
pub(crate) fn attachment_at(path: &Path, session: &str, sha256: &str) -> Result<(String, Vec<u8>), String> {
    with_db(path, |db| {
        let found: Option<(String, i64)> = db
            .query_row(
                "SELECT b.media_type, length(b.data) FROM blobs b WHERE b.session_id=?1 AND b.sha256=?2 AND EXISTS (
                    SELECT 1 FROM messages m, json_each(m.content, '$.images') i
                    WHERE m.session_id=b.session_id AND m.role='user' AND json_extract(i.value, '$.sha256')=b.sha256)",
                [session, sha256],
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
            .query_row("SELECT data FROM blobs WHERE session_id=?1 AND sha256=?2", [session, sha256], |r| r.get(0))
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

/// Full-text search across every agent's saved conversation. Each HAL database
/// is opened strictly read-only (same guard as replay), so no index can be
/// built; matching is a case-insensitive scan of each saved event journal
/// (newest 2000 matching events per agent), covering every agent with history.
pub(crate) fn search(ctx: &Ctx, query: &str, limit: usize) -> Result<Value, String> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).filter(|t| !t.is_empty()).take(8).collect();
    if terms.is_empty() {
        return Ok(json!([]));
    }
    let limit = limit.clamp(1, 200);
    let agents = ctx.db.query(
        "SELECT id,name FROM agents WHERE hal_session_id IS NOT NULL AND hal_session_id<>'' ORDER BY last_active_at DESC",
        [],
    )?;
    let mut hits = Vec::new();
    for a in agents {
        if hits.len() >= limit {
            break;
        }
        let (Some(id), Some(name)) = (a["id"].as_str(), a["name"].as_str()) else { continue };
        // Unreadable or missing history is skipped; search must not fail as a whole.
        let Ok(Some((path, session))) = locate(ctx, id) else { continue };
        if !path.exists() {
            continue;
        }
        let found = with_db(&path, |db| search_db(db, &session, &terms, limit - hits.len()));
        for (seq, role, text, at) in found.unwrap_or_default() {
            hits.push(json!({"agent_id": id, "agent": name, "seq": seq, "role": role, "at": at, "snippet": snippet(&text, &terms[0])}));
        }
    }
    Ok(Value::Array(hits))
}

fn search_db(db: &Connection, session: &str, terms: &[String], limit: usize) -> Result<Vec<(i64, String, String, String)>, String> {
    // The event journal carries `seq`, so a hit can open or fork at that exact message.
    let mut sql = "SELECT seq,payload FROM events WHERE session_id=?1".to_string();
    for i in 0..terms.len() {
        sql.push_str(&format!(" AND instr(lower(payload),?{})>0", i + 2));
    }
    sql.push_str(" ORDER BY seq DESC LIMIT 2000");
    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session];
    for t in terms {
        params.push(t);
    }
    let rows = stmt
        .query_map(params.as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, payload) = row.map_err(|e| e.to_string())?;
        let Ok(v) = serde_json::from_str::<Value>(&payload) else { continue };
        // Match visible conversation text only, not JSON keys, tool output or hashes.
        let (role, text) = match v["type"].as_str() {
            Some("message.created") if v["role"] == "user" => ("user", v["content"].as_str().unwrap_or("")),
            Some("assistant.completed") => ("assistant", v["text"].as_str().unwrap_or("")),
            _ => continue,
        };
        let lower = text.to_lowercase();
        if terms.iter().all(|t| lower.contains(t.as_str())) {
            out.push((seq, role.to_string(), text.to_string(), v["ts"].as_str().unwrap_or("").to_string()));
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

fn snippet(text: &str, term: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    let lower: Vec<char> = flat.to_lowercase().chars().collect();
    let needle: Vec<char> = term.chars().collect();
    let at = if lower.len() == chars.len() {
        lower.windows(needle.len().max(1)).position(|w| w == needle.as_slice()).unwrap_or(0)
    } else { 0 };
    let start = at.saturating_sub(60);
    let end = (at + needle.len() + 100).min(chars.len());
    format!("{}{}{}", if start > 0 { "…" } else { "" }, chars[start..end].iter().collect::<String>(), if end < chars.len() { "…" } else { "" })
}

/// Conversation text up to and including `seq`, for forking from a message.
/// Reads saved events only; nothing after the boundary is included and no tool
/// is replayed. Keeps the newest `max_chars` when the prefix is long.
pub(crate) fn prefix(ctx: &Ctx, raw: &str, seq: i64, max_chars: usize) -> Result<String, String> {
    if seq <= 0 {
        return Err("pick a message to fork from".into());
    }
    let (path, session) = locate(ctx, raw)?.ok_or("agent has no saved conversation to fork")?;
    // A reply's display seq is its first delta; read past it to finish that reply.
    let events = with_db(&path, |db| read_events(db, &session, 0, seq.saturating_add(5000)))?;
    let mut turns: Vec<String> = Vec::new();
    let mut reply = String::new();
    let mut found = false;
    for e in events.as_array().into_iter().flatten() {
        let s = e["seq"].as_i64().unwrap_or(0);
        let kind = e["type"].as_str().unwrap_or("");
        if s > seq && (kind == "message.created" || !found) {
            break;
        }
        found |= s == seq;
        let done = s >= seq && kind == "assistant.completed";
        match kind {
            "message.created" if e["role"] == "user" => {
                if !reply.trim().is_empty() { turns.push(format!("Assistant: {}", reply.trim())); }
                reply.clear();
                turns.push(format!("User: {}", e["content"].as_str().unwrap_or("").trim()));
            }
            "model.delta" => reply.push_str(e["delta"].as_str().unwrap_or("")),
            "assistant.completed" if reply.is_empty() => reply.push_str(e["text"].as_str().unwrap_or("")),
            "assistant.completed" => {}
            _ => {}
        }
        if done {
            break;
        }
    }
    if !reply.trim().is_empty() { turns.push(format!("Assistant: {}", reply.trim())); }
    if !found {
        return Err("that message is not in the saved conversation".into());
    }
    let mut text = turns.join("\n\n");
    let n = text.chars().count();
    if n > max_chars {
        text = format!("…{}", text.chars().skip(n - max_chars).collect::<String>());
    }
    Ok(text)
}

#[cfg(test)]
#[path = "agent_history_tests.rs"]
mod tests;
