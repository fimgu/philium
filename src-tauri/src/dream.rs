//! Dreaming: an idle-time, per-repo pass that re-reads the repo's own agent transcripts and its
//! memories, then confirms, corrects, merges, retires and re-ranks those memories.
//!
//! Invariants (each has a test below):
//! - One dream covers exactly one repo. Input is only `repo:<id>` memories and transcripts of that
//!   repo's non-Strict agents. Output only touches `repo:<id>` rows. Global rows are never read here.
//! - All model traffic goes through Philium's gateway (`library::gateway_completion`).
//! - `user_direct` rows are never edited, merged away, retired or demoted: a correction becomes a
//!   `proposed` row that links to the original. Verified procedures are flagged, never retired.
//! - Corrections supersede (old row kept with status `superseded`). Nothing is hard-deleted.
//! - Every model action is validated in Rust (scope, tier, ids, evidence quotes that must really
//!   appear in the transcripts we sent). Valid actions apply in one transaction, each with an undo
//!   snapshot; invalid ones are logged with a reason in `dream_runs.rejected`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ctx::Ctx;
use crate::db::now;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dream_runs (
  id TEXT PRIMARY KEY,
  repo_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  state TEXT NOT NULL,
  model TEXT,
  tokens_in INTEGER NOT NULL DEFAULT 0,
  tokens_out INTEGER NOT NULL DEFAULT 0,
  applied TEXT NOT NULL DEFAULT '[]',
  rejected TEXT NOT NULL DEFAULT '[]',
  note TEXT);
CREATE INDEX IF NOT EXISTS dream_runs_repo ON dream_runs(repo_id, started_at);
CREATE TABLE IF NOT EXISTS dream_actions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  repo_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  memory_ids TEXT NOT NULL,
  summary TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  evidence TEXT NOT NULL DEFAULT '[]',
  undo TEXT NOT NULL,
  undone_at TEXT);
CREATE INDEX IF NOT EXISTS dream_actions_run ON dream_actions(run_id);
";

/// Max memories pinned into the session-start index. The index prompt shows at most this many lines.
pub const INDEX_BUDGET: usize = 25;
const DEFAULT_IDLE_MINUTES: i64 = 10;
const DEFAULT_MIN_ENTRIES: i64 = 20;
const MIN_INTERVAL_MS: i64 = 6 * 3600 * 1000;
pub const DEFAULT_DAILY_TOKENS: i64 = 50_000;
const MAX_MEMORIES: usize = 80;
const MEMORY_CHARS: usize = 400;
const TRANSCRIPT_CHAR_CAP: usize = 24_000;
const ENTRY_CHARS: usize = 400;
const MAX_ACTIONS: usize = 24;
const MIN_QUOTE_CHARS: usize = 8;
const MAX_OUTPUT_TOKENS: u32 = 2500;

pub fn init(ctx: &Ctx) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))
}

// ---------- Settings ----------

pub fn enabled(ctx: &Ctx) -> bool {
    ctx.setting("dream.enabled", "on") != "off"
}

pub fn daily_budget(ctx: &Ctx) -> i64 {
    ctx.setting("dream.daily_tokens", "").parse::<i64>().ok().filter(|n| *n > 0).unwrap_or(DEFAULT_DAILY_TOKENS)
}

fn setting_i64(ctx: &Ctx, key: &str, default: i64) -> i64 {
    ctx.setting(key, "").parse::<i64>().ok().filter(|n| *n >= 0).unwrap_or(default)
}

pub fn set_config(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    if let Some(on) = args["enabled"].as_bool() {
        ctx.db.kv_set("setting.dream.enabled", if on { "on" } else { "off" })?;
    }
    if let Some(n) = args["daily_tokens"].as_i64() {
        if !(1000..=5_000_000).contains(&n) {
            return Err("daily token budget must be between 1,000 and 5,000,000".into());
        }
        ctx.db.kv_set("setting.dream.daily_tokens", &n.to_string())?;
    }
    ctx.notify("library");
    Ok(json!({ "enabled": enabled(ctx), "daily_tokens": daily_budget(ctx) }))
}

fn pending_key(repo_id: &str) -> String {
    format!("dream.pending.{repo_id}")
}

/// Called by session observation: `n` new transcript entries were observed for this repo.
pub fn note_observed(ctx: &Ctx, repo_id: &str, n: usize) {
    let cur = ctx.db.kv_get(&pending_key(repo_id)).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let _ = ctx.db.kv_set(&pending_key(repo_id), &(cur + n as i64).to_string());
}

fn tokens_used_today(ctx: &Ctx, repo_id: &str) -> i64 {
    ctx.db
        .query(
            "SELECT COALESCE(SUM(tokens_in + tokens_out),0) AS n FROM dream_runs
             WHERE repo_id=?1 AND date(started_at)=date('now')",
            [repo_id],
        )
        .ok()
        .and_then(|r| r.first().and_then(|v| v["n"].as_i64()))
        .unwrap_or(0)
}

fn last_dream_ms(ctx: &Ctx, repo_id: &str) -> Option<i64> {
    ctx.db.kv_get(&format!("dream.last.{repo_id}")).and_then(|s| s.parse().ok())
}

// ---------- Scheduling ----------

/// Busy/cancel key: one app has one data dir, tests run many side by side.
fn key(ctx: &Ctx, repo_id: &str) -> String {
    format!("{}\u{0}{repo_id}", ctx.data_dir.display())
}

fn busy() -> &'static Mutex<HashSet<String>> {
    static B: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn cancel_flags() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static C: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// An agent for this repo started working: stop any dream for it before it writes anything.
pub fn cancel_repo(ctx: &Ctx, repo_id: &str) {
    if let Some(f) = cancel_flags().lock().unwrap_or_else(|e| e.into_inner()).get(&key(ctx, repo_id)) {
        f.store(true, Ordering::SeqCst);
    }
}

