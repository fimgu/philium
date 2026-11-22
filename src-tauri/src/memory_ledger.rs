//! Temporal Fact Ledger, Rollback-by-Source, View Rebuild, Cross-Encoder Reranker, and Memory Config.
//!
//! Grounded in Philium's v2 memory blueprint (CALM / LEDGER / MemLeak / MemPoison / AgentMemRec):
//! - Temporal validity windows `[COALESCE(valid_from, created_at), valid_to)` preserve superseded facts
//!   so historical `as_of` queries answer what was true at an earlier time while active retrieval sees
//!   only currently valid facts. No time-decay forgetting.
//! - Rollback-by-source scrubs contaminated derived memories, vectors, working notes, and referenced
//!   raw event log entries while protecting `user_direct` records unless explicitly targeted.
//! - View rebuild verifies FTS5 integrity and purges orphan/non-active vectors without altering
//!   `user_direct` records.
//! - Cross-encoder reranker (`bge-reranker-v2-m3`, `Qwen3-Reranker-0.6B`, etc.) routes remote
//!   providers strictly through Philium's gateway (`/p/<provider>/rerank`) and restricts direct
//!   endpoints strictly to localhost/loopback (`127.0.0.1`, `localhost`, `[::1]`). Never adds or
//!   leaks candidates outside the ownership-gated input set, and degrades cleanly when offline.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use rusqlite::params;
use serde_json::{json, Value};

use crate::ctx::Ctx;

const SCHEMA_ROLLBACKS: &str = "
CREATE TABLE IF NOT EXISTS memory_rollbacks (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  target_source TEXT NOT NULL,
  affected_ids TEXT NOT NULL,
  scrubbed_events INTEGER NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS memory_rollbacks_scope ON memory_rollbacks(scope, created_at);
";

/// Idempotent schema migration for temporal validity windows and rollback audit log.
pub fn init(ctx: &Ctx) -> Result<(), String> {
    let cols: Vec<String> = ctx
        .db
        .query("PRAGMA table_info(memories)", [])?
        .into_iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();

    if !cols.is_empty() {
        if !cols.iter().any(|c| c == "valid_from") {
            ctx.db.exec("ALTER TABLE memories ADD COLUMN valid_from TEXT", [])?;
        }
        if !cols.iter().any(|c| c == "valid_to") {
            ctx.db.exec("ALTER TABLE memories ADD COLUMN valid_to TEXT", [])?;
        }
        ctx.db.exec(
            "UPDATE memories SET valid_from = created_at WHERE valid_from IS NULL",
            [],
        )?;
        ctx.db.exec(
            "CREATE INDEX IF NOT EXISTS memories_validity ON memories(scope, status, valid_from, valid_to)",
            [],
        )?;
    }

    ctx.db.with(|c| c.execute_batch(SCHEMA_ROLLBACKS))?;
    Ok(())
}

// ---------- Temporal Validity Ledger (CALM / LEDGER) ----------

/// SQL predicate for filtering active queries to currently valid memories.
pub fn is_valid_now_sql_clause() -> &'static str {
    "(valid_from IS NULL OR valid_from <= datetime('now')) AND (valid_to IS NULL OR valid_to > datetime('now'))"
}

/// Normalize an ISO-8601 or SQLite datetime string to canonical UTC `YYYY-MM-DD HH:MM:SS`
/// so direct SQLite index comparisons with `datetime('now')` are always exact.
fn normalize_ts(ctx: &Ctx, raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("timestamp cannot be empty".into());
    }
    let normalized = ctx.db.with(|c| {
        c.query_row("SELECT datetime(?1)", [trimmed], |r| r.get::<_, Option<String>>(0))
    })?;
    normalized.ok_or_else(|| format!("invalid timestamp: {trimmed}"))
}

fn fts_terms(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .take(12)
        .map(|t| format!("\"{t}\""))
        .collect()
}

/// Set or update the `[valid_from, valid_to)` validity window for a memory.
///
/// Passing `valid_from = None` preserves the memory's existing `COALESCE(valid_from, created_at)`.
/// Passing `valid_to = None` leaves the interval open-ended (`NULL`).
pub fn set_validity(
    ctx: &Ctx,
    memory_id: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> Result<(), String> {
    let mid = memory_id.trim();
    if mid.is_empty() {
        return Err("memory_id cannot be empty".into());
    }
    let existing = ctx.db.query(
        "SELECT COALESCE(valid_from, created_at) AS eff_from FROM memories WHERE id=?1",
        [mid],
    )?;
    let row = existing.first().ok_or_else(|| format!("memory not found: {mid}"))?;
    let existing_from = row["eff_from"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);

    let norm_from = match valid_from.map(str::trim).filter(|s| !s.is_empty()) {
        Some(ts) => Some(normalize_ts(ctx, ts)?),
        None => match existing_from {
            Some(ref ef) => Some(normalize_ts(ctx, ef).unwrap_or_else(|_| ef.clone())),
            None => None,
        },
    };
    let norm_to = match valid_to.map(str::trim).filter(|s| !s.is_empty()) {
        Some(ts) => Some(normalize_ts(ctx, ts)?),
        None => None,
    };

    if let (Some(ref vf), Some(ref vt)) = (&norm_from, &norm_to) {
        if vt < vf {
            return Err("valid_to cannot be earlier than valid_from".into());
        }
    }

    ctx.db.exec(
        "UPDATE memories SET valid_from=?2, valid_to=?3, updated_at=datetime('now') WHERE id=?1",
        params![mid, norm_from, norm_to],
    )?;
    ctx.notify("library");
    Ok(())
}

/// Close validity windows on superseded memory rows (`valid_to = at_ts`) so historical
/// `query_as_of` calls accurately reflect what was valid prior to supersession.
pub fn close_superseded_validity(
    ctx: &Ctx,
    superseded_ids: &[String],
    at_ts: &str,
) -> Result<(), String> {
    if superseded_ids.is_empty() {
        return Ok(());
    }
    let ts = if at_ts.trim().is_empty() {
        ctx.db.with(|c| c.query_row("SELECT datetime('now')", [], |r| r.get::<_, String>(0)))?
    } else {
        normalize_ts(ctx, at_ts)?
    };

    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for id in superseded_ids {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                continue;
            }
            tx.execute(
                "UPDATE memories
                 SET valid_to = CASE
                     WHEN valid_to IS NULL OR COALESCE(datetime(valid_to), valid_to) > ?2 THEN ?2
                     ELSE valid_to
                   END,
                   updated_at = datetime('now')
                 WHERE id = ?1",
                params![trimmed, ts],
            )?;
        }
        tx.commit()
    })?;
    ctx.notify("library");
    Ok(())
}

