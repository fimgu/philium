//! Verified-run procedural memory, skill mining, and per-model skill dose (IBM #2 `altk-evolve` + Zep Agent Skills).
//!
//! Blueprint invariants:
//! - Mine procedures ONLY from verified successful runs (NEVER unverified agent guesses, NEVER Strict agents,
//!   NEVER sibling repositories).
//! - Lossless by default: preserve concrete steps, exact verification commands, `support_count`,
//!   `verified_run_ids`, and `source_event_ids`. Never silently drop provenance and never equate frequency
//!   alone with correctness.
//! - Explicit lifecycle: mined procedures start as `candidate`, require operator promotion to `active`,
//!   and can be rolled back to `rolled_back` while preserving full audit history.
//! - Per-model skill dose: small models receive fewer procedures so they do not drown in long-tail
//!   guidelines; measured `skill_dose_profiles` can be persisted per `model_ref`.

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use serde_json::{json, Value};

use crate::ctx::Ctx;
use crate::db::new_id;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS learned_procedures (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  version INTEGER NOT NULL DEFAULT 1,
  title TEXT NOT NULL,
  steps TEXT NOT NULL,
  verify_cmd TEXT NOT NULL DEFAULT '',
  trigger_hint TEXT NOT NULL DEFAULT '',
  support_count INTEGER NOT NULL DEFAULT 1,
  verified_run_ids TEXT NOT NULL DEFAULT '[]',
  source_event_ids TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL DEFAULT 'candidate',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS learned_procedures_scope ON learned_procedures(scope, status);
CREATE TABLE IF NOT EXISTS skill_dose_profiles (
  model_ref TEXT PRIMARY KEY,
  max_procedures INTEGER NOT NULL,
  budget_tokens INTEGER NOT NULL,
  measured_score REAL,
  updated_at TEXT NOT NULL
);
";

/// Idempotently initialize the procedural memory and per-model skill dose tables.
pub fn init(ctx: &Ctx) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    Ok(())
}

// ---------- Normalization & Parsing Helpers ----------

fn normalize_key(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_string_array(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

fn decode_procedure_row(mut row: Value) -> Value {
    let run_ids = parse_string_array(row["verified_run_ids"].as_str());
    let event_ids = parse_string_array(row["source_event_ids"].as_str());
    row["verified_run_ids"] = json!(run_ids);
    row["source_event_ids"] = json!(event_ids);
    row
}

fn get_procedure_row(ctx: &Ctx, id: &str) -> Result<Option<Value>, String> {
    let rows = ctx.db.query(
        "SELECT id, scope, version, title, steps, verify_cmd, trigger_hint, support_count,
                verified_run_ids, source_event_ids, status, created_at, updated_at
         FROM learned_procedures
         WHERE id = ?1",
        [id],
    )?;
    Ok(rows.into_iter().next().map(decode_procedure_row))
}

fn steps_are_improved(existing_steps: &str, new_steps: &str) -> bool {
    let old_norm = normalize_key(existing_steps);
    let new_norm = normalize_key(new_steps);
    if new_norm.is_empty() || new_norm == old_norm {
        return false;
    }
    // Treat new steps as an improvement when they add concrete detail/steps beyond the existing record.
    let old_lines = existing_steps.lines().filter(|l| !l.trim().is_empty()).count();
    let new_lines = new_steps.lines().filter(|l| !l.trim().is_empty()).count();
    new_norm.len() > old_norm.len() || new_lines > old_lines
}

fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() / 4).max(1) + 8
}

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

// ---------- Recording Verified Procedures ----------

enum RecordOutcome {
    Created(Value),
    Updated(Value),
    Unchanged(#[allow(dead_code)] Value),
}

fn record_verified_procedure_inner(
    ctx: &Ctx,
    scope: &str,
    title: &str,
    steps: &str,
    verify_cmd: &str,
    trigger_hint: &str,
    run_id: &str,
    source_event_id: Option<&str>,
) -> Result<RecordOutcome, String> {
    let scope = scope.trim();
    let title = title.trim();
    let steps = steps.trim();
    let verify_cmd = verify_cmd.trim();
    let trigger_hint = trigger_hint.trim();
    let run_id = run_id.trim();
    let source_event_id = source_event_id.map(str::trim).filter(|s| !s.is_empty());

    if scope.is_empty() {
        return Err("scope is required".into());
    }
    if title.is_empty() {
        return Err("title is required".into());
    }
    if steps.is_empty() {
        return Err("steps are required".into());
    }
    if run_id.is_empty() {
        return Err("verified run_id is required".into());
    }

    let norm_title = normalize_key(title);
    let norm_cmd = normalize_key(verify_cmd);

    let existing_rows = ctx.db.query(
        "SELECT id, scope, version, title, steps, verify_cmd, trigger_hint, support_count,
                verified_run_ids, source_event_ids, status, created_at, updated_at
         FROM learned_procedures
         WHERE scope = ?1
         ORDER BY
           CASE status WHEN 'candidate' THEN 0 WHEN 'active' THEN 1 ELSE 2 END,
           version DESC,
           updated_at DESC",
        [scope],
    )?;

    let mut matched_candidate: Option<Value> = None;
    let mut matched_active: Option<Value> = None;
    let mut max_version: i64 = 0;

    for row in existing_rows {
        let row_title = normalize_key(row["title"].as_str().unwrap_or(""));
        let row_cmd = normalize_key(row["verify_cmd"].as_str().unwrap_or(""));
        let is_match = row_title == norm_title || (!norm_cmd.is_empty() && row_cmd == norm_cmd);
        if !is_match {
            continue;
        }
        let ver = row["version"].as_i64().unwrap_or(1);
        if ver > max_version {
            max_version = ver;
        }
        match row["status"].as_str().unwrap_or("") {
            "candidate" if matched_candidate.is_none() => matched_candidate = Some(row),
            "active" if matched_active.is_none() => matched_active = Some(row),
            _ => {}
        }
    }

    if let Some(existing) = matched_candidate.or(matched_active) {
        let existing_id = existing["id"].as_str().unwrap_or("").to_string();
        let existing_status = existing["status"].as_str().unwrap_or("candidate");
        let existing_version = existing["version"].as_i64().unwrap_or(1);
        let existing_steps = existing["steps"].as_str().unwrap_or("");
        let existing_cmd = existing["verify_cmd"].as_str().unwrap_or("");
        let existing_hint = existing["trigger_hint"].as_str().unwrap_or("");
        let existing_support = existing["support_count"].as_i64().unwrap_or(1);

        let mut run_ids = parse_string_array(existing["verified_run_ids"].as_str());
        let mut event_ids = parse_string_array(existing["source_event_ids"].as_str());

        let is_new_run = !run_ids.iter().any(|r| r == run_id);
        if is_new_run {
            run_ids.push(run_id.to_string());
        }
        let mut is_new_event = false;
        if let Some(ev_id) = source_event_id {
            if !event_ids.iter().any(|e| e == ev_id) {
                event_ids.push(ev_id.to_string());
                is_new_event = true;
            }
        }

        let new_support = if is_new_run {
            (existing_support + 1).max(run_ids.len() as i64)
        } else {
            existing_support.max(run_ids.len() as i64)
        };
        let improved = steps_are_improved(existing_steps, steps);

        if existing_status == "active" && improved {
            // Never silently overwrite an operator-promoted active procedure:
            // preserve the active row intact and emit a new versioned candidate with lossless provenance.
            let new_id = format!("proc_{}", new_id());
            let next_version = max_version.max(existing_version) + 1;
            let merged_cmd = if !verify_cmd.is_empty() { verify_cmd } else { existing_cmd };
            let merged_hint = if !trigger_hint.is_empty() { trigger_hint } else { existing_hint };
            let run_json = serde_json::to_string(&run_ids).unwrap_or_else(|_| "[]".into());
            let ev_json = serde_json::to_string(&event_ids).unwrap_or_else(|_| "[]".into());
            ctx.db.exec(
                "INSERT INTO learned_procedures(
                    id, scope, version, title, steps, verify_cmd, trigger_hint,
                    support_count, verified_run_ids, source_event_ids, status, created_at, updated_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'candidate', datetime('now'), datetime('now'))",
                params![
                    new_id,
                    scope,
                    next_version,
                    title,
                    steps,
                    merged_cmd,
                    merged_hint,
                    new_support,
                    run_json,
                    ev_json,
                ],
            )?;
            ctx.log_event(
                "memory_procedure_recorded",
                None,
                &json!({
                    "id": new_id,
                    "scope": scope,
                    "version": next_version,
                    "supersedes_active_id": existing_id,
                    "run_id": run_id,
                }),
            );
            let created = get_procedure_row(ctx, &new_id)?
                .ok_or_else(|| "failed to load created procedure candidate".to_string())?;
            return Ok(RecordOutcome::Created(created));
        }

        let fill_cmd = existing_cmd.is_empty() && !verify_cmd.is_empty();
        let fill_hint = existing_hint.is_empty() && !trigger_hint.is_empty();
        let should_update_steps = existing_status != "active" && improved;

        if !is_new_run && !is_new_event && !should_update_steps && !fill_cmd && !fill_hint {
            let current = get_procedure_row(ctx, &existing_id)?
                .ok_or_else(|| "failed to load existing procedure".to_string())?;
            return Ok(RecordOutcome::Unchanged(current));
        }

        let next_version = if should_update_steps {
            max_version.max(existing_version) + 1
        } else {
            existing_version
        };
        let next_steps = if should_update_steps { steps } else { existing_steps };
        let next_cmd = if fill_cmd { verify_cmd } else { existing_cmd };
        let next_hint = if fill_hint { trigger_hint } else { existing_hint };
        let run_json = serde_json::to_string(&run_ids).unwrap_or_else(|_| "[]".into());
        let ev_json = serde_json::to_string(&event_ids).unwrap_or_else(|_| "[]".into());

        ctx.db.exec(
            "UPDATE learned_procedures
             SET version = ?2,
                 steps = ?3,
                 verify_cmd = ?4,
                 trigger_hint = ?5,
                 support_count = ?6,
                 verified_run_ids = ?7,
                 source_event_ids = ?8,
                 updated_at = datetime('now')
             WHERE id = ?1",
            params![
                existing_id,
                next_version,
                next_steps,
                next_cmd,
                next_hint,
                new_support,
                run_json,
                ev_json,
            ],
        )?;
        let updated = get_procedure_row(ctx, &existing_id)?
            .ok_or_else(|| "failed to load updated procedure".to_string())?;
        return Ok(RecordOutcome::Updated(updated));
    }

