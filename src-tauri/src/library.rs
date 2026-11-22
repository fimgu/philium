//! Philium's skills, provenance-backed memory core, and zero-LLM context slate builder.
//!
//! Design grounded in the Philium v1 memory blueprint:
//! - Raw turns/events remain authoritative; memories are curated derived claims linked by `source_event_id`.
//! - Trust tiers (`user_direct`, `verified_run`, `agent_inferred`, `untrusted`) prevent tool output
//!   or agent guesses from overwriting operator decisions (MemPoison defense).
//! - Code-level conflict resolution (`fact_key` + `supersedes_id` + `status`) resolves contradictions
//!   before prompt injection while preserving historical records (Trust Gap / LEDGER / CALM).
//! - Hard deletion scrubs deleted content from memories, FTS5, vectors, and referenced raw event records.
//! - Hybrid retrieval uses SQLite FTS5 BM25 + real neural embeddings (via configured local runtime or
//!   Philium gateway `/embeddings`, version-pinned in `memory_vectors`) merged with pure RRF (`k=60`)
//!   and NO provenance weighting in retrieval. When no embedding backend is configured, retrieval
//!   honestly falls back to `bm25_only`.
//! - Zero-LLM budgeted slate builder (`build_context_slate`) enforces scope ownership before and after
//!   retrieval, preserves relevance order, caps untrusted items, and greedily fills to per-model budget `B`.
//! - Structured `working_notes` and pipeline-side session observation notes provide cross-session handoffs.
//!
//! rpc (`library.*`):
//!   status {}, import {}, reindex {}, observe_session {agent_id}, skills {},
//!   memories {q?, repo_id?, limit?},
//!   list {scope?, status?, trust_tier?, q?, limit?},
//!   save {content, category, scope?, repo_id?, fact_key?, supersedes_id?, trust_tier?, provenance?, source_event_id?},
//!   promote {id}, quarantine {id, reason?}, supersede {old_id, content, category?, provenance?},
//!   forget {id}, rules_get {}, rules_set {rules},
//!   working_note_get {scope?, repo_id?},
//!   working_note_set {scope?, repo_id?, requirements?, decisions?, failed_attempts?, next_steps?, source_event_id?},
//!   context_slate {repo_id?, q?, budget_tokens?, model_ref?, strict?, include_global?, triggers?},
//!   intentions {scope?, repo_id?}, intention_add {scope?, repo_id?, trigger_kind, trigger_match?, instruction},
//!   intention_remove {id}, consolidate_preview {scope?, repo_id?}, consolidate_promote {id},
//!   sessions {q?, project?, repo_only?, limit?}
use crate::{ctx::Ctx, db::now};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

const MAX_UNTRUSTED_PER_SLATE: usize = 2;
const MAX_WORKING_NOTE_PROMPT_BYTES: usize = 2048;
const MAX_REMINDERS_PROMPT_BYTES: usize = 2048;
const SESSION_CACHE_CAP: usize = 512;
const VECTOR_SCOPE_CACHE_CAP: usize = 64;
const QUERY_EMBED_CACHE_CAP: usize = 256;
const NEURAL_COSINE_MIN: f32 = 0.25;
const CONSOLIDATE_COSINE_MIN: f32 = 0.90;
const RRF_K: f64 = 60.0;
const RRF_POOL_LIMIT: i64 = 128;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS skills (
  name TEXT PRIMARY KEY, description TEXT NOT NULL, body TEXT NOT NULL,
  files TEXT NOT NULL DEFAULT '{}', source TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY, scope TEXT NOT NULL, category TEXT NOT NULL, content TEXT NOT NULL,
  source TEXT NOT NULL, created_at TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1,
  trust_tier TEXT NOT NULL DEFAULT 'user_direct',
  status TEXT NOT NULL DEFAULT 'active',
  fact_key TEXT,
  supersedes_id TEXT,
  source_event_id TEXT,
  provenance TEXT,
  updated_at TEXT,
  valid_from TEXT,
  valid_to TEXT,
  title TEXT,
  files TEXT);
CREATE INDEX IF NOT EXISTS memories_scope ON memories(scope);
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content, content='memories', content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content); END;
CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content); END;
CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
  INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content); END;
CREATE TABLE IF NOT EXISTS working_notes (
  scope TEXT PRIMARY KEY,
  requirements TEXT NOT NULL DEFAULT '',
  decisions TEXT NOT NULL DEFAULT '',
  failed_attempts TEXT NOT NULL DEFAULT '',
  next_steps TEXT NOT NULL DEFAULT '',
  source_event_id TEXT,
  updated_by TEXT NOT NULL DEFAULT 'user',
  updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS memory_vectors (
  memory_id TEXT PRIMARY KEY,
  model_version TEXT NOT NULL,
  dim INTEGER NOT NULL,
  vec BLOB NOT NULL,
  updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS intentions (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  trigger_kind TEXT NOT NULL,
  trigger_match TEXT NOT NULL DEFAULT '',
  instruction TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL DEFAULT 'user',
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS intentions_scope ON intentions(scope, active);
CREATE TABLE IF NOT EXISTS consolidation_runs (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  state TEXT NOT NULL,
  proposals TEXT NOT NULL,
  eval_report TEXT NOT NULL,
  created_at TEXT NOT NULL,
  promoted_at TEXT);
";

/// Safe, idempotent migration for pre-existing databases created before trust_tier/status columns existed.
fn migrate_columns(ctx: &Ctx) -> Result<(), String> {
    let cols: Vec<String> = ctx
        .db
        .query("PRAGMA table_info(memories)", [])?
        .into_iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    let additions = [
        ("trust_tier", "ALTER TABLE memories ADD COLUMN trust_tier TEXT NOT NULL DEFAULT 'user_direct'"),
        ("status", "ALTER TABLE memories ADD COLUMN status TEXT NOT NULL DEFAULT 'active'"),
        ("fact_key", "ALTER TABLE memories ADD COLUMN fact_key TEXT"),
        ("supersedes_id", "ALTER TABLE memories ADD COLUMN supersedes_id TEXT"),
        ("source_event_id", "ALTER TABLE memories ADD COLUMN source_event_id TEXT"),
        ("provenance", "ALTER TABLE memories ADD COLUMN provenance TEXT"),
        ("updated_at", "ALTER TABLE memories ADD COLUMN updated_at TEXT"),
        ("valid_from", "ALTER TABLE memories ADD COLUMN valid_from TEXT"),
        ("valid_to", "ALTER TABLE memories ADD COLUMN valid_to TEXT"),
        ("title", "ALTER TABLE memories ADD COLUMN title TEXT"),
        ("files", "ALTER TABLE memories ADD COLUMN files TEXT"),
    ];
    for (name, sql) in additions {
        if !cols.iter().any(|c| c == name) {
            ctx.db.exec(sql, [])?;
        }
    }
    // Backfill status/trust_tier for legacy rows cleanly.
    let _ = ctx.db.exec(
        "UPDATE memories SET status='deleted' WHERE active=0 AND status='active'",
        [],
    );
    let _ = ctx.db.exec(
        "UPDATE memories SET trust_tier='agent_inferred' WHERE source LIKE 'agent:%' AND trust_tier='user_direct' AND updated_at IS NULL",
        [],
    );
    // Remove legacy fake n-gram vectors so they can never pollute neural cosine retrieval.
    let _ = ctx.db.exec(
        "DELETE FROM memory_vectors WHERE model_version = 'philium-local-ngram-256-v1'",
        [],
    );
    Ok(())
}

/// Indexes over columns that legacy databases only gain in `migrate_columns`. They must never live
/// in `SCHEMA`: on a pre-v1 `memories` table the batch would abort with `no such column: status`
/// before any later table (working notes, vectors, intentions, ledger, procedures) is created.
const POST_MIGRATION_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS memories_status ON memories(status, active);
CREATE INDEX IF NOT EXISTS memories_fact_key ON memories(scope, fact_key);
";

/// Standing rules every agent gets, tuned to Philium's tools. Editable via setting `agent_rules`.
pub const DEFAULT_RULES: &str = "\
## Filip's standing rules
- Keep replies short. Lead with the result. Separate what you verified from what you did not.
- Read lean: bounded file ranges, not whole files. Filter noisy build and test output to the failures.
- Delegate broad exploration to a helper (agent_spawn) and ask for a short summary with file:line references.
- Clean up everything you create before reporting done: containers (run with --rm; register others with resource_register), compose projects (`down -v --rmi local`), worktrees and branches you made, scratch files in $TMPDIR. Never leave uncommitted work silently: push it or say so. Never delete what you did not create. Your final reply states what was removed or kept and why.
- PR reviews and readiness checks: skill_get review-pr first and follow it. Test the real user workflow, not just unit tests. Never call a partial pass end-to-end verified.
- Before answering what a feature does, check the default branch and open PRs, not only the local checkout.
- Do not create markdown notes, handoff or README files unless asked.
- Use your skills: skill_list shows them, skill_get loads one. Use memory_slate or memory_search before asking Filip something he may have told you before, and memory_save for durable preferences or corrections he gives you.
";

// ---------- Real Neural Embeddings + Per-Scope Vector Cache ----------

#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub endpoint_url: String,
    pub auth_token: Option<String>,
    pub model: String,
    pub model_version: String,
    /// Set for the in-process model: its data dir. HTTP fields are unused then.
    pub local_dir: Option<PathBuf>,
}

fn embed_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default()
    })
}

fn is_loopback_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else { return false };
    matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn is_embedding_model_id(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    l.contains("bge-m3")
        || l.contains("qwen3-embedding")
        || l.contains("bge-small")
        || l.contains("bge-base")
        || l.contains("bge-large")
        || l.contains("nomic-embed")
        || l.contains("text-embedding")
        || l.contains("embeddinggemma")
        || l.contains("embed")
}

fn embedding_model_priority(id: &str) -> u8 {
    let l = id.to_ascii_lowercase();
    if l.contains("bge-m3") {
        0
    } else if l.contains("qwen3-embedding") {
        1
    } else if l.contains("bge-small") || l.contains("bge-base") {
        2
    } else if l.contains("nomic-embed") {
        3
    } else if l.contains("text-embedding") {
        4
    } else {
        5
    }
}

/// Resolve an active neural embedding backend from settings or configured trusted providers.
/// Remote providers always route through Philium's gateway (`/p/<provider>/embeddings`).
/// Direct endpoints are restricted strictly to localhost/loopback runtimes (e.g. llama-server, Ollama).
/// Returns `None` when no embedding model is available (honest lexical-only FTS5 fallback).
pub fn resolve_embedding_config(ctx: &Ctx) -> Option<EmbeddingConfig> {
    // 1. Explicit provider + model routed through Philium's gateway.
    let explicit_provider = ctx.db.kv_get("setting.embedding_provider").unwrap_or_default();
    let explicit_model = ctx.db.kv_get("setting.embedding_model").unwrap_or_default();
    if !explicit_provider.trim().is_empty() && !explicit_model.trim().is_empty() {
        let pid = explicit_provider.trim();
        let mid = explicit_model.trim();
        if let Some(p) = crate::providers::get(ctx, pid) {
            if p.enabled && p.trusted {
                return Some(EmbeddingConfig {
                    endpoint_url: format!("{}/p/{}/embeddings", ctx.gateway_url.trim_end_matches('/'), p.id),
                    auth_token: Some(ctx.gateway.token.clone()),
                    model: mid.to_string(),
                    model_version: format!("gateway:{}:{}", p.id, mid),
                    local_dir: None,
                });
            }
        }
    }

    // 2. Explicit local loopback runtime endpoint (Ollama / llama.cpp / vLLM / MLX).
    let explicit_endpoint = ctx.db.kv_get("setting.embedding_endpoint").unwrap_or_default();
    if !explicit_endpoint.trim().is_empty() && !explicit_model.trim().is_empty() {
        let ep = explicit_endpoint.trim();
        if is_loopback_url(ep) {
            let mid = explicit_model.trim();
            let auth = ctx.db.kv_get("setting.embedding_api_key").filter(|s| !s.trim().is_empty());
            return Some(EmbeddingConfig {
                endpoint_url: ep.to_string(),
                auth_token: auth,
                model: mid.to_string(),
                model_version: format!("local:{mid}"),
                local_dir: None,
            });
        }
    }

    // 3. The in-process model, once downloaded and verified. Never leaves the machine.
    if crate::local_models::ready(ctx, &crate::local_models::EMBED) {
        return Some(EmbeddingConfig {
            endpoint_url: String::new(),
            auth_token: None,
            model: crate::local_models::EMBED.label.to_string(),
            model_version: crate::local_models::EMBED.version.to_string(),
            local_dir: Some(ctx.data_dir.clone()),
        });
    }

    // 4. Auto-discover an embedding model on any enabled trusted OpenAI-compatible provider via gateway.
    let providers = crate::providers::all(ctx);
    let mut best: Option<(u8, String, String)> = None;
    for p in providers {
        if !p.enabled || !p.trusted || !matches!(p.kind, crate::providers::Kind::OpenaiChat | crate::providers::Kind::OpenaiResponses) {
            continue;
        }
        let rows = ctx
            .db
            .query("SELECT model_id FROM models WHERE provider_id=?1 AND enabled=1", [&p.id])
            .unwrap_or_default();
        for r in rows {
            let Some(mid) = r["model_id"].as_str() else { continue };
            if !is_embedding_model_id(mid) {
                continue;
            }
            let prio = embedding_model_priority(mid);
            if best.as_ref().is_none_or(|(bp, _, _)| prio < *bp) {
                best = Some((prio, p.id.clone(), mid.to_string()));
            }
        }
    }
    best.map(|(_, pid, mid)| EmbeddingConfig {
        endpoint_url: format!("{}/p/{}/embeddings", ctx.gateway_url.trim_end_matches('/'), pid),
        auth_token: Some(ctx.gateway.token.clone()),
        model: mid.clone(),
        model_version: format!("gateway:{pid}:{mid}"),
        local_dir: None,
    })
}

fn normalize_l2(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn bytes_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Request L2-normalized neural embeddings from an OpenAI-compatible `/embeddings` endpoint.
pub async fn embed_texts_async(cfg: &EmbeddingConfig, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if inputs.is_empty() {
        return Ok(vec![]);
    }
    if let Some(dir) = cfg.local_dir.clone() {
        let texts = inputs.to_vec();
        let out = tokio::task::spawn_blocking(move || crate::local_models::embed(&dir, &texts))
            .await
            .map_err(|e| e.to_string())??;
        return Ok(out.into_iter().map(normalize_l2).collect());
    }
    let mut req = embed_http_client()
        .post(&cfg.endpoint_url)
        .json(&json!({ "model": cfg.model, "input": inputs }));
    if let Some(ref tok) = cfg.auth_token {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("embedding request failed: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("embedding endpoint HTTP {code}: {}", body.chars().take(200).collect::<String>()));
    }
    let v: Value = resp.json().await.map_err(|e| format!("invalid embedding JSON: {e}"))?;
    let data = v["data"].as_array().ok_or_else(|| "embedding response missing `data` array".to_string())?;
    let mut indexed: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (pos, item) in data.iter().enumerate() {
        let idx = item["index"].as_u64().map(|n| n as usize).unwrap_or(pos);
        let arr = item["embedding"]
            .as_array()
            .ok_or_else(|| "embedding entry missing `embedding` array".to_string())?;
        let vec: Vec<f32> = arr
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| "non-numeric embedding value".to_string()))
            .collect::<Result<Vec<f32>, String>>()?;
        if vec.is_empty() {
            return Err("embedding vector is empty".into());
        }
        indexed.push((idx, normalize_l2(vec)));
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    let out: Vec<Vec<f32>> = indexed.into_iter().map(|(_, vec)| vec).collect();
    if out.len() != inputs.len() {
        return Err(format!("embedding count mismatch: expected {}, got {}", inputs.len(), out.len()));
    }
    Ok(out)
}

type ScopeCacheKey = (PathBuf, String, bool, String);
type CachedScopeVectors = Vec<(String, Vec<f32>)>;

fn scope_vector_cache() -> &'static Mutex<HashMap<ScopeCacheKey, CachedScopeVectors>> {
    static C: OnceLock<Mutex<HashMap<ScopeCacheKey, CachedScopeVectors>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn query_embed_cache() -> &'static Mutex<HashMap<(String, String), Vec<f32>>> {
    static C: OnceLock<Mutex<HashMap<(String, String), Vec<f32>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub(crate) fn invalidate_vector_cache(ctx: &Ctx) {
    let mut g = scope_vector_cache().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|(dir, _, _, _), _| dir != &ctx.data_dir);
}

fn store_memory_vector(ctx: &Ctx, memory_id: &str, model_version: &str, vec: &[f32]) -> Result<(), String> {
    if vec.is_empty() {
        return Ok(());
    }
    let blob = vec_to_bytes(vec);
    ctx.db.exec(
        "INSERT INTO memory_vectors(memory_id, model_version, dim, vec, updated_at)
         VALUES(?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(memory_id) DO UPDATE SET model_version=excluded.model_version, dim=excluded.dim, vec=excluded.vec, updated_at=datetime('now')",
        params![memory_id, model_version, vec.len() as i64, blob],
    )?;
    invalidate_vector_cache(ctx);
    Ok(())
}

/// Embed a single memory via the active neural embedding backend and persist it in `memory_vectors`.
/// No-op when no embedding backend is configured. Keeps SQLite write off Tokio worker threads.
pub async fn embed_memory_async(ctx: &Arc<Ctx>, memory_id: &str, content: &str) -> Result<bool, String> {
    let Some(cfg) = resolve_embedding_config(ctx) else {
        return Ok(false);
    };
    let vecs = embed_texts_async(&cfg, &[content.to_string()]).await?;
    let Some(vec) = vecs.into_iter().next() else {
        return Ok(false);
    };
    let (c, mid, mv) = (ctx.clone(), memory_id.to_string(), cfg.model_version);
    tokio::task::spawn_blocking(move || store_memory_vector(&c, &mid, &mv, &vec))
        .await
        .map_err(|e| e.to_string())??;
    Ok(true)
}

/// Embed a query string using the active neural backend (with bounded in-memory caching).
async fn embed_query_cached(ctx: &Ctx, q: &str) -> Option<(String, Vec<f32>)> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cfg = resolve_embedding_config(ctx)?;
    let key = (cfg.model_version.clone(), trimmed.to_string());
    {
        let cache = query_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&key) {
            return Some((cfg.model_version, hit.clone()));
        }
    }
    let vec = embed_texts_async(&cfg, &[trimmed.to_string()]).await.ok()?.into_iter().next()?;
    let mut cache = query_embed_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= QUERY_EMBED_CACHE_CAP {
        cache.clear();
    }
    cache.insert(key, vec.clone());
    Some((cfg.model_version, vec))
}

/// Re-embed all active memories missing a vector for the currently pinned embedding model version.
/// Never mixes vectors from different model versions. Runs DB work on blocking threads.
pub async fn sync_vectors_async(ctx: &Arc<Ctx>) -> Result<Value, String> {
    let Some(cfg) = resolve_embedding_config(ctx) else {
        return Ok(json!({ "embed_model": Value::Null, "indexed": 0, "mode": "bm25_only" }));
    };
    let (c_read, mv_read) = (ctx.clone(), cfg.model_version.clone());
    let missing: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
        c_read
            .db
            .query(
                "SELECT m.id, m.content FROM memories m
                 LEFT JOIN memory_vectors v ON v.memory_id = m.id
                 WHERE m.active=1 AND m.status='active' AND (v.memory_id IS NULL OR v.model_version != ?1)",
                [mv_read],
            )
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|r| Some((r["id"].as_str()?.to_string(), r["content"].as_str()?.to_string())))
                    .collect()
            })
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut indexed = 0usize;
    for chunk in missing.chunks(32) {
        let texts: Vec<String> = chunk.iter().map(|(_, txt)| txt.clone()).collect();
        let vecs = embed_texts_async(&cfg, &texts).await?;
        let pairs: Vec<(String, Vec<f32>)> = chunk.iter().map(|(id, _)| id.clone()).zip(vecs).collect();
        let (c_write, mv_write) = (ctx.clone(), cfg.model_version.clone());
        let batch_n = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            c_write.db.with(|conn| {
                let tx = conn.unchecked_transaction()?;
                for (mid, vec) in &pairs {
                    let blob = vec_to_bytes(vec);
                    tx.execute(
                        "INSERT INTO memory_vectors(memory_id, model_version, dim, vec, updated_at)
                         VALUES(?1, ?2, ?3, ?4, datetime('now'))
                         ON CONFLICT(memory_id) DO UPDATE SET model_version=excluded.model_version, dim=excluded.dim, vec=excluded.vec, updated_at=datetime('now')",
                        params![mid, mv_write, vec.len() as i64, blob],
                    )?;
                }
                tx.commit()?;
                Ok(pairs.len())
            })
        })
        .await
        .map_err(|e| e.to_string())??;
        indexed += batch_n;
    }
    invalidate_vector_cache(ctx);
    Ok(json!({
        "embed_model": cfg.model_version,
        "indexed": indexed,
        "mode": "hybrid_bm25_dense_rrf",
    }))
}

fn load_scope_vectors_cached(
    ctx: &Ctx,
    scope: &str,
    include_global: bool,
    model_version: &str,
) -> Result<CachedScopeVectors, String> {
    let key = (ctx.data_dir.clone(), scope.to_string(), include_global, model_version.to_string());
    {
        let cache = scope_vector_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&key) {
            return Ok(hit.clone());
        }
    }
    // Load ALL active vectors for the authorized scope matching the exact pinned model_version (NO LIMIT 256).
    let loaded = ctx.db.with(|c| -> rusqlite::Result<CachedScopeVectors> {
        let mut stmt = c.prepare(
            "SELECT m.id, v.vec
             FROM memories m
             JOIN memory_vectors v ON v.memory_id = m.id AND v.model_version = ?3
             WHERE m.active=1 AND m.status='active' AND ((?2 AND m.scope='global') OR m.scope=?1)",
        )?;
        let rows = stmt.query_map(params![scope, include_global, model_version], |r| {
            let id: String = r.get(0)?;
            let blob: Vec<u8> = r.get(1)?;
            Ok((id, bytes_to_vec(&blob)))
        })?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            if !row.1.is_empty() {
                out.push(row);
            }
        }
        Ok(out)
    })?;
    let mut cache = scope_vector_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= VECTOR_SCOPE_CACHE_CAP {
        cache.clear();
    }
    cache.insert(key, loaded.clone());
    Ok(loaded)
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    // A legacy database may carry `memories` rows from before the FTS index existed; the external
    // content table is created empty, so rebuild it once or those rows stay invisible to BM25.
    let fts_missing = ctx
        .db
        .query("SELECT 1 AS x FROM sqlite_master WHERE name='memories_fts'", [])
        .map(|r| r.is_empty())
        .unwrap_or(false);
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    if fts_missing {
        let _ = ctx.db.exec("INSERT INTO memories_fts(memories_fts) VALUES('rebuild')", []);
    }
    migrate_columns(ctx)?;
    ctx.db.with(|c| c.execute_batch(POST_MIGRATION_INDEXES))?;
    crate::memory_ledger::init(ctx)?;
    crate::memory_skills::init(ctx)?;
    crate::memory_consolidation::init(ctx)?;
    // Idempotent: remaps previously imported `project:<abs_path>` rows onto
    // their registered `repo:<id>` scopes (no-op when nothing matches).
    sync_imported_repo_scopes(ctx);
    // Memory never reads another tool's files. Earlier builds imported them; drop that data once.
    if ctx.db.kv_get(PURGE_AUTO_IMPORT_KEY).is_none() {
        let r = purge_auto_imported(ctx)?;
        ctx.db.kv_set(PURGE_AUTO_IMPORT_KEY, &now().to_string())?;
        if r["memories"].as_u64().unwrap_or(0) + r["skills"].as_u64().unwrap_or(0) > 0 {
            ctx.log_event("library:purge_auto_import", None, &r);
        }
    }
    Ok(())
}