/// Query memories whose validity window `[COALESCE(valid_from, created_at), valid_to)` covers `at_ts`.
///
/// Includes `active` and `superseded` rows (excluding `deleted`, `quarantined`, and `proposed`),
/// strictly ownership-filtered both in SQL before matching and post-retrieval in Rust (MemLeak).
/// Uses pure FTS5 BM25 ranking when `q` contains searchable terms — zero provenance weighting.
pub fn query_as_of(
    ctx: &Ctx,
    scope: &str,
    include_global: bool,
    at_ts: &str,
    q: &str,
    limit: i64,
) -> Result<Value, String> {
    let ts = normalize_ts(ctx, at_ts)?;
    let limit = limit.clamp(1, 200);
    let terms = fts_terms(q);

    let rows = if terms.is_empty() {
        let raw = ctx.db.query(
            "SELECT m.id, m.scope, m.category, m.content, m.source, m.created_at, m.active,
                    m.trust_tier, m.status, m.fact_key, m.supersedes_id, m.source_event_id,
                    m.provenance, m.updated_at, m.valid_from, m.valid_to, m.title, m.files
             FROM memories m
             WHERE m.status IN ('active', 'superseded')
               AND ((?2 AND m.scope = 'global') OR m.scope = ?1)
               AND COALESCE(datetime(COALESCE(m.valid_from, m.created_at)), COALESCE(m.valid_from, m.created_at)) <= ?3
               AND (m.valid_to IS NULL OR COALESCE(datetime(m.valid_to), m.valid_to) > ?3)
             ORDER BY (m.scope = ?1) DESC,
                      COALESCE(datetime(COALESCE(m.valid_from, m.created_at)), COALESCE(m.valid_from, m.created_at)) DESC,
                      m.id DESC
             LIMIT ?4",
            params![scope, include_global, ts, limit],
        )?;
        let q_trim = q.trim().to_lowercase();
        if q_trim.is_empty() {
            raw
        } else {
            raw.into_iter()
                .filter(|r| {
                    let content = r["content"].as_str().unwrap_or("").to_lowercase();
                    let fk = r["fact_key"].as_str().unwrap_or("").to_lowercase();
                    content.contains(&q_trim) || fk.contains(&q_trim)
                })
                .collect()
        }
    } else {
        ctx.db.query(
            "SELECT m.id, m.scope, m.category, m.content, m.source, m.created_at, m.active,
                    m.trust_tier, m.status, m.fact_key, m.supersedes_id, m.source_event_id,
                    m.provenance, m.updated_at, m.valid_from, m.valid_to, m.title, m.files
             FROM memories_fts f
             JOIN memories m ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1
               AND m.status IN ('active', 'superseded')
               AND ((?3 AND m.scope = 'global') OR m.scope = ?2)
               AND COALESCE(datetime(COALESCE(m.valid_from, m.created_at)), COALESCE(m.valid_from, m.created_at)) <= ?4
               AND (m.valid_to IS NULL OR COALESCE(datetime(m.valid_to), m.valid_to) > ?4)
             ORDER BY bm25(memories_fts)
             LIMIT ?5",
            params![terms.join(" OR "), scope, include_global, ts, limit],
        )?
    };

    // Hard post-retrieval ownership & status gate (defense in depth per MemLeak).
    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|r| {
            let st = r["status"].as_str().unwrap_or("");
            let sc = r["scope"].as_str().unwrap_or("");
            matches!(st, "active" | "superseded")
                && (sc == scope || (include_global && sc == "global"))
        })
        .collect();

    Ok(Value::Array(filtered))
}

// ---------- Rollback by Source & View Rebuild (Report §2 & §4 v2#3) ----------

fn event_body_contains(ctx: &Ctx, event_ref: Option<&str>, needle_low: &str) -> bool {
    let Some(rowid) = event_ref
        .and_then(|s| s.strip_prefix("evt:"))
        .and_then(|s| s.parse::<i64>().ok())
    else {
        return false;
    };
    ctx.db
        .query("SELECT body FROM events WHERE id=?1", [rowid])
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|r| r["body"].as_str().map(|b| b.to_lowercase().contains(needle_low)))
        .unwrap_or(false)
}

fn matches_source_filter(source: &str, provenance: &str, sm_low: &str, exact_user: bool) -> bool {
    if exact_user {
        source.eq_ignore_ascii_case("user")
    } else {
        let src_low = source.to_lowercase();
        let prov_low = provenance.to_lowercase();
        src_low == sm_low || src_low.contains(sm_low) || prov_low.contains(sm_low)
    }
}