    let id = format!("proc_{}", new_id());
    let next_version = max_version + 1;
    let run_ids = vec![run_id.to_string()];
    let event_ids: Vec<String> = source_event_id.into_iter().map(str::to_string).collect();
    let run_json = serde_json::to_string(&run_ids).unwrap_or_else(|_| "[]".into());
    let ev_json = serde_json::to_string(&event_ids).unwrap_or_else(|_| "[]".into());

    ctx.db.exec(
        "INSERT INTO learned_procedures(
            id, scope, version, title, steps, verify_cmd, trigger_hint,
            support_count, verified_run_ids, source_event_ids, status, created_at, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, 'candidate', datetime('now'), datetime('now'))",
        params![
            id,
            scope,
            next_version,
            title,
            steps,
            verify_cmd,
            trigger_hint,
            run_json,
            ev_json,
        ],
    )?;
    ctx.log_event(
        "memory_procedure_recorded",
        None,
        &json!({
            "id": id,
            "scope": scope,
            "version": next_version,
            "run_id": run_id,
        }),
    );
    let created = get_procedure_row(ctx, &id)?
        .ok_or_else(|| "failed to load recorded procedure".to_string())?;
    Ok(RecordOutcome::Created(created))
}

/// Record a verified procedure losslessly, deduplicating by normalized `(scope, title)` or `(scope, verify_cmd)`.
/// Appends `run_id` and `source_event_id` losslessly, increments `support_count` on new runs, and never
/// silently overwrites an operator-promoted `active` procedure.
pub fn record_verified_procedure(
    ctx: &Ctx,
    scope: &str,
    title: &str,
    steps: &str,
    verify_cmd: &str,
    trigger_hint: &str,
    run_id: &str,
    source_event_id: Option<&str>,
) -> Result<Value, String> {
    match record_verified_procedure_inner(
        ctx,
        scope,
        title,
        steps,
        verify_cmd,
        trigger_hint,
        run_id,
        source_event_id,
    )? {
        RecordOutcome::Created(v) | RecordOutcome::Updated(v) | RecordOutcome::Unchanged(v) => Ok(v),
    }
}

// ---------- Verification Command & Evidence Recognition ----------

