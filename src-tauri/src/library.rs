//! Philium's own skills and memory, so agents need nothing from Jcode or Claude.
//!
//! A one-time, read-only import copies `~/.jcode/memory`, `~/.jcode/skills`,
//! `~/.claude/skills` and installed Claude plugin skills into Philium's
//! database. After that Philium owns them: agents read and add through MCP
//! (`skill_list`, `skill_get`, `memory_search`, `memory_save`). Agents never get
//! file access to those folders (they stay on the sandbox private list).
//!
//! rpc: status {} -> counts, import {} -> re-import (idempotent),
//!      skills {} -> [skill], memories {q?, limit?} -> [memory], forget {id}
use crate::{ctx::Ctx, db::now};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_SKILL_FILE: u64 = 64 * 1024;
const MAX_SKILL_FILES: usize = 12;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS skills (
  name TEXT PRIMARY KEY, description TEXT NOT NULL, body TEXT NOT NULL,
  files TEXT NOT NULL DEFAULT '{}', source TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY, scope TEXT NOT NULL, category TEXT NOT NULL, content TEXT NOT NULL,
  source TEXT NOT NULL, created_at TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1);
CREATE INDEX IF NOT EXISTS memories_scope ON memories(scope);
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content, content='memories', content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content); END;
CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content); END;
CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
  INSERT INTO memories_fts(rowid, content) VALUES (new.rowid, new.content); END;
";

/// Standing rules every agent gets, adapted from the user's Jcode overlay to
/// Philium's tools. Editable via setting `agent_rules`.
pub const DEFAULT_RULES: &str = "\
## Filip's standing rules
- Keep replies short. Lead with the result. Separate what you verified from what you did not.
- Read lean: locate with code_map/code_outline/code_find, read exact symbol spans with code_symbol or bounded file ranges, and check call graphs with code_refs/code_impact. Filter noisy build/test output to failures.
- Delegate broad exploration to a helper (agent_spawn) and ask for a short summary with file:line references.
- Clean up everything you create before reporting done: containers (run with --rm; register others with resource_register), compose projects (`down -v --rmi local`), worktrees and branches you made, scratch files in $TMPDIR. Never leave uncommitted work silently: push it or say so. Never delete what you did not create. Your final reply states what was removed or kept and why.
- PR reviews and readiness checks: skill_get review-pr first and follow it. Test the real user workflow, not just unit tests. Never call a partial pass end-to-end verified.
- Before answering what a feature does, check the default branch and open PRs, not only the local checkout.
- Do not create markdown notes, handoff or README files unless asked.
- Use your skills: skill_list shows them, skill_get loads one. Use memory_search before asking Filip something he may have told you before, and memory_save for durable preferences or corrections he gives you.
";

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    if ctx.db.kv_get("library.imported_at").is_none() {
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = import(&c, &home());
            c.log_event("library:import", None, &r.unwrap_or_else(|e| json!({ "error": e })));
        });
    }
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Rules injected into every agent's instructions.
pub fn rules(ctx: &Ctx) -> String {
    ctx.db.kv_get("setting.agent_rules").unwrap_or_else(|| DEFAULT_RULES.to_string())
}