/// Roll back all derived memories and working notes linked to a contaminated source, provenance, or
/// session id (optionally scoped). Refuses to touch `user_direct` records unless explicitly targeted
/// with `source_match = Some("user")`.
///
/// When `!dry_run`: marks matched memories `deleted` with `content = ''`, removes their
/// `memory_vectors`, scrubs referenced `events` (`{"scrubbed":true}`), clears matched
/// `working_notes`, and writes a `memory_rollbacks` audit record.
pub fn rollback_source(
    ctx: &Ctx,
    scope: Option<&str>,
    source_match: Option<&str>,
    session_id: Option<&str>,
    dry_run: bool,
) -> Result<Value, String> {
    let scope_eff = scope.map(str::trim).filter(|s| !s.is_empty());
    let source_eff = source_match.map(str::trim).filter(|s| !s.is_empty());
    let session_eff = session_id.map(str::trim).filter(|s| !s.is_empty());

    if source_eff.is_none() && session_eff.is_none() {
        return Err("provide `source` or `session_id` to roll back".into());
    }

    let allow_user_direct = source_eff.is_some_and(|s| s.eq_ignore_ascii_case("user"));
    let sm_low = source_eff.map(str::to_lowercase);
    let sid_low = session_eff.map(str::to_lowercase);

    // 1. Find matching non-deleted memories.
    let mem_rows = ctx.db.query(
        "SELECT id, scope, source, trust_tier, provenance, source_event_id
         FROM memories
         WHERE status != 'deleted' AND (?1 IS NULL OR scope = ?1)
         ORDER BY created_at DESC, id ASC",
        params![scope_eff],
    )?;

    let mut matched_mem_ids: Vec<String> = Vec::new();
    let mut ref_event_ids: HashSet<i64> = HashSet::new();

    for r in mem_rows {
        let Some(id) = r["id"].as_str() else { continue };
        let tier = r["trust_tier"].as_str().unwrap_or("agent_inferred");
        let src = r["source"].as_str().unwrap_or("");
        let prov = r["provenance"].as_str().unwrap_or("");
        let evt_ref = r["source_event_id"].as_str();

        // Safety invariant: never touch user_direct unless explicitly matched by source='user'.
        if tier == "user_direct" && !(allow_user_direct && src.eq_ignore_ascii_case("user")) {
            continue;
        }

        let source_ok = match &sm_low {
            Some(sm) => matches_source_filter(src, prov, sm, allow_user_direct),
            None => true,
        };
        if !source_ok {
            continue;
        }

        let session_ok = match &sid_low {
            Some(sid) => {
                prov.to_lowercase().contains(sid)
                    || src.to_lowercase().contains(sid)
                    || event_body_contains(ctx, evt_ref, sid)
            }
            None => true,
        };
        if !session_ok {
            continue;
        }

        matched_mem_ids.push(id.to_string());
        if let Some(rowid) = evt_ref
            .and_then(|s| s.strip_prefix("evt:"))
            .and_then(|s| s.parse::<i64>().ok())
        {
            ref_event_ids.insert(rowid);
        }
    }

    // 2. Find matching non-empty working_notes.
    let wn_rows = ctx.db.query(
        "SELECT scope, requirements, decisions, failed_attempts, next_steps, source_event_id, updated_by
         FROM working_notes
         WHERE (?1 IS NULL OR scope = ?1)",
        params![scope_eff],
    )?;

    let mut matched_wn_scopes: Vec<String> = Vec::new();
    for r in wn_rows {
        let Some(wn_scope) = r["scope"].as_str() else { continue };
        let non_empty = ["requirements", "decisions", "failed_attempts", "next_steps"]
            .iter()
            .any(|k| !r[*k].as_str().unwrap_or("").trim().is_empty());
        if !non_empty {
            continue;
        }
        let updated_by = r["updated_by"].as_str().unwrap_or("");
        let evt_ref = r["source_event_id"].as_str();

        // Protect operator-authored working notes unless explicitly rolling back source='user'.
        if updated_by.eq_ignore_ascii_case("user") && !allow_user_direct {
            continue;
        }

        let source_ok = match &sm_low {
            Some(sm) => {
                if allow_user_direct {
                    updated_by.eq_ignore_ascii_case("user")
                } else {
                    let ub_low = updated_by.to_lowercase();
                    ub_low == *sm || ub_low.contains(sm)
                }
            }
            None => true,
        };
        if !source_ok {
            continue;
        }

        let session_ok = match &sid_low {
            Some(sid) => {
                updated_by.to_lowercase().contains(sid) || event_body_contains(ctx, evt_ref, sid)
            }
            None => true,
        };
        if !session_ok {
            continue;
        }

        matched_wn_scopes.push(wn_scope.to_string());
        if let Some(rowid) = evt_ref
            .and_then(|s| s.strip_prefix("evt:"))
            .and_then(|s| s.parse::<i64>().ok())
        {
            ref_event_ids.insert(rowid);
        }
    }

    let mut affected_ids: Vec<String> = matched_mem_ids.clone();
    for wn_scope in &matched_wn_scopes {
        affected_ids.push(format!("working_note:{wn_scope}"));
    }
    let matched = affected_ids.len();

    if dry_run {
        return Ok(json!({
            "matched": matched,
            "rolled_back": 0,
            "scrubbed_events": 0,
            "affected_ids": affected_ids,
            "dry_run": true,
        }));
    }

    let rollback_id = format!("rb_{}", crate::db::new_id());
    let target_label = match (source_eff, session_eff) {
        (Some(s), Some(sid)) => format!("{s} (session:{sid})"),
        (Some(s), None) => s.to_string(),
        (None, Some(sid)) => format!("session:{sid}"),
        (None, None) => unreachable!(),
    };
    let scrub_payload = json!({ "scrubbed": true, "rollback_id": rollback_id }).to_string();

    let (rolled_back, scrubbed_events) = ctx.db.with(|c| -> rusqlite::Result<(usize, usize)> {
        let tx = c.unchecked_transaction()?;
        let mut rolled = 0usize;
        let mut scrubbed = 0usize;

        for mid in &matched_mem_ids {
            let n = tx.execute(
                "UPDATE memories
                 SET active = 0, status = 'deleted', content = '', provenance = 'rolled_back',
                     source_event_id = NULL, updated_at = datetime('now')
                 WHERE id = ?1",
                [mid],
            )?;
            rolled += n;
            let _ = tx.execute("DELETE FROM memory_vectors WHERE memory_id = ?1", [mid]);

            let like_pat = format!("%{mid}%");
            scrubbed += tx.execute(
                "UPDATE events SET body = ?2
                 WHERE kind IN ('memory:write','memory:extract','memory:session_observe')
                   AND body LIKE ?1
                   AND body NOT LIKE '%\"scrubbed\":true%'",
                params![like_pat, scrub_payload],
            )?;
        }

        for wn_scope in &matched_wn_scopes {
            let n = tx.execute(
                "UPDATE working_notes
                 SET requirements = '', decisions = '', failed_attempts = '', next_steps = '',
                     source_event_id = NULL, updated_by = 'rollback', updated_at = datetime('now')
                 WHERE scope = ?1",
                [wn_scope],
            )?;
            rolled += n;
        }

        for rowid in &ref_event_ids {
            scrubbed += tx.execute(
                "UPDATE events SET body = ?2
                 WHERE id = ?1 AND body NOT LIKE '%\"scrubbed\":true%'",
                params![rowid, scrub_payload],
            )?;
        }

        if let Some(sid) = session_eff {
            let sid_pat = format!("%{sid}%");
            scrubbed += tx.execute(
                "UPDATE events SET body = ?2
                 WHERE kind IN ('memory:write','memory:extract','memory:session_observe','working_note:write')
                   AND body LIKE ?1
                   AND body NOT LIKE '%\"scrubbed\":true%'",
                params![sid_pat, scrub_payload],
            )?;
        }

        tx.execute(
            "INSERT INTO memory_rollbacks(id, scope, target_source, affected_ids, scrubbed_events, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![
                rollback_id,
                scope_eff.unwrap_or("all"),
                target_label,
                json!(affected_ids).to_string(),
                scrubbed as i64,
            ],
        )?;

        tx.commit()?;
        Ok((rolled, scrubbed))
    })?;

    ctx.notify("library");
    Ok(json!({
        "id": rollback_id,
        "matched": matched,
        "rolled_back": rolled_back,
        "scrubbed_events": scrubbed_events,
        "affected_ids": affected_ids,
        "dry_run": false,
    }))
}

/// Verify and rebuild FTS5 derived views, purge orphan/deleted/superseded vectors, and confirm
/// all non-deleted `user_direct` memories remain preserved intact.
pub fn rebuild_views(ctx: &Ctx, scope: Option<&str>) -> Result<Value, String> {
    let scope_eff = scope.map(str::trim).filter(|s| !s.is_empty());

    let (rebuilt_memories, removed_orphan_vectors, preserved_user_direct) =
        ctx.db.with(|c| -> rusqlite::Result<(i64, usize, i64)> {
            let tx = c.unchecked_transaction()?;

            tx.execute("INSERT INTO memories_fts(memories_fts) VALUES('rebuild')", [])?;
            tx.execute(
                "INSERT INTO memories_fts(memories_fts) VALUES('integrity-check')",
                [],
            )?;

            let removed = match scope_eff {
                Some(sc) => tx.execute(
                    "DELETE FROM memory_vectors
                     WHERE memory_id NOT IN (SELECT id FROM memories)
                        OR memory_id IN (
                             SELECT id FROM memories
                             WHERE scope = ?1 AND (active = 0 OR status != 'active')
                           )",
                    [sc],
                )?,
                None => tx.execute(
                    "DELETE FROM memory_vectors
                     WHERE memory_id NOT IN (
                         SELECT id FROM memories WHERE active = 1 AND status = 'active'
                     )",
                    [],
                )?,
            };

            let rebuilt: i64 = tx.query_row(
                "SELECT count(*) FROM memories
                 WHERE active = 1 AND status = 'active' AND (?1 IS NULL OR scope = ?1)",
                params![scope_eff],
                |r| r.get(0),
            )?;

            let preserved: i64 = tx.query_row(
                "SELECT count(*) FROM memories
                 WHERE trust_tier = 'user_direct' AND status != 'deleted' AND (?1 IS NULL OR scope = ?1)",
                params![scope_eff],
                |r| r.get(0),
            )?;

            tx.commit()?;
            Ok((rebuilt, removed, preserved))
        })?;

    ctx.notify("library");
    Ok(json!({
        "rebuilt_memories": rebuilt_memories,
        "removed_orphan_vectors": removed_orphan_vectors,
        "preserved_user_direct": preserved_user_direct,
    }))
}