fn extract_tool_command(args: Option<&Value>) -> Option<String> {
    let args = args?;
    if let Some(s) = args.as_str() {
        let trimmed = s.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    for key in ["command", "cmd", "script"] {
        if let Some(s) = args.get(key).and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn is_verification_command(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    let trimmed = lower.trim();
    if trimmed.is_empty() {
        return false;
    }
    const PATTERNS: &[&str] = &[
        "cargo test",
        "cargo check",
        "cargo clippy",
        "cargo build",
        "cargo nextest",
        "npm test",
        "npm run test",
        "npm run build",
        "npm run check",
        "npm run lint",
        "npm run typecheck",
        "pnpm test",
        "pnpm run test",
        "pnpm build",
        "pnpm run build",
        "pnpm check",
        "yarn test",
        "yarn build",
        "bun test",
        "bun run build",
        "vitest",
        "jest",
        "node --test",
        "tsc --noemit",
        "pytest",
        "python -m unittest",
        "python3 -m unittest",
        "mypy",
        "ruff check",
        "go test",
        "go vet",
        "go build",
        "make test",
        "make check",
        "make build",
        "just test",
        "just check",
        "ctest",
        "mvn test",
        "gradle test",
        "gradlew test",
    ];
    PATTERNS.iter().any(|p| contains_command_token(trimmed, p))
}

fn contains_command_token(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before_ok = abs == 0
            || matches!(
                haystack.as_bytes()[abs - 1],
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b'/' | b'='
            );
        let after = abs + needle.len();
        let after_ok = after == haystack.len()
            || matches!(
                haystack.as_bytes()[after],
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b')' | b'"' | b'\''
            );
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn result_indicates_failure(result_val: Option<&Value>, result_text: &str, output_text: &str) -> bool {
    if let Some(obj) = result_val.and_then(Value::as_object) {
        if let Some(code) = obj.get("exit_code").and_then(Value::as_i64) {
            if code != 0 {
                return true;
            }
        }
        if obj.get("ok").and_then(Value::as_bool) == Some(false) {
            return true;
        }
    }
    let combined = format!("{result_text}\n{output_text}");
    let lower = combined.to_ascii_lowercase();
    for marker in ["exit_code:", "exit status:", "exited with code "] {
        if let Some(idx) = lower.find(marker) {
            let rest = lower[idx + marker.len()..].trim_start();
            let digits: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(code) = digits.parse::<i64>() {
                if code != 0 {
                    return true;
                }
            }
        }
    }
    lower.contains("test result: failed")
        || lower.contains("build failed")
        || lower.contains("npm err!")
        || lower.contains("error: test failed")
        || lower.contains("--- fail:")
}

fn clean_single_line(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(max_chars.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn derive_command_title(cmd: &str) -> String {
    format!("Verify with `{}`", clean_single_line(cmd, 96))
}

fn derive_trigger_hint(cmd: &str, context_steps: &[String]) -> String {
    let mut tokens: Vec<String> = Vec::new();
    for w in cmd.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != ':' && c != '.') {
        let w = w.trim();
        if w.len() >= 2 && !tokens.iter().any(|t| t.eq_ignore_ascii_case(w)) {
            tokens.push(w.to_ascii_lowercase());
            if tokens.len() >= 8 {
                break;
            }
        }
    }
    for step in context_steps.iter().rev().take(3) {
        for w in step.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '/' && c != '.') {
            if (w.contains('/') || w.contains('.')) && w.len() >= 3 {
                let lw = w.to_ascii_lowercase();
                if !tokens.contains(&lw) {
                    tokens.push(lw);
                    if tokens.len() >= 12 {
                        break;
                    }
                }
            }
        }
    }
    tokens.join(" ")
}

fn extract_verify_cmd_from_text(text: &str) -> Option<String> {
    // Prefer backticked verification commands first.
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else { break };
        let candidate = after_open[..close].trim();
        if !candidate.is_empty() && candidate.len() <= 240 && is_verification_command(candidate) {
            return Some(clean_single_line(candidate, 200));
        }
        rest = &after_open[close + 1..];
    }
    // Otherwise scan lines for a concrete verification command.
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches(['-', '*', '$', ' ']);
        if !trimmed.is_empty() && trimmed.len() <= 240 && is_verification_command(trimmed) {
            return Some(clean_single_line(trimmed, 200));
        }
    }
    None
}

// ---------- Mining Verified Runs ----------

#[derive(Debug, Clone)]
struct StartedTool {
    seq: i64,
    tool: String,
    command: Option<String>,
    summary: String,
}

fn table_has_column(ctx: &Ctx, table: &str, col: &str) -> bool {
    ctx.db
        .query(&format!("PRAGMA table_info({table})"), [])
        .unwrap_or_default()
        .iter()
        .any(|r| r["name"].as_str() == Some(col))
}

fn table_exists(ctx: &Ctx, table: &str) -> bool {
    ctx.db
        .query(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
        )
        .map(|rows| !rows.is_empty())
        .unwrap_or(false)
}

fn is_strict_agent(ctx: &Ctx, agent_ref: &str) -> bool {
    ctx.db
        .query(
            "SELECT strict FROM agents WHERE id = ?1 OR name = ?1",
            [agent_ref],
        )
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|r| r["strict"].as_i64())
        == Some(1)
}

/// Mine candidate procedural skills strictly from verified passing runs in `repo_id`.
/// Never inspects Strict agents or sibling repos, and never mines failing or unverified runs.
pub fn mine_verified_runs(ctx: &Ctx, repo_id: &str) -> Result<Value, String> {
    let repo_id = repo_id.trim();
    if repo_id.is_empty() {
        return Err("repo_id is required".into());
    }
    let scope = format!("repo:{repo_id}");
    let mut mined = 0usize;
    let mut updated = 0usize;
    let mut touched_ids: Vec<String> = Vec::new();
    let mut touched_map: HashMap<String, Value> = HashMap::new();

    let mut track = |outcome: RecordOutcome| match outcome {
        RecordOutcome::Created(v) => {
            mined += 1;
            if let Some(id) = v["id"].as_str().map(str::to_string) {
                if !touched_map.contains_key(&id) {
                    touched_ids.push(id.clone());
                }
                touched_map.insert(id, v);
            }
        }
        RecordOutcome::Updated(v) => {
            updated += 1;
            if let Some(id) = v["id"].as_str().map(str::to_string) {
                if !touched_map.contains_key(&id) {
                    touched_ids.push(id.clone());
                }
                touched_map.insert(id, v);
            }
        }
        RecordOutcome::Unchanged(_) => {}
    };

    // 1. Standard (non-Strict) agents' HAL event journals scoped strictly to `repo_id`.
    let agents = ctx.db.query(
        "SELECT id, name, hal_session_id, strict
         FROM agents
         WHERE repo_id = ?1
           AND strict = 0
           AND hal_session_id IS NOT NULL
           AND hal_session_id != ''
         ORDER BY last_active_at DESC",
        [repo_id],
    )?;

    for agent in agents {
        if agent["strict"].as_i64().unwrap_or(1) != 0 {
            continue;
        }
        let Some(agent_id) = agent["id"].as_str() else { continue };
        let Some(session_id) = agent["hal_session_id"].as_str() else { continue };
        let Ok(Value::Array(events)) = crate::agents::history::events(ctx, agent_id, 0, 10_000) else {
            continue;
        };

        let mut pending: HashMap<String, StartedTool> = HashMap::new();
        let mut last_started: Option<StartedTool> = None;
        let mut outputs: HashMap<String, String> = HashMap::new();
        let mut recent_steps: Vec<String> = Vec::new();

        for ev in events {
            let seq = ev["seq"].as_i64().unwrap_or(0);
            let ty = ev["type"].as_str().unwrap_or("");
            match ty {
                "tool.started" => {
                    let tool = ev["tool"].as_str().unwrap_or("tool").to_string();
                    let args = ev.get("arguments");
                    let command = extract_tool_command(args);
                    let summary = if let Some(ref cmd) = command {
                        format!("{tool}: `{}`", clean_single_line(cmd, 180))
                    } else if let Some(path) = args
                        .and_then(|a| a.get("path").or_else(|| a.get("file")))
                        .and_then(Value::as_str)
                    {
                        format!("{tool} on `{}`", clean_single_line(path, 120))
                    } else {
                        tool.clone()
                    };
                    let started = StartedTool {
                        seq,
                        tool,
                        command,
                        summary,
                    };
                    if let Some(cid) = ev["call_id"].as_str().filter(|s| !s.is_empty()) {
                        pending.insert(cid.to_string(), started.clone());
                    }
                    last_started = Some(started);
                }
                "tool.output" => {
                    if let (Some(cid), Some(data)) = (ev["call_id"].as_str(), ev["data"].as_str()) {
                        let buf = outputs.entry(cid.to_string()).or_default();
                        if buf.len() < 4096 {
                            buf.push_str(data);
                            buf.push('\n');
                        }
                    }
                }
                "tool.finished" => {
                    let cid_opt = ev["call_id"].as_str().filter(|s| !s.is_empty());
                    let started = cid_opt
                        .and_then(|cid| pending.remove(cid))
                        .or_else(|| last_started.take());
                    let out_txt = cid_opt
                        .and_then(|cid| outputs.remove(cid))
                        .unwrap_or_default();

                    let ok = ev["ok"].as_bool() == Some(true);
                    let result_val = ev.get("result");
                    let res_txt = match result_val {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) if !v.is_null() => v.to_string(),
                        _ => String::new(),
                    };

                    let Some(started) = started else { continue };
                    if !ok || result_indicates_failure(result_val, &res_txt, &out_txt) {
                        continue;
                    }

                    if let Some(ref cmd) = started.command {
                        if is_verification_command(cmd) {
                            let verify_cmd = clean_single_line(cmd, 240);
                            let title = derive_command_title(&verify_cmd);
                            let trigger_hint = derive_trigger_hint(&verify_cmd, &recent_steps);

                            let mut step_lines: Vec<String> = Vec::new();
                            let start_idx = recent_steps.len().saturating_sub(4);
                            for (i, prev) in recent_steps[start_idx..].iter().enumerate() {
                                step_lines.push(format!("{}. {prev}", i + 1));
                            }
                            let verify_step_num = step_lines.len() + 1;
                            step_lines.push(format!(
                                "{verify_step_num}. Run `{verify_cmd}` ({}) and verify it passes.",
                                started.tool
                            ));
                            let evidence_snippet = if !res_txt.trim().is_empty() {
                                clean_single_line(&res_txt, 160)
                            } else if !out_txt.trim().is_empty() {
                                clean_single_line(&out_txt, 160)
                            } else {
                                String::new()
                            };
                            if !evidence_snippet.is_empty() {
                                step_lines.push(format!("Verified output: {evidence_snippet}"));
                            }
                            let steps = step_lines.join("\n");
                            let run_id = format!("hal:{agent_id}:{session_id}:{seq}");
                            let source_event_id =
                                format!("hal:{agent_id}:{session_id}:{}-{}", started.seq, seq);

                            let outcome = record_verified_procedure_inner(
                                ctx,
                                &scope,
                                &title,
                                &steps,
                                &verify_cmd,
                                &trigger_hint,
                                &run_id,
                                Some(&source_event_id),
                            )?;
                            track(outcome);
                            recent_steps.clear();
                            continue;
                        }
                    }

                    // Record preceding successful non-verification action as context for a subsequent verified run.
                    if recent_steps.len() >= 8 {
                        recent_steps.remove(0);
                    }
                    recent_steps.push(started.summary);
                }
                _ => {}
            }
        }
    }

    // 2. Verified tasks strictly scoped to `repo_id` with `status IN ('done','submitted')`
    //    backed by passing `review_result` or board evidence events (excluding Strict agents).
    let has_review_cols = table_has_column(ctx, "tasks", "review_result")
        && table_has_column(ctx, "tasks", "review_evidence");
    let has_check_results = table_has_column(ctx, "tasks", "check_results");
    let has_assigned_agent = table_has_column(ctx, "tasks", "assigned_agent_id");
    let has_task_events = table_exists(ctx, "task_events");

    let task_select = format!(
        "SELECT id, title, detail, status, owner_agent_id, playbook,
                {} AS assigned_agent_id,
                {} AS review_result,
                {} AS review_evidence,
                {} AS check_results
         FROM tasks
         WHERE repo_id = ?1 AND status IN ('done', 'submitted')
         ORDER BY updated_at DESC",
        if has_assigned_agent { "assigned_agent_id" } else { "NULL" },
        if has_review_cols { "review_result" } else { "NULL" },
        if has_review_cols { "review_evidence" } else { "NULL" },
        if has_check_results { "check_results" } else { "'[]'" },
    );

    let tasks = ctx.db.query(&task_select, [repo_id])?;
    for task in tasks {
        let Some(task_id) = task["id"].as_str() else { continue };
        if let Some(owner_id) = task["owner_agent_id"].as_str().filter(|s| !s.is_empty()) {
            if is_strict_agent(ctx, owner_id) {
                continue;
            }
        }
        if let Some(assigned_id) = task["assigned_agent_id"].as_str().filter(|s| !s.is_empty()) {
            if is_strict_agent(ctx, assigned_id) {
                continue;
            }
        }

        let review_result = task["review_result"].as_str().unwrap_or("");
        if matches!(review_result, "fail" | "blocked") {
            continue;
        }
        let review_evidence = task["review_evidence"].as_str().unwrap_or("").trim();

        let check_results: Vec<Value> = task["check_results"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        if check_results.iter().any(|c| c["passed"].as_bool() == Some(false)) {
            continue;
        }
        let has_passing_checks = check_results
            .iter()
            .any(|c| c["passed"].as_bool() == Some(true));

        let mut verified = false;
        let mut evidence_parts: Vec<String> = Vec::new();
        let mut source_event_id: Option<String> = None;

        if review_result == "pass" && !review_evidence.is_empty() {
            verified = true;
            evidence_parts.push(format!("Review passed: {}", clean_single_line(review_evidence, 300)));
            source_event_id = Some(format!("task_review:{task_id}"));
        }

        if has_task_events {
            let tevs = ctx
                .db
                .query(
                    "SELECT id, actor, kind, body FROM task_events WHERE task_id = ?1 ORDER BY id DESC",
                    [task_id],
                )
                .unwrap_or_default();
            let mut strict_actor_seen = false;
            for tev in tevs {
                if let Some(actor) = tev["actor"].as_str().filter(|a| *a != "you" && !a.is_empty()) {
                    if is_strict_agent(ctx, actor) {
                        strict_actor_seen = true;
                        break;
                    }
                }
                let kind = tev["kind"].as_str().unwrap_or("");
                let body_str = tev["body"].as_str().unwrap_or("");
                let body_val: Value = serde_json::from_str(body_str).unwrap_or(Value::Null);
                let tev_id = tev["id"].as_i64().unwrap_or(0);
                match kind {
                    "checked" => {
                        if body_val["result"].as_str() == Some("pass") {
                            verified = true;
                            if source_event_id.is_none() && tev_id > 0 {
                                source_event_id = Some(format!("task_event:{tev_id}"));
                            }
                        } else if matches!(body_val["result"].as_str(), Some("fail" | "blocked")) {
                            verified = false;
                            break;
                        }
                    }
                    "submitted" => {
                        if let Some(summary) = body_val["summary"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                            if has_passing_checks || extract_verify_cmd_from_text(summary).is_some() {
                                verified = true;
                                evidence_parts.push(format!(
                                    "Submitted evidence: {}",
                                    clean_single_line(summary, 300)
                                ));
                                if source_event_id.is_none() && tev_id > 0 {
                                    source_event_id = Some(format!("task_event:{tev_id}"));
                                }
                            }
                        }
                    }
                    "checks" => {
                        if let Some(arr) = body_val.as_array() {
                            if arr.iter().any(|c| c["passed"].as_bool() == Some(false)) {
                                verified = false;
                                break;
                            }
                            if arr.iter().any(|c| c["passed"].as_bool() == Some(true)) {
                                verified = true;
                                if source_event_id.is_none() && tev_id > 0 {
                                    source_event_id = Some(format!("task_event:{tev_id}"));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if strict_actor_seen {
                continue;
            }
        }

        // Also check global `events` table for passing `review_result` / `board` evidence referencing `task_id`.
        if !verified {
            let ev_rows = ctx
                .db
                .query(
                    "SELECT id, kind, agent_id, body FROM events
                     WHERE kind IN ('review_result', 'board_verified', 'board.checked', 'board.submitted')
                     ORDER BY id DESC LIMIT 200",
                    [],
                )
                .unwrap_or_default();
            for evr in ev_rows {
                if let Some(aid) = evr["agent_id"].as_str().filter(|s| !s.is_empty()) {
                    if is_strict_agent(ctx, aid) {
                        continue;
                    }
                }
                let body_str = evr["body"].as_str().unwrap_or("");
                let body_val: Value = serde_json::from_str(body_str).unwrap_or(Value::Null);
                if body_val["task_id"].as_str() != Some(task_id) {
                    continue;
                }
                let passed = body_val["result"].as_str() == Some("pass")
                    || body_val["passed"].as_bool() == Some(true)
                    || body_val["ok"].as_bool() == Some(true);
                if passed {
                    verified = true;
                    if let Some(ev_text) = body_val["evidence"]
                        .as_str()
                        .or_else(|| body_val["summary"].as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        evidence_parts.push(clean_single_line(ev_text, 300));
                    }
                    if let Some(eid) = evr["id"].as_i64() {
                        source_event_id = Some(format!("evt:{eid}"));
                    }
                    break;
                }
            }
        }

        if !verified {
            continue;
        }

        let task_title = task["title"].as_str().unwrap_or("").trim();
        if task_title.is_empty() {
            continue;
        }
        let task_detail = task["detail"].as_str().unwrap_or("").trim();
        let combined_text = format!(
            "{}\n{}\n{}",
            review_evidence,
            evidence_parts.join("\n"),
            task_detail
        );
        let verify_cmd = extract_verify_cmd_from_text(&combined_text).unwrap_or_default();

        let mut step_lines = Vec::new();
        if !task_detail.is_empty() {
            step_lines.push(format!("1. {}", clean_single_line(task_detail, 260)));
        }
        for part in &evidence_parts {
            step_lines.push(format!("{}. {part}", step_lines.len() + 1));
        }
        if !verify_cmd.is_empty() {
            step_lines.push(format!(
                "{}. Verify with `{verify_cmd}`.",
                step_lines.len() + 1
            ));
        }
        if step_lines.is_empty() {
            step_lines.push(format!("1. Complete and verify: {task_title}"));
        }

        let mut hint_parts = vec![task_title.to_ascii_lowercase()];
        if let Some(pb) = task["playbook"].as_str().filter(|s| !s.is_empty()) {
            hint_parts.push(pb.to_ascii_lowercase());
        }
        if !verify_cmd.is_empty() {
            hint_parts.push(verify_cmd.to_ascii_lowercase());
        }
        let trigger_hint = clean_single_line(&hint_parts.join(" "), 160);

        let run_id = format!("task:{task_id}");
        let outcome = record_verified_procedure_inner(
            ctx,
            &scope,
            task_title,
            &step_lines.join("\n"),
            &verify_cmd,
            &trigger_hint,
            &run_id,
            source_event_id.as_deref(),
        )?;
        track(outcome);
    }

    let procedures: Vec<Value> = touched_ids
        .into_iter()
        .filter_map(|id| touched_map.remove(&id))
        .collect();

    Ok(json!({
        "scope": scope,
        "mined": mined,
        "updated": updated,
        "procedures": procedures,
    }))
}

// ---------- Lifecycle Management ----------

/// List learned procedures filtered by optional `scope` and `status`.
pub fn list_procedures(
    ctx: &Ctx,
    scope: Option<&str>,
    status: Option<&str>,
    limit: i64,
) -> Result<Value, String> {
    let limit = limit.clamp(1, 200);
    let scope = scope.map(str::trim).filter(|s| !s.is_empty());
    let status = status.map(str::trim).filter(|s| !s.is_empty());
    let rows = ctx.db.query(
        "SELECT id, scope, version, title, steps, verify_cmd, trigger_hint, support_count,
                verified_run_ids, source_event_ids, status, created_at, updated_at
         FROM learned_procedures
         WHERE (?1 IS NULL OR scope = ?1)
           AND (?2 IS NULL OR status = ?2)
         ORDER BY
           CASE status WHEN 'active' THEN 0 WHEN 'candidate' THEN 1 ELSE 2 END,
           updated_at DESC,
           id ASC
         LIMIT ?3",
        params![scope, status, limit],
    )?;
    Ok(Value::Array(
        rows.into_iter().map(decode_procedure_row).collect(),
    ))
}

/// Promote a candidate procedure to `active`, superseding any older active version of the same procedure.
pub fn promote_procedure(ctx: &Ctx, id: &str) -> Result<Value, String> {
    let id = id.trim();
    let existing = get_procedure_row(ctx, id)?
        .ok_or_else(|| format!("procedure not found: {id}"))?;
    let current_status = existing["status"].as_str().unwrap_or("");
    if current_status == "rolled_back" {
        return Err("cannot promote a rolled_back procedure".into());
    }

    let scope = existing["scope"].as_str().unwrap_or("");
    let norm_title = normalize_key(existing["title"].as_str().unwrap_or(""));
    let norm_cmd = normalize_key(existing["verify_cmd"].as_str().unwrap_or(""));

    // Mark any previously active version of the same normalized procedure as superseded
    // so only one active version exists per procedure while preserving audit history.
    let active_peers = ctx.db.query(
        "SELECT id, title, verify_cmd FROM learned_procedures
         WHERE scope = ?1 AND status = 'active' AND id != ?2",
        params![scope, id],
    )?;
    for peer in active_peers {
        let peer_id = peer["id"].as_str().unwrap_or("");
        let peer_title = normalize_key(peer["title"].as_str().unwrap_or(""));
        let peer_cmd = normalize_key(peer["verify_cmd"].as_str().unwrap_or(""));
        if peer_title == norm_title || (!norm_cmd.is_empty() && peer_cmd == norm_cmd) {
            ctx.db.exec(
                "UPDATE learned_procedures SET status = 'superseded', updated_at = datetime('now') WHERE id = ?1",
                [peer_id],
            )?;
        }
    }

    ctx.db.exec(
        "UPDATE learned_procedures SET status = 'active', updated_at = datetime('now') WHERE id = ?1",
        [id],
    )?;
    let updated = get_procedure_row(ctx, id)?
        .ok_or_else(|| format!("procedure not found after promote: {id}"))?;
    ctx.log_event(
        "memory_procedure_promoted",
        None,
        &json!({
            "id": id,
            "scope": updated["scope"],
            "version": updated["version"],
            "title": updated["title"],
        }),
    );
    Ok(updated)
}

/// Roll back a procedure by marking `status = 'rolled_back'` while preserving full audit history.
pub fn rollback_procedure(ctx: &Ctx, id: &str) -> Result<Value, String> {
    let id = id.trim();
    let _existing = get_procedure_row(ctx, id)?
        .ok_or_else(|| format!("procedure not found: {id}"))?;
    ctx.db.exec(
        "UPDATE learned_procedures SET status = 'rolled_back', updated_at = datetime('now') WHERE id = ?1",
        [id],
    )?;
    let updated = get_procedure_row(ctx, id)?
        .ok_or_else(|| format!("procedure not found after rollback: {id}"))?;
    ctx.log_event(
        "memory_procedure_rolled_back",
        None,
        &json!({
            "id": id,
            "scope": updated["scope"],
            "version": updated["version"],
            "title": updated["title"],
        }),
    );
    Ok(updated)
}

// ---------- Per-Model Skill Dose (IBM #2) ----------

/// Resolve the per-model skill dose profile: persisted `skill_dose_profiles` first,
/// otherwise defaults scaled by `providers::context_window`.
pub fn skill_dose_get(ctx: &Ctx, model_ref: Option<&str>) -> Value {
    let mr = model_ref.map(str::trim).filter(|s| !s.is_empty());

    if let Some(m) = mr {
        if let Ok(rows) = ctx.db.query(
            "SELECT model_ref, max_procedures, budget_tokens, measured_score, updated_at
             FROM skill_dose_profiles
             WHERE model_ref = ?1",
            [m],
        ) {
            if let Some(row) = rows.into_iter().next() {
                return json!({
                    "model_ref": row["model_ref"],
                    "max_procedures": row["max_procedures"],
                    "budget_tokens": row["budget_tokens"],
                    "measured_score": row["measured_score"],
                    "updated_at": row["updated_at"],
                    "source": "profile",
                });
            }
        }

        if let Some((pid, mid)) = crate::providers::split_ref(m) {
            if let Some(cw) = crate::providers::context_window(ctx, pid, mid) {
                let (max_procedures, budget_tokens) = if cw <= 32_768 {
                    (3i64, 450i64)
                } else if cw <= 131_072 {
                    (6i64, 900i64)
                } else {
                    (10i64, 1500i64)
                };
                return json!({
                    "model_ref": m,
                    "max_procedures": max_procedures,
                    "budget_tokens": budget_tokens,
                    "measured_score": Value::Null,
                    "context_window": cw,
                    "source": "context_window",
                });
            }
        }
    }

    json!({
        "model_ref": mr,
        "max_procedures": 5,
        "budget_tokens": 750,
        "measured_score": Value::Null,
        "source": "default",
    })
}

/// Persist a measured per-model skill dose profile.
pub fn skill_dose_set(
    ctx: &Ctx,
    model_ref: &str,
    max_procedures: i64,
    budget_tokens: Option<i64>,
    measured_score: Option<f64>,
) -> Result<Value, String> {
    let model_ref = model_ref.trim();
    if model_ref.is_empty() {
        return Err("model_ref is required".into());
    }
    let max_procedures = max_procedures.clamp(0, 50);
    let budget_tokens = match budget_tokens {
        Some(b) => b.clamp(0, 16_000),
        None => {
            if max_procedures == 0 {
                0
            } else {
                (max_procedures * 150).clamp(150, 8_000)
            }
        }
    };
    if let Some(score) = measured_score {
        if !score.is_finite() {
            return Err("measured_score must be a finite number".into());
        }
    }

    ctx.db.exec(
        "INSERT INTO skill_dose_profiles(model_ref, max_procedures, budget_tokens, measured_score, updated_at)
         VALUES(?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(model_ref) DO UPDATE SET
           max_procedures = excluded.max_procedures,
           budget_tokens = excluded.budget_tokens,
           measured_score = excluded.measured_score,
           updated_at = datetime('now')",
        params![model_ref, max_procedures, budget_tokens, measured_score],
    )?;
    ctx.log_event(
        "memory_skill_dose_updated",
        None,
        &json!({
            "model_ref": model_ref,
            "max_procedures": max_procedures,
            "budget_tokens": budget_tokens,
            "measured_score": measured_score,
        }),
    );
    Ok(skill_dose_get(ctx, Some(model_ref)))
}

fn tokenize_query(q: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for raw in q.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let t = raw.to_ascii_lowercase();
        if t.len() >= 2 && seen.insert(t.clone()) {
            terms.push(t);
        }
    }
    terms
}

fn lexical_score_procedure(proc_val: &Value, query_lower: &str, terms: &[String]) -> u32 {
    if query_lower.is_empty() {
        return 0;
    }
    let title = proc_val["title"].as_str().unwrap_or("").to_ascii_lowercase();
    let trigger = proc_val["trigger_hint"].as_str().unwrap_or("").to_ascii_lowercase();
    let verify = proc_val["verify_cmd"].as_str().unwrap_or("").to_ascii_lowercase();
    let steps = proc_val["steps"].as_str().unwrap_or("").to_ascii_lowercase();

    let mut score = 0u32;
    if title.contains(query_lower) {
        score += 6;
    }
    if trigger.contains(query_lower) || verify.contains(query_lower) {
        score += 4;
    }
    if steps.contains(query_lower) {
        score += 2;
    }
    for term in terms {
        if title.contains(term.as_str()) {
            score += 3;
        }
        if trigger.contains(term.as_str()) {
            score += 2;
        }
        if verify.contains(term.as_str()) {
            score += 2;
        }
        if steps.contains(term.as_str()) {
            score += 1;
        }
    }
    score
}

/// Select `active` procedures strictly for `repo:<repo_id>`, ranked by lexical relevance
/// when `query` is non-empty (else `updated_at DESC`), and strictly bounded by per-model
/// `max_procedures` and `budget_tokens`. Never ranks blindly by `support_count`.
pub fn select_skill_dose(
    ctx: &Ctx,
    repo_id: &str,
    query: &str,
    model_ref: Option<&str>,
) -> Result<Value, String> {
    let repo_id = repo_id.trim();
    if repo_id.is_empty() {
        return Err("repo_id is required".into());
    }
    let scope = format!("repo:{repo_id}");
    let dose = skill_dose_get(ctx, model_ref);
    let max_procedures = dose["max_procedures"].as_u64().unwrap_or(5) as usize;
    let budget_tokens = dose["budget_tokens"].as_u64().unwrap_or(750) as usize;

    let rows = ctx.db.query(
        "SELECT id, scope, version, title, steps, verify_cmd, trigger_hint, support_count,
                verified_run_ids, source_event_ids, status, created_at, updated_at
         FROM learned_procedures
         WHERE scope = ?1 AND status = 'active'
         ORDER BY updated_at DESC, id ASC",
        [&scope],
    )?;

    let mut candidates: Vec<(u32, String, String, Value)> = {
        let q_trimmed = query.trim().to_ascii_lowercase();
        let terms = tokenize_query(&q_trimmed);
        rows.into_iter()
            .map(decode_procedure_row)
            .map(|p| {
                let score = if q_trimmed.is_empty() {
                    0
                } else {
                    lexical_score_procedure(&p, &q_trimmed, &terms)
                };
                let updated_at = p["updated_at"].as_str().unwrap_or("").to_string();
                let id = p["id"].as_str().unwrap_or("").to_string();
                (score, updated_at, id, p)
            })
            .collect()
    };

    if !query.trim().is_empty() {
        // Relevance first, then recency tie-break (explicitly NOT support_count).
        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
    }

    let mut selected = Vec::new();
    let mut excluded = Vec::new();
    let mut used_tokens = 0usize;

    for (score, _, _, mut proc_val) in candidates {
        let text = format!(
            "{} {} {}",
            proc_val["title"].as_str().unwrap_or(""),
            proc_val["verify_cmd"].as_str().unwrap_or(""),
            proc_val["steps"].as_str().unwrap_or("")
        );
        let cost = estimate_tokens(&text);

        if selected.len() >= max_procedures {
            excluded.push(json!({
                "id": proc_val["id"],
                "reason": "max_procedures_cap",
                "tokens": cost,
            }));
            continue;
        }
        if used_tokens + cost > budget_tokens {
            excluded.push(json!({
                "id": proc_val["id"],
                "reason": "budget_exceeded",
                "tokens": cost,
            }));
            continue;
        }

        used_tokens += cost;
        proc_val["tokens"] = json!(cost);
        if !query.trim().is_empty() {
            proc_val["lexical_score"] = json!(score);
        }
        selected.push(proc_val);
    }

    Ok(json!({
        "scope": scope,
        "model_ref": dose["model_ref"],
        "max_procedures": max_procedures,
        "budget_tokens": budget_tokens,
        "used_tokens": used_tokens,
        "measured_score": dose["measured_score"],
        "source": dose["source"],
        "procedures": selected,
        "excluded": excluded,
    }))
}

/// Format a selected skill dose as a concise, UTF-8-safely-truncated Markdown block
/// carrying version, support run count, and verification command provenance tags.
pub fn format_skill_dose_prompt(dose: &Value, max_bytes: usize) -> Option<String> {
    if max_bytes == 0 {
        return None;
    }
    let procedures = dose["procedures"].as_array()?;
    if procedures.is_empty() {
        return None;
    }
    let mut out = String::from("\n## Verified procedures (skill dose)\n");
    for p in procedures {
        let title = p["title"].as_str().unwrap_or("").trim();
        let steps = p["steps"]
            .as_str()
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        if title.is_empty() && steps.is_empty() {
            continue;
        }
        let version = p["version"].as_i64().unwrap_or(1);
        let runs = p["support_count"].as_i64().unwrap_or(1);
        let verify = p["verify_cmd"].as_str().unwrap_or("").trim();
        let tag = if verify.is_empty() {
            format!("v{version} | runs={runs} | verify=none")
        } else {
            format!("v{version} | runs={runs} | verify={verify}")
        };
        if steps.is_empty() {
            out.push_str(&format!("- **{title}** [{tag}]\n"));
        } else {
            out.push_str(&format!("- **{title}** [{tag}]: {steps}\n"));
        }
    }
    if out == "\n## Verified procedures (skill dose)\n" {
        return None;
    }
    Some(truncate_utf8(&out, max_bytes))
}

// ---------- Unit Tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::Arc;

    struct TestEnv {
        ctx: Arc<Ctx>,
    }

    impl TestEnv {
        fn new() -> Self {
            let ctx = Ctx::test();
            init(&ctx).unwrap();
            Self { ctx }
        }

        fn add_repo(&self, id: &str, name: &str, path: &str) {
            self.ctx
                .db
                .exec(
                    "INSERT INTO repos(id, name, path, added_at) VALUES(?1, ?2, ?3, 1)",
                    params![id, name, path],
                )
                .unwrap();
        }

        fn add_agent(&self, id: &str, name: &str, repo_id: &str, strict: bool, session: &str) {
            self.ctx
                .db
                .exec(
                    "INSERT INTO agents(id, name, model_ref, state, repo_id, strict, hal_session_id, created_at, last_active_at)
                     VALUES(?1, ?2, 'p/med', 'idle', ?3, ?4, ?5, 1, 10)",
                    params![id, name, repo_id, if strict { 1i64 } else { 0i64 }, session],
                )
                .unwrap();
        }

        fn write_hal_events(&self, agent_name: &str, session_id: &str, events: &[Value]) {
            let path = self.ctx.agent_dir(agent_name).join("hal/hal/hal.db");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS events(session_id TEXT, seq INTEGER, ts TEXT, type TEXT, payload TEXT, PRIMARY KEY(session_id, seq));",
            )
            .unwrap();
            db.execute(
                "INSERT OR IGNORE INTO sessions(id) VALUES(?1)",
                [session_id],
            )
            .unwrap();
            for ev in events {
                let seq = ev["seq"].as_i64().unwrap();
                let ty = ev["type"].as_str().unwrap();
                let ts = ev["ts"].as_str().unwrap();
                db.execute(
                    "INSERT INTO events(session_id, seq, ts, type, payload) VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![session_id, seq, ts, ty, ev.to_string()],
                )
                .unwrap();
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.ctx.data_dir.parent().unwrap());
        }
    }

    #[test]
    fn mines_only_passing_standard_agent_runs_and_isolates_strict_and_sibling_repos() {
        let env = TestEnv::new();
        env.add_repo("r1", "alpha", "/tmp/alpha");
        env.add_repo("r2", "beta", "/tmp/beta");

        // Standard agent in r1: one passing `cargo test` run and one failing `cargo test` run.
        env.add_agent("a-std", "otter", "r1", false, "s-std");
        env.write_hal_events(
            "otter",
            "s-std",
            &[
                json!({"seq":1,"session_id":"s-std","ts":"2026-11-22T00:00:01Z","type":"tool.started","call_id":"c0","tool":"edit","arguments":{"path":"src/lib.rs"}}),
                json!({"seq":2,"session_id":"s-std","ts":"2026-11-22T00:00:02Z","type":"tool.finished","call_id":"c0","ok":true,"result":"updated src/lib.rs"}),
                json!({"seq":3,"session_id":"s-std","ts":"2026-11-22T00:00:03Z","type":"tool.started","call_id":"c1","tool":"shell_run","arguments":{"command":"cargo test memory_skills::"}}),
                json!({"seq":4,"session_id":"s-std","ts":"2026-11-22T00:00:04Z","type":"tool.finished","call_id":"c1","ok":true,"result":"test result: ok. 5 passed; 0 failed"}),
                json!({"seq":5,"session_id":"s-std","ts":"2026-11-22T00:00:05Z","type":"tool.started","call_id":"c2","tool":"shell_run","arguments":{"command":"cargo test broken_suite"}}),
                json!({"seq":6,"session_id":"s-std","ts":"2026-11-22T00:00:06Z","type":"tool.finished","call_id":"c2","ok":false,"result":"test result: FAILED. 1 failed"}),
                // Tool ok:true but output reports non-zero exit_code must also never be mined.
                json!({"seq":7,"session_id":"s-std","ts":"2026-11-22T00:00:07Z","type":"tool.started","call_id":"c3","tool":"shell_run","arguments":{"command":"pytest tests/bad.py"}}),
                json!({"seq":8,"session_id":"s-std","ts":"2026-11-22T00:00:08Z","type":"tool.finished","call_id":"c3","ok":true,"result":"exit_code: 1 failed"}),
            ],
        );

        // Strict agent in r1 with a passing `npm test` — MUST NEVER be mined.
        env.add_agent("a-strict", "lynx", "r1", true, "s-strict");
        env.write_hal_events(
            "lynx",
            "s-strict",
            &[
                json!({"seq":1,"session_id":"s-strict","ts":"2026-11-22T00:00:01Z","type":"tool.started","call_id":"cs1","tool":"shell_run","arguments":{"command":"npm test"}}),
                json!({"seq":2,"session_id":"s-strict","ts":"2026-11-22T00:00:02Z","type":"tool.finished","call_id":"cs1","ok":true,"result":"all passed"}),
            ],
        );

        // Standard agent in sibling repo r2 with passing `go test ./...` — MUST NEVER leak into r1.
        env.add_agent("a-r2", "falcon", "r2", false, "s-r2");
        env.write_hal_events(
            "falcon",
            "s-r2",
            &[
                json!({"seq":1,"session_id":"s-r2","ts":"2026-11-22T00:00:01Z","type":"tool.started","call_id":"cr2","tool":"shell_run","arguments":{"command":"go test ./..."}}),
                json!({"seq":2,"session_id":"s-r2","ts":"2026-11-22T00:00:02Z","type":"tool.finished","call_id":"cr2","ok":true,"result":"PASS"}),
            ],
        );

        let res = mine_verified_runs(&env.ctx, "r1").unwrap();
        assert_eq!(res["mined"], 1);
        assert_eq!(res["updated"], 0);
        let procs = res["procedures"].as_array().unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["scope"], "repo:r1");
        assert_eq!(procs[0]["status"], "candidate");
        assert_eq!(procs[0]["verify_cmd"], "cargo test memory_skills::");
        assert_eq!(procs[0]["support_count"], 1);
        assert!(procs[0]["steps"].as_str().unwrap().contains("src/lib.rs"));
        assert_eq!(procs[0]["verified_run_ids"], json!(["hal:a-std:s-std:4"]));
        assert_eq!(procs[0]["source_event_ids"], json!(["hal:a-std:s-std:3-4"]));

        // Re-running mine_verified_runs on the same runs is idempotent.
        let again = mine_verified_runs(&env.ctx, "r1").unwrap();
        assert_eq!(again["mined"], 0);
        assert_eq!(again["updated"], 0);
        assert_eq!(again["procedures"].as_array().unwrap().len(), 0);

        // Confirm neither Strict agent nor sibling repo leaked.
        let all_r1 = list_procedures(&env.ctx, Some("repo:r1"), None, 50).unwrap();
        let all_arr = all_r1.as_array().unwrap();
        assert_eq!(all_arr.len(), 1);
        assert!(!all_arr.iter().any(|p| p["verify_cmd"] == "npm test"));
        assert!(!all_arr.iter().any(|p| p["verify_cmd"] == "go test ./..."));
    }

    #[test]
    fn mines_verified_done_tasks_with_passing_review_and_skips_failed_or_unverified() {
        let env = TestEnv::new();
        env.add_repo("r1", "alpha", "/tmp/alpha");
        // Add board review columns as board::init does.
        env.ctx
            .db
            .exec("ALTER TABLE tasks ADD COLUMN review_result TEXT", [])
            .unwrap();
        env.ctx
            .db
            .exec("ALTER TABLE tasks ADD COLUMN review_evidence TEXT", [])
            .unwrap();

        env.ctx
            .db
            .exec(
                "INSERT INTO tasks(id, title, detail, status, repo_id, review_result, review_evidence, created_at, updated_at)
                 VALUES
                 ('t-pass', 'Verify SQLite FTS migration', 'Run migration test suite', 'done', 'r1', 'pass', 'Ran `cargo test db::` on HEAD; 6 passed', 1, 2),
                 ('t-fail', 'Broken release check', 'Check endpoint', 'done', 'r1', 'fail', 'Ran `cargo test broken`; failed', 1, 2),
                 ('t-unverified', 'Unverified guess task', 'Maybe works', 'done', 'r1', NULL, NULL, 1, 2)",
                [],
            )
            .unwrap();

        let res = mine_verified_runs(&env.ctx, "r1").unwrap();
        assert_eq!(res["mined"], 1);
        let procs = res["procedures"].as_array().unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["title"], "Verify SQLite FTS migration");
        assert_eq!(procs[0]["verify_cmd"], "cargo test db::");
        assert_eq!(procs[0]["verified_run_ids"], json!(["task:t-pass"]));
        assert_eq!(procs[0]["source_event_ids"], json!(["task_review:t-pass"]));
    }

    #[test]
    fn lossless_support_accumulation_promote_and_rollback_preserve_audit_history() {
        let env = TestEnv::new();

        let p1 = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Run Rust unit tests",
            "1. Run `cargo test`",
            "cargo test",
            "rust test",
            "run-1",
            Some("evt:10"),
        )
        .unwrap();
        assert_eq!(p1["status"], "candidate");
        assert_eq!(p1["version"], 1);
        assert_eq!(p1["support_count"], 1);
        assert_eq!(p1["verified_run_ids"], json!(["run-1"]));
        assert_eq!(p1["source_event_ids"], json!(["evt:10"]));

        // Deduplicate by normalized verify_cmd with improved steps while still candidate -> bumps version & appends provenance.
        let p2 = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Run Rust unit tests cleanly",
            "1. Build crate\n2. Run `cargo test` and verify 0 failures",
            "  CARGO   TEST ",
            "rust test",
            "run-2",
            Some("evt:20"),
        )
        .unwrap();
        assert_eq!(p2["id"], p1["id"]);
        assert_eq!(p2["version"], 2);
        assert_eq!(p2["support_count"], 2);
        assert_eq!(p2["verified_run_ids"], json!(["run-1", "run-2"]));
        assert_eq!(p2["source_event_ids"], json!(["evt:10", "evt:20"]));

        // Promote to active.
        let promoted = promote_procedure(&env.ctx, p1["id"].as_str().unwrap()).unwrap();
        assert_eq!(promoted["status"], "active");

        // A new run with identical steps appends provenance cleanly to the active record without overwriting steps.
        let p3 = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Run Rust unit tests",
            "1. Build crate\n2. Run `cargo test` and verify 0 failures",
            "cargo test",
            "rust test",
            "run-3",
            Some("evt:30"),
        )
        .unwrap();
        assert_eq!(p3["id"], promoted["id"]);
        assert_eq!(p3["status"], "active");
        assert_eq!(p3["version"], 2);
        assert_eq!(p3["support_count"], 3);
        assert_eq!(p3["verified_run_ids"], json!(["run-1", "run-2", "run-3"]));
        assert_eq!(p3["source_event_ids"], json!(["evt:10", "evt:20", "evt:30"]));

        // A new run with improved steps NEVER silently overwrites the operator-promoted active procedure:
        // it creates a new versioned candidate with full lossless provenance.
        let p4 = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Run Rust unit tests",
            "1. Format and lint\n2. Build crate\n3. Run `cargo test` and verify 0 failures",
            "cargo test",
            "rust test",
            "run-4",
            Some("evt:40"),
        )
        .unwrap();
        assert_ne!(p4["id"], promoted["id"]);
        assert_eq!(p4["status"], "candidate");
        assert_eq!(p4["version"], 3);
        assert_eq!(p4["support_count"], 4);
        assert_eq!(
            p4["verified_run_ids"],
            json!(["run-1", "run-2", "run-3", "run-4"])
        );
        assert_eq!(
            p4["source_event_ids"],
            json!(["evt:10", "evt:20", "evt:30", "evt:40"])
        );

        // Original active procedure remains untouched until operator explicitly rolls it back or promotes p4.
        let still_active = get_procedure_row(&env.ctx, promoted["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(still_active["status"], "active");
        assert_eq!(still_active["version"], 2);

        // Rollback preserves audit history cleanly.
        let rolled = rollback_procedure(&env.ctx, promoted["id"].as_str().unwrap()).unwrap();
        assert_eq!(rolled["status"], "rolled_back");
        assert_eq!(rolled["verified_run_ids"], json!(["run-1", "run-2", "run-3"]));
        assert_eq!(rolled["source_event_ids"], json!(["evt:10", "evt:20", "evt:30"]));
        assert!(promote_procedure(&env.ctx, rolled["id"].as_str().unwrap()).is_err());

        // Verify audit events were logged.
        let evs = env
            .ctx
            .db
            .query(
                "SELECT kind FROM events WHERE kind LIKE 'memory_procedure_%' ORDER BY id",
                [],
            )
            .unwrap();
        let kinds: Vec<&str> = evs.iter().filter_map(|r| r["kind"].as_str()).collect();
        assert!(kinds.contains(&"memory_procedure_promoted"));
        assert!(kinds.contains(&"memory_procedure_rolled_back"));
    }

    #[test]
    fn per_model_skill_dose_scales_by_context_window_override_and_formats_bounded_prompt() {
        let env = TestEnv::new();
        env.ctx
            .db
            .exec(
                "INSERT INTO providers(id, name, kind, created_at) VALUES('p', 'Prov', 'openai_chat', 0)",
                [],
            )
            .unwrap();
        env.ctx
            .db
            .exec(
                "INSERT INTO models(provider_id, model_id, context_window, discovered_at)
                 VALUES('p', 'small', 16384, 0), ('p', 'med', 65536, 0), ('p', 'large', 200000, 0)",
                [],
            )
            .unwrap();

        let small = skill_dose_get(&env.ctx, Some("p/small"));
        assert_eq!(small["max_procedures"], 3);
        assert_eq!(small["budget_tokens"], 450);

        let med = skill_dose_get(&env.ctx, Some("p/med"));
        assert_eq!(med["max_procedures"], 6);
        assert_eq!(med["budget_tokens"], 900);

        let large = skill_dose_get(&env.ctx, Some("p/large"));
        assert_eq!(large["max_procedures"], 10);
        assert_eq!(large["budget_tokens"], 1500);

        let unknown = skill_dose_get(&env.ctx, Some("p/unknown"));
        assert_eq!(unknown["max_procedures"], 5);
        assert_eq!(unknown["budget_tokens"], 750);

        // Explicit measured profile overrides default context_window heuristic.
        let saved = skill_dose_set(&env.ctx, "p/small", 2, Some(300), Some(0.84)).unwrap();
        assert_eq!(saved["max_procedures"], 2);
        assert_eq!(saved["budget_tokens"], 300);
        assert_eq!(saved["measured_score"], 0.84);
        assert_eq!(saved["source"], "profile");

        // Seed 6 active procedures in repo:r1, one with very high support_count (frequent)
        // and one matching the query "sqlite migration" with support_count=1.
        for i in 0..5 {
            let p = record_verified_procedure(
                &env.ctx,
                "repo:r1",
                &format!("General frontend check {i}"),
                &format!("1. Run `npm run check-{i}`"),
                &format!("npm run check-{i}"),
                "frontend ui lint",
                &format!("run-fe-{i}"),
                None,
            )
            .unwrap();
            let pid = p["id"].as_str().unwrap();
            if i == 0 {
                // Artificially give #0 high frequency to prove we do NOT rank blindly by frequency.
                env.ctx
                    .db
                    .exec(
                        "UPDATE learned_procedures SET support_count = 99 WHERE id = ?1",
                        [pid],
                    )
                    .unwrap();
            }
            promote_procedure(&env.ctx, pid).unwrap();
        }
        let target = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Verify SQLite migration integrity",
            "1. Run `cargo test db::migration` and verify schema.",
            "cargo test db::migration",
            "sqlite migration schema",
            "run-db-1",
            Some("evt:99"),
        )
        .unwrap();
        promote_procedure(&env.ctx, target["id"].as_str().unwrap()).unwrap();

        // Also create an unpromoted candidate and a sibling repo active procedure — neither may be selected.
        let _cand = record_verified_procedure(
            &env.ctx,
            "repo:r1",
            "Unpromoted SQLite migration draft",
            "1. Draft",
            "cargo test draft",
            "sqlite migration",
            "run-draft",
            None,
        )
        .unwrap();
        let sib = record_verified_procedure(
            &env.ctx,
            "repo:r2",
            "Sibling SQLite migration",
            "1. Sibling",
            "cargo test sib",
            "sqlite migration",
            "run-sib",
            None,
        )
        .unwrap();
        promote_procedure(&env.ctx, sib["id"].as_str().unwrap()).unwrap();

        // Small model with custom override max_procedures=2 receives at most 2 procedures,
        // and the lexically matching sqlite procedure ranks #1 ahead of support_count=99!
        let dose_small =
            select_skill_dose(&env.ctx, "r1", "sqlite migration", Some("p/small")).unwrap();
        let small_procs = dose_small["procedures"].as_array().unwrap();
        assert_eq!(small_procs.len(), 2);
        assert_eq!(small_procs[0]["title"], "Verify SQLite migration integrity");
        assert!(small_procs.iter().all(|p| p["scope"] == "repo:r1" && p["status"] == "active"));

        // Large model gets all 6 active procedures (up to max_procedures=10).
        let dose_large =
            select_skill_dose(&env.ctx, "r1", "sqlite migration", Some("p/large")).unwrap();
        assert_eq!(dose_large["procedures"].as_array().unwrap().len(), 6);

        // Prompt formatting includes provenance tags and safely truncates UTF-8 to max_bytes.
        let full_prompt = format_skill_dose_prompt(&dose_small, 2048).unwrap();
        assert!(full_prompt.contains("## Verified procedures (skill dose)"));
        assert!(full_prompt.contains("[v1 | runs=1 | verify=cargo test db::migration]"));

        let tiny_prompt = format_skill_dose_prompt(&dose_small, 90).unwrap();
        assert!(tiny_prompt.len() <= 90);
        assert!(tiny_prompt.contains("... [truncated]"));

        // Multibyte UTF-8 truncation never panics or exceeds max_bytes.
        let utf8_dose = json!({
            "procedures": [{
                "version": 1,
                "support_count": 2,
                "title": "ÅÄÖ verifiering 🚀",
                "verify_cmd": "cargo test",
                "steps": "é".repeat(200),
            }]
        });
        let bounded = format_skill_dose_prompt(&utf8_dose, 75).unwrap();
        assert!(bounded.len() <= 75);
        assert!(bounded.is_char_boundary(bounded.len()));

        // Empty query orders by updated_at DESC and strictly enforces budget_tokens even when max_procedures is higher.
        skill_dose_set(&env.ctx, "p/tight", 10, Some(30), None).unwrap();
        let tight = select_skill_dose(&env.ctx, "r1", "", Some("p/tight")).unwrap();
        assert!(tight["used_tokens"].as_u64().unwrap() <= 30);
        assert!(tight["procedures"].as_array().unwrap().len() <= 1);
        assert!(!tight["excluded"].as_array().unwrap().is_empty());
    }

    #[test]
    fn mines_submitted_tasks_with_board_events_and_skips_strict_task_owners() {
        let env = TestEnv::new();
        env.add_repo("r1", "alpha", "/tmp/alpha");
        env.add_agent("a-strict", "lynx", "r1", true, "s-strict");
        env.ctx
            .db
            .exec("ALTER TABLE tasks ADD COLUMN review_result TEXT", [])
            .unwrap();
        env.ctx
            .db
            .exec("ALTER TABLE tasks ADD COLUMN review_evidence TEXT", [])
            .unwrap();
        env.ctx
            .db
            .exec(
                "CREATE TABLE IF NOT EXISTS task_events(id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL)",
                [],
            )
            .unwrap();

        env.ctx
            .db
            .exec(
                "INSERT INTO tasks(id, title, detail, status, owner_agent_id, repo_id, review_result, review_evidence, created_at, updated_at)
                 VALUES
                 ('t-sub', 'Verify frontend production bundle', 'Build dist bundle cleanly', 'submitted', NULL, 'r1', NULL, NULL, 1, 2),
                 ('t-strict', 'Strict agent task', 'Strict check', 'done', 'a-strict', 'r1', 'pass', 'Ran `npm test` on HEAD; ok', 1, 2)",
                [],
            )
            .unwrap();
        env.ctx
            .db
            .exec(
                "INSERT INTO task_events(task_id, actor, kind, body, created_at)
                 VALUES('t-sub', 'you', 'submitted', '{\"summary\":\"Verified with `npm run build` and zero warnings\"}', 2)",
                [],
            )
            .unwrap();

        let res = mine_verified_runs(&env.ctx, "r1").unwrap();
        assert_eq!(res["mined"], 1);
        let procs = res["procedures"].as_array().unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["title"], "Verify frontend production bundle");
        assert_eq!(procs[0]["verify_cmd"], "npm run build");
        assert_eq!(procs[0]["verified_run_ids"], json!(["task:t-sub"]));
        assert_eq!(procs[0]["source_event_ids"], json!(["task_event:1"]));
    }
}