/// Agent state hook (agents::set_state): a running agent cancels its repo's dream.
pub fn on_agent_state(ctx: &Ctx, agent_id: &str, state: &str) {
    if !matches!(state, "running" | "provisioning" | "waiting") || cancel_flags().lock().map(|m| m.is_empty()).unwrap_or(true) {
        return;
    }
    if let Ok(rows) = ctx.db.query("SELECT repo_id FROM agents WHERE id=?1", [agent_id]) {
        if let Some(rid) = rows.first().and_then(|r| r["repo_id"].as_str()) {
            cancel_repo(ctx, rid);
        }
    }
}

/// Why a repo is not due for a dream right now, or `None` when it is.
pub fn ineligible_reason(ctx: &Ctx, repo_id: &str, now_ms: i64) -> Option<String> {
    if !enabled(ctx) {
        return Some("dreaming is off".into());
    }
    if busy().lock().unwrap_or_else(|e| e.into_inner()).contains(&key(ctx, repo_id)) {
        return Some("already dreaming".into());
    }
    if let Some(last) = last_dream_ms(ctx, repo_id) {
        if now_ms - last < MIN_INTERVAL_MS {
            return Some("dreamed in the last 6h".into());
        }
    }
    let pending = ctx.db.kv_get(&pending_key(repo_id)).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    if pending < setting_i64(ctx, "dream.min_entries", DEFAULT_MIN_ENTRIES) {
        return Some("not enough new transcript".into());
    }
    let idle_ms = setting_i64(ctx, "dream.idle_minutes", DEFAULT_IDLE_MINUTES) * 60_000;
    let rows = ctx
        .db
        .query("SELECT state, last_active_at FROM agents WHERE repo_id=?1", [repo_id])
        .unwrap_or_default();
    for r in &rows {
        let st = r["state"].as_str().unwrap_or("");
        if matches!(st, "running" | "provisioning" | "waiting") {
            return Some("an agent is running".into());
        }
        if now_ms - r["last_active_at"].as_i64().unwrap_or(0) < idle_ms {
            return Some("repo was active recently".into());
        }
    }
    if tokens_used_today(ctx, repo_id) >= daily_budget(ctx) {
        return Some("daily token budget used".into());
    }
    None
}