// ---------- Cross-Encoder Reranker (v2#4) + Memory Config ----------

#[derive(Debug, Clone)]
pub struct RerankConfig {
    pub endpoint_url: String,
    pub auth_token: Option<String>,
    pub model: String,
    pub model_version: String,
    /// Set for the in-process cross-encoder: its data dir. HTTP fields are unused then.
    pub local_dir: Option<std::path::PathBuf>,
}

fn rerank_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

fn is_loopback_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn is_rerank_model_id(id: &str) -> bool {
    id.to_ascii_lowercase().contains("rerank")
}

fn rerank_model_priority(id: &str) -> u8 {
    let l = id.to_ascii_lowercase();
    if l.contains("bge-reranker-v2-m3") {
        0
    } else if l.contains("qwen3-reranker") {
        1
    } else if l.contains("bge-reranker") {
        2
    } else {
        3
    }
}

/// Resolve an active cross-encoder reranker backend:
/// 1. Explicit `setting.rerank_provider` + `setting.rerank_model` (trusted gateway `/p/<id>/rerank`)
/// 2. Explicit `setting.rerank_endpoint` + `setting.rerank_model` (strictly loopback-only)
/// 3. Auto-discover any enabled trusted provider model containing `rerank`
///    (`bge-reranker-v2-m3` > `qwen3-reranker` > `rerank`).
pub fn resolve_rerank_config(ctx: &Ctx) -> Option<RerankConfig> {
    let explicit_provider = ctx.db.kv_get("setting.rerank_provider").unwrap_or_default();
    let explicit_model = ctx.db.kv_get("setting.rerank_model").unwrap_or_default();
    if !explicit_provider.trim().is_empty() && !explicit_model.trim().is_empty() {
        let pid = explicit_provider.trim();
        let mid = explicit_model.trim();
        if let Some(p) = crate::providers::get(ctx, pid) {
            if p.enabled && p.trusted {
                return Some(RerankConfig {
                    endpoint_url: format!(
                        "{}/p/{}/rerank",
                        ctx.gateway_url.trim_end_matches('/'),
                        p.id
                    ),
                    auth_token: Some(ctx.gateway.token.clone()),
                    model: mid.to_string(),
                    model_version: format!("gateway:{}:{mid}", p.id),
                    local_dir: None,
                });
            }
        }
    }

    let explicit_endpoint = ctx.db.kv_get("setting.rerank_endpoint").unwrap_or_default();
    if !explicit_endpoint.trim().is_empty() && !explicit_model.trim().is_empty() {
        let ep = explicit_endpoint.trim();
        if is_loopback_url(ep) {
            let mid = explicit_model.trim();
            let auth = ctx
                .db
                .kv_get("setting.rerank_api_key")
                .filter(|s| !s.trim().is_empty());
            return Some(RerankConfig {
                endpoint_url: ep.to_string(),
                auth_token: auth,
                model: mid.to_string(),
                model_version: format!("local:{mid}"),
                local_dir: None,
            });
        }
    }

    if crate::local_models::ready(ctx, &crate::local_models::RERANK) {
        return Some(RerankConfig {
            endpoint_url: String::new(),
            auth_token: None,
            model: crate::local_models::RERANK.label.to_string(),
            model_version: crate::local_models::RERANK.version.to_string(),
            local_dir: Some(ctx.data_dir.clone()),
        });
    }

    let providers = crate::providers::all(ctx);
    let mut best: Option<(u8, String, String)> = None;
    for p in providers {
        if !p.enabled
            || !p.trusted
            || !matches!(
                p.kind,
                crate::providers::Kind::OpenaiChat | crate::providers::Kind::OpenaiResponses
            )
        {
            continue;
        }
        let rows = ctx
            .db
            .query(
                "SELECT model_id FROM models WHERE provider_id=?1 AND enabled=1",
                [&p.id],
            )
            .unwrap_or_default();
        for r in rows {
            let Some(mid) = r["model_id"].as_str() else { continue };
            if !is_rerank_model_id(mid) {
                continue;
            }
            let prio = rerank_model_priority(mid);
            let better = match &best {
                None => true,
                Some((bp, bpid, bmid)) => {
                    (prio, p.id.as_str(), mid) < (*bp, bpid.as_str(), bmid.as_str())
                }
            };
            if better {
                best = Some((prio, p.id.clone(), mid.to_string()));
            }
        }
    }
    best.map(|(_, pid, mid)| RerankConfig {
        endpoint_url: format!("{}/p/{}/rerank", ctx.gateway_url.trim_end_matches('/'), pid),
        auth_token: Some(ctx.gateway.token.clone()),
        model: mid.clone(),
        model_version: format!("gateway:{pid}:{mid}"),
        local_dir: None,
    })
}

fn parse_rerank_entries(body: &Value) -> Option<Vec<(usize, f64)>> {
    let arr = body
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(idx) = item.get("index").and_then(Value::as_u64).map(|n| n as usize) else {
            continue;
        };
        let score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .or_else(|| item.get("score").and_then(Value::as_f64));
        let Some(s) = score.filter(|v| v.is_finite()) else {
            continue;
        };
        out.push((idx, s));
    }
    Some(out)
}

/// Rerank ownership-gated memory candidates with an OpenAI/Cohere/Jina/TEI-compatible `/rerank`
/// endpoint (`{ model, query, documents, top_n }`).
///
/// Preserves the exact ownership gate (never introduces or duplicates items outside `candidates`),
/// tags each reranked item with `rerank_score`, and falls back cleanly to input order returning
/// `None` for the active reranker if unconfigured, offline, or the endpoint returns invalid data.
pub async fn rerank_candidates_async(
    ctx: &Ctx,
    query: &str,
    candidates: Vec<Value>,
    top_n: usize,
) -> (Vec<Value>, Option<String>) {
    if candidates.is_empty() || top_n == 0 {
        return (Vec::new(), None);
    }
    if query.trim().is_empty() {
        return (candidates.into_iter().take(top_n).collect(), None);
    }
    let Some(cfg) = resolve_rerank_config(ctx) else {
        return (candidates.into_iter().take(top_n).collect(), None);
    };

    let req_top_n = top_n.min(candidates.len());
    let documents: Vec<String> = candidates
        .iter()
        .map(|c| c["content"].as_str().unwrap_or("").to_string())
        .collect();

    if let Some(dir) = cfg.local_dir.clone() {
        let q = query.to_string();
        let scored = tokio::task::spawn_blocking(move || crate::local_models::rerank(&dir, &q, &documents)).await;
        return match scored {
            Ok(Ok(entries)) => place_reranked(candidates, entries, req_top_n, top_n, cfg.model_version),
            _ => (candidates.into_iter().take(top_n).collect(), None),
        };
    }

    let mut req = rerank_http_client().post(&cfg.endpoint_url).json(&json!({
        "model": cfg.model,
        "query": query,
        "documents": documents,
        "top_n": req_top_n,
    }));
    if let Some(ref tok) = cfg.auth_token {
        req = req.bearer_auth(tok);
    }

    let resp = match req.send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return (candidates.into_iter().take(top_n).collect(), None),
    };
    let Ok(body) = resp.json::<Value>().await else {
        return (candidates.into_iter().take(top_n).collect(), None);
    };
    let Some(entries) = parse_rerank_entries(&body) else {
        return (candidates.into_iter().take(top_n).collect(), None);
    };
    place_reranked(candidates, entries, req_top_n, top_n, cfg.model_version)
}