const PURGE_AUTO_IMPORT_KEY: &str = "library.auto_import_purged_at";

/// Delete everything the old silent first-launch import wrote: memories and skills
/// copied from other tools, plus their vectors and retrieval feedback.
fn purge_auto_imported(ctx: &Ctx) -> Result<Value, String> {
    // The old importer's fingerprint. Pre-provenance v0 rows are left alone.
    const MATCH: &str = "provenance='imported:jcode' OR scope LIKE 'jcode:%'";
    let (memories, skills) = ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let ids = format!("SELECT id FROM memories WHERE {MATCH}");
        tx.execute(&format!("DELETE FROM memory_vectors WHERE memory_id IN ({ids})"), [])?;
        tx.execute(&format!("DELETE FROM retrieval_feedback WHERE memory_id IN ({ids})"), [])?;
        let m = tx.execute(&format!("DELETE FROM memories WHERE {MATCH}"), [])?;
        let s = tx.execute("DELETE FROM skills WHERE source IN ('jcode','claude','claude-plugin')", [])?;
        tx.execute("DELETE FROM kv WHERE k='library.imported_at'", [])?;
        tx.commit()?;
        Ok((m, s))
    })?;
    if memories > 0 {
        let _ = ctx.db.exec("INSERT INTO memories_fts(memories_fts) VALUES('rebuild')", []);
        invalidate_vector_cache(ctx);
    }
    if memories + skills > 0 {
        ctx.notify("library");
    }
    Ok(json!({ "memories": memories, "skills": skills }))
}

/// Rules injected into every agent's instructions.
pub fn rules(ctx: &Ctx) -> String {
    ctx.db.kv_get("setting.agent_rules").unwrap_or_else(|| DEFAULT_RULES.to_string())
}

pub fn set_rules(ctx: &Ctx, body: &str) -> Result<(), String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("standing rules cannot be empty".into());
    }
    if trimmed.len() > 16_000 {
        return Err("standing rules must be under 16,000 characters".into());
    }
    ctx.db.kv_set("setting.agent_rules", trimmed)?;
    ctx.notify("library");
    Ok(())
}

/// Rebind any `project:<abs_path>` memories to their registered `repo:<id>`
/// scope so Standard agents can retrieve them via `memory_search` and `memory_slate`.
pub fn sync_imported_repo_scopes(ctx: &Ctx) -> usize {
    let rows = ctx.db.query("SELECT id, path FROM repos", []).unwrap_or_default();
    if rows.is_empty() {
        return 0;
    }
    let mut migrated = 0usize;
    for r in rows {
        let (Some(repo_id), Some(repo_path)) = (r["id"].as_str(), r["path"].as_str()) else { continue };
        let target_scope = format!("repo:{repo_id}");
        let mut old_scopes = vec![format!("project:{repo_path}")];
        if let Ok(canon) = std::fs::canonicalize(repo_path) {
            let cs = canon.to_string_lossy().into_owned();
            if cs != repo_path {
                old_scopes.push(format!("project:{cs}"));
            }
        }
        for old_scope in old_scopes {
            if let Ok(n) = ctx.db.exec(
                "UPDATE memories SET scope=?1, updated_at=COALESCE(updated_at, datetime('now')) WHERE scope=?2",
                params![target_scope, old_scope],
            ) {
                migrated += n;
            }
        }
    }
    if migrated > 0 {
        invalidate_vector_cache(ctx);
        ctx.notify("library");
    }
    migrated
}

pub fn skill_list(ctx: &Ctx) -> Result<Value, String> {
    Ok(json!(ctx.db.query("SELECT name,description,source FROM skills ORDER BY name", [])?))
}

pub fn skill_get(ctx: &Ctx, name: &str) -> Result<Value, String> {
    let mut r = ctx
        .db
        .query("SELECT name,description,body,files,source FROM skills WHERE name=?1", [name])?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no skill named {name}; skill_list shows them"))?;
    r["files"] = serde_json::from_str(r["files"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    Ok(r)
}

pub const CATEGORIES: [&str; 10] =
    ["fact", "preference", "correction", "entity", "procedure", "decision", "bugfix", "discovery", "architecture", "gotcha"];

/// Short display/index metadata for a memory: a title (<=80 chars) and the repo-relative files it
/// concerns (<=8, stored as a JSON array). Both are optional and never used for ranking.
#[derive(Default, Clone)]
pub struct MemoryMeta {
    pub title: Option<String>,
    pub files: Vec<String>,
}

fn clean_meta(meta: &MemoryMeta) -> (Option<String>, Option<String>) {
    let title = meta
        .title
        .as_deref()
        .map(|t| t.trim().replace('\n', " ").chars().take(80).collect::<String>())
        .filter(|t| !t.is_empty());
    let files: Vec<String> = meta
        .files
        .iter()
        .map(|f| f.trim().trim_start_matches("./").to_string())
        .filter(|f| !f.is_empty() && f.len() <= 200 && !f.starts_with('/') && !f.contains(".."))
        .take(8)
        .collect();
    (title, (!files.is_empty()).then(|| serde_json::to_string(&files).unwrap_or_default()))
}

fn validate_category(category: &str) -> Result<(), String> {
    if !CATEGORIES.contains(&category) {
        return Err(format!("category must be one of: {}", CATEGORIES.join(", ")));
    }
    Ok(())
}

fn validate_trust_tier(tier: &str) -> Result<(), String> {
    if !matches!(tier, "user_direct" | "verified_run" | "agent_inferred" | "untrusted") {
        return Err("trust_tier must be user_direct, verified_run, agent_inferred or untrusted".into());
    }
    Ok(())
}

fn clean_fact_key(raw: Option<&str>) -> Option<String> {
    let k = raw?.trim().to_lowercase();
    if k.is_empty() || k.len() > 80 {
        return None;
    }
    if k.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c)) {
        Some(k)
    } else {
        None
    }
}

pub struct SaveOpts<'a> {
    pub content: &'a str,
    pub category: &'a str,
    pub scope: &'a str,
    pub source: &'a str,
    pub trust_tier: &'a str,
    pub fact_key: Option<&'a str>,
    pub supersedes_id: Option<&'a str>,
    pub source_event_id: Option<&'a str>,
    pub provenance: Option<&'a str>,
}

/// Core memory writer implementing:
/// - Trust tier enforcement (agents cannot silently override `user_direct` facts).
/// - Code-level supersession by `fact_key` or explicit `supersedes_id` (preserving historical records).
/// - Quarantine for `untrusted` records so tool outputs are never treated as instructions.
/// - Durable `source_event_id` provenance linked to the authoritative event log.
pub fn save_structured(ctx: &Ctx, opts: SaveOpts<'_>) -> Result<Value, String> {
    save_structured_meta(ctx, opts, &MemoryMeta::default())
}

/// `save_structured` plus optional title/files index metadata.
pub fn save_structured_meta(ctx: &Ctx, opts: SaveOpts<'_>, meta: &MemoryMeta) -> Result<Value, String> {
    let (title, files) = clean_meta(meta);
    let content = opts.content.trim();
    if content.is_empty() || content.len() > 4000 {
        return Err("memory must be 1-4000 characters".into());
    }
    validate_category(opts.category)?;
    validate_trust_tier(opts.trust_tier)?;
    let fact_key = clean_fact_key(opts.fact_key);

    let mut status = if opts.trust_tier == "untrusted" { "quarantined" } else { "active" };
    let mut supersedes_id = opts.supersedes_id.filter(|s| !s.trim().is_empty()).map(str::to_string);

    // Check for existing active record on the same (scope, fact_key) for code-level conflict resolution.
    let mut to_supersede: Vec<String> = vec![];
    if let Some(ref fk) = fact_key {
        let existing = ctx.db.query(
            "SELECT id, trust_tier FROM memories WHERE scope=?1 AND fact_key=?2 AND active=1 AND status='active'",
            params![opts.scope, fk],
        )?;
        let blocks_override = existing.iter().any(|r| r["trust_tier"].as_str() == Some("user_direct"))
            && opts.trust_tier != "user_direct";
        if blocks_override {
            // MemPoison / Trust Gap defense: agent cannot silently overwrite a direct operator decision.
            status = "proposed";
        } else {
            for r in &existing {
                if let Some(eid) = r["id"].as_str() {
                    if supersedes_id.is_none() {
                        supersedes_id = Some(eid.to_string());
                    }
                    to_supersede.push(eid.to_string());
                }
            }
        }
    }
    if let Some(ref sid) = supersedes_id {
        if !to_supersede.contains(sid) && status == "active" {
            let target = ctx.db.query(
                "SELECT id, trust_tier, scope FROM memories WHERE id=?1 AND active=1 AND status='active'",
                [sid],
            )?;
            if let Some(t) = target.first() {
                if t["scope"].as_str() == Some(opts.scope) {
                    if t["trust_tier"].as_str() == Some("user_direct") && opts.trust_tier != "user_direct" {
                        status = "proposed";
                    } else {
                        to_supersede.push(sid.clone());
                    }
                }
            }
        }
    }

    let active: i64 = if status == "active" { 1 } else { 0 };
    let id = format!("mem_{}", crate::db::new_id());

    // Authoritative event-log provenance: always anchor writes to an event record if caller omitted one.
    let owned_event_id = match opts.source_event_id.filter(|s| !s.trim().is_empty()) {
        Some(eid) => Some(eid.to_string()),
        None => {
            let agent_tag = opts.source.strip_prefix("agent:").or_else(|| opts.source.strip_prefix("pipeline:"));
            ctx.log_event_id(
                "memory:write",
                agent_tag,
                &json!({
                    "memory_id": id,
                    "scope": opts.scope,
                    "category": opts.category,
                    "trust_tier": opts.trust_tier,
                    "status": status,
                    "fact_key": fact_key,
                    "source": opts.source,
                    "provenance": opts.provenance,
                }),
            )
        }
    };

    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        if status == "active" {
            for old_id in &to_supersede {
                tx.execute(
                    "UPDATE memories SET active=0, status='superseded', updated_at=datetime('now') WHERE id=?1",
                    [old_id],
                )?;
                let _ = tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", [old_id]);
            }
        }
        tx.execute(
            "INSERT INTO memories(id,scope,category,content,source,created_at,active,trust_tier,status,fact_key,supersedes_id,source_event_id,provenance,updated_at,title,files)
             VALUES(?1,?2,?3,?4,?5,datetime('now'),?6,?7,?8,?9,?10,?11,?12,datetime('now'),?13,?14)",
            params![
                id,
                opts.scope,
                opts.category,
                content,
                opts.source,
                active,
                opts.trust_tier,
                status,
                fact_key,
                supersedes_id,
                owned_event_id,
                opts.provenance,
                title,
                files,
            ],
        )?;
        tx.commit()
    })?;
    if status == "active" && !to_supersede.is_empty() {
        let _ = crate::memory_ledger::close_superseded_validity(ctx, &to_supersede, "");
    }
    invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(json!({
        "id": id,
        "scope": opts.scope,
        "status": status,
        "trust_tier": opts.trust_tier,
        "fact_key": fact_key,
        "title": title,
        "supersedes_id": supersedes_id,
        "superseded_ids": to_supersede,
        "source_event_id": owned_event_id,
    }))
}

/// FTS5 + neural dense vector hybrid search (pure Reciprocal Rank Fusion k=60, zero provenance weighting).
/// Falls back honestly to `bm25_only` when no neural embedding model is configured or available.
#[cfg(test)]
pub fn memory_search(ctx: &Ctx, q: &str, project: Option<&str>, limit: i64) -> Result<Value, String> {
    let scope = project.map(|p| format!("project:{p}")).unwrap_or_default();
    let (items, _, _) = search_scope_core(ctx, q, &scope, true, limit, None)?;
    Ok(Value::Array(items))
}

#[cfg(test)]
pub fn agent_memories(ctx: &Ctx, q: &str, repo_id: &str, limit: i64) -> Result<Value, String> {
    let (items, _, _) = search_scope_core(ctx, q, &format!("repo:{repo_id}"), false, limit, None)?;
    Ok(Value::Array(items))
}

pub async fn memory_search_async(ctx: &Arc<Ctx>, q: &str, project: Option<&str>, limit: i64) -> Result<Value, String> {
    let scope = project.map(|p| format!("project:{p}")).unwrap_or_default();
    let q_embed = embed_query_cached(ctx, q).await;
    let (c, q_owned) = (ctx.clone(), q.to_string());
    let pool_limit = limit.clamp(1, 50).max(20);
    let items = tokio::task::spawn_blocking(move || {
        let q_ref = q_embed.as_ref().map(|(mv, v)| (mv.as_str(), v.as_slice()));
        search_scope_core(&c, &q_owned, &scope, true, pool_limit, q_ref).map(|(rows, _, _)| rows)
    })
    .await
    .map_err(|e| e.to_string())??;
    let (reranked, _) = crate::memory_ledger::rerank_candidates_async(ctx, q, items, limit.clamp(1, 50) as usize).await;
    Ok(Value::Array(reranked))
}

pub async fn agent_memories_async(ctx: &Arc<Ctx>, q: &str, repo_id: &str, limit: i64) -> Result<Value, String> {
    let scope = format!("repo:{repo_id}");
    let q_embed = embed_query_cached(ctx, q).await;
    let (c, q_owned) = (ctx.clone(), q.to_string());
    let pool_limit = limit.clamp(1, 50).max(20);
    let items = tokio::task::spawn_blocking(move || {
        let q_ref = q_embed.as_ref().map(|(mv, v)| (mv.as_str(), v.as_slice()));
        search_scope_core(&c, &q_owned, &scope, false, pool_limit, q_ref).map(|(rows, _, _)| rows)
    })
    .await
    .map_err(|e| e.to_string())??;
    let (reranked, _) = crate::memory_ledger::rerank_candidates_async(ctx, q, items, limit.clamp(1, 50) as usize).await;
    Ok(Value::Array(reranked))
}

fn search_scope_core(
    ctx: &Ctx,
    q: &str,
    scope: &str,
    include_global: bool,
    limit: i64,
    query_embed: Option<(&str, &[f32])>,
) -> Result<(Vec<Value>, &'static str, Option<String>), String> {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .take(12)
        .map(|t| format!("\"{t}\""))
        .collect();
    let limit = limit.clamp(1, 50);
    let valid_now = crate::memory_ledger::is_valid_now_sql_clause();
    let active_embed_model = query_embed
        .map(|(mv, _)| mv.to_string())
        .or_else(|| resolve_embedding_config(ctx).map(|c| c.model_version));

    if terms.is_empty() {
        let sql = format!(
            "SELECT m.id,m.scope,m.category,m.content,m.trust_tier,m.status,m.fact_key,m.supersedes_id,m.source,m.source_event_id,m.provenance,m.created_at,m.updated_at,m.valid_from,m.valid_to,m.title,m.files
             FROM memories m
             WHERE m.active=1 AND m.status='active' AND ({valid_now}) AND ((?3 AND m.scope='global') OR m.scope=?1)
             ORDER BY (m.scope=?1) DESC, COALESCE(m.updated_at, m.created_at) DESC LIMIT ?2"
        );
        let rows = ctx.db.query(&sql, params![scope, limit, include_global])?;
        return Ok((rows, "browse", active_embed_model));
    }

    // 1. Lexical candidates (SQLite FTS5 BM25), strictly ownership-filtered BEFORE ranking.
    // Pure BM25 ordering — zero provenance/trust weighting in retrieval (Utility Under Attack / blueprint §3).
    let bm25_sql = format!(
        "SELECT m.id,m.scope,m.category,m.content,m.trust_tier,m.status,m.fact_key,m.supersedes_id,m.source,m.source_event_id,m.provenance,m.created_at,m.updated_at,m.valid_from,m.valid_to,m.title,m.files
         FROM memories_fts f JOIN memories m ON m.rowid=f.rowid
         WHERE memories_fts MATCH ?1 AND m.active=1 AND m.status='active' AND ({valid_now}) AND ((?3 AND m.scope='global') OR m.scope=?2)
         ORDER BY bm25(memories_fts) LIMIT ?4"
    );
    let bm25_rows = ctx.db.query(
        &bm25_sql,
        params![terms.join(" OR "), scope, include_global, RRF_POOL_LIMIT],
    )?;

    // 2. Real neural dense cosine candidates over ALL cached active scope vectors for the pinned model_version.
    let mut dense_scored: Vec<(f32, Value)> = Vec::new();
    let mut used_dense = false;
    if let Some((model_version, q_vec)) = query_embed {
        let cached_vecs = load_scope_vectors_cached(ctx, scope, include_global, model_version)?;
        if !cached_vecs.is_empty() {
            used_dense = true;
            let mut sims: Vec<(f32, String)> = cached_vecs
                .iter()
                .filter_map(|(id, vec)| {
                    let sim = cosine(q_vec, vec);
                    (sim >= NEURAL_COSINE_MIN).then_some((sim, id.clone()))
                })
                .collect();
            sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            sims.truncate(RRF_POOL_LIMIT as usize);

            if !sims.is_empty() {
                let mut by_id: HashMap<String, Value> = HashMap::with_capacity(sims.len());
                for chunk in sims.chunks(64) {
                    let placeholders = vec!["?"; chunk.len()].join(",");
                    let sql = format!(
                        "SELECT id,scope,category,content,trust_tier,status,fact_key,supersedes_id,source,source_event_id,provenance,created_at,updated_at,title,files
                         FROM memories
                         WHERE id IN ({placeholders}) AND active=1 AND status='active' AND ((? AND scope='global') OR scope=?)"
                    );
                    let mut p: Vec<rusqlite::types::Value> = chunk
                        .iter()
                        .map(|(_, id)| rusqlite::types::Value::Text(id.clone()))
                        .collect();
                    p.push(rusqlite::types::Value::Integer(include_global as i64));
                    p.push(rusqlite::types::Value::Text(scope.to_string()));
                    if let Ok(rows) = ctx.db.query(&sql, rusqlite::params_from_iter(p)) {
                        for r in rows {
                            if let Some(id) = r["id"].as_str() {
                                by_id.insert(id.to_string(), r);
                            }
                        }
                    }
                }
                for (sim, id) in sims {
                    if let Some(row) = by_id.remove(&id) {
                        dense_scored.push((sim, row));
                    }
                }
            }
        }
    }

    // 3. Pure Reciprocal Rank Fusion (k=60, AgentMemRec / BRIDGE) — zero provenance bonuses.
    let mut scores: HashMap<String, (f64, usize, Value)> = HashMap::new();
    for (rank, mut r) in bm25_rows.into_iter().enumerate() {
        let Some(id) = r["id"].as_str().map(str::to_string) else { continue };
        let s = 1.0 / (RRF_K + (rank as f64) + 1.0);
        r["retrieval"] = json!("bm25");
        scores.insert(id, (s, rank, r));
    }
    if used_dense {
        for (rank, (sim, mut r)) in dense_scored.into_iter().enumerate() {
            let Some(id) = r["id"].as_str().map(str::to_string) else { continue };
            let contrib = 1.0 / (RRF_K + (rank as f64) + 1.0);
            if let Some((cur_s, _, cur_v)) = scores.get_mut(&id) {
                *cur_s += contrib;
                cur_v["retrieval"] = json!("hybrid_rrf");
                cur_v["cosine"] = json!((sim * 1000.0).round() / 1000.0);
            } else {
                r["retrieval"] = json!("dense");
                r["cosine"] = json!((sim * 1000.0).round() / 1000.0);
                scores.insert(id, (contrib, 10_000 + rank, r));
            }
        }
    }

    // 4. Hard post-retrieval ownership gate (defense in depth per MemLeak).
    let mut merged: Vec<(f64, usize, Value)> = scores
        .into_values()
        .filter(|(_, _, v)| {
            let sc = v["scope"].as_str().unwrap_or("");
            v["status"].as_str() == Some("active") && (sc == scope || (include_global && sc == "global"))
        })
        .collect();
    merged.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });

    let mode = if used_dense { "hybrid_bm25_dense_rrf" } else { "bm25_only" };
    let embed_reported = if used_dense { active_embed_model } else { None };
    Ok((
        merged.into_iter().take(limit as usize).map(|(_, _, v)| v).collect(),
        mode,
        embed_reported,
    ))
}

#[cfg(test)]
pub fn memory_save(ctx: &Ctx, content: &str, category: &str, project: Option<&str>, by: &str) -> Result<Value, String> {
    let scope = project.map(|p| format!("project:{p}")).unwrap_or("global".into());
    let trust = if by == "user" { "user_direct" } else { "agent_inferred" };
    let source = if by == "user" { "user".to_string() } else { format!("agent:{by}") };
    save_structured(ctx, SaveOpts {
        content,
        category,
        scope: &scope,
        source: &source,
        trust_tier: trust,
        fact_key: None,
        supersedes_id: None,
        source_event_id: None,
        provenance: None,
    })
}

#[cfg(test)]
pub fn save_agent_memory(ctx: &Ctx, content: &str, category: &str, repo_id: &str, by: &str) -> Result<Value, String> {
    save_agent_memory_rich(ctx, content, category, repo_id, by, None, None, None, None)
}

/// Agent-facing memory write: strictly repo-scoped and trust-tier clamped (agents can never forge `user_direct`).
#[allow(clippy::too_many_arguments)]
pub fn save_agent_memory_rich(
    ctx: &Ctx,
    content: &str,
    category: &str,
    repo_id: &str,
    by: &str,
    fact_key: Option<&str>,
    supersedes_id: Option<&str>,
    requested_trust: Option<&str>,
    provenance: Option<&str>,
) -> Result<Value, String> {
    let clamped_trust = match requested_trust {
        Some("verified_run") => "verified_run",
        Some("untrusted") => "untrusted",
        _ => "agent_inferred",
    };
    save_structured(ctx, SaveOpts {
        content,
        category,
        scope: &format!("repo:{repo_id}"),
        source: &format!("agent:{by}"),
        trust_tier: clamped_trust,
        fact_key,
        supersedes_id,
        source_event_id: None,
        provenance,
    })
}

/// Attach title/files index metadata to an existing memory (same cleaning as on save).
pub fn set_memory_meta(ctx: &Ctx, id: &str, meta: &MemoryMeta) -> Result<(), String> {
    let (title, files) = clean_meta(meta);
    if title.is_none() && files.is_none() {
        return Ok(());
    }
    ctx.db.exec(
        "UPDATE memories SET title=COALESCE(?2,title), files=COALESCE(?3,files) WHERE id=?1",
        params![id, title, files],
    )?;
    Ok(())
}