/// Background scheduler: checks every minute, and once right away for repos that are overdue.
pub fn start(ctx: &Arc<Ctx>) {
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            tick(&c).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

pub async fn tick(ctx: &Arc<Ctx>) -> Vec<String> {
    let repos: Vec<String> = ctx
        .db
        .query("SELECT id FROM repos", [])
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    let t = now();
    let mut started = vec![];
    for rid in repos {
        if ineligible_reason(ctx, &rid, t).is_none() {
            started.push(rid.clone());
            let c = ctx.clone();
            tokio::spawn(async move {
                let _ = run(&c, &rid, false).await;
            });
        }
    }
    started
}

struct BusyGuard(String);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        busy().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        cancel_flags().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

// ---------- Input gathering ----------

#[derive(Clone)]
struct Entry {
    session: usize,
    seq: i64,
    role: String,
    text: String,
}

struct DreamInput {
    memories: Vec<Value>,
    sessions: Vec<String>,
    entries: Vec<Entry>,
    model_ref: Option<String>,
}

fn gather(ctx: &Ctx, repo_id: &str) -> Result<DreamInput, String> {
    let scope = format!("repo:{repo_id}");
    let memories = ctx.db.query(
        "SELECT m.id, m.title, m.content, m.category, m.fact_key, m.trust_tier, m.status, m.files,
                m.index_rank, m.dream_verified_at,
                (SELECT MAX(f.created_at) FROM retrieval_feedback f WHERE f.memory_id=m.id) AS last_used
         FROM memories m
         WHERE m.scope=?1 AND m.status IN ('active','proposed')
         ORDER BY (m.status='active') DESC, COALESCE(m.updated_at, m.created_at) DESC LIMIT ?2",
        params![scope, MAX_MEMORIES as i64],
    )?;
    // Only this repo's non-Strict agents. Strict transcripts are never read.
    let agents = ctx.db.query(
        "SELECT id, hal_session_id, model_ref FROM agents
         WHERE repo_id=?1 AND strict=0 AND hal_session_id IS NOT NULL AND hal_session_id != ''
         ORDER BY last_active_at DESC LIMIT 8",
        [repo_id],
    )?;
    let mut sessions = vec![];
    let mut entries = vec![];
    let mut model_ref = None;
    for a in &agents {
        let (Some(aid), Some(sid)) = (a["id"].as_str(), a["hal_session_id"].as_str()) else { continue };
        if model_ref.is_none() {
            model_ref = a["model_ref"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        }
        let observed = ctx.db.kv_get(&format!("memory.observed_seq.{aid}")).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let rows = crate::library::extract_hal_db_entries(ctx, aid, sid, (observed - 400).max(0), 400);
        if rows.is_empty() {
            continue;
        }
        let si = sessions.len();
        sessions.push(sid.to_string());
        for (seq, role, text, _) in rows {
            entries.push(Entry { session: si, seq, role, text: text.chars().take(ENTRY_CHARS).collect() });
        }
    }
    Ok(DreamInput { memories, sessions, entries, model_ref })
}

fn words(s: &str) -> HashSet<String> {
    crate::library::normalized_text(s).split(' ').filter(|w| w.len() >= 4).map(str::to_string).collect()
}

/// Pick transcript excerpts relevant to the memories (local embedder when ready, keyword overlap
/// otherwise), plus a little recency, under the character cap. Returned in transcript order.
fn select_excerpts(ctx: &Ctx, memories: &[Value], entries: &[Entry]) -> Vec<Entry> {
    if entries.is_empty() {
        return vec![];
    }
    let mem_texts: Vec<String> = memories.iter().map(|m| m["content"].as_str().unwrap_or("").to_string()).collect();
    let mut scores: Vec<f32> = vec![0.0; entries.len()];
    let mut used_embedder = false;
    if crate::local_models::ready(ctx, &crate::local_models::EMBED) && !mem_texts.is_empty() {
        let mut texts = mem_texts.clone();
        texts.extend(entries.iter().map(|e| e.text.clone()));
        if let Ok(vecs) = crate::local_models::embed(&ctx.data_dir, &texts) {
            let (mv, ev) = vecs.split_at(mem_texts.len());
            for (i, e) in ev.iter().enumerate() {
                scores[i] = mv.iter().map(|m| m.iter().zip(e).map(|(a, b)| a * b).sum::<f32>()).fold(0.0, f32::max);
            }
            used_embedder = true;
        }
    }
    if !used_embedder {
        let mem_words: Vec<HashSet<String>> = mem_texts.iter().map(|t| words(t)).collect();
        for (i, e) in entries.iter().enumerate() {
            let ew = words(&e.text);
            scores[i] = mem_words
                .iter()
                .map(|mw| if mw.is_empty() { 0.0 } else { mw.intersection(&ew).count() as f32 / mw.len() as f32 })
                .fold(0.0, f32::max);
        }
    }
    // User turns carry corrections and preferences; give them and recent entries a small lift.
    let n = entries.len() as f32;
    for (i, e) in entries.iter().enumerate() {
        if e.role == "user" {
            scores[i] += 0.15;
        }
        scores[i] += 0.1 * (i as f32 / n);
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|a, b| scores[*b].partial_cmp(&scores[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let mut picked = vec![];
    let mut used = 0;
    for i in order {
        let len = entries[i].text.len() + 24;
        if used + len > TRANSCRIPT_CHAR_CAP {
            continue;
        }
        used += len;
        picked.push(i);
    }
    picked.sort();
    picked.into_iter().map(|i| entries[i].clone()).collect()
}

const DREAM_PROMPT: &str = "You are reviewing one repository's engineering memory against the original agent transcripts, like sleep consolidates memory. Fix what is wrong, merge duplicates, retire what is stale or trivially re-readable from the code, and keep the small session index useful.\n\
Return strict JSON only: {\"actions\":[...]} with at most 24 actions of these shapes:\n\
{\"type\":\"confirm\",\"id\":\"mem_..\",\"evidence\":[\"exact quote\"],\"reason\":\"..\"}\n\
{\"type\":\"correct\",\"id\":\"mem_..\",\"new_content\":\"1-3 sentences\",\"title\":\"<=80 chars\",\"evidence\":[\"exact quote\"],\"reason\":\"..\"}\n\
{\"type\":\"merge\",\"ids\":[\"mem_..\",\"mem_..\"],\"merged\":{\"title\":\"..\",\"content\":\"..\",\"category\":\"..\"},\"reason\":\"..\"}\n\
{\"type\":\"retire\",\"id\":\"mem_..\",\"reason\":\"..\"}\n\
{\"type\":\"promote\",\"id\":\"mem_..\",\"reason\":\"..\"} / {\"type\":\"demote\",\"id\":\"mem_..\",\"reason\":\"..\"}\n\
{\"type\":\"new\",\"title\":\"..\",\"content\":\"..\",\"category\":\"..\",\"files\":[],\"fact_key\":null,\"evidence\":[\"quote from session A\",\"quote from session B\"],\"reason\":\"..\"}\n\
Rules: evidence must be verbatim substrings of the transcript lines below (not of memories); only use ids listed under MEMORIES; `new` only for patterns that span at least two sessions; promote only things every future session should see; never include secrets.";

fn build_prompt(input_memories: &[Value], sessions: &[String], excerpts: &[Entry]) -> String {
    let mut out = String::from("MEMORIES\n");
    for m in input_memories {
        let content: String = m["content"].as_str().unwrap_or("").replace('\n', " ").chars().take(MEMORY_CHARS).collect();
        out.push_str(&format!(
            "- id={} status={} tier={} category={} fact_key={} index={} last_used={} title={:?} files={} :: {}\n",
            m["id"].as_str().unwrap_or(""),
            m["status"].as_str().unwrap_or(""),
            m["trust_tier"].as_str().unwrap_or(""),
            m["category"].as_str().unwrap_or(""),
            m["fact_key"].as_str().unwrap_or("-"),
            m["index_rank"].as_i64().unwrap_or(0),
            m["last_used"].as_str().unwrap_or("never"),
            m["title"].as_str().unwrap_or(""),
            m["files"].as_str().unwrap_or("[]"),
            content,
        ));
    }
    out.push_str("\nTRANSCRIPTS\n");
    let mut cur = usize::MAX;
    for e in excerpts {
        if e.session != cur {
            cur = e.session;
            out.push_str(&format!("## session S{} ({})\n", e.session + 1, sessions.get(e.session).map(String::as_str).unwrap_or("")));
        }
        out.push_str(&format!("[S{}#{} {}] {}\n", e.session + 1, e.seq, e.role, e.text.replace('\n', " ")));
    }
    out
}

// ---------- Model selection ----------

fn pick_models(ctx: &Ctx, model_ref: Option<&str>) -> Option<(crate::providers::Provider, Vec<String>)> {
    let (p, mid) = crate::providers::resolve(ctx, model_ref).ok()?;
    if !p.enabled || !p.trusted {
        return None;
    }
    let sibling = crate::library::observation_model(ctx, &p.id, &mid);
    let mut models = vec![mid];
    if !models.contains(&sibling) {
        models.push(sibling);
    }
    Some((p, models))
}

fn estimate_tokens(s: &str) -> i64 {
    (s.len() as i64 + 3) / 4
}

// ---------- Run ----------

fn finish_run(ctx: &Ctx, run_id: &str, state: &str, note: &str) {
    let _ = ctx.db.exec(
        "UPDATE dream_runs SET state=?2, note=?3, finished_at=datetime('now') WHERE id=?1",
        params![run_id, state, note],
    );
}

/// One dream for one repo. `manual` skips the idle/new-entries/6h gates (Dream now) but still
/// honours the toggle, the daily budget and the per-repo busy set.
pub async fn run(ctx: &Arc<Ctx>, repo_id: &str, manual: bool) -> Result<Value, String> {
    if !enabled(ctx) {
        return Ok(json!({ "skipped": true, "reason": "dreaming is off" }));
    }
    if ctx.db.query("SELECT id FROM repos WHERE id=?1", [repo_id])?.is_empty() {
        return Err(crate::ctx::not_found(format!("repo {repo_id}")));
    }
    if !manual {
        if let Some(r) = ineligible_reason(ctx, repo_id, now()) {
            return Ok(json!({ "skipped": true, "reason": r }));
        }
    }
    {
        let mut b = busy().lock().unwrap_or_else(|e| e.into_inner());
        if !b.insert(key(ctx, repo_id)) {
            return Ok(json!({ "skipped": true, "reason": "already dreaming" }));
        }
    }
    let flag = Arc::new(AtomicBool::new(false));
    cancel_flags().lock().unwrap_or_else(|e| e.into_inner()).insert(key(ctx, repo_id), flag.clone());
    let _guard = BusyGuard(key(ctx, repo_id));

    let run_id = format!("dream_{}", crate::db::new_id());
    ctx.db.exec(
        "INSERT INTO dream_runs(id,repo_id,started_at,state) VALUES(?1,?2,datetime('now'),'running')",
        params![run_id, repo_id],
    )?;
    let used = tokens_used_today(ctx, repo_id);
    let budget = daily_budget(ctx);

    let (c, rid) = (ctx.clone(), repo_id.to_string());
    let (input, prompt, excerpts) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let input = gather(&c, &rid)?;
        let excerpts = select_excerpts(&c, &input.memories, &input.entries);
        let prompt = build_prompt(&input.memories, &input.sessions, &excerpts);
        Ok((input, prompt, excerpts))
    })
    .await
    .map_err(|e| e.to_string())??;

    if input.memories.is_empty() && excerpts.is_empty() {
        finish_run(ctx, &run_id, "skipped", "nothing to dream about");
        return Ok(json!({ "skipped": true, "reason": "nothing to dream about", "run_id": run_id }));
    }
    let est_in = estimate_tokens(DREAM_PROMPT) + estimate_tokens(&prompt);
    if used + est_in + MAX_OUTPUT_TOKENS as i64 > budget {
        finish_run(ctx, &run_id, "skipped", "daily token budget used");
        return Ok(json!({ "skipped": true, "reason": "daily token budget used", "run_id": run_id }));
    }
    let Some((provider, models)) = pick_models(ctx, input.model_ref.as_deref()) else {
        finish_run(ctx, &run_id, "skipped", "no trusted model available");
        return Ok(json!({ "skipped": true, "reason": "no trusted model available", "run_id": run_id }));
    };

    let mut reply = None;
    let mut used_model = String::new();
    let mut tokens_in = 0;
    for mid in &models {
        if flag.load(Ordering::SeqCst) {
            break;
        }
        tokens_in += est_in;
        let call = crate::library::gateway_completion(ctx, &provider, mid, DREAM_PROMPT, &prompt, MAX_OUTPUT_TOKENS, "memory-dreamer");
        tokio::pin!(call);
        let res = loop {
            tokio::select! {
                r = &mut call => break Some(r),
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if flag.load(Ordering::SeqCst) { break None; }
                }
            }
        };
        match res {
            Some(Ok(t)) => {
                used_model = mid.clone();
                reply = Some(t);
                break;
            }
            Some(Err(status)) => {
                ctx.log_event("memory:dream_llm_failed", None, &json!({ "repo_id": repo_id, "model": mid, "status": status }));
                if used + tokens_in + est_in > budget {
                    break;
                }
            }
            None => break,
        }
    }
    let tokens_out = reply.as_deref().map(estimate_tokens).unwrap_or(0);
    let _ = ctx.db.exec(
        "UPDATE dream_runs SET model=?2, tokens_in=?3, tokens_out=?4 WHERE id=?1",
        params![run_id, format!("{}/{}", provider.id, used_model), tokens_in, tokens_out],
    );
    if flag.load(Ordering::SeqCst) {
        finish_run(ctx, &run_id, "cancelled", "an agent started in this repo");
        return Ok(json!({ "skipped": true, "reason": "cancelled", "run_id": run_id }));
    }
    let Some(reply) = reply else {
        finish_run(ctx, &run_id, "failed", "model call failed");
        return Ok(json!({ "skipped": true, "reason": "model call failed", "run_id": run_id }));
    };
    let actions = crate::library::first_json_object(&reply)
        .and_then(|v| v["actions"].as_array().cloned())
        .unwrap_or_default();

    let (c, rid, rn, fl) = (ctx.clone(), repo_id.to_string(), run_id.clone(), flag.clone());
    let sessions = input.sessions.clone();
    let result = tokio::task::spawn_blocking(move || apply(&c, &rid, &rn, &actions, &excerpts, &sessions, &fl))
        .await
        .map_err(|e| e.to_string())??;

    if result["cancelled"] == true {
        finish_run(ctx, &run_id, "cancelled", "an agent started in this repo");
        return Ok(json!({ "skipped": true, "reason": "cancelled", "run_id": run_id }));
    }
    let _ = ctx.db.kv_set(&format!("dream.last.{repo_id}"), &now().to_string());
    let _ = ctx.db.kv_set(&pending_key(repo_id), "0");
    finish_run(ctx, &run_id, "done", "");
    for (id, content) in result["embed"].as_array().into_iter().flatten().filter_map(|v| Some((v[0].as_str()?.to_string(), v[1].as_str()?.to_string()))) {
        let _ = crate::library::embed_memory_async(ctx, &id, &content).await;
    }
    crate::library::invalidate_vector_cache(ctx);
    ctx.notify("library");
    ctx.log_event("memory:dream", None, &json!({ "repo_id": repo_id, "run_id": run_id, "applied": result["applied"].as_array().map(Vec::len), "rejected": result["rejected"].as_array().map(Vec::len) }));
    Ok(json!({ "run_id": run_id, "applied": result["applied"], "rejected": result["rejected"] }))
}

// ---------- Validation and apply ----------

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn evidence_list(a: &Value) -> Vec<String> {
    match &a["evidence"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(v) => v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => vec![],
    }
}

/// Sessions (by index) whose sent excerpts contain every quote; Err names the first invented quote.
fn check_evidence(quotes: &[String], per_session: &[String]) -> Result<HashSet<usize>, String> {
    let mut hit = HashSet::new();
    for q in quotes {
        let nq = norm_ws(q.trim_matches(|c| c == '"' || c == '\''));
        if nq.chars().count() < MIN_QUOTE_CHARS {
            return Err(format!("evidence too short: {q:?}"));
        }
        let found: Vec<usize> = per_session.iter().enumerate().filter(|(_, s)| s.contains(&nq)).map(|(i, _)| i).collect();
        if found.is_empty() {
            return Err(format!("evidence not found in transcripts: {:?}", q.chars().take(80).collect::<String>()));
        }
        hit.extend(found);
    }
    Ok(hit)
}

const SNAP_COLS: &str = "id, scope, active, status, content, title, files, category, trust_tier, fact_key, supersedes_id, provenance, updated_at, valid_to, dream_verified_at, index_rank";

fn snapshot(c: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    c.query_row(&format!("SELECT {SNAP_COLS} FROM memories WHERE id=?1"), [id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?, "scope": r.get::<_, String>(1)?, "active": r.get::<_, i64>(2)?,
            "status": r.get::<_, String>(3)?, "content": r.get::<_, String>(4)?, "title": r.get::<_, Option<String>>(5)?,
            "files": r.get::<_, Option<String>>(6)?, "category": r.get::<_, String>(7)?, "trust_tier": r.get::<_, String>(8)?,
            "fact_key": r.get::<_, Option<String>>(9)?, "supersedes_id": r.get::<_, Option<String>>(10)?,
            "provenance": r.get::<_, Option<String>>(11)?, "updated_at": r.get::<_, Option<String>>(12)?,
            "valid_to": r.get::<_, Option<String>>(13)?, "dream_verified_at": r.get::<_, Option<String>>(14)?,
            "index_rank": r.get::<_, i64>(15)?,
        }))
    })
    .optional()
}