/// Order `candidates` by `(index, score)` entries. Only existing slots are used (ownership gate).
fn place_reranked(
    candidates: Vec<Value>,
    mut entries: Vec<(usize, f64)>,
    req_top_n: usize,
    top_n: usize,
    model_version: String,
) -> (Vec<Value>, Option<String>) {
    entries.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    // Strict ownership & deduplication gate: only pull existing slots from `candidates`.
    let mut slots: Vec<Option<Value>> = candidates.into_iter().map(Some).collect();
    let mut reranked: Vec<Value> = Vec::with_capacity(req_top_n);
    for (idx, score) in entries {
        if reranked.len() >= req_top_n {
            break;
        }
        if idx >= slots.len() {
            continue;
        }
        if let Some(mut item) = slots[idx].take() {
            let rounded = (score * 10_000.0).round() / 10_000.0;
            item["rerank_score"] = json!(rounded);
            reranked.push(item);
        }
    }

    if reranked.is_empty() {
        let fallback: Vec<Value> = slots.into_iter().flatten().take(top_n).collect();
        return (fallback, None);
    }

    (reranked, Some(model_version))
}

/// Read current embedding & cross-encoder reranker configuration and active backend status.
pub fn config_get(ctx: &Ctx) -> Value {
    let active_embed = crate::library::resolve_embedding_config(ctx).map(|c| c.model_version);
    let active_rerank = resolve_rerank_config(ctx).map(|c| c.model_version);
    let mode = if active_embed.is_some() {
        "hybrid_bm25_dense_rrf"
    } else {
        "bm25_only"
    };
    json!({
        "embedding_provider": ctx.db.kv_get("setting.embedding_provider").unwrap_or_default(),
        "embedding_endpoint": ctx.db.kv_get("setting.embedding_endpoint").unwrap_or_default(),
        "embedding_model": ctx.db.kv_get("setting.embedding_model").unwrap_or_default(),
        "active_embed_model": active_embed,
        "rerank_provider": ctx.db.kv_get("setting.rerank_provider").unwrap_or_default(),
        "rerank_endpoint": ctx.db.kv_get("setting.rerank_endpoint").unwrap_or_default(),
        "rerank_model": ctx.db.kv_get("setting.rerank_model").unwrap_or_default(),
        "active_rerank_model": active_rerank,
        "mode": mode,
        "reranker_active": active_rerank.is_some(),
        "local_models": crate::local_models::status(ctx),
    })
}

fn validate_trusted_provider(ctx: &Ctx, field: &str, pid: &str) -> Result<(), String> {
    let p = crate::providers::get(ctx, pid)
        .ok_or_else(|| format!("unknown {field}: {pid}"))?;
    if !p.enabled {
        return Err(format!("{field} `{pid}` is disabled"));
    }
    if !p.trusted {
        return Err(format!(
            "{field} `{pid}` must be trusted (untrusted providers cannot handle personal memory)"
        ));
    }
    Ok(())
}

fn validate_model_setting(field: &str, model: &str) -> Result<(), String> {
    if model.len() > 200 || model.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("invalid {field}: {model:?}"));
    }
    Ok(())
}

