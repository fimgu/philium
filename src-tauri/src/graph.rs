//! Code graph (Graphify integration with a built-in fallback).
//!
//! rpc methods (`graph.*`):
//! - `status {}` -> `{graphify: bool, how: "path"|"python"|null, fallback: true}`
//! - `build {repo_path}` -> `{source: "graphify"|"builtin", nodes, edges, truncated}` (also caches the result)
//! - `get {repo_path}` -> `{nodes:[{id,label,kind,group}], edges:[{source,target,kind}], source, truncated}`;
//!   builds with the fallback if nothing is cached.
//!
//! Fallback: file + import graph for JS/TS/Rust/Python by regex, capped at
//! `MAX_FILES` files and `MAX_EDGES` edges.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::Regex;
use serde_json::{json, Value};

use crate::ctx::{arg_str, Ctx};

const MAX_FILES: usize = 2000;
const MAX_EDGES: usize = 8000;
const MAX_BYTES: u64 = 512 * 1024;
const SKIP: &[&str] = &["node_modules", "target", ".git", "dist", "build", ".venv", "venv", "__pycache__", ".next", "vendor"];

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS graph_cache(repo TEXT PRIMARY KEY, ts INTEGER NOT NULL, body TEXT NOT NULL);"))
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extra = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    std::env::split_paths(&path).chain(extra).map(|d| d.join(bin)).find(|p| p.is_file())
}

fn detect() -> Option<(&'static str, PathBuf)> {
    if let Some(p) = which("graphify") {
        return Some(("path", p));
    }
    let py = which("python3")?;
    let ok = std::process::Command::new(&py)
        .args(["-c", "import graphify"])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(("python", py))
}

fn lang(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => Some("js"),
        "rs" => Some("rs"),
        "py" => Some("py"),
        _ => None,
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        if out.len() >= MAX_FILES {
            return;
        }
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if !SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&p, out);
            }
        } else if ft.is_file() && lang(&p).is_some() && e.metadata().map(|m| m.len() <= MAX_BYTES).unwrap_or(false) {
            out.push(p);
        }
    }
}

fn imports(l: &str, src: &str) -> Vec<String> {
    let res: &[&str] = match l {
        "js" => &[
            r#"(?m)\bimport\s+(?:[^'";]*?\sfrom\s+)?['"]([^'"]+)['"]"#,
            r#"\brequire\(\s*['"]([^'"]+)['"]\s*\)"#,
            r#"\bexport\s+[^'";]*?\sfrom\s+['"]([^'"]+)['"]"#,
        ],
        "rs" => &[r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", r"(?m)^\s*(?:pub\s+)?use\s+(?:crate|super|self)::(\w+)"],
        _ => &[r"(?m)^\s*from\s+([\w.]+)\s+import", r"(?m)^\s*import\s+([\w.]+)"],
    };
    let mut v = vec![];
    for r in res {
        let re = Regex::new(r).unwrap();
        for c in re.captures_iter(src) {
            v.push(c[1].to_string());
        }
    }
    v
}

fn resolve(l: &str, from: &Path, spec: &str, root: &Path, files: &HashSet<PathBuf>) -> Option<PathBuf> {
    let dir = from.parent()?;
    let mut cands: Vec<PathBuf> = vec![];
    match l {
        "js" => {
            if !spec.starts_with('.') {
                return None;
            }
            let base = dir.join(spec);
            cands.push(base.clone());
            for e in ["js", "jsx", "ts", "tsx", "mjs"] {
                cands.push(PathBuf::from(format!("{}.{e}", base.display())));
                cands.push(base.join(format!("index.{e}")));
            }
        }
        "rs" => {
            cands.push(dir.join(format!("{spec}.rs")));
            cands.push(dir.join(spec).join("mod.rs"));
            cands.push(root.join("src").join(format!("{spec}.rs")));
            cands.push(from.parent()?.join(from.file_stem()?).join(format!("{spec}.rs")));
        }
        _ => {
            let rel = spec.trim_start_matches('.').replace('.', "/");
            for b in [dir.to_path_buf(), root.to_path_buf(), root.join("src")] {
                cands.push(b.join(format!("{rel}.py")));
                cands.push(b.join(&rel).join("__init__.py"));
            }
        }
    }
    cands.into_iter().map(|c| normalize(&c)).find(|c| files.contains(c) && c != from)
}

fn normalize(p: &Path) -> PathBuf {
    let mut o = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                o.pop();
            }
            std::path::Component::CurDir => {}
            c => o.push(c),
        }
    }
    o
}

pub fn build_fallback(repo: &Path) -> Value {
    let repo = normalize(repo);
    let mut files = vec![];
    walk(&repo, &mut files);
    let truncated_files = files.len() >= MAX_FILES;
    let set: HashSet<PathBuf> = files.iter().cloned().collect();
    let rel = |p: &Path| p.strip_prefix(&repo).unwrap_or(p).to_string_lossy().into_owned();
    let mut nodes = vec![];
    let mut edges = vec![];
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut truncated_edges = false;
    for f in &files {
        let l = lang(f).unwrap();
        let id = rel(f);
        let group = id.split('/').next().filter(|_| id.contains('/')).unwrap_or(".").to_string();
        nodes.push(json!({"id": id, "label": f.file_name().unwrap().to_string_lossy(), "kind": l, "group": group}));
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        for spec in imports(l, &src) {
            if let Some(t) = resolve(l, f, &spec, &repo, &set) {
                let key = format!("{}>{}", id, rel(&t));
                if seen.insert(key, ()).is_none() {
                    if edges.len() >= MAX_EDGES {
                        truncated_edges = true;
                        break;
                    }
                    edges.push(json!({"source": id, "target": rel(&t), "kind": "import"}));
                }
            }
        }
    }
    json!({"nodes": nodes, "edges": edges, "source": "builtin", "truncated": truncated_files || truncated_edges})
}