fn restore(c: &Connection, s: &Value) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE memories SET active=?2, status=?3, content=?4, title=?5, files=?6, category=?7, trust_tier=?8, fact_key=?9,
           supersedes_id=?10, provenance=?11, updated_at=?12, valid_to=?13, dream_verified_at=?14, index_rank=?15
         WHERE id=?1",
        params![
            s["id"].as_str(), s["active"].as_i64(), s["status"].as_str(), s["content"].as_str(), s["title"].as_str(),
            s["files"].as_str(), s["category"].as_str(), s["trust_tier"].as_str(), s["fact_key"].as_str(),
            s["supersedes_id"].as_str(), s["provenance"].as_str(), s["updated_at"].as_str(), s["valid_to"].as_str(),
            s["dream_verified_at"].as_str(), s["index_rank"].as_i64()
        ],
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_memory(
    c: &Connection,
    scope: &str,
    category: &str,
    content: &str,
    title: Option<&str>,
    files: Option<&str>,
    status: &str,
    fact_key: Option<&str>,
    supersedes: Option<&str>,
    provenance: &str,
) -> rusqlite::Result<String> {
    let id = format!("mem_{}", crate::db::new_id());
    c.execute(
        "INSERT INTO memories(id,scope,category,content,source,created_at,active,trust_tier,status,fact_key,supersedes_id,provenance,updated_at,title,files,dream_verified_at)
         VALUES(?1,?2,?3,?4,'pipeline:dream',datetime('now'),?5,'agent_inferred',?6,?7,?8,?9,datetime('now'),?10,?11,datetime('now'))",
        params![id, scope, category, content, if status == "active" { 1 } else { 0 }, status, fact_key, supersedes, provenance, title, files],
    )?;
    Ok(id)
}

