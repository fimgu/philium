//! Native skills store. Files on disk are the source of truth:
//! - global: `~/Philium/skills/<name>/SKILL.md`
//! - per repo: `<repo>/.philium/skills/<name>/SKILL.md`
//! Every scan mirrors them into the `skills` table so agents' `skill_list` / `skill_get` see them.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ctx::{arg_str, Ctx};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

pub fn global_dir() -> PathBuf {
    home().join("Philium/skills")
}

fn repo_dir(ctx: &Ctx, repo_id: &str) -> Result<PathBuf, String> {
    let rows = ctx.db.query("SELECT path FROM repos WHERE id=?1", [repo_id])?;
    let p = rows.first().and_then(|r| r["path"].as_str()).ok_or("unknown repo")?;
    Ok(Path::new(p).join(".philium/skills"))
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.starts_with(['-', '.']) && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
}

/// (name, description) from SKILL.md front matter.
fn front_matter(body: &str) -> (Option<String>, String) {
    let (mut name, mut desc) = (None, String::new());
    let mut lines = body.lines();
    if lines.next().map(str::trim) == Some("---") {
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
    (name, desc)
}

fn scan(dir: &Path, scope: &str) -> Vec<Value> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        let Some(folder) = e.file_name().to_str().map(str::to_string) else { continue };
        if !valid_name(&folder) || std::fs::symlink_metadata(&p).map(|m| !m.is_dir()).unwrap_or(true) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(p.join("SKILL.md")) else { continue };
        let (_, desc) = front_matter(&body);
        out.push(json!({ "name": folder, "description": desc, "body": body, "scope": scope, "path": p.join("SKILL.md") }));
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

fn collect(ctx: &Ctx, repo_id: Option<&str>) -> Vec<Value> {
    let mut all = scan(&global_dir(), "global");
    if let Some(r) = repo_id {
        if let Ok(d) = repo_dir(ctx, r) {
            all.extend(scan(&d, &format!("repo:{r}")));
        }
    }
    all
}

/// Mirror disk skills into the agent-facing table. Repo skills win over global on name clash.
fn sync(ctx: &Ctx, skills: &[Value]) -> Result<(), String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    ctx.db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("DELETE FROM skills WHERE source='philium' OR source LIKE 'philium-repo:%'", [])?;
        for s in skills {
            let source = match s["scope"].as_str() {
                Some("global") => "philium".to_string(),
                Some(o) => format!("philium-{o}"),
                None => continue,
            };
            tx.execute(
                "INSERT INTO skills(name,description,body,files,source,updated_at) VALUES(?1,?2,?3,'{}',?4,?5)
                 ON CONFLICT(name) DO UPDATE SET description=excluded.description, body=excluded.body,
                 files=excluded.files, source=excluded.source, updated_at=excluded.updated_at",
                rusqlite::params![s["name"].as_str(), s["description"].as_str(), s["body"].as_str(), source, now],
            )?;
        }
        tx.commit()
    })?;
    Ok(())
}

fn target_dir(ctx: &Ctx, args: &Value) -> Result<PathBuf, String> {
    match args["scope"].as_str().unwrap_or("global") {
        "global" => Ok(global_dir()),
        s => repo_dir(ctx, s.strip_prefix("repo:").ok_or("scope must be global or repo:<id>")?),
    }
}

pub async fn call(ctx: &Ctx, m: &str, args: Value) -> Result<Value, String> {
    let repo = args["repo_id"].as_str();
    match m {
        "list" => {
            let all = collect(ctx, repo);
            sync(ctx, &all)?;
            Ok(json!({
                "skills": all,
                "global_dir": global_dir(),
            }))
        }
        "get" => {
            let name = arg_str(&args, "name")?;
            collect(ctx, repo).into_iter().rev().find(|s| s["name"] == name.as_str()).ok_or_else(|| format!("no skill named {name}"))
        }
        "save" => {
            let name = arg_str(&args, "name")?;
            if !valid_name(&name) {
                return Err("Name may use letters, digits, - and _ (max 64)".into());
            }
            let body = arg_str(&args, "body")?;
            let dir = target_dir(ctx, &args)?.join(&name);
            if args["create"].as_bool() == Some(true) && dir.exists() {
                return Err(format!("A skill named {name} already exists"));
            }
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("SKILL.md"), body).map_err(|e| e.to_string())?;
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        "delete" => {
            let name = arg_str(&args, "name")?;
            if !valid_name(&name) {
                return Err("bad name".into());
            }
            let dir = target_dir(ctx, &args)?.join(&name);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            }
            sync(ctx, &collect(ctx, repo))?;
            ctx.notify("library");
            Ok(Value::Null)
        }
        _ => Err(format!("unknown skills.{m}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe() {
        assert!(valid_name("review-pr"));
        assert!(!valid_name("../x") && !valid_name("") && !valid_name(".hidden") && !valid_name("a b"));
    }
}