/// Promote a `proposed` or `quarantined` memory to `active` (operator action), superseding any prior
/// active record with the same `fact_key` or `supersedes_id`.
pub fn promote_memory(ctx: &Ctx, id: &str) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT id, scope, content, fact_key, supersedes_id FROM memories WHERE id=?1 AND status != 'deleted'",
        [id],
    )?;
    let row = rows.first().ok_or_else(|| "memory not found or deleted".to_string())?;
    let scope = row["scope"].as_str().unwrap_or("global").to_string();
    let content = row["content"].as_str().unwrap_or("").to_string();
    let fact_key = row["fact_key"].as_str().map(str::to_string);
    let supersedes_id = row["supersedes_id"].as_str().map(str::to_string);

    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        if let Some(ref fk) = fact_key {
            let superseded: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT id FROM memories WHERE scope=?1 AND fact_key=?2 AND id != ?3 AND active=1 AND status='active'",
                )?;
                let iter = st.query_map(params![scope, fk, id], |r| r.get::<_, String>(0))?;
                iter.flatten().collect()
            };
            for old_id in superseded {
                tx.execute(
                    "UPDATE memories SET active=0, status='superseded', updated_at=datetime('now') WHERE id=?1",
                    [&old_id],
                )?;
                let _ = tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", [&old_id]);
            }
        }
        if let Some(ref sid) = supersedes_id {
            tx.execute(
                "UPDATE memories SET active=0, status='superseded', updated_at=datetime('now') WHERE id=?1 AND id != ?2",
                params![sid, id],
            )?;
            let _ = tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", [sid]);
        }
        tx.execute(
            "UPDATE memories SET active=1, status='active', trust_tier='user_direct', updated_at=datetime('now') WHERE id=?1",
            [id],
        )?;
        tx.commit()
    })?;
    invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(json!({ "id": id, "status": "active", "trust_tier": "user_direct", "content": content }))
}

/// Promote a memory and immediately index its neural vector when an embedding backend is active.
pub async fn promote_memory_async(ctx: &Arc<Ctx>, id: &str) -> Result<Value, String> {
    let (c, mid) = (ctx.clone(), id.to_string());
    let mut res = tokio::task::spawn_blocking(move || promote_memory(&c, &mid))
        .await
        .map_err(|e| e.to_string())??;
    if let Some(content) = res["content"].as_str().filter(|s| !s.is_empty()) {
        if let Ok(indexed) = embed_memory_async(ctx, id, content).await {
            res["vector_indexed"] = json!(indexed);
        }
    }
    if let Some(obj) = res.as_object_mut() {
        obj.remove("content");
    }
    Ok(res)
}

pub fn quarantine_memory(ctx: &Ctx, id: &str, reason: Option<&str>) -> Result<Value, String> {
    let prov = reason.filter(|s| !s.trim().is_empty()).map(|r| format!("quarantined: {}", r.trim()));
    if let Some(p) = prov {
        ctx.db.exec(
            "UPDATE memories SET active=0, status='quarantined', trust_tier='untrusted', provenance=?2, updated_at=datetime('now') WHERE id=?1",
            params![id, p],
        )?;
    } else {
        ctx.db.exec(
            "UPDATE memories SET active=0, status='quarantined', trust_tier='untrusted', updated_at=datetime('now') WHERE id=?1",
            [id],
        )?;
    }
    let _ = ctx.db.exec("DELETE FROM memory_vectors WHERE memory_id=?1", [id]);
    invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(json!({ "id": id, "status": "quarantined" }))
}

/// Hard deletion scrub (LEDGER+EP): marks deleted, blanks content so FTS5 erases the entry, deletes
/// vectors, invalidates caches, and scrubs the referenced raw event record so deleted values can
/// never resurface in queries, slates, or raw-event search.
pub fn forget_memory(ctx: &Ctx, id: &str) -> Result<(), String> {
    let existing = ctx
        .db
        .query("SELECT source_event_id FROM memories WHERE id=?1", [id])
        .unwrap_or_default();
    let source_evt = existing.first().and_then(|r| r["source_event_id"].as_str()).map(str::to_string);

    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute(
            "UPDATE memories SET active=0, status='deleted', content='', provenance='deleted', source_event_id=NULL, updated_at=datetime('now') WHERE id=?1",
            [id],
        )?;
        tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", [id])?;
        if let Some(ref eid) = source_evt {
            if let Some(rowid) = eid.strip_prefix("evt:").and_then(|s| s.parse::<i64>().ok()) {
                let _ = tx.execute(
                    "UPDATE events SET body=?2 WHERE id=?1",
                    params![rowid, json!({ "scrubbed": true, "memory_id": id }).to_string()],
                );
            }
        }
        let like_pattern = format!("%{id}%");
        let _ = tx.execute(
            "UPDATE events SET body=?2 WHERE kind IN ('memory:write','memory:extract','memory:session_observe') AND body LIKE ?1",
            params![like_pattern, json!({ "scrubbed": true, "memory_id": id }).to_string()],
        );
        tx.commit()
    })?;
    invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(())
}

// ---------- Explicit Prospective Intentions (Pro-Memory / Price of Scale) ----------

/// Per-agent pending reminders delivered on the next MCP tool call (e.g. compaction / turn error)
/// and per-agent fired `path_match` deduplication set so path reminders fire once per session until compaction.
fn pending_agent_reminders() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static Q: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    Q.get_or_init(Default::default)
}

fn fired_path_reminders() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static F: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

pub fn push_agent_reminder(agent_id: &str, text: String) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut map = pending_agent_reminders().lock().unwrap_or_else(|e| e.into_inner());
    let entry = map.entry(agent_id.to_string()).or_default();
    if !entry.iter().any(|existing| existing == trimmed) && entry.len() < 8 {
        entry.push(trimmed.to_string());
    }
}

pub fn take_agent_reminders(agent_id: &str) -> Option<String> {
    let mut map = pending_agent_reminders().lock().unwrap_or_else(|e| e.into_inner());
    let items = map.remove(agent_id)?;
    if items.is_empty() {
        return None;
    }
    Some(truncate_utf8(&items.join("\n"), MAX_REMINDERS_PROMPT_BYTES))
}

pub fn reset_agent_path_reminders(agent_id: &str) {
    fired_path_reminders().lock().unwrap_or_else(|e| e.into_inner()).remove(agent_id);
}

pub fn clear_agent_reminder_state(agent_id: &str) {
    pending_agent_reminders().lock().unwrap_or_else(|e| e.into_inner()).remove(agent_id);
    fired_path_reminders().lock().unwrap_or_else(|e| e.into_inner()).remove(agent_id);
}

pub fn intention_list(ctx: &Ctx, scope: Option<&str>) -> Result<Value, String> {
    Ok(json!(ctx.db.query(
        "SELECT id, scope, trigger_kind, trigger_match, instruction, active, created_by, created_at
         FROM intentions
         WHERE active=1 AND (?1 IS NULL OR scope=?1 OR scope='global')
         ORDER BY created_at DESC",
        params![scope.filter(|s| !s.is_empty())],
    )?))
}

pub fn intention_add(
    ctx: &Ctx,
    scope: &str,
    trigger_kind: &str,
    trigger_match: &str,
    instruction: &str,
    created_by: &str,
) -> Result<Value, String> {
    if !matches!(
        trigger_kind,
        "pre_release" | "on_pr" | "on_error" | "on_session_start" | "path_match" | "on_compact"
    ) {
        return Err(
            "trigger_kind must be pre_release, on_pr, on_error, on_session_start, path_match or on_compact".into(),
        );
    }
    let inst = instruction.trim();
    if inst.is_empty() || inst.len() > 1000 {
        return Err("instruction must be 1-1000 characters".into());
    }
    let id = format!("int_{}", crate::db::new_id());
    ctx.db.exec(
        "INSERT INTO intentions(id, scope, trigger_kind, trigger_match, instruction, active, created_by, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, datetime('now'))",
        params![id, scope, trigger_kind, trigger_match.trim(), inst, created_by],
    )?;
    ctx.notify("library");
    Ok(json!({
        "id": id,
        "scope": scope,
        "trigger_kind": trigger_kind,
        "trigger_match": trigger_match.trim(),
        "instruction": inst,
    }))
}

pub fn intention_remove(ctx: &Ctx, id: &str) -> Result<(), String> {
    ctx.db.exec("UPDATE intentions SET active=0 WHERE id=?1", [id])?;
    ctx.notify("library");
    Ok(())
}

/// Active intentions matching `repo_id` (or global only when `repo_id` is None) and `trigger_kinds`.
/// If `context_haystack` is Some, intentions with a non-empty `trigger_match` must match it
/// case-insensitively. For `path_match`, a non-empty `trigger_match` is required when haystack is checked.
pub fn matching_intentions(
    ctx: &Ctx,
    repo_id: Option<&str>,
    trigger_kinds: &[&str],
    context_haystack: Option<&str>,
) -> Vec<Value> {
    if trigger_kinds.is_empty() {
        return vec![];
    }
    let repo_scope = repo_id.filter(|s| !s.is_empty()).map(|r| format!("repo:{r}"));
    let rows = match repo_scope {
        Some(ref rs) => ctx.db.query(
            "SELECT id, scope, trigger_kind, trigger_match, instruction, created_by, created_at
             FROM intentions
             WHERE active=1 AND (scope=?1 OR scope='global')
             ORDER BY (scope=?1) DESC, created_at DESC LIMIT 40",
            params![rs],
        ),
        None => ctx.db.query(
            "SELECT id, scope, trigger_kind, trigger_match, instruction, created_by, created_at
             FROM intentions
             WHERE active=1 AND scope='global'
             ORDER BY created_at DESC LIMIT 40",
            [],
        ),
    }
    .unwrap_or_default();
    let hay_low = context_haystack.map(|h| h.to_lowercase());
    rows.into_iter()
        .filter(|r| {
            let kind = r["trigger_kind"].as_str().unwrap_or("");
            if !trigger_kinds.contains(&kind) {
                return false;
            }
            let tm = r["trigger_match"].as_str().unwrap_or("").trim();
            if tm.is_empty() {
                // `path_match` with empty trigger_match would fire on every path; ignore empty match unless none-haystack.
                return kind != "path_match" || hay_low.is_none();
            }
            match &hay_low {
                Some(h) => h.contains(&tm.to_lowercase()),
                None => true,
            }
        })
        .collect()
}

/// Bounded prompt block of active `on_session_start` reminders for trusted Standard agents ONLY.
pub fn session_start_reminders_prompt(ctx: &Ctx, repo_id: Option<&str>) -> Option<String> {
    let items = matching_intentions(ctx, repo_id, &["on_session_start"], None);
    if items.is_empty() {
        return None;
    }
    let mut out = String::from("\n## Session start reminders\n");
    for item in items {
        let inst = item["instruction"].as_str().unwrap_or("").trim();
        if inst.is_empty() {
            continue;
        }
        let tm = item["trigger_match"].as_str().unwrap_or("").trim();
        if tm.is_empty() {
            out.push_str(&format!("- {inst}\n"));
        } else {
            out.push_str(&format!("- [{tm}] {inst}\n"));
        }
    }
    if out == "\n## Session start reminders\n" {
        return None;
    }
    Some(truncate_utf8(&out, MAX_REMINDERS_PROMPT_BYTES))
}

/// Matching `on_pr` / `pre_release` reminders surfaced to trusted Standard agents on `board_submit`.
pub fn submit_reminders(ctx: &Ctx, repo_id: Option<&str>, haystack: &str) -> Vec<Value> {
    matching_intentions(ctx, repo_id, &["on_pr", "pre_release"], Some(haystack))
        .into_iter()
        .map(|r| {
            json!({
                "id": r["id"],
                "scope": r["scope"],
                "trigger_kind": r["trigger_kind"],
                "trigger_match": r["trigger_match"],
                "instruction": r["instruction"],
            })
        })
        .collect()
}

/// Matching `on_error` reminders surfaced when a tool/shell command or turn fails (Standard agents only).
pub fn error_reminders(ctx: &Ctx, repo_id: Option<&str>, error_haystack: &str) -> Vec<Value> {
    matching_intentions(ctx, repo_id, &["on_error"], Some(error_haystack))
}

/// Matching `path_match` reminders deduplicated per `(agent_id, intention_id)` until compaction or stop.
pub fn path_reminders_once(ctx: &Ctx, agent_id: &str, repo_id: Option<&str>, path_haystack: &str) -> Vec<Value> {
    if path_haystack.trim().is_empty() {
        return vec![];
    }
    let matched = matching_intentions(ctx, repo_id, &["path_match"], Some(path_haystack));
    if matched.is_empty() {
        return vec![];
    }
    let mut fired_map = fired_path_reminders().lock().unwrap_or_else(|e| e.into_inner());
    let seen = fired_map.entry(agent_id.to_string()).or_default();
    let mut fresh = Vec::new();
    for item in matched {
        let Some(id) = item["id"].as_str() else { continue };
        if seen.insert(id.to_string()) {
            fresh.push(item);
        }
    }
    fresh
}

/// Matching `on_compact` reminders fired on `context.compacted` events (Standard agents only).
pub fn compaction_reminders(ctx: &Ctx, repo_id: Option<&str>) -> Vec<Value> {
    matching_intentions(ctx, repo_id, &["on_compact"], None)
}

pub fn format_trigger_reminders(kind: &str, items: &[Value]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for item in items {
        let inst = item["instruction"].as_str().unwrap_or("").trim();
        if inst.is_empty() {
            continue;
        }
        let tm = item["trigger_match"].as_str().unwrap_or("").trim();
        if tm.is_empty() {
            lines.push(format!("[Reminder ({kind})] {inst}"));
        } else {
            lines.push(format!("[Reminder ({kind}:{tm})] {inst}"));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(truncate_utf8(&lines.join("\n"), MAX_REMINDERS_PROMPT_BYTES))
    }
}

// ---------- Offline Consolidation (v3 preview with REAL safety + recall assertions) ----------

fn normalized_text(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn distinctive_tokens(s: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "that", "with", "this", "from", "have", "must", "should", "when", "into", "only",
        "always", "never", "your", "their", "about", "after", "before", "every", "still", "work", "mode",
    ];
    let mut toks: Vec<String> = s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 4 && !STOP.contains(t))
        .map(str::to_string)
        .collect();
    toks.sort();
    toks.dedup();
    toks
}

/// Eval-gated offline consolidation preview (Useful Memories Become Faulty #1 + Anthropic Dreams):
/// Reads active memories read-only, detects exact-normalized duplicates (or neural cosine >= 0.90 when
/// real embeddings are present), and runs REAL Tier-0 safety + recall preservation assertions before
/// setting `eval_report.passed`. NEVER mutates live memories until explicitly promoted.
pub fn consolidate_preview(ctx: &Ctx, scope: &str) -> Result<Value, String> {
    let embed_cfg = resolve_embedding_config(ctx);
    let rows = ctx.db.query(
        "SELECT id, category, content, trust_tier, fact_key, created_at FROM memories
         WHERE scope=?1 AND active=1 AND status='active'
         ORDER BY (trust_tier='user_direct') DESC, created_at DESC",
        [scope],
    )?;

    let vec_map: HashMap<String, Vec<f32>> = match &embed_cfg {
        Some(cfg) => load_scope_vectors_cached(ctx, scope, false, &cfg.model_version)?
            .into_iter()
            .collect(),
        None => HashMap::new(),
    };

    let mut proposals: Vec<Value> = vec![];
    let mut seen_dup: HashSet<String> = HashSet::new();
    let mut mode = "exact_normalized";

    for i in 0..rows.len() {
        for j in (i + 1)..rows.len() {
            let r_keep = &rows[i];
            let r_drop = &rows[j];
            let (Some(id_keep), Some(c_keep)) = (r_keep["id"].as_str(), r_keep["content"].as_str()) else { continue };
            let (Some(id_drop), Some(c_drop)) = (r_drop["id"].as_str(), r_drop["content"].as_str()) else { continue };
            if seen_dup.contains(id_drop) {
                continue;
            }
            // Safety invariant: never propose superseding a user_direct decision automatically.
            if r_drop["trust_tier"].as_str() == Some("user_direct") {
                continue;
            }
            let exact = normalized_text(c_keep) == normalized_text(c_drop);
            let sim_opt = match (vec_map.get(id_keep), vec_map.get(id_drop)) {
                (Some(vk), Some(vd)) => {
                    mode = "embedding_cosine";
                    Some(cosine(vk, vd))
                }
                _ => None,
            };
            let matched = exact || sim_opt.is_some_and(|s| s >= CONSOLIDATE_COSINE_MIN);
            if matched {
                seen_dup.insert(id_drop.to_string());
                let sim_val = sim_opt.unwrap_or(1.0);
                proposals.push(json!({
                    "kind": "deduplicate",
                    "keep_id": id_keep,
                    "supersede_id": id_drop,
                    "similarity": (sim_val * 1000.0).round() / 1000.0,
                    "match_basis": if exact { "exact_normalized" } else { "embedding_cosine" },
                    "keep_preview": c_keep.chars().take(140).collect::<String>(),
                    "drop_preview": c_drop.chars().take(140).collect::<String>(),
                }));
            }
        }
    }

    // ---- REAL Tier-0 Safety & Recall Preservation Gate (never hardcoded!) ----
    let mut user_direct_preserved = true;
    let mut no_drop_cycle = true;
    let mut recall_preserved = true;
    let mut recall_failures: Vec<Value> = vec![];

    let keep_ids: HashSet<&str> = proposals.iter().filter_map(|p| p["keep_id"].as_str()).collect();
    let drop_ids: HashSet<&str> = proposals.iter().filter_map(|p| p["supersede_id"].as_str()).collect();
    if keep_ids.iter().any(|k| drop_ids.contains(k)) {
        no_drop_cycle = false;
    }

    let content_by_id: HashMap<&str, (&str, &str)> = rows
        .iter()
        .filter_map(|r| Some((r["id"].as_str()?, (r["content"].as_str()?, r["trust_tier"].as_str()?))))
        .collect();

    for p in &proposals {
        let (Some(kid), Some(did)) = (p["keep_id"].as_str(), p["supersede_id"].as_str()) else {
            recall_preserved = false;
            continue;
        };
        if let Some((_, tier)) = content_by_id.get(did) {
            if *tier == "user_direct" {
                user_direct_preserved = false;
            }
        } else {
            user_direct_preserved = false;
        }
        // Recall preservation check: distinctive tokens from dropped memory must still retrieve `keep_id`.
        if let (Some((c_keep, _)), Some((c_drop, _))) = (content_by_id.get(kid), content_by_id.get(did)) {
            let drop_toks = distinctive_tokens(c_drop);
            if !drop_toks.is_empty() {
                let keep_low = c_keep.to_lowercase();
                let missing_toks: Vec<&String> = drop_toks.iter().filter(|t| !keep_low.contains(t.as_str())).collect();
                // Require at least 80% distinctive-token coverage AND actual FTS retrieval of keep_id.
                let coverage = 1.0 - (missing_toks.len() as f64 / drop_toks.len() as f64);
                let probe_q = drop_toks.iter().take(6).cloned().collect::<Vec<_>>().join(" ");
                let fts_hits = search_scope_core(ctx, &probe_q, scope, false, 5, None)
                    .map(|(h, _, _)| h.iter().any(|r| r["id"].as_str() == Some(kid)))
                    .unwrap_or(false);
                if coverage < 0.80 || !fts_hits {
                    recall_preserved = false;
                    recall_failures.push(json!({
                        "keep_id": kid,
                        "supersede_id": did,
                        "token_coverage": (coverage * 100.0).round() / 100.0,
                        "fts_hit": fts_hits,
                    }));
                }
            }
        } else {
            recall_preserved = false;
        }
    }

    let unscrubbed_deleted = ctx
        .db
        .query("SELECT count(*) AS n FROM memories WHERE status='deleted' AND content != ''", [])?
        .first()
        .and_then(|r| r["n"].as_i64())
        .unwrap_or(1);
    let dangling_deleted_vecs = ctx
        .db
        .query(
            "SELECT count(*) AS n FROM memory_vectors v JOIN memories m ON m.id=v.memory_id WHERE m.status='deleted'",
            [],
        )?
        .first()
        .and_then(|r| r["n"].as_i64())
        .unwrap_or(1);
    let deleted_scrub_intact = unscrubbed_deleted == 0 && dangling_deleted_vecs == 0;

    let tier0_passed = user_direct_preserved && deleted_scrub_intact && no_drop_cycle && recall_preserved;
    let replay_eval = crate::memory_consolidation::run_replay_eval_gate(ctx, scope, &proposals)?;
    let replay_passed = replay_eval["passed"].as_bool().unwrap_or(false);
    let passed = tier0_passed && replay_passed;

    let eval_report = json!({
        "passed": passed,
        "assertions": {
            "user_direct_preserved": user_direct_preserved,
            "deleted_scrub_intact": deleted_scrub_intact,
            "no_drop_cycle": no_drop_cycle,
            "recall_preserved": recall_preserved,
            "replay_eval_passed": replay_passed,
        },
        "recall_failures": recall_failures,
        "replay_eval": replay_eval,
        "v3_replay_eval": if replay_passed { "passed" } else { "failed" },
        "mode": mode,
        "checked_records": rows.len(),
        "duplicates_found": proposals.len(),
        "embed_model": embed_cfg.map(|c| c.model_version),
    });
    let run_id = format!("con_{}", crate::db::new_id());
    ctx.db.exec(
        "INSERT INTO consolidation_runs(id, scope, state, proposals, eval_report, created_at)
         VALUES(?1, ?2, 'preview', ?3, ?4, datetime('now'))",
        params![run_id, scope, json!(proposals).to_string(), eval_report.to_string()],
    )?;
    Ok(json!({
        "id": run_id,
        "scope": scope,
        "state": "preview",
        "proposals": proposals,
        "eval_report": eval_report,
    }))
}

pub fn consolidate_promote(ctx: &Ctx, run_id: &str) -> Result<Value, String> {
    let row = ctx
        .db
        .query("SELECT id, scope, state, proposals, eval_report FROM consolidation_runs WHERE id=?1", [run_id])?
        .into_iter()
        .next()
        .ok_or_else(|| "consolidation run not found".to_string())?;
    if row["state"].as_str() == Some("promoted") {
        return Ok(json!({ "id": run_id, "state": "promoted", "applied": 0 }));
    }
    let eval: Value = serde_json::from_str(row["eval_report"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    if eval["passed"].as_bool() != Some(true) {
        return Err("Refusing to promote consolidation run that did not pass the eval gate".into());
    }
    let scope_str = row["scope"].as_str().unwrap_or("global").to_string();
    let version_id = crate::memory_consolidation::snapshot_scope_version(ctx, &scope_str, Some(run_id))?;
    let proposals: Vec<Value> = serde_json::from_str(row["proposals"].as_str().unwrap_or("[]")).unwrap_or_default();
    let mut applied = 0usize;
    let mut superseded_ids = Vec::new();
    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        for p in &proposals {
            if let Some(sid) = p["supersede_id"].as_str() {
                let n = tx.execute(
                    "UPDATE memories SET active=0, status='superseded', provenance=COALESCE(provenance,'') || ' [consolidated:' || ?2 || ']', updated_at=datetime('now')
                     WHERE id=?1 AND trust_tier != 'user_direct' AND status='active'",
                    params![sid, run_id],
                )?;
                if n > 0 {
                    let _ = tx.execute("DELETE FROM memory_vectors WHERE memory_id=?1", [sid]);
                    superseded_ids.push(sid.to_string());
                }
                applied += n;
            }
        }
        tx.execute(
            "UPDATE consolidation_runs SET state='promoted', promoted_at=datetime('now') WHERE id=?1",
            [run_id],
        )?;
        tx.commit()
    })?;
    let _ = crate::memory_ledger::close_superseded_validity(ctx, &superseded_ids, "");
    invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(json!({ "id": run_id, "version_id": version_id, "state": "promoted", "applied": applied }))
}