fn supersede(c: &Connection, id: &str) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE memories SET active=0, status='superseded', valid_to=COALESCE(valid_to, datetime('now')), updated_at=datetime('now') WHERE id=?1",
        [id],
    )
}

fn clean_text(v: &Value, max: usize) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(max).collect())
}

fn files_json(v: &Value) -> Option<String> {
    let files: Vec<String> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f.as_str())
        .map(|f| f.trim().trim_start_matches("./").to_string())
        .filter(|f| !f.is_empty() && f.len() <= 200 && !f.starts_with('/') && !f.contains(".."))
        .take(8)
        .collect();
    (!files.is_empty()).then(|| serde_json::to_string(&files).unwrap_or_default())
}

fn clean_fact_key(raw: Option<&str>) -> Option<String> {
    let k = raw?.trim().to_lowercase();
    (!k.is_empty() && k.len() <= 80 && k.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c))).then_some(k)
}

/// Validate every action against the DB and the sent transcript, then apply the valid ones in one
/// transaction with an undo record each. Returns applied/rejected lists and ids to embed.
fn apply(
    ctx: &Ctx,
    repo_id: &str,
    run_id: &str,
    actions: &[Value],
    excerpts: &[Entry],
    sessions: &[String],
    cancel: &AtomicBool,
) -> Result<Value, String> {
    let scope = format!("repo:{repo_id}");
    let mut per_session = vec![String::new(); sessions.len()];
    for e in excerpts {
        if let Some(s) = per_session.get_mut(e.session) {
            s.push_str(&norm_ws(&e.text));
            s.push('\n');
        }
    }
    let mut applied: Vec<Value> = vec![];
    let mut rejected: Vec<Value> = vec![];
    let mut embed: Vec<Value> = vec![];
    let mut cancelled = false;

    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let mut touched: HashSet<String> = HashSet::new();
        let mut promoted: i64 = tx.query_row(
            "SELECT COUNT(*) FROM memories WHERE scope=?1 AND active=1 AND status='active' AND index_rank > 0",
            [&scope],
            |r| r.get(0),
        )?;
        for (idx, a) in actions.iter().enumerate() {
            let kind = a["type"].as_str().unwrap_or("").to_string();
            let reason = clean_text(&a["reason"], 300).unwrap_or_default();
            let mut reject = |why: String| rejected.push(json!({ "index": idx, "type": kind, "ids": a.get("ids").or(a.get("id")), "reason": why }));
            if idx >= MAX_ACTIONS {
                reject("too many actions".into());
                continue;
            }
            // Every referenced id must be a live row in this repo, touched at most once per run.
            let ids: Vec<String> = match kind.as_str() {
                "merge" => a["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                "new" => vec![],
                _ => a["id"].as_str().map(|s| vec![s.to_string()]).unwrap_or_default(),
            };
            let mut rows = vec![];
            let mut bad = None;
            for id in &ids {
                match snapshot(&tx, id)? {
                    None => bad = Some(format!("unknown id {id}")),
                    Some(s) if s["scope"].as_str() != Some(scope.as_str()) => bad = Some(format!("id {id} is outside this repo")),
                    Some(s) if !matches!(s["status"].as_str(), Some("active" | "proposed")) => bad = Some(format!("id {id} is not active")),
                    Some(_) if touched.contains(id) => bad = Some(format!("id {id} already changed in this run")),
                    Some(s) => rows.push(s),
                }
                if bad.is_some() {
                    break;
                }
            }
            if let Some(b) = bad {
                reject(b);
                continue;
            }
            let quotes = evidence_list(a);
            let ev_sessions = if quotes.is_empty() {
                Ok(HashSet::new())
            } else {
                check_evidence(&quotes, &per_session)
            };
            let ev_sessions = match ev_sessions {
                Ok(s) => s,
                Err(e) => {
                    reject(e);
                    continue;
                }
            };
            let tier = |r: &Value| r["trust_tier"].as_str().unwrap_or("").to_string();
            let is_user = |r: &Value| tier(r) == "user_direct";
            let is_verified_proc = |r: &Value| tier(r) == "verified_run" && r["category"].as_str() == Some("procedure");
            let provenance = format!("dream:{run_id}");
            let mut undo: Vec<Value> = vec![];
            let mut created: Vec<String> = vec![];
            let summary: String;
            match kind.as_str() {
                "confirm" => {
                    if rows.len() != 1 || quotes.is_empty() {
                        reject("confirm needs one id and transcript evidence".into());
                        continue;
                    }
                    undo.push(rows[0].clone());
                    tx.execute("UPDATE memories SET dream_verified_at=datetime('now') WHERE id=?1", [&ids[0]])?;
                    summary = format!("Confirmed {}", label(&rows[0]));
                }
                "correct" => {
                    let Some(new_content) = clean_text(&a["new_content"], 1200) else {
                        reject("correct needs new_content".into());
                        continue;
                    };
                    if rows.len() != 1 || quotes.is_empty() {
                        reject("correct needs one id and transcript evidence".into());
                        continue;
                    }
                    let old = &rows[0];
                    if crate::library::normalized_text(&new_content) == crate::library::normalized_text(old["content"].as_str().unwrap_or("")) {
                        reject("correction is identical to the memory".into());
                        continue;
                    }
                    let title = clean_text(&a["title"], 80).or_else(|| old["title"].as_str().map(str::to_string));
                    // user_direct and verified procedures: stage a proposed correction, original untouched.
                    let proposed = is_user(old) || is_verified_proc(old) || old["status"].as_str() == Some("proposed");
                    let status = if proposed { "proposed" } else { "active" };
                    if !proposed {
                        undo.push(old.clone());
                        supersede(&tx, &ids[0])?;
                    }
                    let nid = insert_memory(
                        &tx, &scope, old["category"].as_str().unwrap_or("fact"), &new_content, title.as_deref(), old["files"].as_str(),
                        status, old["fact_key"].as_str(), Some(&ids[0]), &provenance,
                    )?;
                    if !proposed {
                        embed.push(json!([nid, new_content]));
                    }
                    created.push(nid);
                    summary = if proposed {
                        format!("Proposed a correction to {}", label(old))
                    } else {
                        format!("Corrected {}", label(old))
                    };
                }
                "merge" => {
                    let uniq: HashSet<&String> = ids.iter().collect();
                    if rows.len() < 2 || uniq.len() != ids.len() {
                        reject("merge needs two or more distinct ids".into());
                        continue;
                    }
                    if rows.iter().any(|r| is_user(r) || is_verified_proc(r)) {
                        reject("cannot merge away a user_direct memory or verified procedure".into());
                        continue;
                    }
                    if rows.iter().any(|r| r["status"].as_str() != Some("active")) {
                        reject("merge only covers active memories".into());
                        continue;
                    }
                    let m = &a["merged"];
                    let Some(content) = clean_text(&m["content"], 1200) else {
                        reject("merge needs merged.content".into());
                        continue;
                    };
                    let cat = m["category"].as_str().filter(|c| crate::library::CATEGORIES.contains(c)).or(rows[0]["category"].as_str()).unwrap_or("fact").to_string();
                    let fks: HashSet<&str> = rows.iter().filter_map(|r| r["fact_key"].as_str()).collect();
                    let fk = (fks.len() == 1).then(|| *fks.iter().next().unwrap());
                    for r in &rows {
                        undo.push(r.clone());
                    }
                    for id in &ids {
                        supersede(&tx, id)?;
                    }
                    let prov = format!("{provenance}:merged:{}", ids.join(","));
                    let nid = insert_memory(&tx, &scope, &cat, &content, clean_text(&m["title"], 80).as_deref(), rows[0]["files"].as_str(), "active", fk, Some(&ids[0]), &prov)?;
                    embed.push(json!([nid, content]));
                    created.push(nid);
                    summary = format!("Merged {} memories into one", ids.len());
                }
                "retire" | "demote" => {
                    if rows.len() != 1 || reason.is_empty() {
                        reject(format!("{kind} needs one id and a reason"));
                        continue;
                    }
                    let r = &rows[0];
                    if is_user(r) {
                        reject(format!("cannot {kind} a user_direct memory"));
                        continue;
                    }
                    if kind == "retire" && is_verified_proc(r) {
                        // Never auto-retired: record a flag the user can see, change nothing.
                        applied.push(json!({ "type": "flag", "ids": ids, "summary": format!("Flagged {} as possibly stale", label(r)), "reason": reason }));
                        continue;
                    }
                    undo.push(r.clone());
                    if kind == "retire" {
                        tx.execute("UPDATE memories SET active=0, status='retired', valid_to=COALESCE(valid_to, datetime('now')), updated_at=datetime('now') WHERE id=?1", [&ids[0]])?;
                        summary = format!("Retired {}", label(r));
                    } else {
                        if r["index_rank"].as_i64().unwrap_or(0) > 0 {
                            promoted -= 1;
                        }
                        tx.execute("UPDATE memories SET index_rank=-1 WHERE id=?1", [&ids[0]])?;
                        summary = format!("Moved {} out of the session index", label(r));
                    }
                }
                "promote" => {
                    if rows.len() != 1 || rows[0]["status"].as_str() != Some("active") {
                        reject("promote needs one active id".into());
                        continue;
                    }
                    if rows[0]["index_rank"].as_i64().unwrap_or(0) > 0 {
                        reject("already in the index".into());
                        continue;
                    }
                    if promoted >= INDEX_BUDGET as i64 {
                        reject(format!("index budget of {INDEX_BUDGET} reached"));
                        continue;
                    }
                    promoted += 1;
                    undo.push(rows[0].clone());
                    tx.execute("UPDATE memories SET index_rank=1 WHERE id=?1", [&ids[0]])?;
                    summary = format!("Pinned {} to the session index", label(&rows[0]));
                }
                "new" => {
                    let Some(content) = clean_text(&a["content"], 1200) else {
                        reject("new needs content".into());
                        continue;
                    };
                    if ev_sessions.len() < 2 {
                        reject("new memories need evidence from at least two sessions".into());
                        continue;
                    }
                    let cat = a["category"].as_str().unwrap_or("fact").to_lowercase();
                    if !crate::library::CATEGORIES.contains(&cat.as_str()) {
                        reject(format!("bad category {cat}"));
                        continue;
                    }
                    let norm = crate::library::normalized_text(&content);
                    let dup: bool = {
                        let mut st = tx.prepare("SELECT content FROM memories WHERE scope=?1 AND active=1 AND status='active'")?;
                        let it = st.query_map([&scope], |r| r.get::<_, String>(0))?;
                        let found = it.flatten().any(|c| crate::library::normalized_text(&c) == norm);
                        found
                    };
                    if dup {
                        reject("already remembered".into());
                        continue;
                    }
                    let fk = clean_fact_key(a["fact_key"].as_str());
                    // A fact_key held by user_direct stays theirs: stage as proposed. Otherwise supersede.
                    let mut status = "active";
                    let mut sup = None;
                    if let Some(ref k) = fk {
                        let mut st = tx.prepare("SELECT id, trust_tier FROM memories WHERE scope=?1 AND fact_key=?2 AND active=1 AND status='active'")?;
                        let holders: Vec<(String, String)> = st.query_map(params![scope, k], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
                        if holders.iter().any(|(_, t)| t == "user_direct") {
                            status = "proposed";
                            sup = holders.first().map(|h| h.0.clone());
                        } else {
                            for (hid, _) in &holders {
                                if touched.contains(hid) {
                                    continue;
                                }
                                if let Some(s) = snapshot(&tx, hid)? {
                                    undo.push(s);
                                }
                                supersede(&tx, hid)?;
                                touched.insert(hid.clone());
                                sup.get_or_insert(hid.clone());
                            }
                        }
                    }
                    let nid = insert_memory(&tx, &scope, &cat, &content, clean_text(&a["title"], 80).as_deref(), files_json(&a["files"]).as_deref(), status, fk.as_deref(), sup.as_deref(), &provenance)?;
                    if status == "active" {
                        embed.push(json!([nid, content]));
                    }
                    created.push(nid);
                    summary = format!("Learned: {}", clean_text(&a["title"], 80).unwrap_or_else(|| content.chars().take(60).collect()));
                }
                other => {
                    reject(format!("unknown action type {other:?}"));
                    continue;
                }
            }
            for id in &ids {
                touched.insert(id.clone());
            }
            let all_ids: Vec<String> = ids.iter().chain(created.iter()).cloned().collect();
            tx.execute(
                "INSERT INTO dream_actions(run_id,repo_id,kind,memory_ids,summary,reason,evidence,undo) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    run_id, repo_id, kind, json!(all_ids).to_string(), summary, reason, json!(quotes).to_string(),
                    json!({ "restore": undo, "created": created }).to_string()
                ],
            )?;
            applied.push(json!({ "action_id": tx.last_insert_rowid(), "type": kind, "ids": all_ids, "summary": summary, "reason": reason }));
        }
        // Last chance to stop: an agent started while we validated. Nothing is committed.
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            return Ok(());
        }
        tx.execute(
            "UPDATE dream_runs SET applied=?2, rejected=?3 WHERE id=?1",
            params![run_id, json!(applied).to_string(), json!(rejected).to_string()],
        )?;
        tx.commit()
    })?;
    if cancelled {
        return Ok(json!({ "cancelled": true }));
    }
    if !rejected.is_empty() {
        ctx.log_event("memory:dream_rejected", None, &json!({ "repo_id": repo_id, "run_id": run_id, "rejected": rejected }));
    }
    Ok(json!({ "applied": applied, "rejected": rejected, "embed": embed }))
}