/// Validate and persist embedding and cross-encoder reranker settings.
///
/// Enforces blueprint invariants:
/// - Direct endpoints (`embedding_endpoint`, `rerank_endpoint`) MUST be localhost/loopback-only
///   (`127.0.0.1`, `localhost`, `[::1]`).
/// - Remote providers (`embedding_provider`, `rerank_provider`) MUST be enabled, trusted
///   providers routed through Philium's gateway.
pub fn config_set(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let obj = args
        .as_object()
        .ok_or_else(|| "config_set expects a JSON object".to_string())?;

    let mut updates: Vec<(&'static str, String)> = Vec::new();

    if let Some(v) = obj.get("embedding_provider") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() {
            validate_trusted_provider(ctx, "embedding_provider", val)?;
        }
        updates.push(("setting.embedding_provider", val.to_string()));
    }
    if let Some(v) = obj.get("embedding_endpoint") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() && !is_loopback_url(val) {
            return Err(
                "direct embedding_endpoint must be localhost/loopback (127.0.0.1, localhost, or [::1]); route remote providers via embedding_provider"
                    .into(),
            );
        }
        updates.push(("setting.embedding_endpoint", val.to_string()));
    }
    if let Some(v) = obj.get("embedding_model") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() {
            validate_model_setting("embedding_model", val)?;
        }
        updates.push(("setting.embedding_model", val.to_string()));
    }
    if let Some(v) = obj.get("embedding_api_key") {
        let val = v.as_str().unwrap_or("").trim();
        updates.push(("setting.embedding_api_key", val.to_string()));
    }

    if let Some(v) = obj.get("rerank_provider") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() {
            validate_trusted_provider(ctx, "rerank_provider", val)?;
        }
        updates.push(("setting.rerank_provider", val.to_string()));
    }
    if let Some(v) = obj.get("rerank_endpoint") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() && !is_loopback_url(val) {
            return Err(
                "direct rerank_endpoint must be localhost/loopback (127.0.0.1, localhost, or [::1]); route remote providers via rerank_provider"
                    .into(),
            );
        }
        updates.push(("setting.rerank_endpoint", val.to_string()));
    }
    if let Some(v) = obj.get("rerank_model") {
        let val = v.as_str().unwrap_or("").trim();
        if !val.is_empty() {
            validate_model_setting("rerank_model", val)?;
        }
        updates.push(("setting.rerank_model", val.to_string()));
    }
    if let Some(v) = obj.get("rerank_api_key") {
        let val = v.as_str().unwrap_or("").trim();
        updates.push(("setting.rerank_api_key", val.to_string()));
    }

    for (k, val) in updates {
        ctx.db.kv_set(k, &val)?;
    }

    ctx.notify("library");
    Ok(config_get(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{self, SaveOpts};
    use axum::{routing::post, Json, Router};
    use std::sync::Arc;

    fn test_ctx() -> Arc<Ctx> {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        library::init(&ctx).unwrap();
        init(&ctx).unwrap();
        ctx
    }

    #[test]
    fn init_is_idempotent_and_backfills_valid_from() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        library::init(&ctx).unwrap();

        // Insert a legacy row before memory_ledger::init runs.
        let saved = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Legacy fact before temporal migration",
                category: "fact",
                scope: "repo:r1",
                source: "user",
                trust_tier: "user_direct",
                fact_key: Some("legacy.key"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        let id = saved["id"].as_str().unwrap();

        init(&ctx).unwrap();
        init(&ctx).unwrap(); // Idempotent second call.

        let rows = ctx
            .db
            .query(
                "SELECT created_at, valid_from, valid_to FROM memories WHERE id=?1",
                [id],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["valid_from"], rows[0]["created_at"]);
        assert!(rows[0]["valid_to"].is_null());

        let idx = ctx
            .db
            .query(
                "SELECT name FROM sqlite_master WHERE type='index' AND name='memories_validity'",
                [],
            )
            .unwrap();
        assert_eq!(idx.len(), 1);

        let tbl = ctx
            .db
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='memory_rollbacks'",
                [],
            )
            .unwrap();
        assert_eq!(tbl.len(), 1);
    }

    #[test]
    fn monday_vs_thursday_query_as_of_and_valid_now_clause() {
        let ctx = test_ctx();

        // Monday: deploy via staging branch.
        let mon = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Monday rule: deploy releases via staging branch",
                category: "preference",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: Some("deploy.branch"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        let mon_id = mon["id"].as_str().unwrap().to_string();
        set_validity(&ctx, &mon_id, Some("2026-11-02T09:00:00Z"), None).unwrap();

        // Thursday: superseded by direct release tag workflow.
        let thu = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Thursday rule: deploy releases via signed release tags",
                category: "preference",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: Some("deploy.branch"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        let thu_id = thu["id"].as_str().unwrap().to_string();
        close_superseded_validity(&ctx, &[mon_id.clone()], "2026-11-05T14:00:00Z").unwrap();
        set_validity(&ctx, &thu_id, Some("2026-11-05T14:00:00Z"), None).unwrap();

        // Global memory active across the whole week.
        let glob = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Global rule: never force push release branches",
                category: "preference",
                scope: "global",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        set_validity(&ctx, glob["id"].as_str().unwrap(), Some("2026-11-01T00:00:00Z"), None).unwrap();

        // Sibling repo canary, quarantined record, and deleted record must NEVER appear.
        let sib = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Sibling repo canary: deploy releases via secret sibling path",
                category: "fact",
                scope: "repo:rb",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        set_validity(&ctx, sib["id"].as_str().unwrap(), Some("2026-11-01T00:00:00Z"), None).unwrap();

        let quar = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Quarantined untrusted deploy rule",
                category: "fact",
                scope: "repo:ra",
                source: "tool:web",
                trust_tier: "untrusted",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("web:untrusted"),
            },
        )
        .unwrap();
        set_validity(&ctx, quar["id"].as_str().unwrap(), Some("2026-11-01T00:00:00Z"), None).unwrap();

        let del = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Deleted deploy canary",
                category: "fact",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        set_validity(&ctx, del["id"].as_str().unwrap(), Some("2026-11-01T00:00:00Z"), None).unwrap();
        library::forget_memory(&ctx, del["id"].as_str().unwrap()).unwrap();

        // Tuesday as_of -> sees Monday superseded rule, NOT Thursday, NOT sibling, NOT quarantined/deleted.
        let tue = query_as_of(&ctx, "repo:ra", false, "2026-11-03T12:00:00Z", "deploy", 10).unwrap();
        let tue_arr = tue.as_array().unwrap();
        assert_eq!(tue_arr.len(), 1, "Tuesday hits: {tue}");
        assert_eq!(tue_arr[0]["id"], mon_id);
        assert_eq!(tue_arr[0]["status"], "superseded");

        // Exact Thursday supersession instant -> half-open [valid_from, valid_to) switches cleanly to Thursday.
        let exact_thu = query_as_of(&ctx, "repo:ra", false, "2026-11-05T14:00:00Z", "deploy", 10).unwrap();
        let exact_arr = exact_thu.as_array().unwrap();
        assert_eq!(exact_arr.len(), 1, "Exact Thursday hits: {exact_thu}");
        assert_eq!(exact_arr[0]["id"], thu_id);
        assert_eq!(exact_arr[0]["status"], "active");

        // Friday with include_global=true -> sees Thursday repo rule + global rule, never sibling.
        let fri = query_as_of(&ctx, "repo:ra", true, "2026-11-06T12:00:00Z", "", 10).unwrap();
        let fri_arr = fri.as_array().unwrap();
        assert_eq!(fri_arr.len(), 2, "Friday hits with global: {fri}");
        assert!(fri_arr.iter().any(|r| r["id"] == thu_id));
        assert!(fri_arr.iter().any(|r| r["scope"] == "global"));
        assert!(!fri.to_string().contains("Sibling repo canary"));

        // Sunday before Monday -> no repo:ra rules valid yet.
        let sun = query_as_of(&ctx, "repo:ra", false, "2026-11-01T12:00:00Z", "", 10).unwrap();
        assert!(sun.as_array().unwrap().is_empty(), "Sunday should be empty: {sun}");

        // Invalid range rejected.
        assert!(set_validity(&ctx, &thu_id, Some("2026-11-10"), Some("2026-11-09")).is_err());

        // `is_valid_now_sql_clause()` filters future-dated and expired active memories accurately.
        let future = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Future rule effective in 2099",
                category: "fact",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        set_validity(&ctx, future["id"].as_str().unwrap(), Some("2099-01-01T00:00:00Z"), None).unwrap();

        let expired = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Expired rule ended in 2020",
                category: "fact",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        set_validity(
            &ctx,
            expired["id"].as_str().unwrap(),
            Some("2019-01-01T00:00:00Z"),
            Some("2020-01-01T00:00:00Z"),
        )
        .unwrap();

        let sql = format!(
            "SELECT id FROM memories WHERE scope='repo:ra' AND status='active' AND {}",
            is_valid_now_sql_clause()
        );
        let now_ids: Vec<String> = ctx
            .db
            .query(&sql, [])
            .unwrap()
            .into_iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        assert_eq!(now_ids, vec![thu_id]);
    }

    #[test]
    fn rollback_source_dry_run_live_scrub_and_user_direct_protection() {
        let ctx = test_ctx();

        // Operator user_direct fact (must be protected unless source='user' explicitly).
        let direct = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Operator rule: always keep TLS enabled",
                category: "preference",
                scope: "repo:ra",
                source: "user",
                trust_tier: "user_direct",
                fact_key: Some("security.tls"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator:agent:poison_mentioned_here"),
            },
        )
        .unwrap();
        let direct_id = direct["id"].as_str().unwrap().to_string();

        // Contaminated agent/session memories + vectors + working note.
        let bad1 = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Poisoned claim 1: disable TLS checks",
                category: "fact",
                scope: "repo:ra",
                source: "agent:poison",
                trust_tier: "agent_inferred",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("pipeline:session:sess_bad_99:seq:1-4"),
            },
        )
        .unwrap();
        let bad1_id = bad1["id"].as_str().unwrap().to_string();
        let bad1_evt = bad1["source_event_id"].as_str().unwrap().to_string();

        let bad2 = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Poisoned claim 2: skip code review",
                category: "fact",
                scope: "repo:ra",
                source: "pipeline:poison",
                trust_tier: "untrusted",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("pipeline:session:sess_bad_99:seq:5-8"),
            },
        )
        .unwrap();
        let bad2_id = bad2["id"].as_str().unwrap().to_string();

        // Insert vectors for direct + poisoned memories.
        for mid in [&direct_id, &bad1_id, &bad2_id] {
            ctx.db
                .exec(
                    "INSERT INTO memory_vectors(memory_id, model_version, dim, vec, updated_at)
                     VALUES(?1, 'local:bge-m3', 2, X'0000803F00000000', datetime('now'))",
                    [mid],
                )
                .unwrap();
        }

        // Contaminated working note in repo:ra + clean operator working note in repo:rb.
        library::working_note_patch(
            &ctx,
            "repo:ra",
            Some("Poisoned requirements"),
            Some("Poisoned decisions"),
            None,
            None,
            None,
            "agent:poison",
        )
        .unwrap();
        library::working_note_patch(
            &ctx,
            "repo:rb",
            Some("Clean operator requirements"),
            None,
            None,
            None,
            None,
            "user",
        )
        .unwrap();

        // Dry run matches poisoned items + poisoned working note, touches NOTHING.
        let dry = rollback_source(&ctx, Some("repo:ra"), Some("poison"), None, true).unwrap();
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["matched"], 3, "{dry}");
        assert_eq!(dry["rolled_back"], 0);
        assert_eq!(dry["scrubbed_events"], 0);
        assert!(!dry["affected_ids"].to_string().contains(&direct_id));
        assert_eq!(
            ctx.db
                .query("SELECT content FROM memories WHERE id=?1", [&bad1_id])
                .unwrap()[0]["content"],
            "Poisoned claim 1: disable TLS checks"
        );

        // Live rollback scrubs memories, vectors, events, working note, and writes audit row.
        let live = rollback_source(&ctx, Some("repo:ra"), Some("poison"), None, false).unwrap();
        assert_eq!(live["dry_run"], false);
        assert_eq!(live["matched"], 3, "{live}");
        assert_eq!(live["rolled_back"], 3, "{live}");
        assert!(live["scrubbed_events"].as_i64().unwrap() >= 2, "{live}");

        // Poisoned memories deleted & content blanked.
        for mid in [&bad1_id, &bad2_id] {
            let row = &ctx
                .db
                .query("SELECT status, active, content FROM memories WHERE id=?1", [mid])
                .unwrap()[0];
            assert_eq!(row["status"], "deleted");
            assert_eq!(row["active"], 0);
            assert_eq!(row["content"], "");
            assert!(ctx
                .db
                .query("SELECT memory_id FROM memory_vectors WHERE memory_id=?1", [mid])
                .unwrap()
                .is_empty());
        }

        // Referenced raw event is scrubbed.
        let evt_rowid = bad1_evt.strip_prefix("evt:").unwrap().parse::<i64>().unwrap();
        let evt_body = ctx
            .db
            .query("SELECT body FROM events WHERE id=?1", [evt_rowid])
            .unwrap()[0]["body"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(evt_body.contains("\"scrubbed\":true"), "event body: {evt_body}");

        // Contaminated working note cleared; operator working note untouched.
        let wn_ra = library::working_note_get(&ctx, "repo:ra").unwrap();
        assert_eq!(wn_ra["requirements"], "");
        assert_eq!(wn_ra["decisions"], "");
        let wn_rb = library::working_note_get(&ctx, "repo:rb").unwrap();
        assert_eq!(wn_rb["requirements"], "Clean operator requirements");

        // user_direct memory and its vector are completely untouched!
        let direct_row = &ctx
            .db
            .query("SELECT status, content FROM memories WHERE id=?1", [&direct_id])
            .unwrap()[0];
        assert_eq!(direct_row["status"], "active");
        assert_eq!(direct_row["content"], "Operator rule: always keep TLS enabled");
        assert_eq!(
            ctx.db
                .query("SELECT count(*) AS n FROM memory_vectors WHERE memory_id=?1", [&direct_id])
                .unwrap()[0]["n"],
            1
        );

        // Audit row persisted in memory_rollbacks.
        let audits = ctx.db.query("SELECT * FROM memory_rollbacks", []).unwrap();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0]["scope"], "repo:ra");
        assert_eq!(audits[0]["target_source"], "poison");

        // Explicit source='user' DOES allow rolling back user_direct when intended.
        let user_rb = rollback_source(&ctx, Some("repo:ra"), Some("user"), None, false).unwrap();
        assert_eq!(user_rb["rolled_back"], 1, "{user_rb}");
        assert_eq!(
            ctx.db
                .query("SELECT status FROM memories WHERE id=?1", [&direct_id])
                .unwrap()[0]["status"],
            "deleted"
        );

        // Refuses empty filter (no blanket wipe).
        assert!(rollback_source(&ctx, None, None, None, true).is_err());
    }

    #[test]
    fn rebuild_views_verifies_fts_and_purges_orphan_vectors() {
        let ctx = test_ctx();

        let keep = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Active user direct memory about sqlite WAL",
                category: "fact",
                scope: "repo:r1",
                source: "user",
                trust_tier: "user_direct",
                fact_key: Some("db.wal"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("operator"),
            },
        )
        .unwrap();
        let keep_id = keep["id"].as_str().unwrap().to_string();

        let old = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Old superseded memory",
                category: "fact",
                scope: "repo:r1",
                source: "agent:a",
                trust_tier: "agent_inferred",
                fact_key: Some("old.key"),
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("agent"),
            },
        )
        .unwrap();
        let old_id = old["id"].as_str().unwrap().to_string();
        ctx.db
            .exec("UPDATE memories SET active=0, status='superseded' WHERE id=?1", [&old_id])
            .unwrap();

        let del = library::save_structured(
            &ctx,
            SaveOpts {
                content: "Deleted memory",
                category: "fact",
                scope: "repo:r1",
                source: "agent:a",
                trust_tier: "agent_inferred",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: Some("agent"),
            },
        )
        .unwrap();
        let del_id = del["id"].as_str().unwrap().to_string();
        library::forget_memory(&ctx, &del_id).unwrap();

        // Seed 1 valid vector + 3 stale/orphan vectors (superseded, deleted, ghost).
        for mid in [keep_id.as_str(), old_id.as_str(), del_id.as_str(), "mem_ghost_orphan"] {
            ctx.db
                .exec(
                    "INSERT INTO memory_vectors(memory_id, model_version, dim, vec, updated_at)
                     VALUES(?1, 'local:bge-m3', 2, X'0000803F00000000', datetime('now'))",
                    [mid],
                )
                .unwrap();
        }

        let res = rebuild_views(&ctx, Some("repo:r1")).unwrap();
        assert_eq!(res["rebuilt_memories"], 1, "{res}");
        assert_eq!(res["removed_orphan_vectors"], 3, "{res}");
        assert_eq!(res["preserved_user_direct"], 1, "{res}");

        let remaining: Vec<String> = ctx
            .db
            .query("SELECT memory_id FROM memory_vectors", [])
            .unwrap()
            .into_iter()
            .filter_map(|r| r["memory_id"].as_str().map(str::to_string))
            .collect();
        assert_eq!(remaining, vec![keep_id]);

        // FTS5 search still resolves rebuilt active row cleanly.
        let hits = ctx
            .db
            .query(
                "SELECT m.id FROM memories_fts f JOIN memories m ON m.rowid=f.rowid WHERE memories_fts MATCH 'sqlite'",
                [],
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn config_set_validates_loopback_and_trusted_gateway_providers() {
        let ctx = test_ctx();

        ctx.db
            .exec(
                "INSERT INTO providers(id,name,kind,base_url,trust,guard_mode,enabled,created_at)
                 VALUES
                   ('trusted-gw','Trusted GW','openai_chat','https://api.example.com/v1','trusted','redact',1,0),
                   ('untrusted-gw','Untrusted GW','openai_chat','https://untrusted.example.com/v1','untrusted','redact',1,0),
                   ('disabled-gw','Disabled GW','openai_chat','https://disabled.example.com/v1','trusted','redact',0,0)",
                [],
            )
            .unwrap();
        ctx.db
            .exec(
                "INSERT INTO models(provider_id,model_id,enabled,discovered_at)
                 VALUES
                   ('trusted-gw','qwen3-reranker-0.6b',1,0),
                   ('trusted-gw','BAAI/bge-reranker-v2-m3',1,0),
                   ('trusted-gw','generic-rerank-v1',1,0)",
                [],
            )
            .unwrap();

        // Auto-discovery picks bge-reranker-v2-m3 over qwen3-reranker and generic rerank via gateway.
        let auto = resolve_rerank_config(&ctx).expect("auto-discovered reranker");
        assert_eq!(auto.model, "BAAI/bge-reranker-v2-m3");
        assert!(auto.endpoint_url.ends_with("/p/trusted-gw/rerank"));
        assert_eq!(auto.auth_token.as_deref(), Some("test-token"));

        // Rejects non-loopback direct endpoints, credentials in URL, untrusted/disabled/unknown providers.
        assert!(config_set(
            &ctx,
            &json!({ "embedding_endpoint": "https://api.openai.com/v1/embeddings" })
        )
        .is_err());
        assert!(config_set(
            &ctx,
            &json!({ "rerank_endpoint": "https://api.cohere.com/v1/rerank" })
        )
        .is_err());
        assert!(config_set(
            &ctx,
            &json!({ "rerank_endpoint": "http://0.0.0.0:8080/rerank" })
        )
        .is_err());
        assert!(config_set(
            &ctx,
            &json!({ "rerank_endpoint": "http://user:pass@127.0.0.1:8080/rerank" })
        )
        .is_err());
        assert!(config_set(&ctx, &json!({ "embedding_provider": "untrusted-gw" })).is_err());
        assert!(config_set(&ctx, &json!({ "rerank_provider": "untrusted-gw" })).is_err());
        assert!(config_set(&ctx, &json!({ "rerank_provider": "disabled-gw" })).is_err());
        assert!(config_set(&ctx, &json!({ "rerank_provider": "nonexistent" })).is_err());

        // Accepts loopback IPv4, localhost, IPv6 [::1], and trusted gateway provider.
        let cfg = config_set(
            &ctx,
            &json!({
                "embedding_endpoint": "http://127.0.0.1:11434/v1/embeddings",
                "embedding_model": "bge-m3",
                "rerank_endpoint": "http://[::1]:8081/rerank",
                "rerank_model": "Qwen3-Reranker-0.6B"
            }),
        )
        .unwrap();
        assert_eq!(cfg["mode"], "hybrid_bm25_dense_rrf");
        assert_eq!(cfg["active_embed_model"], "local:bge-m3");
        assert_eq!(cfg["reranker_active"], true);
        assert_eq!(cfg["active_rerank_model"], "local:Qwen3-Reranker-0.6B");

        // Explicit trusted gateway provider overrides loopback endpoint.
        let cfg2 = config_set(
            &ctx,
            &json!({
                "rerank_provider": "trusted-gw",
                "rerank_model": "qwen3-reranker-0.6b"
            }),
        )
        .unwrap();
        assert_eq!(cfg2["active_rerank_model"], "gateway:trusted-gw:qwen3-reranker-0.6b");
    }

    #[tokio::test]
    async fn rerank_candidates_async_real_loopback_http_and_offline_fallback() {
        async fn mock_rerank_handler(Json(req): Json<Value>) -> Json<Value> {
            let q = req["query"].as_str().unwrap_or("").to_lowercase();
            let docs = req["documents"].as_array().cloned().unwrap_or_default();
            let mut results: Vec<Value> = docs
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let txt = d.as_str().unwrap_or("").to_lowercase();
                    let score = if txt.contains("sqlite-vec") && q.contains("vector") {
                        0.97
                    } else if txt.contains("fts5") {
                        0.62
                    } else {
                        0.15
                    };
                    json!({ "index": i, "relevance_score": score })
                })
                .collect();
            // Inject adversarial out-of-bounds and duplicate indices to verify ownership/dedup gate!
            results.push(json!({ "index": 999, "relevance_score": 1.0 }));
            results.push(json!({ "index": 2, "relevance_score": 0.99 }));
            Json(json!({ "model": req["model"], "results": results }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/rerank", listener.local_addr().unwrap());
        let app = Router::new().route("/rerank", post(mock_rerank_handler));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let ctx = test_ctx();
        let candidates = vec![
            json!({ "id": "m1", "scope": "repo:ra", "content": "Unrelated coffee preference" }),
            json!({ "id": "m2", "scope": "repo:ra", "content": "SQLite FTS5 provides lexical BM25" }),
            json!({ "id": "m3", "scope": "repo:ra", "content": "Pin sqlite-vec for local vector search" }),
        ];

        // 1. Unconfigured -> clean fallback to input order, active reranker = None.
        let (unconf, model_none) = rerank_candidates_async(&ctx, "vector search", candidates.clone(), 2).await;
        assert!(model_none.is_none());
        assert_eq!(unconf.len(), 2);
        assert_eq!(unconf[0]["id"], "m1");
        assert!(unconf[0].get("rerank_score").is_none());

        // 2. Configured loopback server -> reorders m3 to top, tags rerank_score, enforces top_n & ownership gate.
        config_set(
            &ctx,
            &json!({
                "rerank_endpoint": url,
                "rerank_model": "bge-reranker-v2-m3"
            }),
        )
        .unwrap();

        let (ranked, active_model) = rerank_candidates_async(&ctx, "local vector search", candidates.clone(), 2).await;
        assert_eq!(active_model.as_deref(), Some("local:bge-reranker-v2-m3"));
        assert_eq!(ranked.len(), 2, "must honor top_n=2 and dedup/drop invalid indices: {ranked:?}");
        assert_eq!(ranked[0]["id"], "m3");
        assert_eq!(ranked[1]["id"], "m2");
        assert!(ranked[0]["rerank_score"].as_f64().unwrap() > ranked[1]["rerank_score"].as_f64().unwrap());

        // 3. Offline / dead loopback port -> graceful fallback to input order, active reranker = None.
        server.abort();
        config_set(
            &ctx,
            &json!({
                "rerank_endpoint": "http://127.0.0.1:9/rerank",
                "rerank_model": "bge-reranker-v2-m3"
            }),
        )
        .unwrap();
        let (offline, offline_model) = rerank_candidates_async(&ctx, "local vector search", candidates, 2).await;
        assert!(offline_model.is_none());
        assert_eq!(offline.len(), 2);
        assert_eq!(offline[0]["id"], "m1");
        assert_eq!(offline[1]["id"], "m2");
    }
}