// ---------- Structured Working Notes (Partial-Patch Semantics) ----------

/// Structured per-repo / per-agent working-state handoff note (Field Guide / BRIDGE).
pub fn working_note_get(ctx: &Ctx, scope: &str) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT scope, requirements, decisions, failed_attempts, next_steps, source_event_id, updated_by, updated_at
         FROM working_notes WHERE scope=?1",
        [scope],
    )?;
    Ok(rows.into_iter().next().unwrap_or_else(|| {
        json!({
            "scope": scope,
            "requirements": "",
            "decisions": "",
            "failed_attempts": "",
            "next_steps": "",
            "source_event_id": null,
            "updated_by": null,
            "updated_at": null,
        })
    }))
}

/// Partial-patch update for a working note: omitted (`None`) fields preserve existing values instead
/// of overwriting with `""`. Pass `Some("")` explicitly to clear a field.
#[allow(clippy::too_many_arguments)]
pub fn working_note_patch(
    ctx: &Ctx,
    scope: &str,
    requirements: Option<&str>,
    decisions: Option<&str>,
    failed_attempts: Option<&str>,
    next_steps: Option<&str>,
    source_event_id: Option<&str>,
    updated_by: &str,
) -> Result<Value, String> {
    let existing = working_note_get(ctx, scope)?;
    let clip = |s: &str| -> String { s.trim().chars().take(2000).collect() };
    let pick = |opt: Option<&str>, key: &str| -> String {
        match opt {
            Some(v) => clip(v),
            None => existing[key].as_str().unwrap_or("").to_string(),
        }
    };
    let req = pick(requirements, "requirements");
    let dec = pick(decisions, "decisions");
    let fail = pick(failed_attempts, "failed_attempts");
    let next = pick(next_steps, "next_steps");

    let owned_event_id = match source_event_id.filter(|s| !s.trim().is_empty()) {
        Some(eid) => Some(eid.to_string()),
        None => {
            let agent_tag = updated_by.strip_prefix("agent:").or_else(|| updated_by.strip_prefix("pipeline:"));
            let patched_fields: Vec<&str> = [
                requirements.map(|_| "requirements"),
                decisions.map(|_| "decisions"),
                failed_attempts.map(|_| "failed_attempts"),
                next_steps.map(|_| "next_steps"),
            ]
            .into_iter()
            .flatten()
            .collect();
            ctx.log_event_id(
                "working_note:write",
                agent_tag,
                &json!({
                    "scope": scope,
                    "updated_by": updated_by,
                    "patched_fields": patched_fields,
                }),
            )
        }
    };

    ctx.db.exec(
        "INSERT INTO working_notes(scope, requirements, decisions, failed_attempts, next_steps, source_event_id, updated_by, updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,datetime('now'))
         ON CONFLICT(scope) DO UPDATE SET
           requirements=excluded.requirements,
           decisions=excluded.decisions,
           failed_attempts=excluded.failed_attempts,
           next_steps=excluded.next_steps,
           source_event_id=COALESCE(excluded.source_event_id, working_notes.source_event_id),
           updated_by=excluded.updated_by,
           updated_at=datetime('now')",
        params![scope, req, dec, fail, next, owned_event_id, updated_by],
    )?;
    ctx.notify("library");
    working_note_get(ctx, scope)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn working_note_set(
    ctx: &Ctx,
    scope: &str,
    requirements: &str,
    decisions: &str,
    failed_attempts: &str,
    next_steps: &str,
    source_event_id: Option<&str>,
    updated_by: &str,
) -> Result<Value, String> {
    working_note_patch(
        ctx,
        scope,
        Some(requirements),
        Some(decisions),
        Some(failed_attempts),
        Some(next_steps),
        source_event_id,
        updated_by,
    )
}

/// Truncate `s` cleanly on a UTF-8 boundary so the resulting string is at most `max_bytes` bytes.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let suffix = "\n... [truncated]\n";
    if max_bytes <= suffix.len() {
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        return s[..end].to_string();
    }
    let mut end = max_bytes - suffix.len();
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &s[..end])
}

/// Concise, size-bounded (<= 2048 bytes) prompt block for a repo's working handoff note (if non-empty).
pub fn working_note_prompt(ctx: &Ctx, repo_id: &str) -> Option<String> {
    let note = working_note_get(ctx, &format!("repo:{repo_id}")).ok()?;
    let req = note["requirements"].as_str().unwrap_or("").trim();
    let dec = note["decisions"].as_str().unwrap_or("").trim();
    let fail = note["failed_attempts"].as_str().unwrap_or("").trim();
    let next = note["next_steps"].as_str().unwrap_or("").trim();
    if req.is_empty() && dec.is_empty() && fail.is_empty() && next.is_empty() {
        return None;
    }
    let mut out = String::from("\n## Workspace working note (curated handoff)\n");
    if !req.is_empty() {
        out.push_str(&format!("- Requirements: {req}\n"));
    }
    if !dec.is_empty() {
        out.push_str(&format!("- Current decisions: {dec}\n"));
    }
    if !fail.is_empty() {
        out.push_str(&format!("- Known failed attempts: {fail}\n"));
    }
    if !next.is_empty() {
        out.push_str(&format!("- Next steps: {next}\n"));
    }
    Some(truncate_utf8(&out, MAX_WORKING_NOTE_PROMPT_BYTES))
}

// ---------- Per-Model Budget B & Zero-LLM Context Slate Builder ----------

fn estimate_tokens(s: &str) -> usize {
    // Fast ~4 chars/token approximation + small framing overhead.
    (s.chars().count() / 4).max(1) + 8
}

/// Per-model memory dose budget `B` (IBM #2 / Report §2).
/// Small models get compact budgets so they do not drown in long-tail memories; frontier models get larger budgets.
pub fn model_memory_budget(ctx: &Ctx, model_ref: Option<&str>) -> usize {
    if let Some(mr) = model_ref.filter(|s| !s.is_empty()) {
        if let Some(custom) = ctx
            .db
            .kv_get(&format!("setting.memory_budget.{mr}"))
            .and_then(|s| s.parse::<usize>().ok())
        {
            return custom.clamp(256, 32_000);
        }
        if let Some((pid, mid)) = crate::providers::split_ref(mr) {
            if let Some(cw) = crate::providers::context_window(ctx, pid, mid) {
                return if cw <= 32_768 {
                    1_500
                } else if cw <= 131_072 {
                    2_500
                } else {
                    4_000
                };
            }
        }
    }
    2_000
}

pub struct SlateOpts<'a> {
    pub repo_id: Option<&'a str>,
    pub query: &'a str,
    pub budget_tokens: usize,
    pub strict: bool,
    pub include_global: bool,
    pub due_triggers: &'a [&'a str],
}

/// Zero-LLM budgeted context slate builder (AgentMemRec + IBM dose budget + MemPoison cap + Trust Gap).
/// Hard-filters to authorized scope before and after retrieval, preserves RRF/BM25 relevance order
/// (no provenance weighting), includes ONLY due intentions counted within `budget_tokens`, caps
/// untrusted items, and greedily packs within `budget_tokens`.
#[cfg(test)]
pub fn build_context_slate(
    ctx: &Ctx,
    repo_id: Option<&str>,
    query: &str,
    budget_tokens: usize,
    strict: bool,
    include_global: bool,
) -> Result<Value, String> {
    build_context_slate_core(
        ctx,
        SlateOpts {
            repo_id,
            query,
            budget_tokens,
            strict,
            include_global,
            due_triggers: &["on_session_start"],
        },
        None,
    )
}

#[cfg(test)]
pub async fn build_context_slate_async(
    ctx: &Arc<Ctx>,
    repo_id: Option<&str>,
    query: &str,
    budget_tokens: usize,
    strict: bool,
    include_global: bool,
    due_triggers: Vec<String>,
) -> Result<Value, String> {
    build_context_slate_for_model_async(ctx, repo_id, query, budget_tokens, strict, include_global, due_triggers, None).await
}

#[allow(clippy::too_many_arguments)]
pub async fn build_context_slate_for_model_async(
    ctx: &Arc<Ctx>,
    repo_id: Option<&str>,
    query: &str,
    budget_tokens: usize,
    strict: bool,
    include_global: bool,
    due_triggers: Vec<String>,
    model_ref: Option<&str>,
) -> Result<Value, String> {
    let q_embed = if !strict { embed_query_cached(ctx, query).await } else { None };
    let (c, rid, q_owned, mref) = (
        ctx.clone(),
        repo_id.map(str::to_string),
        query.to_string(),
        model_ref.map(str::to_string),
    );
    let mut slate = tokio::task::spawn_blocking(move || {
        let trig_refs: Vec<&str> = if due_triggers.is_empty() {
            vec!["on_session_start"]
        } else {
            due_triggers.iter().map(String::as_str).collect()
        };
        let q_ref = q_embed.as_ref().map(|(mv, v)| (mv.as_str(), v.as_slice()));
        build_context_slate_core(
            &c,
            SlateOpts {
                repo_id: rid.as_deref(),
                query: &q_owned,
                budget_tokens,
                strict,
                include_global,
                due_triggers: &trig_refs,
            },
            q_ref,
        )
    })
    .await
    .map_err(|e| e.to_string())??;

    if strict {
        return Ok(slate);
    }

    // Cross-encoder rerank over selected/retrieved items when configured & query is non-empty.
    if !query.trim().is_empty() {
        if let Some(items) = slate["items"].as_array().cloned() {
            if items.len() > 1 {
                let top_n = items.len();
                let (reranked, active_reranker) =
                    crate::memory_ledger::rerank_candidates_async(ctx, query, items, top_n).await;
                if let Some(rr) = active_reranker {
                    slate["items"] = Value::Array(reranked);
                    slate["reranker"] = json!(rr);
                }
            }
        }
    }

    // Apply trained AgentMemRec learned feature ranker if active (pure retrieval features, zero provenance inflation).
    if !query.trim().is_empty() {
        let ranker = crate::memory_consolidation::active_ranker(ctx);
        if ranker["active"].as_bool() == Some(true) {
            if let Some(items) = slate["items"].as_array().cloned() {
                let q_low = query.to_lowercase();
                let scope_target = repo_id.map(|r| format!("repo:{r}")).unwrap_or_else(|| "global".into());
                let mut scored: Vec<(f64, usize, Value)> = items
                    .into_iter()
                    .enumerate()
                    .map(|(idx, item)| {
                        let rank_rrf = 1.0 / (RRF_K + (idx as f64) + 1.0);
                        let ret = item["retrieval"].as_str().unwrap_or("");
                        let f = crate::memory_consolidation::RankFeatures {
                            bm25_rrf: if matches!(ret, "bm25" | "hybrid_rrf") { rank_rrf } else { 0.0 },
                            dense_rrf: if matches!(ret, "dense" | "hybrid_rrf") { rank_rrf } else { 0.0 },
                            rerank_score: item["rerank_score"].as_f64().unwrap_or(0.0),
                            exact_key_match: item["fact_key"]
                                .as_str()
                                .filter(|k| !k.is_empty() && q_low.contains(&k.to_lowercase()))
                                .map(|_| 1.0)
                                .unwrap_or(0.0),
                            scope_local: if item["scope"].as_str() == Some(scope_target.as_str()) { 1.0 } else { 0.0 },
                            token_efficiency: 1.0 / (item["tokens"].as_f64().unwrap_or(32.0).max(8.0) / 32.0),
                        };
                        let s = crate::memory_consolidation::score_with_ranker(&ranker["weights"], &f);
                        (s, idx, item)
                    })
                    .collect();
                scored.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1.cmp(&b.1))
                });
                slate["items"] = Value::Array(scored.into_iter().map(|(_, _, v)| v).collect());
                slate["ranker_version"] = ranker["version"].clone();
            }
        }
    }

    // Attach per-model verified skill dose (IBM #2 / Zep Skills) for Standard repo-scoped slates.
    if let Some(rid) = repo_id.filter(|s| !s.is_empty()) {
        if let Ok(dose) = crate::memory_skills::select_skill_dose(ctx, rid, query, mref.as_deref()) {
            slate["procedures"] = dose["procedures"].clone();
            slate["skill_dose"] = json!({
                "max_procedures": dose["max_procedures"],
                "budget_tokens": dose["budget_tokens"],
                "used_tokens": dose["used_tokens"],
            });
        }
    }

    Ok(slate)
}

fn build_context_slate_core(
    ctx: &Ctx,
    opts: SlateOpts<'_>,
    query_embed: Option<(&str, &[f32])>,
) -> Result<Value, String> {
    let budget = opts.budget_tokens.clamp(256, 32_000);
    let repo_scope = opts.repo_id.filter(|s| !s.is_empty()).map(|r| format!("repo:{r}"));

    if opts.strict {
        return Ok(json!({
            "strict": true,
            "scope": repo_scope.unwrap_or_else(|| "none".into()),
            "embed_model": Value::Null,
            "retrieval": "disabled_strict",
            "budget_tokens": budget,
            "used_tokens": 0,
            "working_note": null,
            "intentions": [],
            "items": [],
            "excluded": [{ "reason": "Strict mode disables personal and repository memories" }],
            "counts": { "active": 0, "proposed": 0, "superseded": 0, "quarantined": 0 },
        }));
    }

    let scope_filter = repo_scope.clone().unwrap_or_else(|| "global".into());
    let include_g = opts.include_global || repo_scope.is_none();

    // Counts within the exact authorized scope (never leaks sibling repos).
    let count_sql = "SELECT status, count(*) AS n FROM memories WHERE status != 'deleted' AND ((?2 AND scope='global') OR scope=?1) GROUP BY status";
    let mut counts = serde_json::Map::from_iter([
        ("active".into(), json!(0)),
        ("proposed".into(), json!(0)),
        ("superseded".into(), json!(0)),
        ("quarantined".into(), json!(0)),
    ]);
    for r in ctx.db.query(count_sql, params![scope_filter, include_g])? {
        if let Some(st) = r["status"].as_str() {
            counts.insert(st.to_string(), r["n"].clone());
        }
    }

    // Ownership-gated candidate retrieval (active, non-superseded, non-deleted only), preserving RRF/BM25 rank.
    let (ranked, retrieval_mode, embed_model) =
        search_scope_core(ctx, opts.query, &scope_filter, include_g, 50, query_embed)?;

    // Working note takes priority inside the budget if present.
    let working_note = if let Some(ref rs) = repo_scope {
        let wn = working_note_get(ctx, rs)?;
        let non_empty = ["requirements", "decisions", "failed_attempts", "next_steps"]
            .iter()
            .any(|k| !wn[*k].as_str().unwrap_or("").trim().is_empty());
        if non_empty { Some(wn) } else { None }
    } else {
        None
    };

    let mut used = 0usize;
    if let Some(ref wn) = working_note {
        let wn_text = format!(
            "{} {} {} {}",
            wn["requirements"].as_str().unwrap_or(""),
            wn["decisions"].as_str().unwrap_or(""),
            wn["failed_attempts"].as_str().unwrap_or(""),
            wn["next_steps"].as_str().unwrap_or("")
        );
        used += estimate_tokens(&wn_text);
    }

    let mut excluded: Vec<Value> = vec![];

    // Due intentions only (never dump all active intentions unfiltered), and count them in `budget`.
    let hay = (!opts.query.trim().is_empty()).then_some(opts.query);
    let raw_intentions = matching_intentions(ctx, opts.repo_id, opts.due_triggers, hay);
    let mut due_intentions: Vec<Value> = vec![];
    for mut it in raw_intentions {
        let inst = it["instruction"].as_str().unwrap_or("");
        let cost = estimate_tokens(inst);
        if used + cost > budget {
            excluded.push(json!({
                "id": it["id"],
                "kind": "intention",
                "reason": "budget_exceeded",
                "tokens": cost,
            }));
            continue;
        }
        used += cost;
        it["tokens"] = json!(cost);
        due_intentions.push(it);
    }

    // Greedy fill to budget B in exact retrieval relevance order (NO provenance re-sorting!).
    let mut selected: Vec<Value> = vec![];
    let mut untrusted_used = 0usize;
    for mut item in ranked {
        let tier = item["trust_tier"].as_str().unwrap_or("agent_inferred");
        let content = item["content"].as_str().unwrap_or("");
        let cost = estimate_tokens(content);
        if tier == "untrusted" {
            if untrusted_used >= MAX_UNTRUSTED_PER_SLATE {
                excluded.push(json!({
                    "id": item["id"],
                    "reason": "untrusted_cap",
                    "tokens": cost,
                }));
                continue;
            }
            untrusted_used += 1;
            item["data_only_warning"] = json!(true);
        }
        if used + cost > budget {
            excluded.push(json!({
                "id": item["id"],
                "reason": "budget_exceeded",
                "tokens": cost,
            }));
            continue;
        }
        used += cost;
        item["tokens"] = json!(cost);
        selected.push(item);
    }

    Ok(json!({
        "strict": false,
        "scope": scope_filter,
        "include_global": include_g,
        "embed_model": embed_model,
        "retrieval": retrieval_mode,
        "budget_tokens": budget,
        "used_tokens": used,
        "working_note": working_note,
        "intentions": due_intentions,
        "items": selected,
        "excluded": excluded,
        "counts": counts,
    }))
}

/// Render the budgeted memory items section of a context slate for post-cache-break prompt injection.
/// Keeps provenance (`trust_tier`, `fact_key`, `source_event_id`) visible to the model.
pub fn format_slate_memories_prompt(slate: &Value, max_bytes: usize) -> Option<String> {
    if slate["strict"].as_bool() == Some(true) {
        return None;
    }
    let items = slate["items"].as_array()?;
    if items.is_empty() {
        return None;
    }
    let budget = slate["budget_tokens"].as_u64().unwrap_or(2000);
    let used = slate["used_tokens"].as_u64().unwrap_or(0);
    let mut out = format!("\n## Workspace memory slate ({used}/{budget} tok)\n");
    for item in items {
        let content = item["content"].as_str().unwrap_or("").trim().replace('\n', " ");
        if content.is_empty() {
            continue;
        }
        let tier = item["trust_tier"].as_str().unwrap_or("agent_inferred");
        let cat = item["category"].as_str().unwrap_or("fact");
        let mut tags = vec![tier.to_string(), cat.to_string()];
        if let Some(fk) = item["fact_key"].as_str().filter(|s| !s.is_empty()) {
            tags.push(format!("key={fk}"));
        }
        if let Some(eid) = item["source_event_id"].as_str().filter(|s| !s.is_empty()) {
            tags.push(format!("src={eid}"));
        }
        if item["data_only_warning"].as_bool() == Some(true) {
            tags.push("DATA_ONLY_NOT_INSTRUCTION".into());
        }
        out.push_str(&format!("- [{}] {}\n", tags.join(" | "), content));
    }
    Some(truncate_utf8(&out, max_bytes))
}

/// Progressive disclosure: a compact index (id, category, title, files) of this repository's other
/// active memories, so the agent can pull full text with `memory_get` only when it needs it.
/// Repo scope only; callers must already have excluded Strict agents.
pub fn memory_index_prompt(ctx: &Ctx, repo_id: &str, exclude: &[String], max_items: usize, max_bytes: usize) -> Option<String> {
    let rows = ctx
        .db
        .query(
            "SELECT id, category, title, content, files FROM memories
             WHERE scope=?1 AND status='active' AND active=1
             ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 200",
            [format!("repo:{repo_id}")],
        )
        .ok()?;
    let mut out = String::new();
    let mut n = 0;
    for r in &rows {
        let id = r["id"].as_str().unwrap_or("");
        if id.is_empty() || exclude.iter().any(|e| e == id) {
            continue;
        }
        let title = r["title"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string).unwrap_or_else(|| {
            let c = r["content"].as_str().unwrap_or("").trim().replace('\n', " ");
            truncate_utf8(&c, 80)
        });
        let files: Vec<String> = r["files"].as_str().and_then(|f| serde_json::from_str(f).ok()).unwrap_or_default();
        let mut line = format!("- {id} · {} · {}", r["category"].as_str().unwrap_or("fact"), title.trim());
        if !files.is_empty() {
            line.push_str(&format!(" · {}", files.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
        line.push('\n');
        if out.len() + line.len() > max_bytes {
            break;
        }
        out.push_str(&line);
        n += 1;
        if n >= max_items {
            break;
        }
    }
    (n > 0).then(|| format!("\n## Memory index (more in this repo; read with `memory_get {{ids}}`)\n{out}"))
}

/// Full records for ids from the memory index, hard-gated to this repository's scope.
pub fn agent_memory_get(ctx: &Ctx, repo_id: &str, ids: &[String]) -> Result<Value, String> {
    let scope = format!("repo:{repo_id}");
    let mut out = Vec::new();
    for id in ids.iter().take(20) {
        let rows = ctx.db.query(
            "SELECT id, category, title, content, files, fact_key, trust_tier, status, created_at, updated_at
             FROM memories WHERE id=?1 AND scope=?2 AND status != 'deleted'",
            params![id, scope],
        )?;
        out.extend(rows);
    }
    Ok(Value::Array(out))
}

/// UI list helper: inspect active, proposed, superseded, or quarantined memories with provenance.
pub fn list_memories(
    ctx: &Ctx,
    scope: Option<&str>,
    status: Option<&str>,
    trust_tier: Option<&str>,
    q: &str,
    limit: i64,
) -> Result<Value, String> {
    let limit = limit.clamp(1, 200);
    let q_trim = q.trim().to_lowercase();
    let rows = ctx.db.query(
        "SELECT id, scope, category, content, source, created_at, active, trust_tier, status, fact_key, supersedes_id, source_event_id, provenance, updated_at, valid_from, valid_to, title, files
         FROM memories
         WHERE status != 'deleted'
           AND (?1 IS NULL OR scope=?1)
           AND (?2 IS NULL OR status=?2)
           AND (?3 IS NULL OR trust_tier=?3)
         ORDER BY
           CASE status WHEN 'proposed' THEN 0 WHEN 'active' THEN 1 WHEN 'quarantined' THEN 2 ELSE 3 END,
           (trust_tier='user_direct') DESC,
           COALESCE(updated_at, created_at) DESC
         LIMIT ?4",
        params![
            scope.filter(|s| !s.is_empty()),
            status.filter(|s| !s.is_empty()),
            trust_tier.filter(|s| !s.is_empty()),
            limit,
        ],
    )?;
    if q_trim.is_empty() {
        return Ok(json!(rows));
    }
    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|r| {
            let hay = format!(
                "{} {} {} {} {}",
                r["content"].as_str().unwrap_or(""),
                r["fact_key"].as_str().unwrap_or(""),
                r["category"].as_str().unwrap_or(""),
                r["scope"].as_str().unwrap_or(""),
                r["provenance"].as_str().unwrap_or(""),
            )
            .to_lowercase();
            hay.contains(&q_trim)
        })
        .collect();
    Ok(json!(filtered))
}

/// Global preferences and corrections, kept for legacy test coverage.
#[cfg(test)]
pub fn standing_memories(ctx: &Ctx) -> String {
    let rows = ctx
        .db
        .query(
            "SELECT content FROM memories WHERE scope='global' AND active=1 AND status='active' AND category IN ('preference','correction') ORDER BY created_at LIMIT 40",
            [],
        )
        .unwrap_or_default();
    if rows.is_empty() {
        return String::new();
    }
    let mut s = String::from("## What Filip has told agents before\n");
    for r in rows {
        if let Some(c) = r["content"].as_str() {
            s.push_str("- ");
            s.push_str(&c.chars().take(600).collect::<String>().replace('\n', " "));
            s.push('\n');
        }
    }
    s
}

// ---------- Same-Repo Raw Evidence Lookup (GrepMem / ReFind) & Session Search ----------

pub fn session_search(
    ctx: &Ctx,
    query: &str,
    project: Option<&str>,
    repo_only: bool,
    limit: usize,
) -> Result<Value, String> {
    if repo_only && project.filter(|p| !p.is_empty()).is_none() {
        return Err("A repository is required for scoped history".into());
    }
    search_sessions(ctx, query, project, repo_only, None, limit)
}

fn matches_repo(working_dir: &str, project: Option<&str>) -> bool {
    let Some(proj) = project.filter(|p| !p.is_empty()) else {
        return false;
    };
    Path::new(proj).is_absolute() && Path::new(working_dir) == Path::new(proj)
}

/// Standard-agent raw evidence + session lookup, strictly bound to `repo_id`.
/// Never opens imported histories or sibling-repo sessions; never callable by Strict agents.
pub fn agent_sessions(ctx: &Ctx, query: &str, repo_id: &str, limit: usize) -> Result<Value, String> {
    search_sessions(ctx, query, None, true, Some(repo_id), limit)
}

#[derive(Clone)]
struct CachedHalEvents {
    stamp: (SystemTime, u64),
    /// (role/kind, excerpt <= 360 chars, lowercased full text, event_ref)
    entries: Vec<(String, String, String, String)>,
}

fn hal_session_cache() -> &'static Mutex<HashMap<PathBuf, CachedHalEvents>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, CachedHalEvents>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

#[cfg(test)]
static SESSION_FILE_PARSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn load_hal_events_cached(path: &Path, sid: &str) -> Option<CachedHalEvents> {
    let md = std::fs::metadata(path).ok()?;
    let stamp = (md.modified().ok()?, md.len());
    {
        let cache = hal_session_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(path) {
            if hit.stamp == stamp {
                return Some(hit.clone());
            }
        }
    }
    let raw = std::fs::read_to_string(path).ok()?;
    #[cfg(test)]
    SESSION_FILE_PARSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut entries = vec![];
    for (idx, line) in raw.lines().take(300).enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let role = match v["t"].as_str().or_else(|| v["type"].as_str()) {
            Some("user" | "message.created") => "user",
            Some("assistant" | "assistant.completed") => "assistant",
            Some("tool" | "tool_output" | "tool.output" | "tool.finished") => "tool",
            Some("error" | "turn.failed") => "error",
            _ => continue,
        };
        let txt = v["text"]
            .as_str()
            .or_else(|| v["content"].as_str())
            .or_else(|| v["data"].as_str())
            .or_else(|| v["error"].as_str())
            .unwrap_or("")
            .trim();
        if txt.is_empty() {
            continue;
        }
        entries.push((
            role.to_string(),
            txt.chars().take(360).collect::<String>(),
            txt.to_lowercase(),
            format!("hal-jsonl:{sid}:{}", idx + 1),
        ));
    }
    let item = CachedHalEvents { stamp, entries };
    let mut cache = hal_session_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= SESSION_CACHE_CAP {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), item.clone());
    Some(item)
}