/// Run graphify and read its graph.json if one is produced; otherwise None.
fn run_graphify(repo: &Path, how: &str, bin: &Path) -> Result<Value, String> {
    let mut cmd = std::process::Command::new(bin);
    if how == "python" {
        cmd.args(["-m", "graphify"]);
    }
    let st = cmd.arg(repo).current_dir(repo).output().map_err(|e| e.to_string())?;
    if !st.status.success() {
        return Err(String::from_utf8_lossy(&st.stderr).chars().take(300).collect());
    }
    let cands = [repo.join("graphify-out/graph.json"), repo.join("graph.json"), repo.join(".graphify/graph.json")];
    let p = cands.iter().find(|p| p.is_file()).ok_or("graphify produced no graph.json")?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let nodes: Vec<Value> = v["nodes"]
        .as_array()
        .ok_or("graph.json has no nodes")?
        .iter()
        .map(|n| {
            let id = n["id"].as_str().map(str::to_string).unwrap_or_else(|| n["id"].to_string());
            json!({"id": id, "label": n["label"].as_str().or(n["name"].as_str()).unwrap_or(&id),
                   "kind": n["kind"].as_str().or(n["type"].as_str()).unwrap_or("node"), "group": n["group"].as_str().or(n["community"].as_str()).unwrap_or(".")})
        })
        .collect();
    let edges: Vec<Value> = v["edges"]
        .as_array()
        .or(v["links"].as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| json!({"source": e["source"], "target": e["target"], "kind": e["kind"].as_str().or(e["type"].as_str()).unwrap_or("edge")}))
        .collect();
    Ok(json!({"nodes": nodes, "edges": edges, "source": "graphify", "truncated": false}))
}

fn repo_arg(args: &Value) -> Result<PathBuf, String> {
    let p = PathBuf::from(arg_str(args, "repo_path")?);
    if !p.is_dir() {
        return Err(format!("not a directory: {}", p.display()));
    }
    std::fs::canonicalize(&p).map_err(|e| e.to_string())
}

fn build(ctx: &Ctx, repo: &Path) -> Result<Value, String> {
    let g = detect()
        .and_then(|(how, bin)| run_graphify(repo, how, &bin).ok())
        .unwrap_or_else(|| build_fallback(repo));
    ctx.db.exec(
        "INSERT OR REPLACE INTO graph_cache(repo,ts,body) VALUES(?1,?2,?3)",
        rusqlite::params![repo.to_string_lossy(), crate::db::now(), g.to_string()],
    )?;
    Ok(g)
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let d = detect();
            Ok(json!({"graphify": d.is_some(), "how": d.map(|d| d.0), "fallback": true}))
        }
        "build" => {
            let repo = repo_arg(&args)?;
            let c = ctx.clone();
            let g = tokio::task::spawn_blocking(move || build(&c, &repo)).await.map_err(|e| e.to_string())??;
            Ok(json!({"source": g["source"], "nodes": g["nodes"].as_array().map(Vec::len), "edges": g["edges"].as_array().map(Vec::len), "truncated": g["truncated"]}))
        }
        "get" => {
            let repo = repo_arg(&args)?;
            let rows = ctx.db.query("SELECT body FROM graph_cache WHERE repo=?1", [repo.to_string_lossy().to_string()])?;
            if let Some(b) = rows.first().and_then(|r| r["body"].as_str()) {
                return serde_json::from_str(b).map_err(|e| e.to_string());
            }
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || build(&c, &repo)).await.map_err(|e| e.to_string())?
        }
        _ => Err(format!("graph.{method}: unknown method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_graph() {
        let d = std::env::temp_dir().join(format!("g-{}", crate::db::new_id()));
        std::fs::create_dir_all(d.join("src/lib")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::write(d.join("src/a.js"), "import b from './lib/b.js';\nimport r from 'react';\nconst c = require('./c');").unwrap();
        std::fs::write(d.join("src/lib/b.js"), "export * from '../c';").unwrap();
        std::fs::write(d.join("src/c.ts"), "").unwrap();
        std::fs::write(d.join("node_modules/x/i.js"), "").unwrap();
        std::fs::write(d.join("src/main.rs"), "mod util;\nuse crate::util::x;").unwrap();
        std::fs::write(d.join("src/util.rs"), "").unwrap();
        std::fs::write(d.join("tool.py"), "from helper import f\nimport os").unwrap();
        std::fs::write(d.join("helper.py"), "").unwrap();
        let g = build_fallback(&d);
        let n = g["nodes"].as_array().unwrap();
        assert_eq!(n.len(), 7, "{n:?}");
        let e: Vec<String> = g["edges"].as_array().unwrap().iter().map(|e| format!("{}>{}", e["source"].as_str().unwrap(), e["target"].as_str().unwrap())).collect();
        for want in ["src/a.js>src/lib/b.js", "src/a.js>src/c.ts", "src/lib/b.js>src/c.ts", "src/main.rs>src/util.rs", "tool.py>helper.py"] {
            assert!(e.contains(&want.to_string()), "{want} in {e:?}");
        }
        assert_eq!(e.iter().filter(|x| *x == "src/main.rs>src/util.rs").count(), 1);
    }
}