fn label(r: &Value) -> String {
    let t = r["title"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_string).unwrap_or_else(|| {
        r["content"].as_str().unwrap_or("").replace('\n', " ").chars().take(60).collect()
    });
    format!("\u{201c}{}\u{201d}", t.trim())
}

// ---------- Undo ----------

fn undo_action_tx(c: &Connection, action_id: i64) -> rusqlite::Result<bool> {
    let row: Option<(String, Option<String>)> = c
        .query_row("SELECT undo, undone_at FROM dream_actions WHERE id=?1", [action_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((undo, undone)) = row else { return Ok(false) };
    if undone.is_some() {
        return Ok(false);
    }
    let u: Value = serde_json::from_str(&undo).unwrap_or(Value::Null);
    // Rows the dream created: retire them softly (no hard deletes), they never existed before.
    for id in u["created"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        c.execute("UPDATE memories SET active=0, status='deleted', updated_at=datetime('now') WHERE id=?1", [id])?;
    }
    for s in u["restore"].as_array().into_iter().flatten() {
        restore(c, s)?;
    }
    c.execute("UPDATE dream_actions SET undone_at=datetime('now') WHERE id=?1", [action_id])?;
    Ok(true)
}

/// Undo one action (`action_id`) or every action of a run (`run_id`, newest first).
pub fn undo(ctx: &Ctx, args: &Value) -> Result<Value, String> {
    let ids: Vec<i64> = if let Some(aid) = args["action_id"].as_i64() {
        vec![aid]
    } else if let Some(rid) = args["run_id"].as_str() {
        ctx.db
            .query("SELECT id FROM dream_actions WHERE run_id=?1 AND undone_at IS NULL ORDER BY id DESC", [rid])?
            .iter()
            .filter_map(|r| r["id"].as_i64())
            .collect()
    } else {
        return Err("action_id or run_id is required".into());
    };
    let n = ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        let mut n = 0;
        for id in &ids {
            if undo_action_tx(&tx, *id)? {
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    })?;
    crate::library::invalidate_vector_cache(ctx);
    ctx.notify("library");
    Ok(json!({ "undone": n }))
}

// ---------- Status for the UI ----------

pub fn status(ctx: &Ctx, repo_id: &str) -> Result<Value, String> {
    let last = ctx.db.query(
        "SELECT id, state, started_at, finished_at, model, tokens_in, tokens_out, rejected, note FROM dream_runs
         WHERE repo_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
        [repo_id],
    )?;
    let last_done = ctx.db.query(
        "SELECT finished_at FROM dream_runs WHERE repo_id=?1 AND state='done' ORDER BY finished_at DESC LIMIT 1",
        [repo_id],
    )?;
    let actions = match last.first().and_then(|r| r["id"].as_str()) {
        Some(rid) => ctx.db.query(
            "SELECT id, kind, memory_ids, summary, reason, evidence, undone_at FROM dream_actions WHERE run_id=?1 ORDER BY id",
            [rid],
        )?,
        None => vec![],
    };
    let mut last_run = last.into_iter().next().unwrap_or(Value::Null);
    if let Some(o) = last_run.as_object_mut() {
        let rej: Value = o.get("rejected").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!([]));
        o.insert("rejected".into(), rej);
    }
    Ok(json!({
        "enabled": enabled(ctx),
        "daily_tokens": daily_budget(ctx),
        "used_today": tokens_used_today(ctx, repo_id),
        "running": busy().lock().unwrap_or_else(|e| e.into_inner()).contains(&key(ctx, repo_id)),
        "last_dreamed_at": last_done.first().and_then(|r| r["finished_at"].as_str()),
        "last_run": last_run,
        "actions": actions,
    }))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: &Value) -> Option<Result<Value, String>> {
    let repo = || crate::ctx::arg_str(args, "repo_id");
    Some(match method {
        "dream_status" => match repo() {
            Ok(r) => status(ctx, &r),
            Err(e) => Err(e),
        },
        "dream_now" => match repo() {
            Ok(r) => run(ctx, &r, true).await,
            Err(e) => Err(e),
        },
        "dream_undo" => undo(ctx, args),
        "dream_config_get" => Ok(json!({ "enabled": enabled(ctx), "daily_tokens": daily_budget(ctx) })),
        "dream_config_set" => set_config(ctx, args),
        _ => return None,
    })
}

#[cfg(test)]
#[path = "dream_tests.rs"]
pub(crate) mod tests;