/// Extract searchable raw turns & tool outputs from a Philium agent's read-only HAL SQLite event journal.
fn extract_hal_db_entries(ctx: &Ctx, agent_id: &str, sid: &str, after_seq: i64, limit: i64) -> Vec<(i64, String, String, String)> {
    let Ok(Value::Array(events)) = crate::agents::history::events(ctx, agent_id, after_seq, limit) else {
        return vec![];
    };
    let mut out: Vec<(i64, String, String, String)> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut delta_buf = String::new();
    let mut delta_seq = 0i64;

    let flush_delta = |buf: &mut String, seq: &mut i64, out: &mut Vec<(i64, String, String, String)>| {
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            out.push((
                *seq,
                "assistant".into(),
                trimmed.chars().take(1200).collect::<String>(),
                format!("hal:{agent_id}:{sid}:{}", *seq),
            ));
        }
        buf.clear();
        *seq = 0;
    };

    for ev in events {
        let seq = ev["seq"].as_i64().unwrap_or(0);
        let ty = ev["type"].as_str().unwrap_or("");
        match ty {
            "message.created" => {
                flush_delta(&mut delta_buf, &mut delta_seq, &mut out);
                if ev["role"].as_str() == Some("user") {
                    if let Some(txt) = ev["content"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        out.push((seq, "user".into(), txt.chars().take(1200).collect(), format!("hal:{agent_id}:{sid}:{seq}")));
                    }
                }
            }
            "model.delta" => {
                if let Some(d) = ev["delta"].as_str() {
                    if delta_seq == 0 {
                        delta_seq = seq;
                    }
                    if delta_buf.len() < 4096 {
                        delta_buf.push_str(d);
                    }
                }
            }
            "assistant.completed" => {
                if let Some(txt) = ev["text"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    delta_buf.clear();
                    delta_seq = 0;
                    out.push((seq, "assistant".into(), txt.chars().take(1200).collect(), format!("hal:{agent_id}:{sid}:{seq}")));
                } else {
                    flush_delta(&mut delta_buf, &mut delta_seq, &mut out);
                }
            }
            "tool.started" => {
                flush_delta(&mut delta_buf, &mut delta_seq, &mut out);
                if let (Some(cid), Some(tool)) = (ev["call_id"].as_str(), ev["tool"].as_str()) {
                    tool_names.insert(cid.to_string(), tool.to_string());
                    let arg_txt = ev.get("arguments").map(|a| a.to_string()).unwrap_or_default();
                    if !arg_txt.is_empty() && arg_txt != "null" && arg_txt != "{}" {
                        let line = format!("{tool} {}", arg_txt.chars().take(300).collect::<String>());
                        out.push((seq, "tool_call".into(), line, format!("hal:{agent_id}:{sid}:{seq}")));
                    }
                }
            }
            "tool.output" => {
                if let Some(data) = ev["data"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    let tname = ev["call_id"]
                        .as_str()
                        .and_then(|cid| tool_names.get(cid))
                        .map(String::as_str)
                        .unwrap_or("tool");
                    let line = format!("[{tname}] {}", data.chars().take(1000).collect::<String>());
                    out.push((seq, "tool_output".into(), line, format!("hal:{agent_id}:{sid}:{seq}")));
                }
            }
            "tool.finished" => {
                let tname = ev["call_id"]
                    .as_str()
                    .and_then(|cid| tool_names.get(cid))
                    .map(String::as_str)
                    .unwrap_or("tool");
                let ok = ev["ok"].as_bool().unwrap_or(true);
                let res_txt = match ev.get("result") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) if !v.is_null() => v.to_string(),
                    _ => String::new(),
                };
                if !res_txt.is_empty() || !ok {
                    let status_tag = if ok { "ok" } else { "err" };
                    let line = format!("[{tname}:{status_tag}] {}", res_txt.chars().take(1000).collect::<String>());
                    out.push((seq, "tool_result".into(), line, format!("hal:{agent_id}:{sid}:{seq}")));
                }
            }
            "turn.failed" => {
                flush_delta(&mut delta_buf, &mut delta_seq, &mut out);
                let err = ev["error"].as_str().map(str::to_string).unwrap_or_else(|| ev["error"].to_string());
                if !err.trim().is_empty() && err != "null" {
                    out.push((seq, "error".into(), err.chars().take(800).collect(), format!("hal:{agent_id}:{sid}:{seq}")));
                }
            }
            _ => {}
        }
    }
    flush_delta(&mut delta_buf, &mut delta_seq, &mut out);
    out
}

fn search_sessions(
    ctx: &Ctx,
    query: &str,
    project: Option<&str>,
    repo_only: bool,
    repo_id: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    let limit = limit.clamp(1, 25);
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|s| s.len() >= 2)
        .map(str::to_string)
        .collect();
    let mut candidates: Vec<(i64, Value)> = vec![];

    // 1. Philium agents & same-repo HAL session raw evidence (hal.db + events.jsonl + non-scrubbed events).
    let agents = ctx.db.query(
        "SELECT a.id,a.name,a.role,a.state,t.title AS task_title, \
         (SELECT w.branch FROM worktrees w WHERE w.agent_id=a.id AND w.repo_id=r.id AND w.state='active' ORDER BY w.created_at DESC LIMIT 1) AS branch, \
         a.hal_session_id,a.last_active_at,r.path AS repo_path FROM agents a LEFT JOIN repos r ON r.id=a.repo_id \
         LEFT JOIN tasks t ON t.id=a.task_id AND t.repo_id=r.id WHERE (?1 IS NULL OR r.id=?1) ORDER BY a.last_active_at DESC LIMIT 60",
        params![repo_id],
    )?;
    for a in agents {
        let aid = a["id"].as_str().unwrap_or("");
        let rpath = a["repo_path"].as_str().unwrap_or("");
        let same_repo = repo_id.is_some() || matches_repo(rpath, project);
        if repo_only && project.is_some() && !same_repo {
            continue;
        }
        let header_hay = format!(
            "{} {} {} {}",
            a["name"].as_str().unwrap_or(""),
            a["task_title"].as_str().unwrap_or(""),
            a["branch"].as_str().unwrap_or(""),
            a["role"].as_str().unwrap_or("")
        )
        .to_lowercase();
        let mut score: i64 = if same_repo { 20 } else { 0 };
        for t in &terms {
            if header_hay.contains(t) {
                score += 15;
            }
        }
        let mut excerpts = vec![];
        if let Some(sid) = a["hal_session_id"].as_str().filter(|s| !s.is_empty()) {
            // 1a. Authoritative read-only HAL SQLite event journal (`hal.db`) — same-repo raw turns & tool outputs.
            let db_entries = extract_hal_db_entries(ctx, aid, sid, 0, 300);
            if !db_entries.is_empty() {
                for (_, role, text, event_ref) in db_entries {
                    let low = text.to_lowercase();
                    let term_hits = terms.iter().filter(|t| low.contains(t.as_str())).count();
                    if terms.is_empty() || term_hits > 0 {
                        score += (term_hits as i64) * 12;
                        if excerpts.len() < 5 {
                            excerpts.push(json!({
                                "role": role,
                                "text": text.chars().take(360).collect::<String>(),
                                "event_ref": event_ref,
                            }));
                        }
                    }
                }
            } else {
                // 1b. Legacy/JSONL HAL event transcript fallback (mtime-cached).
                let ev_path = ctx.root.join("hal/sessions").join(sid).join("events.jsonl");
                if let Some(cached) = load_hal_events_cached(&ev_path, sid) {
                    for (role, excerpt, low, event_ref) in &cached.entries {
                        let term_hits = terms.iter().filter(|t| low.contains(t.as_str())).count();
                        if terms.is_empty() || term_hits > 0 {
                            score += (term_hits as i64) * 10;
                            if excerpts.len() < 5 {
                                excerpts.push(json!({
                                    "role": role,
                                    "text": excerpt,
                                    "event_ref": event_ref,
                                }));
                            }
                        }
                    }
                }
            }
        }
        // 1c. Same-agent Philium audit events (skipping scrubbed records).
        if !aid.is_empty() && !terms.is_empty() && excerpts.len() < 5 {
            if let Ok(ev_rows) = ctx.db.query(
                "SELECT id, kind, body FROM events
                 WHERE agent_id=?1 AND body NOT LIKE '%\"scrubbed\":true%'
                 ORDER BY id DESC LIMIT 80",
                [aid],
            ) {
                for ev in ev_rows {
                    let body = ev["body"].as_str().unwrap_or("");
                    let low = body.to_lowercase();
                    let hits = terms.iter().filter(|t| low.contains(t.as_str())).count();
                    if hits > 0 {
                        score += (hits as i64) * 8;
                        if excerpts.len() < 5 {
                            excerpts.push(json!({
                                "role": ev["kind"].as_str().unwrap_or("event"),
                                "text": body.chars().take(360).collect::<String>(),
                                "event_ref": format!("evt:{}", ev["id"].as_i64().unwrap_or(0)),
                            }));
                        }
                    }
                }
            }
        }
        if terms.is_empty() || score > if same_repo { 20 } else { 0 } {
            candidates.push((
                score,
                json!({
                    "source": "philium",
                    "session_id": a["id"],
                    "name": a["name"],
                    "branch": a["branch"],
                    "task_title": a["task_title"],
                    "working_dir": rpath,
                    "updated_at": a["last_active_at"],
                    "same_repo": same_repo,
                    "excerpts": excerpts,
                }),
            ));
        }
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(Value::Array(candidates.into_iter().take(limit).map(|(_, v)| v).collect()))
}

// ---------- Pipeline-Side Per-Session Observation Note & Candidate Extraction (v1#4 / AgeMem) ----------

fn observing_agents() -> &'static Mutex<HashSet<String>> {
    static O: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    O.get_or_init(Default::default)
}

/// Schedule a non-blocking pipeline observation pass on session idle/stop (Standard agents only).
pub fn schedule_session_observe(ctx: &Arc<Ctx>, agent_id: &str) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
    {
        let mut busy = observing_agents().lock().unwrap_or_else(|e| e.into_inner());
        if !busy.insert(agent_id.to_string()) {
            return;
        }
    }
    let (c, aid) = (ctx.clone(), agent_id.to_string());
    handle.spawn(async move {
        let _ = observe_session(&c, &aid).await;
        observing_agents().lock().unwrap_or_else(|e| e.into_inner()).remove(&aid);
    });
}

#[derive(Default)]
struct ExtractedSessionObs {
    requirements: Option<String>,
    decisions: Option<String>,
    failed_attempts: Option<String>,
    next_steps: Option<String>,
    candidates: Vec<ObsCandidate>,
}

struct ObsCandidate {
    content: String,
    category: String,
    fact_key: Option<String>,
    trust_tier: String,
    meta: MemoryMeta,
}

fn infer_fact_key(text: &str) -> Option<String> {
    let low = text.to_lowercase();
    if low.contains("offline") || low.contains("network") || low.contains("airplane") {
        return Some("network.policy".into());
    }
    if low.contains("push") && low.contains("main") {
        return Some("git.push".into());
    }
    if low.contains("package manager") || low.contains("pnpm") || low.contains("bun ") || low.contains("npm ") {
        return Some("tooling.package_manager".into());
    }
    if low.contains("sqlite") || low.contains("postgres") || low.contains("database") {
        return Some("storage.engine".into());
    }
    if low.contains("port ") || low.contains("localhost:") || low.contains("127.0.0.1:") {
        return Some("runtime.ports".into());
    }
    None
}

fn extract_deterministic_observation(entries: &[(i64, String, String, String)]) -> ExtractedSessionObs {
    let mut obs = ExtractedSessionObs::default();
    let mut reqs: Vec<String> = vec![];
    let mut decs: Vec<String> = vec![];
    let mut fails: Vec<String> = vec![];
    let mut nexts: Vec<String> = vec![];

    for (_, role, text, _) in entries {
        let low = text.to_lowercase();
        let snippet: String = text.trim().replace('\n', " ").chars().take(240).collect();
        if snippet.is_empty() {
            continue;
        }
        match role.as_str() {
            "user" => {
                if low.contains("must ")
                    || low.contains("never ")
                    || low.contains("don't ")
                    || low.contains("always ")
                    || low.contains("prefer ")
                    || low.contains("offline")
                {
                    if reqs.len() < 3 {
                        reqs.push(snippet.clone());
                    }
                    if obs.candidates.len() < 4 {
                        let cat = if low.contains("never ") || low.contains("don't ") {
                            "correction"
                        } else {
                            "preference"
                        };
                        obs.candidates.push(ObsCandidate {
                            content: snippet,
                            category: cat.into(),
                            fact_key: infer_fact_key(text),
                            trust_tier: "agent_inferred".into(),
                            meta: MemoryMeta::default(),
                        });
                    }
                }
            }
            "error" => {
                if fails.len() < 3 {
                    fails.push(snippet);
                }
            }
            "tool_result" | "tool_output" => {
                if low.contains(":err]")
                    || low.contains("enetunreach")
                    || low.contains("error:")
                    || low.contains("failed")
                    || low.contains("exit_code\":") && !low.contains("exit_code\":0") && !low.contains("exit_code\": 0")
                {
                    if fails.len() < 3 {
                        fails.push(snippet);
                    }
                } else if (low.contains("cargo test") || low.contains("npm test") || low.contains("pytest"))
                    && (low.contains("test result: ok") || low.contains("passed") || low.contains(":ok]"))
                {
                    if obs.candidates.len() < 5 {
                        obs.candidates.push(ObsCandidate {
                            content: format!("Verified run succeeded: {snippet}"),
                            category: "procedure".into(),
                            fact_key: None,
                            trust_tier: "verified_run".into(),
                            meta: MemoryMeta::default(),
                        });
                    }
                }
            }
            "assistant" => {
                if low.contains("decided ") || low.contains("switched to ") || low.contains("implemented ") {
                    if decs.len() < 2 {
                        decs.push(snippet.clone());
                    }
                }
                if low.contains("next step") || low.contains("remaining") || low.contains("todo") {
                    if nexts.len() < 2 {
                        nexts.push(snippet);
                    }
                }
            }
            _ => {}
        }
    }

    if !reqs.is_empty() {
        obs.requirements = Some(reqs.join(" | "));
    }
    if !decs.is_empty() {
        obs.decisions = Some(decs.join(" | "));
    }
    if !fails.is_empty() {
        obs.failed_attempts = Some(fails.join(" | "));
    }
    if !nexts.is_empty() {
        obs.next_steps = Some(nexts.join(" | "));
    }
    obs
}

fn append_note_line(existing: &str, addition: &str) -> String {
    let add = addition.trim();
    if add.is_empty() {
        return existing.to_string();
    }
    if existing.trim().is_empty() {
        return add.chars().take(2000).collect();
    }
    if existing.to_lowercase().contains(&add.to_lowercase()) {
        return existing.to_string();
    }
    let merged = format!("{}\n{}", existing.trim(), add);
    merged.chars().take(2000).collect()
}

/// Cheapest sibling model on the same provider for background observation: a small, fast tier
/// when the provider offers one, otherwise the agent's own model. Never crosses providers.
fn observation_model(ctx: &Ctx, pid: &str, agent_model: &str) -> String {
    const CHEAP: [&str; 6] = ["haiku", "flash-lite", "flash", "mini", "luna", "nano"];
    let ids: Vec<String> = ctx
        .db
        .query("SELECT model_id FROM models WHERE provider_id=?1 AND enabled=1", [pid])
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["model_id"].as_str().map(str::to_string))
        .collect();
    for tag in CHEAP {
        if agent_model.to_lowercase().contains(tag) {
            return agent_model.to_string();
        }
        let mut hits: Vec<&String> = ids.iter().filter(|m| m.to_lowercase().contains(tag)).collect();
        // Prefer the newest-looking id (lexicographically largest) within a tier.
        hits.sort();
        if let Some(m) = hits.last() {
            return (*m).clone();
        }
    }
    agent_model.to_string()
}

/// Concatenated assistant text from any wire shape the gateway can return: chat.completions JSON,
/// chat.completions SSE, OpenAI Responses JSON/SSE, or Anthropic Messages JSON.
fn completion_text(body: &str) -> Option<String> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with('{') {
        let mut out = String::new();
        let mut done_text: Option<String> = None;
        for line in body.lines() {
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(t) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) {
                out.push_str(t);
            } else if v["type"] == "response.output_text.delta" {
                out.push_str(v["delta"].as_str().unwrap_or(""));
            } else if v["type"] == "response.output_text.done" {
                done_text = v["text"].as_str().map(str::to_string);
            } else if v["type"] == "content_block_delta" {
                out.push_str(v.pointer("/delta/text").and_then(Value::as_str).unwrap_or(""));
            }
        }
        let text = if out.trim().is_empty() { done_text.unwrap_or_default() } else { out };
        return (!text.trim().is_empty()).then_some(text);
    }
    let v: Value = serde_json::from_str(trimmed).ok()?;
    if let Some(t) = v.pointer("/choices/0/message/content").and_then(Value::as_str) {
        return Some(t.to_string());
    }
    if let Some(t) = v["output_text"].as_str() {
        return Some(t.to_string());
    }
    let mut out = String::new();
    for item in v["output"].as_array().into_iter().flatten() {
        for c in item["content"].as_array().into_iter().flatten() {
            out.push_str(c["text"].as_str().unwrap_or(""));
        }
    }
    for c in v["content"].as_array().into_iter().flatten() {
        if c["type"] == "text" {
            out.push_str(c["text"].as_str().unwrap_or(""));
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// The first balanced top-level JSON object in a model reply (tolerates fences and prose).
fn first_json_object(raw: &str) -> Option<Value> {
    let start = raw.find('{')?;
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, ch) in raw[start..].char_indices() {
        if in_str {
            match (esc, ch) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&raw[start..start + i + 1]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

const OBSERVE_PROMPT: &str = "You maintain an engineering memory for one repository. From the session transcript, extract only durable knowledge a future agent in this repo would need. Skip chit-chat, transient status and anything already obvious from the code.\n\
Return strict JSON only:\n\
{\"working_note\":{\"requirements\":null,\"decisions\":null,\"failed_attempts\":null,\"next_steps\":null},\
\"observations\":[{\"title\":\"<=80 chars\",\"category\":\"decision|bugfix|discovery|architecture|gotcha|preference|correction|procedure|fact|entity\",\"content\":\"1-3 sentences, self-contained\",\"files\":[\"repo/relative/path\"],\"fact_key\":\"optional.dotted.key for facts that can change\",\"trust_tier\":\"agent_inferred|verified_run\"}]}\n\
Rules: at most 5 observations; use verified_run only when a command in the transcript succeeded and proves it; use fact_key only for a single current value (e.g. tooling.package_manager); working_note fields are one short line each or null; never include secrets, tokens or personal data.";

/// Try a single bounded LLM extraction call through Philium's gateway for a trusted Standard agent.
/// Works with every provider kind the gateway serves (API keys and subscriptions), using a cheap
/// sibling model when one exists. Returns `None` when offline, untrusted, or the reply is unusable.
async fn try_llm_session_observation(
    ctx: &Ctx,
    model_ref: &str,
    entries: &[(i64, String, String, String)],
) -> Option<ExtractedSessionObs> {
    if ctx.gateway_url.ends_with(":9") || ctx.gateway_url.is_empty() {
        return None;
    }
    let (pid, agent_mid) = crate::providers::split_ref(model_ref)?;
    let p = crate::providers::get(ctx, pid)?;
    if !p.enabled || !p.trusted {
        return None;
    }
    let mid = observation_model(ctx, pid, agent_mid);
    let mut transcript = String::new();
    for (seq, role, text, _) in entries.iter().take(60) {
        let line = format!("[seq={seq} {role}] {}\n", text.chars().take(400).collect::<String>());
        if transcript.len() + line.len() > 9000 {
            break;
        }
        transcript.push_str(&line);
    }
    use crate::providers::Kind;
    let base = format!("{}/p/{}", ctx.gateway_url.trim_end_matches('/'), p.id);
    let (url, body) = match p.kind {
        Kind::OpenaiResponses | Kind::ChatgptOauth => (
            format!("{base}/responses"),
            json!({
                "model": mid,
                "stream": p.kind == Kind::ChatgptOauth,
                "store": false,
                "instructions": OBSERVE_PROMPT,
                "input": [{ "role": "user", "content": [{ "type": "input_text", "text": transcript }] }],
            }),
        ),
        // chat.completions; the gateway translates for Anthropic, Claude and Antigravity.
        _ => (
            format!("{base}/chat/completions"),
            json!({
                "model": mid,
                "stream": false,
                "temperature": 0,
                "max_tokens": 1200,
                "messages": [
                    { "role": "system", "content": OBSERVE_PROMPT },
                    { "role": "user", "content": transcript }
                ]
            }),
        ),
    };
    let resp = observe_http_client()
        .post(&url)
        .bearer_auth(&ctx.gateway.token)
        .header("x-philium-agent", "memory-observer")
        .json(&body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        ctx.log_event("memory:observe_llm_failed", None, &json!({ "provider": p.id, "model": mid, "status": resp.status().as_u16() }));
        return None;
    }
    let text = resp.text().await.ok()?;
    let parsed = first_json_object(&completion_text(&text)?)?;
    let obs = parse_observation(&parsed);
    ctx.log_event("memory:observe_llm", None, &json!({ "provider": p.id, "model": mid, "observations": obs.candidates.len() }));
    Some(obs)
}

fn observe_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_default()
    })
}

fn parse_observation(parsed: &Value) -> ExtractedSessionObs {
    let mut obs = ExtractedSessionObs::default();
    let field = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty() && *s != "null").map(|s| s.chars().take(400).collect::<String>());
    if let Some(wn) = parsed.get("working_note") {
        obs.requirements = field(&wn["requirements"]);
        obs.decisions = field(&wn["decisions"]);
        obs.failed_attempts = field(&wn["failed_attempts"]);
        obs.next_steps = field(&wn["next_steps"]);
    }
    let items = parsed["observations"].as_array().or(parsed["candidates"].as_array());
    for item in items.into_iter().flatten().take(5) {
        let Some(content) = item["content"].as_str().map(str::trim).filter(|s| !s.is_empty()) else { continue };
        let cat = item["category"].as_str().unwrap_or("fact").trim().to_lowercase();
        if validate_category(&cat).is_err() {
            continue;
        }
        let tier = match item["trust_tier"].as_str() {
            Some("verified_run") => "verified_run",
            _ => "agent_inferred",
        };
        let files = item["files"]
            .as_array()
            .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        obs.candidates.push(ObsCandidate {
            content: content.chars().take(1200).collect(),
            category: cat,
            fact_key: clean_fact_key(item["fact_key"].as_str()),
            trust_tier: tier.into(),
            meta: MemoryMeta { title: item["title"].as_str().map(str::to_string), files },
        });
    }
    obs
}