/// Same hash Jcode uses for per-project memory files (std DefaultHasher over the path).
fn jcode_project_hash(p: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Candidate project paths so hashed Jcode project files map back to real repos.
fn known_projects(ctx: &Ctx, home: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ctx
        .db
        .query("SELECT path FROM repos", [])
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["path"].as_str().map(PathBuf::from))
        .collect();
    for base in ["Developer", "Documents", "Desktop", ""] {
        if let Ok(rd) = std::fs::read_dir(home.join(base)) {
            v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
    }
    v.push(home.to_path_buf());
    v
}

pub fn import(ctx: &Arc<Ctx>, home: &Path) -> Result<Value, String> {
    let mut skills = 0;
    let mut memories = 0;
    // Later sources win on name clashes: plugins < ~/.claude/skills < ~/.jcode/skills.
    let mut dirs: Vec<(PathBuf, &str)> = vec![];
    if let Ok(s) = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            for list in v["plugins"].as_object().into_iter().flat_map(|o| o.values()) {
                if let Some(p) = list[0]["installPath"].as_str() {
                    dirs.push((PathBuf::from(p).join("skills"), "claude-plugin"));
                }
            }
        }
    }
    dirs.push((home.join(".claude/skills"), "claude"));
    dirs.push((home.join(".jcode/skills"), "jcode"));
    for (dir, source) in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if let Some(s) = read_skill(&e.path(), source) {
                ctx.db.exec(
                    "INSERT INTO skills(name,description,body,files,source,updated_at) VALUES(?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(name) DO UPDATE SET description=excluded.description, body=excluded.body,
                     files=excluded.files, source=excluded.source, updated_at=excluded.updated_at",
                    params![s.0, s.1, s.2, s.3.to_string(), source, now()],
                )?;
                skills += 1;
            }
        }
    }

    let mem = home.join(".jcode/memory");
    let projects: std::collections::HashMap<String, String> = known_projects(ctx, home)
        .into_iter()
        .map(|p| (jcode_project_hash(&p), p.to_string_lossy().into_owned()))
        .collect();
    let mut files = vec![(mem.join("global.json"), "global".to_string())];
    if let Ok(rd) = std::fs::read_dir(mem.join("projects")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let hash = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let scope = projects.get(&hash).cloned().map(|p| format!("project:{p}")).unwrap_or(format!("jcode:{hash}"));
            files.push((p, scope));
        }
    }
    for (path, scope) in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(map) = v["memories"].as_object() else { continue };
        ctx.db.with(|c| {
            let tx = c.unchecked_transaction()?;
            for m in map.values() {
                let (Some(id), Some(content)) = (m["id"].as_str(), m["content"].as_str()) else { continue };
                let category = m["category"].as_str().unwrap_or("fact");
                let active = m["active"].as_bool().unwrap_or(true) as i64;
                tx.execute(
                    "INSERT INTO memories(id,scope,category,content,source,created_at,active) VALUES(?1,?2,?3,?4,'jcode',?5,?6)
                     ON CONFLICT(id) DO UPDATE SET content=excluded.content, category=excluded.category, active=excluded.active, scope=excluded.scope",
                    params![id, scope, category, content, m["created_at"].as_str().unwrap_or(""), active],
                )?;
                memories += 1;
            }
            tx.commit()
        })?;
    }
    ctx.db.kv_set("library.imported_at", &now().to_string())?;
    Ok(json!({ "skills": skills, "memories": memories }))
}

/// (name, description, body, small text supporting files) from a skill folder.
fn read_skill(dir: &Path, _source: &str) -> Option<(String, String, String, Value)> {
    if std::fs::symlink_metadata(dir).ok()?.file_type().is_symlink() {
        return None;
    }
    let body = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let (mut name, mut desc) = (None, String::new());
    let mut lines = body.lines();
    if lines.next() == Some("---") {
        for l in lines {
            if l.trim() == "---" {
                break;
            }
            if let Some(v) = l.strip_prefix("name:") {
                name = Some(v.trim().trim_matches('"').to_string());
            }
            if let Some(v) = l.strip_prefix("description:") {
                desc = v.trim().trim_matches('"').to_string();
            }
        }
    }
    let name = name.or_else(|| dir.file_name()?.to_str().map(str::to_string))?;
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_ ".contains(c)) {
        return None;
    }
    let mut files = serde_json::Map::new();
    for e in std::fs::read_dir(dir).ok()?.flatten().take(64) {
        let p = e.path();
        let fname = e.file_name().to_string_lossy().into_owned();
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if fname == "SKILL.md" || !meta.is_file() || meta.len() > MAX_SKILL_FILE || files.len() >= MAX_SKILL_FILES {
            continue;
        }
        if let Ok(t) = std::fs::read_to_string(&p) {
            files.insert(fname, json!(t));
        }
    }
    Some((name.trim().to_string(), desc, body, Value::Object(files)))
}

pub fn skill_list(ctx: &Ctx) -> Result<Value, String> {
    Ok(json!(ctx.db.query("SELECT name,description FROM skills ORDER BY name", [])?))
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

/// FTS search over active memories in global scope plus the given project.
pub fn memory_search(ctx: &Ctx, q: &str, project: Option<&str>, limit: i64) -> Result<Value, String> {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .take(12)
        .map(|t| format!("\"{t}\""))
        .collect();
    let scope = project.map(|p| format!("project:{p}")).unwrap_or_default();
    let limit = limit.clamp(1, 50);
    if terms.is_empty() {
        return Ok(json!(ctx.db.query(
            "SELECT id,scope,category,content FROM memories WHERE active=1 AND (scope='global' OR scope=?1) ORDER BY created_at DESC LIMIT ?2",
            params![scope, limit],
        )?));
    }
    Ok(json!(ctx.db.query(
        "SELECT m.id,m.scope,m.category,m.content FROM memories_fts f JOIN memories m ON m.rowid=f.rowid
         WHERE memories_fts MATCH ?1 AND m.active=1 AND (m.scope='global' OR m.scope=?2)
         ORDER BY (m.category='correction') DESC, bm25(memories_fts) LIMIT ?3",
        params![terms.join(" OR "), scope, limit],
    )?))
}

pub fn memory_save(ctx: &Ctx, content: &str, category: &str, project: Option<&str>, by: &str) -> Result<Value, String> {
    let content = content.trim();
    if content.is_empty() || content.len() > 4000 {
        return Err("memory must be 1-4000 characters".into());
    }
    if !matches!(category, "fact" | "preference" | "correction" | "entity") {
        return Err("category must be fact, preference, correction or entity".into());
    }
    let id = format!("mem_{}", crate::db::new_id());
    let scope = project.map(|p| format!("project:{p}")).unwrap_or("global".into());
    ctx.db.exec(
        "INSERT INTO memories(id,scope,category,content,source,created_at) VALUES(?1,?2,?3,?4,?5,datetime('now'))",
        params![id, scope, category, content, format!("agent:{by}")],
    )?;
    Ok(json!({ "id": id, "scope": scope }))
}

/// Global preferences and corrections, injected into every agent's instructions.
pub fn standing_memories(ctx: &Ctx) -> String {
    let rows = ctx
        .db
        .query(
            "SELECT content FROM memories WHERE scope='global' AND active=1 AND category IN ('preference','correction') ORDER BY created_at LIMIT 40",
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

pub fn session_search(
    ctx: &Ctx,
    query: &str,
    project: Option<&str>,
    repo_only: bool,
    limit: usize,
) -> Result<Value, String> {
    session_search_at(ctx, &home(), query, project, repo_only, limit)
}

fn strip_system_reminders(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</system-reminder>") {
            rest = &rest[start + end + "</system-reminder>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn matches_repo(working_dir: &str, project: Option<&str>) -> bool {
    let Some(proj) = project.filter(|p| !p.is_empty()) else {
        return false;
    };
    if working_dir.starts_with(proj) || proj.starts_with(working_dir) {
        return true;
    }
    let proj_name = Path::new(proj).file_name().and_then(|s| s.to_str()).unwrap_or("");
    !proj_name.is_empty()
        && (working_dir.ends_with(&format!("/{proj_name}"))
            || working_dir.contains(&format!("/{proj_name}/"))
            || working_dir.contains(&format!("/{proj_name}-")))
}

pub fn session_search_at(
    ctx: &Ctx,
    home_dir: &Path,
    query: &str,
    project: Option<&str>,
    repo_only: bool,
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

    // 1. Philium agents & HAL session transcripts
    let agents = ctx
        .db
        .query(
            "SELECT a.id, a.name, a.role, a.state, a.task_title, a.branch, a.hal_session_id, a.last_active_at, r.path AS repo_path \
             FROM agents a LEFT JOIN repos r ON r.id=a.repo_id ORDER BY a.last_active_at DESC LIMIT 60",
            [],
        )
        .unwrap_or_default();
    for a in agents {
        let rpath = a["repo_path"].as_str().unwrap_or("");
        let same_repo = matches_repo(rpath, project);
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
            let ev_path = ctx.root.join("hal/sessions").join(sid).join("events.jsonl");
            if let Ok(raw) = std::fs::read_to_string(&ev_path) {
                for line in raw.lines().take(200) {
                    let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                    let role = match v["t"].as_str() {
                        Some("user") => "user",
                        Some("assistant") => "assistant",
                        _ => continue,
                    };
                    let txt = v["text"].as_str().unwrap_or("").trim();
                    if txt.is_empty() {
                        continue;
                    }
                    let low = txt.to_lowercase();
                    let term_hits = terms.iter().filter(|t| low.contains(t.as_str())).count();
                    if terms.is_empty() || term_hits > 0 {
                        score += (term_hits as i64) * 10;
                        if excerpts.len() < 3 {
                            excerpts.push(json!({
                                "role": role,
                                "text": txt.chars().take(280).collect::<String>(),
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

    // 2. Jcode sessions (~/.jcode/sessions/*.json)
    let jcode_dir = home_dir.join(".jcode/sessions");
    if let Ok(rd) = std::fs::read_dir(&jcode_dir) {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_str()?;
                if !name.ends_with(".json") || name.contains("canary") {
                    return None;
                }
                let m = e.metadata().ok()?.modified().ok()?;
                Some((m, p))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, p) in files.into_iter().take(120) {
            let Ok(raw) = std::fs::read_to_string(&p) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
            if v["debug"].as_bool() == Some(true) || v["canary"].as_bool() == Some(true) {
                continue;
            }
            let wd = v["working_dir"].as_str().unwrap_or("");
            let same_repo = matches_repo(wd, project);
            if repo_only && project.is_some() && !same_repo {
                continue;
            }
            let short = v["short_name"].as_str().unwrap_or("");
            let title = v["title"].as_str().unwrap_or("");
            let sid = v["id"].as_str().unwrap_or("");
            let meta_low = format!("{short} {title} {wd} {sid}").to_lowercase();
            let mut score: i64 = if same_repo { 25 } else { 0 };
            for t in &terms {
                if meta_low.contains(t) {
                    score += 15;
                }
            }
            let mut excerpts = vec![];
            if let Some(msgs) = v["messages"].as_array() {
                for m in msgs.iter().take(120) {
                    let role = m["role"].as_str().unwrap_or("");
                    if role != "user" && role != "assistant" {
                        continue;
                    }
                    let Some(blocks) = m["content"].as_array() else { continue };
                    for b in blocks {
                        if b["type"].as_str() != Some("text") {
                            continue;
                        }
                        let clean = strip_system_reminders(b["text"].as_str().unwrap_or(""));
                        if clean.is_empty() {
                            continue;
                        }
                        let low = clean.to_lowercase();
                        let hits = terms.iter().filter(|t| low.contains(t.as_str())).count();
                        if terms.is_empty() || hits > 0 {
                            score += (hits as i64) * 12;
                            if excerpts.len() < 3 {
                                excerpts.push(json!({
                                    "role": role,
                                    "text": clean.chars().take(280).collect::<String>(),
                                }));
                            }
                        }
                    }
                }
            }
            if (terms.is_empty() && !excerpts.is_empty()) || score > if same_repo { 25 } else { 0 } {
                candidates.push((
                    score,
                    json!({
                        "source": "jcode",
                        "session_id": sid,
                        "name": short,
                        "title": title,
                        "working_dir": wd,
                        "updated_at": v["updated_at"],
                        "same_repo": same_repo,
                        "excerpts": excerpts,
                    }),
                ));
            }
        }
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(Value::Array(candidates.into_iter().take(limit).map(|(_, v)| v).collect()))
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => Ok(json!({
            "skills": ctx.db.query("SELECT count(*) AS n FROM skills", [])?[0]["n"],
            "memories": ctx.db.query("SELECT count(*) AS n FROM memories WHERE active=1", [])?[0]["n"],
            "imported_at": ctx.db.kv_get("library.imported_at"),
        })),
        "import" => {
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || import(&c, &home())).await.map_err(|e| e.to_string())?
        }
        "skills" => skill_list(ctx),
        "memories" => memory_search(ctx, args["q"].as_str().unwrap_or(""), None, args["limit"].as_i64().unwrap_or(50)),
        "sessions" => session_search(
            ctx,
            args["q"].as_str().unwrap_or(""),
            args["project"].as_str(),
            args["repo_only"].as_bool().unwrap_or(false),
            args["limit"].as_u64().unwrap_or(8) as usize,
        ),
        "forget" => {
            ctx.db.exec("UPDATE memories SET active=0 WHERE id=?1", [crate::ctx::arg_str(&args, "id")?])?;
            Ok(Value::Null)
        }
        _ => Err(format!("unknown library.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_home() -> PathBuf {
        let h = std::env::temp_dir().join(format!("philium-lib-{}", crate::db::new_id()));
        std::fs::create_dir_all(h.join(".jcode/skills/review-pr")).unwrap();
        std::fs::create_dir_all(h.join(".jcode/memory/projects")).unwrap();
        std::fs::create_dir_all(h.join("Developer/philium")).unwrap();
        std::fs::write(h.join(".jcode/skills/review-pr/SKILL.md"), "---\nname: review-pr\ndescription: Review PRs\n---\n# Steps\n1. test").unwrap();
        std::fs::write(h.join(".jcode/skills/review-pr/gate.sh"), "echo ok").unwrap();
        let g = json!({"memories": {
            "m1": {"id":"m1","category":"preference","content":"Never add Co-authored-by trailers","active":true,"created_at":"2026-01-01"},
            "m2": {"id":"m2","category":"fact","content":"old retired thing","active":false,"created_at":"2026-01-02"}}});
        std::fs::write(h.join(".jcode/memory/global.json"), g.to_string()).unwrap();
        let hash = jcode_project_hash(&h.join("Developer/philium"));
        let p = json!({"memories": {"p1": {"id":"p1","category":{"custom":"x"},"content":"Philium quit must finish under one second","created_at":"2026-01-03"}}});
        std::fs::write(h.join(format!(".jcode/memory/projects/{hash}.json")), p.to_string()).unwrap();
        std::fs::create_dir_all(h.join(".jcode/sessions")).unwrap();
        let sess1 = json!({
            "id": "session_dolphin_1",
            "short_name": "dolphin",
            "title": "Codegraph PageRank optimization",
            "working_dir": h.join("Developer/philium").to_string_lossy(),
            "updated_at": "2026-04-19T12:00:00Z",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "<system-reminder>noisy</system-reminder>Make code_outline token optimal"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "Added tree-sitter AST symbol ranges and bidirectional code_refs."}]}
            ]
        });
        std::fs::write(h.join(".jcode/sessions/session_dolphin_1.json"), sess1.to_string()).unwrap();
        h
    }

    /// Dry run against the real home, into an in-memory DB. Read-only on sources.
    #[test]
    #[ignore]
    fn real_home_dry_run() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let r = import(&ctx, &home()).unwrap();
        let scopes = ctx.db.query("SELECT substr(scope,1,8) AS s, count(*) AS n FROM memories GROUP BY 1", []).unwrap();
        let names: Vec<_> = skill_list(&ctx).unwrap().as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        println!("import {r}\nscopes {}\nskills {}", json!(scopes), names.join(" "));
        println!("search 'coauthor' -> {}", memory_search(&ctx, "co-authored trailers", None, 2).unwrap());
        println!("standing bytes {}", standing_memories(&ctx).len() + rules(&ctx).len());
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
    fn imports_skills_and_memories_and_serves_them() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let home = fake_home();
        let r = import(&ctx, &home).unwrap();
        assert_eq!(r, json!({"skills": 1, "memories": 3}));
        // Idempotent: a second import does not duplicate.
        import(&ctx, &home).unwrap();
        assert_eq!(ctx.db.query("SELECT count(*) AS n FROM memories", []).unwrap()[0]["n"], 3);

        let s = skill_get(&ctx, "review-pr").unwrap();
        assert!(s["body"].as_str().unwrap().contains("# Steps"));
        assert_eq!(s["files"]["gate.sh"], "echo ok");
        assert!(skill_get(&ctx, "nope").is_err());

        let proj = home.join("Developer/philium").to_string_lossy().into_owned();
        let hits = memory_search(&ctx, "quit second", Some(&proj), 10).unwrap();
        assert_eq!(hits[0]["id"], "p1", "{hits}");
        assert_eq!(hits[0]["scope"], format!("project:{proj}"));
        // Inactive memories never surface.
        assert!(memory_search(&ctx, "retired", None, 10).unwrap().as_array().unwrap().is_empty());
        // Search input cannot inject FTS syntax.
        memory_search(&ctx, "\"unbalanced * NEAR( OR", None, 5).unwrap();

        memory_save(&ctx, "Prefers Philium over Jcode", "preference", None, "tester").unwrap();
        let standing = standing_memories(&ctx);
        assert!(standing.contains("Co-authored-by") && standing.contains("Prefers Philium"), "{standing}");
        assert!(!standing.contains("retired"));
        assert!(memory_save(&ctx, "x", "bogus", None, "t").is_err());

        let shits = session_search_at(&ctx, &home, "code_outline bidirectional", Some(&proj), true, 5).unwrap();
        let arr = shits.as_array().unwrap();
        assert_eq!(arr.len(), 1, "{shits}");
        assert_eq!(arr[0]["source"], "jcode");
        assert_eq!(arr[0]["name"], "dolphin");
        assert_eq!(arr[0]["same_repo"], true);
        let ex_text = arr[0]["excerpts"][0]["text"].as_str().unwrap();
        assert!(ex_text.contains("Make code_outline token optimal") && !ex_text.contains("noisy"), "{ex_text}");

        let _ = std::fs::remove_dir_all(home);
    }
}