/// Pipeline-side per-session observation pass (Standard agents only).
/// Reads newly persisted raw session turns/tool outputs, updates the repo working note via partial patch,
/// and writes candidate memories linked to `source_event_id` (staging conflicts against `user_direct` as `proposed`).
pub async fn observe_session(ctx: &Arc<Ctx>, agent_id: &str) -> Result<Value, String> {
    let (c_load, aid) = (ctx.clone(), agent_id.to_string());
    let loaded = tokio::task::spawn_blocking(move || -> Result<Option<(String, String, String, String, i64, Vec<(i64, String, String, String)>)>, String> {
        let Some(row) = crate::agents::get_row(&c_load, &aid) else {
            return Ok(None);
        };
        if row["strict"].as_i64() == Some(1) {
            return Ok(None);
        }
        let Some(repo_id) = row["repo_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string) else {
            return Ok(None);
        };
        if c_load.db.query("SELECT id FROM repos WHERE id=?1", [&repo_id])?.is_empty() {
            return Ok(None);
        }
        let Some(sid) = row["hal_session_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string) else {
            return Ok(None);
        };
        let name = row["name"].as_str().unwrap_or("agent").to_string();
        let model_ref = row["model_ref"].as_str().unwrap_or("").to_string();
        let kv_key = format!("memory.observed_seq.{aid}");
        let last_seq = c_load.db.kv_get(&kv_key).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let entries = extract_hal_db_entries(&c_load, &aid, &sid, last_seq, 200);
        if entries.is_empty() {
            return Ok(None);
        }
        Ok(Some((name, repo_id, sid, model_ref, last_seq, entries)))
    })
    .await
    .map_err(|e| e.to_string())??;

    let Some((name, repo_id, sid, model_ref, last_seq, entries)) = loaded else {
        return Ok(json!({ "skipped": true }));
    };

    let first_seq = entries.first().map(|e| e.0).unwrap_or(last_seq + 1);
    let max_seq = entries.iter().map(|e| e.0).max().unwrap_or(last_seq);
    if max_seq <= last_seq {
        return Ok(json!({ "skipped": true }));
    }

    let obs = match try_llm_session_observation(ctx, &model_ref, &entries).await {
        Some(llm_obs) => llm_obs,
        None => extract_deterministic_observation(&entries),
    };

    let (c_save, aid_save, name_save, repo_save, sid_save) = (ctx.clone(), agent_id.to_string(), name, repo_id, sid);
    let (saved_ids, active_to_embed, source_event_id) = tokio::task::spawn_blocking(move || -> Result<(Vec<Value>, Vec<(String, String)>, Option<String>), String> {
        let scope = format!("repo:{repo_save}");
        let provenance = format!("pipeline:session:{sid_save}:seq:{first_seq}-{max_seq}");
        let source_event_id = c_save.log_event_id(
            "memory:session_observe",
            Some(&aid_save),
            &json!({
                "scope": scope,
                "session_id": sid_save,
                "seq_from": first_seq,
                "seq_to": max_seq,
                "candidate_count": obs.candidates.len(),
            }),
        );

        // Merge into existing working note without clobbering existing operator notes.
        if obs.requirements.is_some()
            || obs.decisions.is_some()
            || obs.failed_attempts.is_some()
            || obs.next_steps.is_some()
        {
            let cur = working_note_get(&c_save, &scope)?;
            let req = obs.requirements.as_deref().map(|a| append_note_line(cur["requirements"].as_str().unwrap_or(""), a));
            let dec = obs.decisions.as_deref().map(|a| append_note_line(cur["decisions"].as_str().unwrap_or(""), a));
            let fail = obs.failed_attempts.as_deref().map(|a| append_note_line(cur["failed_attempts"].as_str().unwrap_or(""), a));
            let next = obs.next_steps.as_deref().map(|a| append_note_line(cur["next_steps"].as_str().unwrap_or(""), a));
            working_note_patch(
                &c_save,
                &scope,
                req.as_deref(),
                dec.as_deref(),
                fail.as_deref(),
                next.as_deref(),
                source_event_id.as_deref(),
                &format!("pipeline:{name_save}"),
            )?;
        }

        let mut saved = Vec::new();
        let mut to_embed = Vec::new();
        for ObsCandidate { content, category, fact_key, trust_tier, meta } in obs.candidates {
            // Skip near-exact duplicates already active in this scope.
            let norm = normalized_text(&content);
            let already = c_save
                .db
                .query(
                    "SELECT content FROM memories WHERE scope=?1 AND active=1 AND status='active'",
                    [&scope],
                )
                .unwrap_or_default()
                .iter()
                .any(|r| normalized_text(r["content"].as_str().unwrap_or("")) == norm);
            if already {
                continue;
            }
            if let Ok(res) = save_structured_meta(&c_save, SaveOpts {
                content: &content,
                category: &category,
                scope: &scope,
                source: &format!("pipeline:{name_save}"),
                trust_tier: &trust_tier,
                fact_key: fact_key.as_deref(),
                supersedes_id: None,
                source_event_id: source_event_id.as_deref(),
                provenance: Some(&provenance),
            }, &meta) {
                if res["status"].as_str() == Some("active") {
                    if let Some(mid) = res["id"].as_str() {
                        to_embed.push((mid.to_string(), content));
                    }
                }
                saved.push(res);
            }
        }

        let _ = c_save.db.kv_set(&format!("memory.observed_seq.{aid_save}"), &max_seq.to_string());
        let _ = crate::memory_skills::mine_verified_runs(&c_save, &repo_save);
        Ok((saved, to_embed, source_event_id))
    })
    .await
    .map_err(|e| e.to_string())??;

    for (mid, content) in active_to_embed {
        let _ = embed_memory_async(ctx, &mid, &content).await;
    }

    Ok(json!({
        "skipped": false,
        "seq_to": max_seq,
        "source_event_id": source_event_id,
        "saved": saved_ids,
    }))
}

// ---------- RPC Dispatch ----------

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let embed_model = resolve_embedding_config(ctx).map(|c| c.model_version);
            Ok(json!({
                "skills": ctx.db.query("SELECT count(*) AS n FROM skills", [])?[0]["n"],
                "memories": ctx.db.query("SELECT count(*) AS n FROM memories WHERE active=1 AND status='active'", [])?[0]["n"],
                "proposed": ctx.db.query("SELECT count(*) AS n FROM memories WHERE status='proposed'", [])?[0]["n"],
                "superseded": ctx.db.query("SELECT count(*) AS n FROM memories WHERE status='superseded'", [])?[0]["n"],
                "quarantined": ctx.db.query("SELECT count(*) AS n FROM memories WHERE status='quarantined'", [])?[0]["n"],
                "intentions": ctx.db.query("SELECT count(*) AS n FROM intentions WHERE active=1", [])?[0]["n"],
                "embed_model": embed_model,
                "retrieval": if embed_model.is_some() { "hybrid_bm25_dense_rrf" } else { "bm25_only" },
                "imported_at": ctx.db.kv_get("library.imported_at"),
            }))
        }
        "reindex" => sync_vectors_async(ctx).await,
        "observe_session" => observe_session(ctx, &crate::ctx::arg_str(&args, "agent_id")?).await,
        "skills" => skill_list(ctx),
        "memories" => match args["repo_id"].as_str() {
            Some(repo) => agent_memories_async(ctx, args["q"].as_str().unwrap_or(""), repo, args["limit"].as_i64().unwrap_or(50)).await,
            None => memory_search_async(ctx, args["q"].as_str().unwrap_or(""), None, args["limit"].as_i64().unwrap_or(50)).await,
        },
        "count" => {
            let scope = args["scope"].as_str().filter(|s| !s.is_empty() && *s != "*");
            let status = args["status"].as_str().unwrap_or("active");
            let n = match scope {
                Some(s) => ctx.db.query("SELECT count(*) AS n FROM memories WHERE scope=?1 AND status=?2", params![s, status])?,
                None => ctx.db.query("SELECT count(*) AS n FROM memories WHERE status=?1", [status])?,
            };
            Ok(json!({ "count": n.first().map(|r| r["n"].clone()).unwrap_or(json!(0)) }))
        }
        "list" => {
            let scope = args["scope"].as_str().or_else(|| args["repo_id"].as_str().map(|_| ""));
            let scope_owned = args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}"));
            let eff_scope = scope_owned.as_deref().or_else(|| scope.filter(|s| !s.is_empty()));
            list_memories(
                ctx,
                eff_scope,
                args["status"].as_str(),
                args["trust_tier"].as_str(),
                args["q"].as_str().unwrap_or(""),
                args["limit"].as_i64().unwrap_or(100),
            )
        }
        "save" => {
            let content = crate::ctx::arg_str(&args, "content")?;
            let category = args["category"].as_str().unwrap_or("fact").to_string();
            let scope = if let Some(s) = args["scope"].as_str().filter(|s| !s.is_empty()) {
                s.to_string()
            } else if let Some(r) = args["repo_id"].as_str().filter(|s| !s.is_empty()) {
                format!("repo:{r}")
            } else {
                "global".to_string()
            };
            let trust = args["trust_tier"].as_str().unwrap_or("user_direct").to_string();
            let source = args["source"].as_str().unwrap_or("user").to_string();
            let fact_key = args["fact_key"].as_str().map(str::to_string);
            let supersedes_id = args["supersedes_id"].as_str().map(str::to_string);
            let source_event_id = args["source_event_id"].as_str().map(str::to_string);
            let provenance = args["provenance"].as_str().unwrap_or("operator:ui").to_string();
            let valid_from = args["valid_from"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string);
            let valid_to = args["valid_to"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string);
            let (c, content_for_embed) = (ctx.clone(), content.clone());
            let res = tokio::task::spawn_blocking(move || {
                let saved = save_structured(&c, SaveOpts {
                    content: &content,
                    category: &category,
                    scope: &scope,
                    source: &source,
                    trust_tier: &trust,
                    fact_key: fact_key.as_deref(),
                    supersedes_id: supersedes_id.as_deref(),
                    source_event_id: source_event_id.as_deref(),
                    provenance: Some(&provenance),
                })?;
                if valid_from.is_some() || valid_to.is_some() {
                    if let Some(mid) = saved["id"].as_str() {
                        crate::memory_ledger::set_validity(&c, mid, valid_from.as_deref(), valid_to.as_deref())?;
                    }
                }
                Ok::<Value, String>(saved)
            })
            .await
            .map_err(|e| e.to_string())??;
            if res["status"].as_str() == Some("active") {
                if let Some(mid) = res["id"].as_str() {
                    let _ = embed_memory_async(ctx, mid, &content_for_embed).await;
                }
            }
            Ok(res)
        }
        "supersede" => {
            let old_id = crate::ctx::arg_str(&args, "old_id")?;
            let content = crate::ctx::arg_str(&args, "content")?;
            let old = ctx
                .db
                .query("SELECT scope, category, fact_key FROM memories WHERE id=?1", [&old_id])?
                .into_iter()
                .next()
                .ok_or_else(|| "memory to supersede not found".to_string())?;
            let scope = old["scope"].as_str().unwrap_or("global").to_string();
            let category = args["category"]
                .as_str()
                .or_else(|| old["category"].as_str())
                .unwrap_or("fact")
                .to_string();
            let fk = args["fact_key"].as_str().or_else(|| old["fact_key"].as_str()).map(str::to_string);
            let source_event_id = args["source_event_id"].as_str().map(str::to_string);
            let provenance = args["provenance"].as_str().unwrap_or("operator:superseded").to_string();
            let (c, content_for_embed) = (ctx.clone(), content.clone());
            let res = tokio::task::spawn_blocking(move || {
                save_structured(&c, SaveOpts {
                    content: &content,
                    category: &category,
                    scope: &scope,
                    source: "user",
                    trust_tier: "user_direct",
                    fact_key: fk.as_deref(),
                    supersedes_id: Some(&old_id),
                    source_event_id: source_event_id.as_deref(),
                    provenance: Some(&provenance),
                })
            })
            .await
            .map_err(|e| e.to_string())??;
            if res["status"].as_str() == Some("active") {
                if let Some(mid) = res["id"].as_str() {
                    let _ = embed_memory_async(ctx, mid, &content_for_embed).await;
                }
            }
            Ok(res)
        }
        "promote" => promote_memory_async(ctx, &crate::ctx::arg_str(&args, "id")?).await,
        "quarantine" => quarantine_memory(ctx, &crate::ctx::arg_str(&args, "id")?, args["reason"].as_str()),
        "forget" => {
            forget_memory(ctx, &crate::ctx::arg_str(&args, "id")?)?;
            Ok(Value::Null)
        }
        "rules_get" => Ok(json!({ "rules": rules(ctx), "is_default": ctx.db.kv_get("setting.agent_rules").is_none() })),
        "rules_set" => {
            set_rules(ctx, &crate::ctx::arg_str(&args, "rules")?)?;
            Ok(json!({ "rules": rules(ctx) }))
        }
        "working_note_get" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            working_note_get(ctx, &scope)
        }
        "working_note_set" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            working_note_patch(
                ctx,
                &scope,
                args.get("requirements").and_then(Value::as_str),
                args.get("decisions").and_then(Value::as_str),
                args.get("failed_attempts").and_then(Value::as_str),
                args.get("next_steps").and_then(Value::as_str),
                args.get("source_event_id").and_then(Value::as_str),
                args["updated_by"].as_str().unwrap_or("user"),
            )
        }
        "context_slate" => {
            let budget = args["budget_tokens"]
                .as_u64()
                .filter(|b| *b > 0)
                .map(|b| b as usize)
                .unwrap_or_else(|| model_memory_budget(ctx, args["model_ref"].as_str()));
            let triggers: Vec<String> = args["triggers"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            build_context_slate_for_model_async(
                ctx,
                args["repo_id"].as_str(),
                args["q"].as_str().unwrap_or(""),
                budget,
                args["strict"].as_bool().unwrap_or(false),
                args["include_global"].as_bool().unwrap_or(false),
                triggers,
                args["model_ref"].as_str(),
            )
            .await
        }
        "config_get" => Ok(crate::memory_ledger::config_get(ctx)),
        "models_status" => Ok(crate::local_models::status(ctx)),
        "models_download" => {
            if let Some(on) = args["enabled"].as_bool() {
                crate::local_models::set_enabled(ctx, on)?;
                invalidate_vector_cache(ctx);
            }
            let c = ctx.clone();
            tokio::spawn(async move {
                crate::local_models::ensure(&c).await;
            });
            Ok(crate::local_models::status(ctx))
        }
        "config_set" => {
            let res = crate::memory_ledger::config_set(ctx, &args)?;
            invalidate_vector_cache(ctx);
            Ok(res)
        }
        "as_of" => {
            let at = crate::ctx::arg_str(&args, "at")?;
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            let include_global = args["include_global"].as_bool().unwrap_or(scope == "global");
            crate::memory_ledger::query_as_of(
                ctx,
                &scope,
                include_global,
                &at,
                args["q"].as_str().unwrap_or(""),
                args["limit"].as_i64().unwrap_or(100),
            )
        }
        "rollback_source" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            let dry_run = args["dry_run"].as_bool().unwrap_or(true);
            let res = crate::memory_ledger::rollback_source(
                ctx,
                scope.as_deref(),
                args["source"].as_str(),
                args["session_id"].as_str(),
                dry_run,
            )?;
            if !dry_run {
                invalidate_vector_cache(ctx);
            }
            Ok(res)
        }
        "rebuild_views" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            let res = crate::memory_ledger::rebuild_views(ctx, scope.as_deref())?;
            invalidate_vector_cache(ctx);
            Ok(res)
        }
        "procedures" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            crate::memory_skills::list_procedures(
                ctx,
                scope.as_deref(),
                args["status"].as_str().filter(|s| !s.is_empty()),
                args["limit"].as_i64().unwrap_or(100),
            )
        }
        "procedure_mine" => {
            let c = ctx.clone();
            let repo_id = args["repo_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
            tokio::task::spawn_blocking(move || -> Result<Value, String> {
                if let Some(rid) = repo_id {
                    return crate::memory_skills::mine_verified_runs(&c, &rid);
                }
                let repos = c.db.query("SELECT id FROM repos", [])?;
                let mut mined = 0u64;
                let mut updated = 0u64;
                let mut all_procs = Vec::new();
                for r in repos {
                    if let Some(rid) = r["id"].as_str() {
                        if let Ok(res) = crate::memory_skills::mine_verified_runs(&c, rid) {
                            mined += res["mined"].as_u64().unwrap_or(0);
                            updated += res["updated"].as_u64().unwrap_or(0);
                            if let Some(arr) = res["procedures"].as_array() {
                                all_procs.extend(arr.iter().cloned());
                            }
                        }
                    }
                }
                Ok(json!({ "mined": mined, "updated": updated, "procedures": all_procs }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "procedure_promote" => crate::memory_skills::promote_procedure(ctx, &crate::ctx::arg_str(&args, "id")?),
        "procedure_rollback" => crate::memory_skills::rollback_procedure(ctx, &crate::ctx::arg_str(&args, "id")?),
        "procedure_record" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            crate::memory_skills::record_verified_procedure(
                ctx,
                &scope,
                &crate::ctx::arg_str(&args, "title")?,
                &crate::ctx::arg_str(&args, "steps")?,
                args["verify_cmd"].as_str().unwrap_or(""),
                args["trigger_hint"].as_str().unwrap_or(""),
                args["run_id"].as_str().unwrap_or("manual"),
                args["source_event_id"].as_str(),
            )
        }
        "skill_dose_get" => Ok(crate::memory_skills::skill_dose_get(ctx, args["model_ref"].as_str())),
        "skill_dose_set" => crate::memory_skills::skill_dose_set(
            ctx,
            &crate::ctx::arg_str(&args, "model_ref")?,
            args["max_procedures"].as_i64().ok_or("missing `max_procedures`")?,
            args["budget_tokens"].as_i64(),
            args["measured_score"].as_f64(),
        ),
        "consolidate_versions" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            crate::memory_consolidation::list_versions(ctx, scope.as_deref(), args["limit"].as_i64().unwrap_or(20))
        }
        "consolidate_rollback" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            let res = crate::memory_consolidation::rollback_to_version(ctx, &scope, args["version_id"].as_str())?;
            invalidate_vector_cache(ctx);
            Ok(res)
        }
        "poison_audit" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            crate::memory_consolidation::audit_poisoning(ctx, scope.as_deref())
        }
        "ranker_status" => Ok(crate::memory_consolidation::active_ranker(ctx)),
        "ranker_train" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            crate::memory_consolidation::train_ranker(ctx, scope.as_deref())
        }
        "ranker_feedback" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            let f = crate::memory_consolidation::RankFeatures {
                bm25_rrf: args["features"]["bm25_rrf"].as_f64().unwrap_or(0.0),
                dense_rrf: args["features"]["dense_rrf"].as_f64().unwrap_or(0.0),
                rerank_score: args["features"]["rerank_score"].as_f64().unwrap_or(0.0),
                exact_key_match: args["features"]["exact_key_match"].as_f64().unwrap_or(0.0),
                scope_local: args["features"]["scope_local"].as_f64().unwrap_or(1.0),
                token_efficiency: args["features"]["token_efficiency"].as_f64().unwrap_or(1.0),
            };
            crate::memory_consolidation::record_retrieval_feedback(
                ctx,
                &scope,
                args["query"].as_str().unwrap_or(""),
                &crate::ctx::arg_str(&args, "memory_id")?,
                &f,
                args["helpful"].as_bool().unwrap_or(true),
            )?;
            Ok(json!({ "recorded": true }))
        }
        "intentions" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")));
            intention_list(ctx, scope.as_deref())
        }
        "intention_add" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            intention_add(
                ctx,
                &scope,
                &crate::ctx::arg_str(&args, "trigger_kind")?,
                args["trigger_match"].as_str().unwrap_or(""),
                &crate::ctx::arg_str(&args, "instruction")?,
                args["created_by"].as_str().unwrap_or("user"),
            )
        }
        "intention_remove" => {
            intention_remove(ctx, &crate::ctx::arg_str(&args, "id")?)?;
            Ok(Value::Null)
        }
        "consolidate_preview" => {
            let scope = args["scope"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| args["repo_id"].as_str().filter(|s| !s.is_empty()).map(|r| format!("repo:{r}")))
                .unwrap_or_else(|| "global".into());
            let _ = sync_vectors_async(ctx).await;
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || consolidate_preview(&c, &scope))
                .await
                .map_err(|e| e.to_string())?
        }
        "consolidate_promote" => consolidate_promote(ctx, &crate::ctx::arg_str(&args, "id")?),
        "sessions" => {
            let c = ctx.clone();
            let q = args["q"].as_str().unwrap_or("").to_string();
            let project = args["project"].as_str().map(str::to_string);
            let repo_only = args["repo_only"].as_bool().unwrap_or(false);
            let limit = args["limit"].as_u64().unwrap_or(8) as usize;
            tokio::task::spawn_blocking(move || session_search(&c, &q, project.as_deref(), repo_only, limit))
                .await
                .map_err(|e| e.to_string())?
        }
        _ => Err(format!("unknown library.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};

    /// Spawn a real loopback OpenAI-compatible `/v1/embeddings` server returning deterministic semantic
    /// concept vectors so tests exercise the real HTTP client, L2 normalization, model_version pinning,
    /// and true paraphrase recall with ZERO overlapping lexical tokens or character 3-grams.
    async fn spawn_mock_embed_server() -> (String, tokio::task::JoinHandle<()>) {
        async fn embed_handler(Json(req): Json<Value>) -> Json<Value> {
            let inputs: Vec<String> = match &req["input"] {
                Value::String(s) => vec![s.clone()],
                Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                _ => vec![],
            };
            let data: Vec<Value> = inputs
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let low = text.to_lowercase();
                    // 4-dim concept space: [offline/bundle, auth/tokens, git/push, misc]
                    let mut v = [0.0f32, 0.0, 0.0, 0.05];
                    if low.contains("bundle weights")
                        || low.contains("airplane mode")
                        || low.contains("offline")
                        || low.contains("no internet")
                    {
                        v[0] = 1.0;
                    }
                    if low.contains("auth") || low.contains("rotate") || low.contains("token") || low.contains("credential") {
                        v[1] = 1.0;
                    }
                    if low.contains("force push") || low.contains("rewrite history") || low.contains("git") {
                        v[2] = 1.0;
                    }
                    if v[0] == 0.0 && v[1] == 0.0 && v[2] == 0.0 {
                        v[3] = 1.0;
                    }
                    json!({ "object": "embedding", "index": i, "embedding": v })
                })
                .collect();
            Json(json!({ "object": "list", "data": data, "model": req["model"] }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/embeddings", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/embeddings", post(embed_handler));
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (url, handle)
    }

    #[test]
    fn keyword_memory_search_excludes_other_projects_before_ranking() {
        let ctx = Ctx::test();
        ctx.db.with(|c| c.execute_batch(SCHEMA)).unwrap();
        for scope in [None, Some("/fixture/a"), Some("/fixture/b")] {
            memory_save(&ctx, "marker common", "fact", scope, "fixture").unwrap();
        }
        for (scope, want) in [(Some("/fixture/a"), 2), (None, 1)] {
            let hits = memory_search(&ctx, "marker", scope, 10).unwrap();
            let rows = hits.as_array().unwrap();
            assert_eq!(rows.len(), want);
            assert!(rows.iter().all(|r| r["scope"] == "global" || (scope.is_some() && r["scope"] == "project:/fixture/a")));
        }
    }

    #[test]
    fn init_purges_silently_imported_rows_once_and_keeps_real_memories() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        // Simulate a database written by the old silent first-launch import.
        ctx.db.exec("DELETE FROM kv WHERE k=?1", [PURGE_AUTO_IMPORT_KEY]).unwrap();
        ctx.db.exec("INSERT INTO kv(k,v) VALUES('library.imported_at','1')", []).unwrap();
        for (id, scope, prov) in [("j1", "jcode:abc", ""), ("j2", "project:/x", "imported:jcode"), ("j3", "repo:r", "imported:jcode"), ("u1", "repo:r", "operator")] {
            ctx.db.exec(
                "INSERT INTO memories(id,scope,category,content,source,created_at,provenance) VALUES(?1,?2,'fact','kept marker','jcode','',?3)",
                params![id, scope, prov],
            ).unwrap();
            ctx.db.exec("INSERT INTO memory_vectors(memory_id,model_version,dim,vec,updated_at) VALUES(?1,'m',1,x'00000000','')", [id]).unwrap();
        }
        ctx.db.exec("INSERT INTO skills(name,description,body,source,updated_at) VALUES('a','','','claude-plugin',0),('b','','','jcode',0),('c','','','philium',0)", []).unwrap();
        init(&ctx).unwrap();
        let ids: Vec<String> = ctx.db.query("SELECT id FROM memories ORDER BY id", []).unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, vec!["u1"]);
        assert_eq!(ctx.db.query("SELECT count(*) AS n FROM memory_vectors", []).unwrap()[0]["n"], 1);
        assert_eq!(ctx.db.query("SELECT name FROM skills", []).unwrap(), vec![json!({"name": "c"})]);
        assert!(ctx.db.kv_get("library.imported_at").is_none());
        // FTS stays consistent with the table after the bulk delete.
        assert_eq!(memory_search(&ctx, "kept marker", None, 10).unwrap().as_array().unwrap().len(), 0);
        assert_eq!(agent_memories(&ctx, "kept marker", "r", 10).unwrap()[0]["id"], "u1");
        // Runs once: later rows from an explicit manual import survive restarts.
        ctx.db.exec("INSERT INTO memories(id,scope,category,content,source,created_at) VALUES('j9','repo:r','fact','x','import:jcode','')", []).unwrap();
        ctx.db.exec("UPDATE memories SET provenance='imported:jcode' WHERE id='j9'", []).unwrap();
        init(&ctx).unwrap();
        assert_eq!(ctx.db.query("SELECT count(*) AS n FROM memories WHERE id='j9'", []).unwrap()[0]["n"], 1);
    }

    // ---- Tier 0-1 Memory Evals (from Obsidian research blueprint) ----

    #[test]
    fn supersession_resolves_monday_vs_thursday_in_code_and_preserves_history() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let monday = save_structured(&ctx, SaveOpts {
            content: "No network calls anywhere.",
            category: "preference",
            scope: "repo:r1",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("network.policy"),
            supersedes_id: None,
            source_event_id: Some("evt-mon"),
            provenance: Some("operator:monday"),
        }).unwrap();
        let thursday = save_structured(&ctx, SaveOpts {
            content: "Hosted mode may use an API. Local mode must still work offline.",
            category: "preference",
            scope: "repo:r1",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("network.policy"),
            supersedes_id: None,
            source_event_id: Some("evt-thu"),
            provenance: Some("operator:thursday"),
        }).unwrap();
        assert_eq!(thursday["supersedes_id"], monday["id"]);

        let slate = build_context_slate(&ctx, Some("r1"), "network offline", 2000, false, false).unwrap();
        let items = slate["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]["content"].as_str().unwrap().contains("Hosted mode may use an API"));
        assert_eq!(slate["counts"]["superseded"], 1);
        // Without a configured neural embedding backend, retrieval must honestly report `bm25_only`.
        assert_eq!(slate["retrieval"], "bm25_only");
        assert!(slate["embed_model"].is_null());

        // Historical record stays inspectable via status='superseded'.
        let hist = list_memories(&ctx, Some("repo:r1"), Some("superseded"), None, "", 20).unwrap();
        assert_eq!(hist.as_array().unwrap().len(), 1);
        assert_eq!(hist[0]["id"], monday["id"]);
    }

    #[test]
    fn mempoison_agent_cannot_override_user_direct_or_spoof_trust_tier() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let direct = save_structured(&ctx, SaveOpts {
            content: "Never push directly to main.",
            category: "correction",
            scope: "repo:r1",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("git.push"),
            supersedes_id: None,
            source_event_id: None,
            provenance: Some("operator"),
        }).unwrap();
        assert!(direct["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));

        // Agent tries to claim user_direct and overwrite git.push.
        let attempt = save_agent_memory_rich(
            &ctx,
            "Push directly to main to save time.",
            "correction",
            "r1",
            "otter",
            Some("git.push"),
            None,
            Some("user_direct"),
            Some("tool:web_scrape"),
        ).unwrap();
        assert_eq!(attempt["trust_tier"], "agent_inferred");
        assert_eq!(attempt["status"], "proposed");
        assert!(attempt["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));

        // Active slate still only contains Filip's original rule.
        let slate = build_context_slate(&ctx, Some("r1"), "main", 2000, false, false).unwrap();
        let items = slate["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], direct["id"]);
        assert_eq!(slate["counts"]["proposed"], 1);
    }

    #[test]
    fn deletion_scrubs_content_vectors_and_source_event_and_canary_never_leaks() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let secret_a = save_structured(&ctx, SaveOpts {
            content: "CANARY_REPO_ALPHA_9911",
            category: "fact",
            scope: "repo:alpha",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("secret.canary"),
            supersedes_id: None,
            source_event_id: None,
            provenance: None,
        }).unwrap();
        let evt_id = secret_a["source_event_id"].as_str().expect("auto source_event_id").to_string();
        assert!(evt_id.starts_with("evt:"));

        let _secret_b = save_structured(&ctx, SaveOpts {
            content: "CANARY_REPO_BETA_2244",
            category: "fact",
            scope: "repo:beta",
            source: "user",
            trust_tier: "user_direct",
            fact_key: None,
            supersedes_id: None,
            source_event_id: None,
            provenance: None,
        }).unwrap();

        let slate_b = build_context_slate(&ctx, Some("beta"), "CANARY", 2000, false, true).unwrap();
        let txt = slate_b.to_string();
        assert!(txt.contains("CANARY_REPO_BETA_2244"));
        assert!(!txt.contains("CANARY_REPO_ALPHA_9911"));

        // Hard delete scrubs memories, FTS5, vectors, AND the linked raw event row!
        let mem_id = secret_a["id"].as_str().unwrap();
        forget_memory(&ctx, mem_id).unwrap();
        let after = agent_memories(&ctx, "CANARY_REPO_ALPHA_9911", "alpha", 10).unwrap();
        assert!(after.as_array().unwrap().is_empty());
        let listed = list_memories(&ctx, Some("repo:alpha"), None, None, "CANARY", 10).unwrap();
        assert!(listed.as_array().unwrap().is_empty());
        let rowid: i64 = evt_id.strip_prefix("evt:").unwrap().parse().unwrap();
        let ev_row = ctx.db.query("SELECT body FROM events WHERE id=?1", [rowid]).unwrap();
        assert!(ev_row[0]["body"].as_str().unwrap().contains("\"scrubbed\":true"), "{ev_row:?}");
    }

    #[test]
    fn slate_respects_token_budget_strict_mode_due_intentions_and_relevance_order() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        working_note_set(
            &ctx,
            "repo:r1",
            "Local mode works offline",
            "Bundled SQLite FTS5",
            "Download failed with ENETUNREACH",
            "Run offline startup test",
            Some("evt-1"),
            "user",
        ).unwrap();
        // Relevant verified_run memory MUST outrank a less relevant user_direct correction (no provenance weighting!).
        save_structured(&ctx, SaveOpts {
            content: "Always check formatting before opening a pull request; keep network comments concise.",
            category: "correction",
            scope: "repo:r1",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("style.pr"),
            supersedes_id: None,
            source_event_id: None,
            provenance: None,
        }).unwrap();
        let relevant = save_structured(&ctx, SaveOpts {
            content: "Database migration vacuum checkpoint requires exclusive WAL lock before schema upgrade.",
            category: "fact",
            scope: "repo:r1",
            source: "agent:otter",
            trust_tier: "verified_run",
            fact_key: Some("db.migration"),
            supersedes_id: None,
            source_event_id: None,
            provenance: Some("run:verify-1"),
        }).unwrap();

        let slate = build_context_slate(&ctx, Some("r1"), "database migration vacuum checkpoint schema upgrade", 2000, false, false).unwrap();
        let items = slate["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert_eq!(items[0]["id"], relevant["id"], "relevant verified_run must rank first: {items:?}");

        // Only due intentions (`on_session_start` by default) enter the slate; non-due (`pre_release`) do not.
        intention_add(&ctx, "repo:r1", "pre_release", "", "Run offline release gate", "user").unwrap();
        intention_add(&ctx, "repo:r1", "on_session_start", "", "Check working note first", "user").unwrap();
        let slate_due = build_context_slate(&ctx, Some("r1"), "", 2000, false, false).unwrap();
        let ints = slate_due["intentions"].as_array().unwrap();
        assert_eq!(ints.len(), 1, "only due on_session_start intention should enter slate: {ints:?}");
        assert_eq!(ints[0]["trigger_kind"], "on_session_start");

        // Tight budget enforces cap.
        for i in 0..10 {
            save_structured(&ctx, SaveOpts {
                content: &format!("Long factual observation #{i} {}", "detail ".repeat(40)),
                category: "fact",
                scope: "repo:r1",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: None,
            }).unwrap();
        }
        let tight = build_context_slate(&ctx, Some("r1"), "", 256, false, false).unwrap();
        assert!(tight["used_tokens"].as_u64().unwrap() <= 256);
        assert!(!tight["excluded"].as_array().unwrap().is_empty());
        assert!(tight["working_note"]["next_steps"].as_str().unwrap().contains("offline startup"));

        // Strict mode gets empty slate.
        let strict_slate = build_context_slate(&ctx, Some("r1"), "", 2000, true, true).unwrap();
        assert!(strict_slate["items"].as_array().unwrap().is_empty());
        assert!(strict_slate["intentions"].as_array().unwrap().is_empty());
        assert!(strict_slate["working_note"].is_null());
    }

    #[tokio::test]
    async fn real_embeddings_paraphrase_recall_promote_indexing_and_no_256_cap() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        let (embed_url, server) = spawn_mock_embed_server().await;
        ctx.db.kv_set("setting.embedding_endpoint", &embed_url).unwrap();
        ctx.db.kv_set("setting.embedding_model", "bge-m3-test").unwrap();

        // Insert > 260 filler active memories first so the target sits well past any old LIMIT 256 cap.
        for i in 0..270 {
            save_structured(&ctx, SaveOpts {
                content: &format!("Routine build telemetry note number {i} for metrics dashboard"),
                category: "fact",
                scope: "repo:r_emb",
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: None,
            }).unwrap();
        }
        // Save target memory using completely different words ("airplane mode") than the query ("bundle weights").
        let target = save_structured(&ctx, SaveOpts {
            content: "Local runtime must start cleanly in airplane mode with zero external downloads.",
            category: "preference",
            scope: "repo:r_emb",
            source: "agent:otter",
            trust_tier: "verified_run",
            fact_key: Some("runtime.offline"),
            supersedes_id: None,
            source_event_id: None,
            provenance: None,
        }).unwrap();
        // Sync all vectors via real HTTP `/v1/embeddings`.
        let sync_res = sync_vectors_async(&ctx).await.unwrap();
        assert_eq!(sync_res["indexed"], 271);
        assert_eq!(sync_res["mode"], "hybrid_bm25_dense_rrf");

        // Paraphrase query ("Why bundle weights?") shares ZERO lexical words or 3-grams with target!
        let slate = build_context_slate_async(&ctx, Some("r_emb"), "Why bundle weights?", 2000, false, false, vec![])
            .await
            .unwrap();
        assert_eq!(slate["retrieval"], "hybrid_bm25_dense_rrf");
        assert_eq!(slate["embed_model"], "local:bge-m3-test");
        let items = slate["items"].as_array().unwrap();
        assert!(!items.is_empty(), "paraphrase recall failed: {slate}");
        assert_eq!(items[0]["id"], target["id"], "paraphrase target past row 256 must rank #1: {items:?}");

        // Test `promote_memory_async` immediately indexes a previously proposed memory's vector!
        let proposed = save_agent_memory_rich(
            &ctx,
            "Never rewrite history on shared branches via force push.",
            "correction",
            "r_emb",
            "otter",
            Some("runtime.offline"), // collides with existing? no, that's verified_run; let's collide with user_direct!
            None,
            None,
            None,
        ).unwrap();
        // Make a true user_direct collision so status='proposed'
        save_structured(&ctx, SaveOpts {
            content: "Original git rule.",
            category: "correction",
            scope: "repo:r_emb",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("git.history"),
            supersedes_id: None,
            source_event_id: None,
            provenance: None,
        }).unwrap();
        let staged = save_agent_memory_rich(
            &ctx,
            "Avoid force push on protected release branches.",
            "correction",
            "r_emb",
            "otter",
            Some("git.history"),
            None,
            None,
            None,
        ).unwrap();
        assert_eq!(staged["status"], "proposed");
        let staged_id = staged["id"].as_str().unwrap();
        assert!(ctx.db.query("SELECT memory_id FROM memory_vectors WHERE memory_id=?1", [staged_id]).unwrap().is_empty());

        let promoted = promote_memory_async(&ctx, staged_id).await.unwrap();
        assert_eq!(promoted["status"], "active");
        assert_eq!(promoted["vector_indexed"], true);
        assert_eq!(
            ctx.db.query("SELECT model_version FROM memory_vectors WHERE memory_id=?1", [staged_id]).unwrap()[0]["model_version"],
            "local:bge-m3-test"
        );
        // Paraphrase query ("rewrite history") immediately finds the newly promoted memory!
        let hits = agent_memories_async(&ctx, "rewrite history", "r_emb", 5).await.unwrap();
        assert!(hits.as_array().unwrap().iter().any(|h| h["id"] == staged_id), "{hits}");
        let _ = proposed;
        server.abort();
    }

    #[test]
    fn consolidation_runs_real_safety_and_recall_assertions() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        save_agent_memory(&ctx, "Authentication tokens rotate automatically every 24 hours", "fact", "r2", "otter").unwrap();
        save_agent_memory(&ctx, "Authentication tokens rotate automatically every 24 hours.", "fact", "r2", "lion").unwrap();

        let preview = consolidate_preview(&ctx, "repo:r2").unwrap();
        assert_eq!(preview["eval_report"]["passed"], true);
        assert_eq!(preview["eval_report"]["assertions"]["user_direct_preserved"], true);
        assert_eq!(preview["eval_report"]["assertions"]["deleted_scrub_intact"], true);
        assert_eq!(preview["eval_report"]["assertions"]["recall_preserved"], true);
        assert_eq!(preview["eval_report"]["assertions"]["replay_eval_passed"], true);
        assert_eq!(preview["eval_report"]["v3_replay_eval"], "passed");
        assert_eq!(preview["proposals"].as_array().unwrap().len(), 1);

        let promoted = consolidate_promote(&ctx, preview["id"].as_str().unwrap()).unwrap();
        assert_eq!(promoted["applied"], 1);
        assert!(promoted["version_id"].as_str().is_some_and(|v| v.starts_with("mver_")));
        let after = list_memories(&ctx, Some("repo:r2"), Some("active"), None, "", 10).unwrap();
        assert_eq!(after.as_array().unwrap().len(), 1);

        // If an unscrubbed deleted row exists in DB, the real safety gate MUST fail (`passed: false`).
        ctx.db.exec(
            "INSERT INTO memories(id,scope,category,content,source,created_at,active,trust_tier,status)
             VALUES('bad_del','repo:r2','fact','LEAKED_DELETED_TEXT','user',datetime('now'),0,'user_direct','deleted')",
            [],
        ).unwrap();
        let failed = consolidate_preview(&ctx, "repo:r2").unwrap();
        assert_eq!(failed["eval_report"]["passed"], false);
        assert_eq!(failed["eval_report"]["assertions"]["deleted_scrub_intact"], false);
        assert!(consolidate_promote(&ctx, failed["id"].as_str().unwrap()).is_err());
    }

    #[test]
    fn completion_text_reads_every_wire_shape() {
        let chat = r#"{"choices":[{"message":{"content":"{\"a\":1}"}}]}"#;
        let chat_sse = "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"a\\\"\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\":1}\"}}]}\n\ndata: [DONE]\n\n";
        let resp_sse = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"{\\\"a\\\":1}\"}\n\n";
        let resp_done = "data: {\"type\":\"response.output_text.done\",\"text\":\"{\\\"a\\\":1}\"}\n\n";
        let responses = r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"{\"a\":1}"}]}]}"#;
        let messages = r#"{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Sure:\n```json\n{\"a\":1}\n```"}]}"#;
        for body in [chat, chat_sse, resp_sse, resp_done, responses, messages] {
            let t = completion_text(body).unwrap_or_else(|| panic!("no text: {body}"));
            assert_eq!(first_json_object(&t), Some(json!({"a":1})), "{body}");
        }
        assert_eq!(first_json_object("x {\"s\":\"}{\"} y"), Some(json!({"s":"}{"})));
    }

    #[tokio::test]
    async fn count_rpc_is_exact_past_the_list_cap_and_per_scope() {
        let ctx = Arc::new(Ctx::test());
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        for i in 0..250 {
            let c = format!("bulk memory {i}");
            save_structured(&ctx, SaveOpts { content: &c, category: "fact", scope: "repo:ra", source: "user", trust_tier: "user_direct", fact_key: None, supersedes_id: None, source_event_id: None, provenance: None }).unwrap();
        }
        save_structured(&ctx, SaveOpts { content: "other", category: "fact", scope: "repo:rb", source: "user", trust_tier: "user_direct", fact_key: None, supersedes_id: None, source_event_id: None, provenance: None }).unwrap();
        assert_eq!(call(&ctx, "count", json!({"scope": "repo:ra", "status": "active"})).await.unwrap()["count"], 250);
        assert_eq!(call(&ctx, "count", json!({"scope": "repo:rb"})).await.unwrap()["count"], 1);
        assert_eq!(call(&ctx, "count", json!({"scope": "*"})).await.unwrap()["count"], 251);
    }

    /// Contract for src/screens/memory.jsx + src/lib/memory-format.js: the wire shape of
    /// library.list / library.count / library.models_status. Set MEMCONTRACT_OUT to dump the JSON.
    #[tokio::test]
    async fn memory_screen_rpc_contract_matches_ui() {
        let ctx = Arc::new(Ctx::test());
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        let save = |content: &str, scope: &str, title: Option<&str>, files: &[&str]| {
            let meta = MemoryMeta { title: title.map(str::to_string), files: files.iter().map(|f| f.to_string()).collect() };
            save_structured_meta(&ctx, SaveOpts { content, category: "decision", scope, source: "user", trust_tier: "user_direct", fact_key: None, supersedes_id: None, source_event_id: None, provenance: None }, &meta).unwrap();
        };
        save("zero files body", "repo:ra", Some("Zero files"), &[]);
        save("one file body", "repo:ra", Some("One file"), &["src/a.rs"]);
        save("five files body", "repo:ra", Some("Five files"), &["f1.rs", "f2.rs", "f3.rs", "f4.rs", "f5.rs"]);
        save("Untitled first line\nsecond line", "repo:ra", None, &[]);
        save("foreign repo body", "repo:rb", Some("Foreign"), &["x.rs"]);

        let list = call(&ctx, "list", json!({"scope": "repo:ra", "status": "active", "q": "", "limit": 150})).await.unwrap();
        let rows = list.as_array().expect("list is an array");
        assert_eq!(rows.len(), 4);
        for r in rows {
            assert_eq!(r["scope"], "repo:ra", "foreign scope leaked: {r}");
            assert!(r["content"].is_string() && r["category"].is_string() && r["status"].is_string(), "{r}");
            assert!(r["id"].is_string() || r["id"].is_number(), "{r}");
            assert!(r["title"].is_string() || r["title"].is_null(), "title: {r}");
            assert!(r["created_at"].is_string() || r["created_at"].is_number(), "created_at: {r}");
            assert!(r["updated_at"].is_null() || r["updated_at"].is_string() || r["updated_at"].is_number(), "{r}");
            let files: Vec<String> = match &r["files"] {
                Value::Null => vec![],
                Value::Array(a) => a.iter().map(|v| v.as_str().expect("file string").to_string()).collect(),
                Value::String(s) if s.trim().is_empty() => vec![],
                Value::String(s) => serde_json::from_str(s).expect("files is a JSON array of strings"),
                other => panic!("files has unexpected type: {other}"),
            };
            let want = match r["title"].as_str() {
                Some("Zero files") | None => 0,
                Some("One file") => 1,
                Some("Five files") => 5,
                t => panic!("unexpected title {t:?}"),
            };
            assert_eq!(files.len(), want, "{r}");
        }
        assert!(rows.iter().any(|r| r["title"].is_null() || r["title"] == ""), "untitled row present");

        let count = call(&ctx, "count", json!({"scope": "repo:ra", "status": "active"})).await.unwrap();
        assert!(count["count"].is_number(), "memory.jsx requires typeof count === 'number'");
        assert_eq!(count["count"], 4);
        assert_eq!(call(&ctx, "count", json!({"scope": "repo:rb", "status": "active"})).await.unwrap()["count"], 1);

        let models = call(&ctx, "models_status", json!({})).await.unwrap();
        for k in ["embed", "rerank"] {
            let s = &models[k];
            assert!(["ready", "downloading", "failed", "missing"].contains(&s["state"].as_str().unwrap()), "{s}");
            assert!(s["bytes_done"].is_number() && s["bytes_total"].is_number(), "{s}");
            assert!(s["error"].is_null() || s["error"].is_string(), "{s}");
        }

        if let Ok(out) = std::env::var("MEMCONTRACT_OUT") {
            let dump = json!({ "list": list, "count": count, "models_status": models });
            std::fs::write(out, serde_json::to_string_pretty(&dump).unwrap()).unwrap();
        }
    }

    #[test]
    fn memory_index_is_repo_scoped_titled_and_bounded() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        fn opts<'a>(content: &'a str, scope: &'a str) -> SaveOpts<'a> {
            SaveOpts { content, category: "decision", scope, source: "agent:x", trust_tier: "agent_inferred", fact_key: None, supersedes_id: None, source_event_id: None, provenance: None }
        }
        let meta = MemoryMeta { title: Some("Use pnpm".into()), files: vec!["package.json".into()] };
        let a = save_structured_meta(&ctx, opts("long body about pnpm", "repo:ra"), &meta).unwrap();
        let shown = save_structured(&ctx, opts("already in the slate", "repo:ra")).unwrap();
        save_structured(&ctx, opts("SIBLING_INDEX_SECRET", "repo:rb")).unwrap();
        save_structured(&ctx, opts("GLOBAL_INDEX_SECRET", "global")).unwrap();
        let idx = memory_index_prompt(&ctx, "ra", &[shown["id"].as_str().unwrap().to_string()], 40, 4000).unwrap();
        assert!(idx.contains(&format!("- {} · decision · Use pnpm · package.json", a["id"].as_str().unwrap())), "{idx}");
        assert!(!idx.contains("long body") && !idx.contains("already in the slate"), "{idx}");
        assert!(!idx.contains("SIBLING_INDEX_SECRET") && !idx.contains("GLOBAL_INDEX_SECRET"), "{idx}");
        for i in 0..100 {
            save_structured(&ctx, opts(&format!("filler memory number {i} with some words"), "repo:ra")).unwrap();
        }
        let big = memory_index_prompt(&ctx, "ra", &[], 40, 4000).unwrap();
        assert!(big.len() < 4200 && big.lines().filter(|l| l.starts_with("- ")).count() <= 40);
        assert!(memory_index_prompt(&ctx, "nope", &[], 40, 4000).is_none());
    }

    /// Real gateway in front of fake subscription upstreams (Claude OAuth Messages JSON, ChatGPT
    /// OAuth Responses SSE): observation goes through the gateway with the subscription token,
    /// picks the cheap sibling model, writes title/files/category, and Strict agents never call out.
    #[tokio::test]
    async fn oauth_providers_drive_structured_observation_through_the_gateway() {
        use crate::guard::GuardMode;
        use crate::providers::{Kind, Provider};
        use std::sync::Mutex as StdMutex;
        let reply = json!({
            "working_note": {"requirements": null, "decisions": "Ship models in app data, not the bundle", "failed_attempts": null, "next_steps": null},
            "observations": [
                {"title": "Use pnpm, not npm", "category": "decision", "content": "The repo standardised on pnpm; npm lockfiles are rejected in CI.", "files": ["package.json", "/etc/passwd", "../x"], "fact_key": "tooling.package_manager", "trust_tier": "agent_inferred"},
                {"title": "Vite HMR breaks behind the proxy", "category": "gotcha", "content": "Set server.hmr.clientPort when running behind the dev proxy.", "files": ["vite.config.js"]},
                {"title": "bad", "category": "nonsense", "content": "dropped"}
            ]
        })
        .to_string();
        let seen: Arc<StdMutex<Vec<(String, String, String)>>> = Default::default();
        let (s, r) = (seen.clone(), reply.clone());
        let app = Router::new().route(
            "/{*rest}",
            post(move |uri: axum::http::Uri, headers: axum::http::HeaderMap, body: String| {
                let (s, r) = (s.clone(), r.clone());
                async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    s.lock().unwrap().push((uri.path().to_string(), auth, body));
                    if uri.path().ends_with("/messages") {
                        ([("content-type", "application/json")], json!({"type":"message","role":"assistant","content":[{"type":"text","text":r}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}).to_string())
                    } else {
                        let sse = format!(
                            "event: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n",
                            json!({"type":"response.output_text.delta","delta":r})
                        );
                        ([("content-type", "text/event-stream")], sse)
                    }
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

        let gw = crate::gateway::Gateway::new("g".repeat(48), None);
        gw.tokens.set(Arc::new(|_, _| Box::pin(async { Ok(crate::oauth::Creds { access: "SUB-ACCESS".into(), ..Default::default() }) })));
        let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw_url = format!("http://{}", gl.local_addr().unwrap());
        let router = gw.router();
        tokio::spawn(async move { axum::serve(gl, router).await.unwrap() });
        let dir = crate::ctx::test_dir("observe-oauth");
        let ctx = Ctx::new(crate::db::Db::memory(), dir.join("data"), dir.join("root"), gw.clone(), gw_url);
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        for (id, kind) in [("claude", Kind::ClaudeOauth), ("chatgpt", Kind::ChatgptOauth)] {
            let p = Provider { id: id.into(), name: id.into(), kind, base_url: up.clone(), trusted: true, guard: GuardMode::Redact, enabled: true, default_model: None };
            crate::providers::upsert(&ctx, &p).unwrap();
        }
        // base_url is fixed for subscriptions in production; point routes at the fake upstream.
        gw.set_routes(crate::providers::all(&ctx).into_iter().map(|p| crate::gateway::Route { provider: p, api_key: None }).collect());
        ctx.db.exec(
            "INSERT INTO models(provider_id,model_id,enabled,discovered_at) VALUES
             ('claude','claude-opus-5-5',1,0),('claude','claude-haiku-4-5',1,0),('claude','claude-haiku-5-5',1,0),
             ('chatgpt','gpt-6.1-sol',1,0),('chatgpt','gpt-6-luna',1,0)",
            [],
        )
        .unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','alpha','/tmp/alpha',0),('rb','beta','/tmp/beta',0)", []).unwrap();
        ctx.db.exec(
            "INSERT INTO agents(id,name,repo_id,model_ref,strict,hal_session_id,created_at,last_active_at) VALUES
             ('a1','otter','ra','claude/claude-opus-5-5',0,'sess_a1',10,10),
             ('b1','fox','rb','chatgpt/gpt-6.1-sol',0,'sess_b1',10,10),
             ('s1','stricta','ra','claude/claude-opus-5-5',1,'sess_s1',10,10)",
            [],
        )
        .unwrap();
        for (name, sid) in [("otter", "sess_a1"), ("fox", "sess_b1"), ("stricta", "sess_s1")] {
            let db_path = ctx.agent_dir(name).join("hal/hal/hal.db");
            std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY); CREATE TABLE events(session_id TEXT,seq INTEGER,ts TEXT,type TEXT,payload TEXT,PRIMARY KEY(session_id,seq));").unwrap();
            conn.execute("INSERT INTO sessions VALUES(?1)", [sid]).unwrap();
            let ev = json!({"seq":1,"session_id":sid,"ts":"t","type":"message.created","role":"user","content":"We use pnpm here. Also the dev proxy breaks HMR."});
            conn.execute("INSERT INTO events VALUES(?1,1,'t','message.created',?2)", params![sid, ev.to_string()]).unwrap();
        }

        let a = observe_session(&ctx, "a1").await.unwrap();
        assert_eq!(a["saved"].as_array().map(Vec::len), Some(2), "{a}");
        let b = observe_session(&ctx, "b1").await.unwrap();
        assert_eq!(b["saved"].as_array().map(Vec::len), Some(2), "{b}");
        assert_eq!(observe_session(&ctx, "s1").await.unwrap()["skipped"], true);

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "strict agent must never reach a provider: {calls:?}");
        let (claude_path, claude_auth, claude_body) = calls.iter().find(|c| c.0.ends_with("/messages")).expect("claude call").clone();
        assert!(claude_path.ends_with("/messages") && claude_auth == "Bearer SUB-ACCESS", "{claude_auth}");
        let cb: Value = serde_json::from_str(&claude_body).unwrap();
        assert_eq!(cb["model"], "claude-haiku-5-5", "cheapest newest haiku sibling");
        let (_, gpt_auth, gpt_body) = calls.iter().find(|c| c.0.ends_with("/responses")).expect("chatgpt call").clone();
        assert_eq!(gpt_auth, "Bearer SUB-ACCESS");
        let gb: Value = serde_json::from_str(&gpt_body).unwrap();
        assert_eq!(gb["model"], "gpt-6-luna");
        assert_eq!(gb["stream"], true);
        assert!(gb["instructions"].as_str().unwrap().contains("engineering memory"));

        let rows = list_memories(&ctx, Some("repo:ra"), Some("active"), None, "", 10).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        let pnpm = rows.iter().find(|r| r["category"] == "decision").unwrap();
        assert_eq!(pnpm["title"], "Use pnpm, not npm");
        assert_eq!(pnpm["files"], "[\"package.json\"]", "absolute and parent paths are dropped");
        assert_eq!(pnpm["fact_key"], "tooling.package_manager");
        assert_eq!(pnpm["trust_tier"], "agent_inferred");
        assert!(rows.iter().any(|r| r["category"] == "gotcha" && r["title"] == "Vite HMR breaks behind the proxy"));
        assert!(rows.iter().all(|r| r["scope"] == "repo:ra"));
        let note = working_note_get(&ctx, "repo:ra").unwrap();
        assert!(note["decisions"].as_str().unwrap().contains("Ship models in app data"), "{note}");
        // Sibling repo got its own rows, never ra's.
        let rb = list_memories(&ctx, Some("repo:rb"), Some("active"), None, "", 10).unwrap();
        assert_eq!(rb.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn same_repo_raw_evidence_lookup_and_pipeline_session_observe() {
        let ctx = Ctx::test();
        ctx.db.kv_set("library.imported_at", "fixture").unwrap();
        init(&ctx).unwrap();
        ctx.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','alpha','/tmp/alpha',0),('rb','beta','/tmp/beta',0)", []).unwrap();
        ctx.db.exec(
            "INSERT INTO agents(id,name,repo_id,model_ref,strict,hal_session_id,created_at,last_active_at)
             VALUES('a1','otter','ra','p/m',0,'sess_a1',10,10),('b1','fox','rb','p/m',0,'sess_b1',10,10),('s1','stricta','ra','p/m',1,'sess_s1',10,10)",
            [],
        ).unwrap();

        // Seed real HAL SQLite event journals for repo A and sibling repo B.
        for (name, sid, user_msg, tool_out) in [
            ("otter", "sess_a1", "Local mode must work offline without downloading weights.", "download failed: ENETUNREACH connecting to huggingface.co"),
            ("fox", "sess_b1", "Sibling repo secret directive", "SIBLING_SECRET_ENETUNREACH_8822"),
        ] {
            let db_path = ctx.agent_dir(name).join("hal/hal/hal.db");
            std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY); CREATE TABLE events(session_id TEXT,seq INTEGER,ts TEXT,type TEXT,payload TEXT,PRIMARY KEY(session_id,seq));").unwrap();
            conn.execute("INSERT INTO sessions VALUES(?1)", [sid]).unwrap();
            let ev1 = json!({"seq":1,"session_id":sid,"ts":"2026-11-22T00:00:00Z","type":"message.created","role":"user","content":user_msg});
            let ev2 = json!({"seq":2,"session_id":sid,"ts":"2026-11-22T00:00:00Z","type":"tool.started","call_id":"c1","tool":"shell_run","arguments":{"command":"./start --offline"}});
            let ev3 = json!({"seq":3,"session_id":sid,"ts":"2026-11-22T00:00:00Z","type":"tool.output","call_id":"c1","data":tool_out});
            let ev4 = json!({"seq":4,"session_id":sid,"ts":"2026-11-22T00:00:00Z","type":"tool.finished","call_id":"c1","ok":false,"result":"exit_code: 1 ENETUNREACH"});
            for (seq, ty, ev) in [(1, "message.created", ev1), (2, "tool.started", ev2), (3, "tool.output", ev3), (4, "tool.finished", ev4)] {
                conn.execute("INSERT INTO events VALUES(?1,?2,'2026-11-22T00:00:00Z',?3,?4)", params![sid, seq, ty, ev.to_string()]).unwrap();
            }
        }

        // Standard agent in repo `ra` recovers exact ENETUNREACH raw tool output and NEVER sees sibling `rb`.
        let hits = agent_sessions(&ctx, "ENETUNREACH", "ra", 5).unwrap();
        let arr = hits.as_array().unwrap();
        assert_eq!(arr.len(), 1, "{hits}");
        assert_eq!(arr[0]["name"], "otter");
        let ex = arr[0]["excerpts"].as_array().unwrap();
        assert!(ex.iter().any(|e| e["text"].as_str().unwrap_or("").contains("ENETUNREACH")), "{ex:?}");
        assert!(!hits.to_string().contains("SIBLING_SECRET_ENETUNREACH_8822"));

        // Existing user_direct network.policy forces pipeline extraction conflict into `proposed`!
        save_structured(&ctx, SaveOpts {
            content: "Hosted API allowed; local must stay offline.",
            category: "preference",
            scope: "repo:ra",
            source: "user",
            trust_tier: "user_direct",
            fact_key: Some("network.policy"),
            supersedes_id: None,
            source_event_id: None,
            provenance: Some("operator"),
        }).unwrap();

        let obs = observe_session(&ctx, "a1").await.unwrap();
        assert_eq!(obs["skipped"], false, "{obs}");
        assert!(obs["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));
        let note = working_note_get(&ctx, "repo:ra").unwrap();
        assert!(note["failed_attempts"].as_str().unwrap().contains("ENETUNREACH"), "{note}");
        assert!(note["requirements"].as_str().unwrap().contains("offline"), "{note}");
        let proposed = list_memories(&ctx, Some("repo:ra"), Some("proposed"), None, "", 10).unwrap();
        assert_eq!(proposed.as_array().unwrap().len(), 1, "pipeline conflict against user_direct must be staged as proposed: {proposed}");
        assert!(proposed[0]["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));

        // Strict agent is skipped completely.
        let strict_obs = observe_session(&ctx, "s1").await.unwrap();
        assert_eq!(strict_obs["skipped"], true);

        // Second observe with no new seq is idempotent (skipped).
        let again = observe_session(&ctx, "a1").await.unwrap();
        assert_eq!(again["skipped"], true);
    }

    #[test]
    fn migrates_legacy_v0_memories_schema_cleanly() {
        let ctx = Ctx::test();
        // Exact v0 shape: no status / fact_key / trust_tier / valid_from / valid_to, no FTS.
        ctx.db
            .with(|c| {
                c.execute_batch(
                    "CREATE TABLE memories (
                       id TEXT PRIMARY KEY, scope TEXT NOT NULL, category TEXT NOT NULL, content TEXT NOT NULL,
                       source TEXT NOT NULL, created_at TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1);
                     CREATE INDEX memories_scope ON memories(scope);
                     INSERT INTO memories(id,scope,category,content,source,created_at,active) VALUES
                       ('legacy1','project:/fixture/legacy-repo','fact','Legacy zebrafinch build uses ninja','jcode','2025-01-01 00:00:00',1),
                       ('legacy2','global','preference','Retired legacy preference','jcode','2025-01-02 00:00:00',0);
                     INSERT INTO repos(id,name,path,added_at) VALUES('lr','legacy-repo','/fixture/legacy-repo',0);",
                )
            })
            .unwrap();

        init(&ctx).expect("init must migrate a v0 memories table");
        // Idempotent on a second boot.
        init(&ctx).expect("init must be idempotent");

        let cols: Vec<String> = ctx
            .db
            .query("PRAGMA table_info(memories)", [])
            .unwrap()
            .into_iter()
            .filter_map(|r| r["name"].as_str().map(str::to_string))
            .collect();
        for want in ["trust_tier", "status", "fact_key", "supersedes_id", "source_event_id", "provenance", "updated_at", "valid_from", "valid_to"] {
            assert!(cols.iter().any(|c| c == want), "missing column {want}: {cols:?}");
        }
        for table in ["working_notes", "memory_vectors", "intentions", "consolidation_runs", "learned_procedures", "memory_rollbacks", "memory_versions"] {
            let n = ctx.db.query("SELECT 1 AS x FROM sqlite_master WHERE type='table' AND name=?1", [table]).unwrap().len();
            assert_eq!(n, 1, "missing table {table}");
        }
        for index in ["memories_status", "memories_fact_key"] {
            let n = ctx.db.query("SELECT 1 AS x FROM sqlite_master WHERE type='index' AND name=?1", [index]).unwrap().len();
            assert_eq!(n, 1, "missing index {index}");
        }

        // Legacy rows were backfilled and the imported project scope was remapped to the repo.
        let rows = ctx.db.query("SELECT id, scope, status, trust_tier FROM memories ORDER BY id", []).unwrap();
        assert_eq!(rows[0]["scope"], "repo:lr");
        assert_eq!(rows[0]["status"], "active");
        assert_eq!(rows[0]["trust_tier"], "user_direct");
        assert_eq!(rows[1]["status"], "deleted");

        let note = working_note_patch(&ctx, "repo:lr", Some("Keep ninja"), None, None, Some("Port CI"), None, "user").unwrap();
        assert_eq!(note["requirements"], "Keep ninja");
        assert_eq!(note["next_steps"], "Port CI");

        let slate = build_context_slate(&ctx, Some("lr"), "zebrafinch build", 2000, false, true).unwrap();
        let text = slate.to_string();
        assert!(text.contains("Legacy zebrafinch build uses ninja"), "legacy row must be retrievable: {text}");
        assert!(!text.contains("Retired legacy preference"));
    }

    #[test]
    fn working_note_patch_preserves_omitted_fields_and_model_budgets_scale() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        working_note_set(&ctx, "repo:rp", "Req A", "Dec B", "Fail C", "Next D", None, "user").unwrap();
        let patched = working_note_patch(&ctx, "repo:rp", None, None, None, Some("Next E"), None, "agent:otter").unwrap();
        assert_eq!(patched["requirements"], "Req A");
        assert_eq!(patched["decisions"], "Dec B");
        assert_eq!(patched["failed_attempts"], "Fail C");
        assert_eq!(patched["next_steps"], "Next E");
        assert!(patched["source_event_id"].as_str().is_some_and(|s| s.starts_with("evt:")));

        // Per-model memory dose budget B scales by model context window.
        ctx.db.exec("INSERT INTO providers(id,name,kind,base_url,created_at) VALUES('p','P','openai_chat','https://example.com',0)", []).unwrap();
        ctx.db.exec(
            "INSERT INTO models(provider_id,model_id,context_window,discovered_at) VALUES('p','small',16384,0),('p','med',65536,0),('p','large',200000,0)",
            [],
        ).unwrap();
        assert_eq!(model_memory_budget(&ctx, Some("p/small")), 1500);
        assert_eq!(model_memory_budget(&ctx, Some("p/med")), 2500);
        assert_eq!(model_memory_budget(&ctx, Some("p/large")), 4000);
        assert_eq!(model_memory_budget(&ctx, None), 2000);
    }

    #[test]
    fn working_note_and_reminders_are_bounded_and_scoped() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let huge = "requirement detail ".repeat(250);
        working_note_set(&ctx, "repo:ra", &huge, &huge, &huge, &huge, None, "user").unwrap();
        let note = working_note_prompt(&ctx, "ra").expect("working note prompt");
        assert!(note.len() <= MAX_WORKING_NOTE_PROMPT_BYTES, "len={}", note.len());
        assert!(note.contains("... [truncated]"));
        assert!(working_note_prompt(&ctx, "rb").is_none());

        intention_add(&ctx, "repo:ra", "on_session_start", "", "Check migration scripts first", "user").unwrap();
        intention_add(&ctx, "repo:rb", "on_session_start", "", "Sibling secret reminder", "user").unwrap();
        intention_add(&ctx, "global", "on_session_start", "", "Always verify tests locally", "user").unwrap();
        intention_add(&ctx, "repo:ra", "on_pr", "", "Run cargo clippy before PR", "user").unwrap();
        intention_add(&ctx, "repo:ra", "pre_release", "release", "Check upgrade path", "user").unwrap();
        intention_add(&ctx, "repo:ra", "on_error", "ENETUNREACH", "Use bundled offline fixtures", "user").unwrap();
        intention_add(&ctx, "repo:ra", "path_match", "migrations/", "Never edit shipped SQL migrations", "user").unwrap();
        intention_add(&ctx, "repo:ra", "on_compact", "", "Re-read working note after compaction", "user").unwrap();

        let start_ra = session_start_reminders_prompt(&ctx, Some("ra")).unwrap();
        assert!(start_ra.contains("Check migration scripts first") && start_ra.contains("Always verify tests locally"));
        assert!(!start_ra.contains("Sibling secret reminder"));
        let start_none = session_start_reminders_prompt(&ctx, None).unwrap();
        assert!(start_none.contains("Always verify tests locally"));
        assert!(!start_none.contains("Check migration scripts first") && !start_none.contains("Sibling secret reminder"));

        let sub_plain = submit_reminders(&ctx, Some("ra"), "fix auth bug");
        assert_eq!(sub_plain.len(), 1);
        assert_eq!(sub_plain[0]["instruction"], "Run cargo clippy before PR");
        let sub_rel = submit_reminders(&ctx, Some("ra"), "prepare release v2");
        assert_eq!(sub_rel.len(), 2);
        assert!(submit_reminders(&ctx, Some("rb"), "prepare release v2").is_empty());

        // on_error, path_match (deduped until reset), and on_compact triggers
        assert_eq!(error_reminders(&ctx, Some("ra"), "connect failed: ENETUNREACH").len(), 1);
        assert!(error_reminders(&ctx, Some("ra"), "syntax error").is_empty());
        assert_eq!(path_reminders_once(&ctx, "a1", Some("ra"), "db/migrations/001.sql").len(), 1);
        assert!(path_reminders_once(&ctx, "a1", Some("ra"), "db/migrations/001.sql").is_empty(), "deduped within session");
        reset_agent_path_reminders("a1");
        assert_eq!(path_reminders_once(&ctx, "a1", Some("ra"), "db/migrations/001.sql").len(), 1, "re-fires after compaction reset");
        assert_eq!(compaction_reminders(&ctx, Some("ra")).len(), 1);
    }
}
