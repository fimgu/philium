//! Structural code index (tree-sitter) for agents and the Code screen.
//!
//! Symbols (fn/method/class/struct/enum/trait/type/const/mod/interface) and references
//! (calls, type uses, imports, by name) are stored in SQLite. One *base snapshot* per repo
//! at the default branch commit (rebuilt only when that commit changes) plus a small *delta*
//! per worktree (files that differ from the base commit, plus untracked files). Delta rows
//! shadow base rows by path. Indexing is lazy (first query, plus a watcher for agent
//! worktrees), serialized by one lock, incremental by file stamp (mtime+len), and debounced.
//!
//! Target args (all methods): `worktree_id` | `agent` (id or name) | `repo_id`.
//! `repo_id` alone is the indexed base commit. Add `working_tree: true` for the
//! local checkout, represented by the stable virtual worktree id `local:{repo_id}`.
//! Query results carry `text`, compact and compact by design (`max_tokens` is only a safety limit, default 60000)
//! with "N more, narrow with ..." hints.
//!
//! rpc `codeindex.*`:
//! - `status {}` -> `{repo_id, base_commit, files, syms, refs, built_at, worktree?:{id,delta_files,indexed_at}}`
//! - `map {path?, depth?}` -> `{text}`
//! - `find {query, kind?}` -> `{text, items:[{path,line,kind,name,sig,parent}]}`
//! - `refs {symbol}` -> `{text, defs, refs:[{path,line,kind,from,text}]}`
//! - `impact {}` -> `{text, changed, dependents}` (changed vs merge-base, dependents depth 2)
//! - `clashes {repo_id?}` -> `{text, clashes:[{a,b,severity,detail}]}`
//! - `tree {}` -> `{files:[{path,rank,syms:[{name,kind,line,end,parent}]}], truncated}` (for the UI)
//! - `graph {level?: "file"|"symbol", path?, worktrees?: bool}` -> `{nodes, edges, truncated}`;
//!   nodes carry `changed_by:[{worktree_id, agent}]` when `worktrees` is true, plus
//!   `weight` (= `refs_in` + `refs_out`, summed edge weights) and, for files, `syms` (symbol count).
//!   Edges carry `weight` (number of references between the two nodes).
//! - `reindex {}`
//! - `source {path}` -> bounded source at the selected base blob or current disk
//! - `changes {}` -> net committed/staged/unstaged/untracked changes vs graph base
//! - `file_diff {path}` -> bounded patch vs graph base (never external diff/textconv)
//!
//! Events: `codeindex:clashes` (list) when the clash set changes; audit event `code_clash`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tree_sitter::{Node, Parser};

use crate::ctx::Ctx;
use crate::workspace::git;

#[path = "codeinspect.rs"]
mod inspect;

const MAX_FILE: u64 = 400 * 1024;
const MAX_FILES: usize = 20000;
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "vendor", ".git", "__pycache__", ".next", "venv", ".venv"];
/// Safety limit only (pathological cases); answers are compact by design, not budgeted.
const DEFAULT_TOKENS: usize = 60000;
const DEBOUNCE: Duration = Duration::from_secs(1);
const STOP_METHODS: &[&str] = &["new", "default", "main", "init", "from", "get", "set", "run", "call", "len", "clone", "fmt", "test", "drop", "next", "build", "close", "open", "read", "write"];

// ---------------------------------------------------------------- parsing

#[derive(Clone, Copy, PartialEq, Debug)]
enum Lang {
    Rust,
    Ts,
    Tsx,
    Js,
    Py,
    Go,
}

fn lang_of(path: &str) -> Option<Lang> {
    match path.rsplit('.').next()? {
        "rs" => Some(Lang::Rust),
        "ts" | "mts" | "cts" => Some(Lang::Ts),
        "tsx" => Some(Lang::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::Js),
        "py" => Some(Lang::Py),
        "go" => Some(Lang::Go),
        _ => None,
    }
}

fn skipped(path: &str) -> bool {
    path.split('/').any(|c| SKIP_DIRS.contains(&c))
}

#[derive(Clone, Debug, Default)]
pub struct Sym {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end: u32,
    pub sig: String,
    pub parent: String,
}

#[derive(Clone, Debug, Default)]
pub struct Ref {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub text: String,
    pub from: String,
    pub from_line: u32,
}

struct Frame {
    name: String,
    kind: String,
    sym: Option<usize>,
}

struct W<'a> {
    lang: Lang,
    src: &'a [u8],
    lines: Vec<&'a str>,
    path: &'a str,
    syms: Vec<Sym>,
    refs: Vec<Ref>,
    frames: Vec<Frame>,
    skip: HashSet<usize>,
    seen: HashSet<(String, u32, String, String)>,
    in_import: u32,
}

struct Def {
    kind: &'static str,
    name: String,
    name_node: Option<usize>,
    parent: Option<String>,
    /// node id whose range/first line describe the symbol (own node or its parent)
    span_is_parent: bool,
}

fn text<'a>(n: Node, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn first_line_sig(n: Node, src: &[u8], py: bool) -> String {
    let t = text(n, src);
    let mut l = t.lines().next().unwrap_or("").trim();
    if let Some(i) = l.find('{') {
        l = l[..i].trim();
    }
    let mut s = l.to_string();
    if py {
        s = s.trim_end_matches(':').to_string();
    }
    if s.chars().count() > 110 {
        s = s.chars().take(110).collect::<String>() + "...";
    }
    s
}

impl<'a> W<'a> {
    fn nearest_sym_kind(&self) -> Option<&str> {
        self.frames.iter().rev().find(|f| f.sym.is_some()).map(|f| f.kind.as_str())
    }

    fn def(&self, n: Node) -> Option<Def> {
        let k = n.kind();
        let field = |f: &str| n.child_by_field_name(f);
        let named = |kind: &'static str| -> Option<Def> {
            let nn = field("name")?;
            let name = text(nn, self.src).to_string();
            if name.is_empty() {
                return None;
            }
            Some(Def { kind, name, name_node: Some(nn.id()), parent: None, span_is_parent: false })
        };
        let in_fn = matches!(self.nearest_sym_kind(), Some("fn") | Some("method"));
        let top = self.frames.is_empty();
        match self.lang {
            Lang::Rust => match k {
                "function_item" | "function_signature_item" => {
                    if in_fn {
                        return None;
                    }
                    let m = matches!(self.frames.last().map(|f| f.kind.as_str()), Some("impl") | Some("trait"));
                    named(if m { "method" } else { "fn" })
                }
                "struct_item" | "union_item" => named("struct"),
                "enum_item" => named("enum"),
                "trait_item" => named("trait"),
                "type_item" => named("type"),
                "const_item" | "static_item" => {
                    if in_fn {
                        None
                    } else {
                        named("const")
                    }
                }
                "mod_item" => named("mod"),
                _ => None,
            },
            Lang::Ts | Lang::Tsx | Lang::Js => match k {
                "function_declaration" | "generator_function_declaration" => {
                    if in_fn {
                        None
                    } else {
                        named("fn")
                    }
                }
                "class_declaration" | "abstract_class_declaration" => named("class"),
                "method_definition" => named("method"),
                "interface_declaration" => named("interface"),
                "type_alias_declaration" => named("type"),
                "enum_declaration" => named("enum"),
                "variable_declarator" if top => {
                    let nn = field("name")?;
                    if nn.kind() != "identifier" {
                        return None;
                    }
                    let p = n.parent()?;
                    let pk = p.kind();
                    if pk != "lexical_declaration" && pk != "variable_declaration" {
                        return None;
                    }
                    let is_fn = field("value").map(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function")).unwrap_or(false);
                    let is_const = text(p, self.src).starts_with("const");
                    if !is_fn && !is_const {
                        return None;
                    }
                    Some(Def { kind: if is_fn { "fn" } else { "const" }, name: text(nn, self.src).to_string(), name_node: Some(nn.id()), parent: None, span_is_parent: true })
                }
                _ => None,
            },
            Lang::Py => match k {
                "function_definition" => {
                    if in_fn {
                        return None;
                    }
                    let m = matches!(self.frames.last().map(|f| f.kind.as_str()), Some("class"));
                    named(if m { "method" } else { "fn" })
                }
                "class_definition" => named("class"),
                "assignment" if top => {
                    let l = field("left")?;
                    let name = text(l, self.src);
                    if l.kind() == "identifier" && name.len() > 1 && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                        Some(Def { kind: "const", name: name.to_string(), name_node: Some(l.id()), parent: None, span_is_parent: false })
                    } else {
                        None
                    }
                }
                _ => None,
            },
            Lang::Go => match k {
                "function_declaration" => named("fn"),
                "method_declaration" => {
                    let mut d = named("method")?;
                    if let Some(r) = field("receiver") {
                        let t = text(r, self.src);
                        let t = t.trim_matches(|c| c == '(' || c == ')');
                        let ty = t.split_whitespace().last().unwrap_or("").trim_start_matches('*');
                        d.parent = Some(ty.to_string());
                    }
                    Some(d)
                }
                "type_spec" => {
                    let kind = match field("type").map(|t| t.kind()) {
                        Some("struct_type") => "struct",
                        Some("interface_type") => "interface",
                        _ => "type",
                    };
                    named(kind)
                }
                "const_spec" | "var_spec" if top => named("const"),
                _ => None,
            },
        }
    }

    fn add_ref(&mut self, n: Node) {
        let name = text(n, self.src);
        if name.len() < 2 || matches!(name, "self" | "this" | "super" | "crate" | "None" | "true" | "false" | "undefined" | "null") {
            return;
        }
        let kind = if self.in_import > 0 {
            "import"
        } else if n.kind() == "type_identifier" {
            "type"
        } else if is_call(n) {
            "call"
        } else {
            "use"
        };
        let (from, from_line) = self
            .frames
            .iter()
            .rev()
            .find_map(|f| f.sym.map(|i| (self.syms[i].name.clone(), self.syms[i].line)))
            .unwrap_or_default();
        let line = n.start_position().row as u32 + 1;
        let key = (from.clone(), from_line, name.to_string(), kind.to_string());
        if !self.seen.insert(key) {
            return;
        }
        let mut t = self.lines.get(line as usize - 1).copied().unwrap_or("").trim().to_string();
        if t.chars().count() > 140 {
            t = t.chars().take(140).collect();
        }
        self.refs.push(Ref { path: self.path.to_string(), name: name.to_string(), kind: kind.into(), line, text: t, from, from_line });
    }

    fn walk(&mut self, n: Node) {
        let k = n.kind();
        let mut imp = false;
        if matches!(k, "use_declaration" | "import_statement" | "import_from_statement" | "import_declaration" | "extern_crate_declaration" | "future_import_statement") {
            self.in_import += 1;
            imp = true;
        }
        let mut pushed = false;
        if let Some(d) = self.def(n) {
            if let Some(id) = d.name_node {
                self.skip.insert(id);
            }
            let span = if d.span_is_parent { n.parent().unwrap_or(n) } else { n };
            let parent = d.parent.clone().unwrap_or_else(|| self.frames.last().map(|f| f.name.clone()).unwrap_or_default());
            let sym = Sym {
                path: self.path.to_string(),
                name: d.name.clone(),
                kind: d.kind.to_string(),
                line: span.start_position().row as u32 + 1,
                end: span.end_position().row as u32 + 1,
                sig: first_line_sig(span, self.src, self.lang == Lang::Py),
                parent,
            };
            self.syms.push(sym);
            let idx = self.syms.len() - 1;
            self.frames.push(Frame { name: d.name, kind: d.kind.to_string(), sym: Some(idx) });
            pushed = true;
        } else if self.lang == Lang::Rust && k == "impl_item" {
            if let Some(t) = n.child_by_field_name("type") {
                let raw = text(t, self.src);
                let name = raw.split('<').next().unwrap_or(raw).trim().trim_start_matches('&').rsplit("::").next().unwrap_or("").to_string();
                self.frames.push(Frame { name, kind: "impl".into(), sym: None });
                pushed = true;
            }
        }
        if matches!(k, "identifier" | "type_identifier" | "property_identifier" | "field_identifier") && !self.skip.contains(&n.id()) {
            self.add_ref(n);
        }
        for i in 0..n.child_count() {
            if let Some(c) = n.child(i as u32) {
                self.walk(c);
            }
        }
        if pushed {
            self.frames.pop();
        }
        if imp {
            self.in_import -= 1;
        }
    }
}

fn is_call(n: Node) -> bool {
    let callish = |k: &str| matches!(k, "call_expression" | "call" | "macro_invocation");
    let Some(p) = n.parent() else { return false };
    if callish(p.kind()) {
        return p.child_by_field_name("function").map(|f| f.id() == n.id()).unwrap_or(false) || p.child_by_field_name("macro").map(|f| f.id() == n.id()).unwrap_or(false);
    }
    if matches!(p.kind(), "member_expression" | "field_expression" | "selector_expression" | "attribute" | "scoped_identifier") {
        let last = p.named_child(p.named_child_count().saturating_sub(1) as u32);
        if last.map(|l| l.id()) == Some(n.id()) {
            if let Some(g) = p.parent() {
                if callish(g.kind()) {
                    return g.child_by_field_name("function").map(|f| f.id() == p.id()).unwrap_or(false);
                }
            }
        }
    }
    false
}

pub fn parse(path: &str, src: &str) -> (Vec<Sym>, Vec<Ref>) {
    let Some(lang) = lang_of(path) else { return Default::default() };
    let mut parser = Parser::new();
    let l: tree_sitter::Language = match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Py => tree_sitter_python::LANGUAGE.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
    };
    if parser.set_language(&l).is_err() {
        return Default::default();
    }
    let Some(tree) = parser.parse(src, None) else { return Default::default() };
    let mut w = W {
        lang,
        src: src.as_bytes(),
        lines: src.lines().collect(),
        path,
        syms: vec![],
        refs: vec![],
        frames: vec![],
        skip: HashSet::new(),
        seen: HashSet::new(),
        in_import: 0,
    };
    w.walk(tree.root_node());
    (w.syms, w.refs)
}

// ---------------------------------------------------------------- storage

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ci_snap(repo_id TEXT PRIMARY KEY, commit_id TEXT NOT NULL, built_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ci_wt(wt_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, base_commit TEXT NOT NULL, indexed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ci_files(scope TEXT NOT NULL, path TEXT NOT NULL, stamp TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(scope,path));
CREATE TABLE IF NOT EXISTS ci_syms(scope TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL, end_line INTEGER NOT NULL, sig TEXT NOT NULL, parent TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS ci_syms_scope_path ON ci_syms(scope,path);
CREATE TABLE IF NOT EXISTS ci_refs(scope TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, line INTEGER NOT NULL, text TEXT NOT NULL, from_name TEXT NOT NULL, from_line INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS ci_refs_scope_path ON ci_refs(scope,path);
";

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    let c = ctx.clone();
    std::thread::spawn(move || watcher(c));
    Ok(())
}

fn index_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn debounce() -> &'static Mutex<HashMap<String, Instant>> {
    static L: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

type Parsed = (Vec<Sym>, Vec<Ref>);

fn store_file(c: &Connection, scope: &str, path: &str, stamp: &str, parsed: Option<&Parsed>) -> rusqlite::Result<()> {
    c.execute("DELETE FROM ci_syms WHERE scope=?1 AND path=?2", params![scope, path])?;
    c.execute("DELETE FROM ci_refs WHERE scope=?1 AND path=?2", params![scope, path])?;
    c.execute("INSERT OR REPLACE INTO ci_files(scope,path,stamp,deleted) VALUES(?1,?2,?3,?4)", params![scope, path, stamp, parsed.is_none() as i64])?;
    if let Some((syms, refs)) = parsed {
        let mut s = c.prepare_cached("INSERT INTO ci_syms(scope,path,name,kind,line,end_line,sig,parent) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        for y in syms {
            s.execute(params![scope, path, y.name, y.kind, y.line, y.end, y.sig, y.parent])?;
        }
        let mut r = c.prepare_cached("INSERT INTO ci_refs(scope,path,name,kind,line,text,from_name,from_line) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
        for y in refs {
            r.execute(params![scope, path, y.name, y.kind, y.line, y.text, y.from, y.from_line])?;
        }
    }
    Ok(())
}

fn flush(ctx: &Ctx, scope: &str, batch: &mut Vec<(String, String, Parsed)>) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    ctx.db.with(|c| {
        c.execute_batch("BEGIN")?;
        for (p, st, parsed) in batch.iter() {
            store_file(c, scope, p, st, Some(parsed))?;
        }
        c.execute_batch("COMMIT")
    })?;
    batch.clear();
    Ok(())
}

fn rev_parse(repo: &Path, branch: &str) -> Result<String, String> {
    inspect::inspect_revision(repo, branch).or_else(|_| inspect::inspect_revision(repo, "HEAD"))
}

fn read_blobs(repo: &Path, shas: &[String], mut f: impl FnMut(usize, Vec<u8>)) -> Result<(), String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("stdin")?;
    let list = shas.to_vec();
    let h = std::thread::spawn(move || {
        for s in list {
            if writeln!(stdin, "{s}").is_err() {
                break;
            }
        }
    });
    let mut r = BufReader::new(child.stdout.take().ok_or("stdout")?);
    for i in 0..shas.len() {
        let mut hdr = String::new();
        r.read_line(&mut hdr).map_err(|e| e.to_string())?;
        let size: usize = hdr.split_whitespace().nth(2).and_then(|s| s.parse().ok()).ok_or("bad cat-file header")?;
        let mut buf = vec![0u8; size];
        r.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut nl = [0u8; 1];
        r.read_exact(&mut nl).map_err(|e| e.to_string())?;
        f(i, buf);
    }
    let _ = h.join();
    let _ = child.wait();
    Ok(())
}

pub fn base_scope(repo_id: &str, commit: &str) -> String {
    format!("b:{repo_id}:{commit}")
}

/// Build the base snapshot for `branch` of the repo if its commit changed. Returns the commit.
pub fn ensure_base(ctx: &Ctx, repo_id: &str, repo: &Path, branch: &str) -> Result<String, String> {
    let _g = index_lock().lk();
    let commit = rev_parse(repo, branch)?;
    let cur = ctx.db.query("SELECT commit_id FROM ci_snap WHERE repo_id=?1", [repo_id])?;
    if cur.first().and_then(|r| r["commit_id"].as_str()) == Some(commit.as_str()) {
        return Ok(commit);
    }
    let scope = base_scope(repo_id, &commit);
    ctx.db.with(|c| {
        let like = format!("b:{repo_id}:%");
        c.execute("DELETE FROM ci_syms WHERE scope LIKE ?1", [&like])?;
        c.execute("DELETE FROM ci_refs WHERE scope LIKE ?1", [&like])?;
        c.execute("DELETE FROM ci_files WHERE scope LIKE ?1", [&like])?;
        // worktree deltas were computed against the old base
        c.execute("DELETE FROM ci_syms WHERE scope IN (SELECT 'w:'||wt_id FROM ci_wt WHERE repo_id=?1)", [repo_id])?;
        c.execute("DELETE FROM ci_refs WHERE scope IN (SELECT 'w:'||wt_id FROM ci_wt WHERE repo_id=?1)", [repo_id])?;
        c.execute("DELETE FROM ci_files WHERE scope IN (SELECT 'w:'||wt_id FROM ci_wt WHERE repo_id=?1)", [repo_id])?;
        c.execute("DELETE FROM ci_wt WHERE repo_id=?1", [repo_id])?;
        Ok(())
    })?;
    debounce().lk().clear();
    let ls = git(repo, &["-c", "core.quotepath=off", "ls-tree", "-r", "-l", &commit])?;
    let mut files: Vec<(String, String)> = vec![]; // (path, sha)
    for l in ls.lines() {
        let Some((meta, path)) = l.split_once('\t') else { continue };
        let m: Vec<&str> = meta.split_whitespace().collect();
        if m.len() < 4 || m[1] != "blob" {
            continue;
        }
        let size: u64 = m[3].parse().unwrap_or(u64::MAX);
        if size > MAX_FILE || lang_of(path).is_none() || skipped(path) || inspect::private_name(Path::new(path)) || m[0] == "120000" {
            continue;
        }
        files.push((path.to_string(), m[2].to_string()));
        if files.len() >= MAX_FILES {
            break;
        }
    }
    let shas: Vec<String> = files.iter().map(|f| f.1.clone()).collect();
    let mut batch: Vec<(String, String, Parsed)> = vec![];
    let mut err: Option<String> = None;
    read_blobs(repo, &shas, |i, bytes| {
        let (path, sha) = &files[i];
        let src = String::from_utf8_lossy(&bytes);
        batch.push((path.clone(), sha.clone(), parse(path, &src)));
        if batch.len() >= 200 {
            if let Err(e) = flush(ctx, &scope, &mut batch) {
                err = Some(e);
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    flush(ctx, &scope, &mut batch)?;
    ctx.db.with(|c| {
        c.execute("INSERT OR REPLACE INTO ci_snap(repo_id,commit_id,built_at) VALUES(?1,?2,?3)", params![repo_id, commit, crate::db::now()])?;
        Ok(())
    })?;
    Ok(commit)
}

fn lines_of(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// Files that differ from `base` in the worktree (tracked diff + untracked), indexable only.
fn delta_files(wt: &Path, base: &str) -> Vec<String> {
    let mut v: BTreeSet<String> = BTreeSet::new();
    if let Ok(o) = git(wt, &["-c", "core.quotepath=off", "diff", "--name-only", "--no-renames", base]) {
        v.extend(lines_of(&o));
    }
    if let Ok(o) = git(wt, &["-c", "core.quotepath=off", "ls-files", "--others", "--exclude-standard"]) {
        v.extend(lines_of(&o));
    }
    v.into_iter().filter(|p| lang_of(p).is_some() && !skipped(p)).collect()
}

fn stamp_of(p: &Path) -> Option<String> {
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_FILE {
        return None;
    }
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(format!("{t}:{}", m.len()))
}

pub fn wt_scope(wt_id: &str) -> String {
    format!("w:{wt_id}")
}

/// Reparse only the worktree files that differ from the base commit. Returns files reparsed.
pub fn ensure_delta(ctx: &Ctx, repo_id: &str, base: &str, wt_id: &str, wt: &Path, force: bool) -> Result<usize, String> {
    if !force {
        if let Some(t) = debounce().lk().get(wt_id) {
            if t.elapsed() < DEBOUNCE {
                return Ok(0);
            }
        }
    }
    let _g = index_lock().lk();
    let scope = wt_scope(wt_id);
    let prev = ctx.db.query("SELECT base_commit FROM ci_wt WHERE wt_id=?1", [wt_id])?;
    if prev.first().and_then(|r| r["base_commit"].as_str()).map(|b| b != base).unwrap_or(false) {
        ctx.db.with(|c| {
            c.execute("DELETE FROM ci_syms WHERE scope=?1", [&scope])?;
            c.execute("DELETE FROM ci_refs WHERE scope=?1", [&scope])?;
            c.execute("DELETE FROM ci_files WHERE scope=?1", [&scope])?;
            Ok(())
        })?;
    }
    let files = delta_files(wt, base);
    let existing: HashMap<String, String> = ctx
        .db
        .query("SELECT path,stamp FROM ci_files WHERE scope=?1", [&scope])?
        .into_iter()
        .filter_map(|r| Some((r["path"].as_str()?.to_string(), r["stamp"].as_str()?.to_string())))
        .collect();
    let set: HashSet<&String> = files.iter().collect();
    let mut n = 0;
    let mut batch: Vec<(String, String, Parsed)> = vec![];
    let mut gone: Vec<String> = vec![];
    for p in &files {
        match inspect::checked(wt, p).ok().and_then(|safe| stamp_of(&safe)) {
            Some(st) => {
                if existing.get(p) == Some(&st) {
                    continue;
                }
                let src = std::fs::read(wt.join(p)).map_err(|e| e.to_string())?;
                batch.push((p.clone(), st, parse(p, &String::from_utf8_lossy(&src))));
                n += 1;
            }
            None => {
                if existing.get(p).map(|s| s != "deleted").unwrap_or(true) {
                    gone.push(p.clone());
                    n += 1;
                }
            }
        }
    }
    let stale: Vec<String> = existing.keys().filter(|p| !set.contains(p)).cloned().collect();
    ctx.db.with(|c| {
        c.execute_batch("BEGIN")?;
        for (p, st, parsed) in &batch {
            store_file(c, &scope, p, st, Some(parsed))?;
        }
        for p in &gone {
            store_file(c, &scope, p, "deleted", None)?;
        }
        for p in &stale {
            c.execute("DELETE FROM ci_syms WHERE scope=?1 AND path=?2", params![scope, p])?;
            c.execute("DELETE FROM ci_refs WHERE scope=?1 AND path=?2", params![scope, p])?;
            c.execute("DELETE FROM ci_files WHERE scope=?1 AND path=?2", params![scope, p])?;
        }
        c.execute("INSERT OR REPLACE INTO ci_wt(wt_id,repo_id,base_commit,indexed_at) VALUES(?1,?2,?3,?4)", params![wt_id, repo_id, base, crate::db::now()])?;
        c.execute_batch("COMMIT")
    })?;
    debounce().lk().insert(wt_id.to_string(), Instant::now());
    Ok(n + stale.len())
}

#[derive(Default, Clone)]
pub struct View {
    pub syms: Vec<Sym>,
    pub refs: Vec<Ref>,
}

pub fn load_view(ctx: &Ctx, repo_id: &str, base: &str, wt_id: Option<&str>) -> Result<View, String> {
    let b = base_scope(repo_id, base);
    let w = wt_id.map(wt_scope).unwrap_or_else(|| "none".into());
    ctx.db.with(|c| {
        let mut v = View::default();
        let mut s = c.prepare(
            "SELECT path,name,kind,line,end_line,sig,parent FROM ci_syms WHERE scope=?1 AND path NOT IN (SELECT path FROM ci_files WHERE scope=?2)
             UNION ALL SELECT path,name,kind,line,end_line,sig,parent FROM ci_syms WHERE scope=?2",
        )?;
        let rows = s.query_map(params![b, w], |r| {
            Ok(Sym { path: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, line: r.get(3)?, end: r.get(4)?, sig: r.get(5)?, parent: r.get(6)? })
        })?;
        for r in rows {
            v.syms.push(r?);
        }
        let mut s = c.prepare(
            "SELECT path,name,kind,line,text,from_name,from_line FROM ci_refs WHERE scope=?1 AND path NOT IN (SELECT path FROM ci_files WHERE scope=?2)
             UNION ALL SELECT path,name,kind,line,text,from_name,from_line FROM ci_refs WHERE scope=?2",
        )?;
        let rows = s.query_map(params![b, w], |r| {
            Ok(Ref { path: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, line: r.get(3)?, text: r.get(4)?, from: r.get(5)?, from_line: r.get(6)? })
        })?;
        for r in rows {
            v.refs.push(r?);
        }
        // keep only references to names defined somewhere in the effective view
        let defined: HashSet<&str> = v.syms.iter().map(|s| s.name.as_str()).collect();
        let refs = std::mem::take(&mut v.refs);
        v.refs = refs.into_iter().filter(|r| defined.contains(r.name.as_str())).collect();
        Ok(v)
    })
}

/// Build base + delta and load the effective view. Blocking.
pub fn index_target(ctx: &Ctx, repo_id: &str, repo: &Path, branch: &str, wt: Option<(&str, &Path)>, force: bool) -> Result<(View, String), String> {
    let base = ensure_base(ctx, repo_id, repo, branch)?;
    if let Some((id, p)) = wt {
        ensure_delta(ctx, repo_id, &base, id, p, force)?;
    }
    let v = load_view(ctx, repo_id, &base, wt.map(|w| w.0))?;
    Ok((v, base))
}

// ---------------------------------------------------------------- changes

/// path -> None (whole file) or changed new-side line ranges.
pub type ChangeSet = BTreeMap<String, Option<Vec<(u32, u32)>>>;

pub fn change_set(wt: &Path, base: &str) -> ChangeSet {
    let mut cs = ChangeSet::new();
    let mb = git(wt, &["merge-base", "HEAD", base]).unwrap_or_else(|_| base.to_string());
    if let Ok(d) = git(wt, &["-c", "core.quotepath=off", "diff", "-U0", "--no-renames", &mb]) {
        let mut cur: Option<String> = None;
        for l in d.lines() {
            if let Some(p) = l.strip_prefix("+++ ") {
                cur = p.strip_prefix("b/").map(str::to_string);
                if let Some(c) = &cur {
                    cs.entry(c.clone()).or_insert_with(|| Some(vec![]));
                }
            } else if l.starts_with("@@") {
                let (Some(c), Some(plus)) = (&cur, l.split_whitespace().find(|t| t.starts_with('+'))) else { continue };
                let t = plus.trim_start_matches('+');
                let (s, n) = t.split_once(',').unwrap_or((t, "1"));
                let (s, n): (u32, u32) = (s.parse().unwrap_or(1), n.parse().unwrap_or(1));
                if let Some(Some(v)) = cs.get_mut(c) {
                    v.push((s, if n == 0 { s } else { s + n - 1 }));
                }
            }
        }
    }
    if let Ok(o) = git(wt, &["-c", "core.quotepath=off", "ls-files", "--others", "--exclude-standard"]) {
        for p in lines_of(&o) {
            cs.insert(p, None);
        }
    }
    cs.retain(|p, _| lang_of(p).is_some() && !skipped(p));
    cs
}

fn overlaps(r: &Option<Vec<(u32, u32)>>, a: u32, b: u32) -> bool {
    match r {
        None => true,
        Some(v) => v.iter().any(|(s, e)| *s <= b && *e >= a),
    }
}

pub fn changed_syms<'a>(v: &'a View, cs: &ChangeSet) -> Vec<&'a Sym> {
    v.syms.iter().filter(|s| cs.get(&s.path).map(|r| overlaps(r, s.line, s.end)).unwrap_or(false)).collect()
}

// ---------------------------------------------------------------- rendering

struct Out {
    buf: String,
    cap: usize,
    omitted: usize,
}

impl Out {
    fn new(cap: usize) -> Out {
        Out { buf: String::new(), cap: cap.max(20), omitted: 0 }
    }
    fn line(&mut self, l: impl AsRef<str>) -> bool {
        let l = l.as_ref();
        if self.omitted > 0 || (self.buf.len() + l.len() + 1) / 4 > self.cap {
            self.omitted += 1;
            return false;
        }
        self.buf.push_str(l);
        self.buf.push('\n');
        true
    }
    fn finish(mut self, hint: &str) -> String {
        if self.omitted > 0 {
            self.buf.push_str(&format!("... {} more, narrow with {}\n", self.omitted, hint));
        }
        self.buf
    }
}

fn rank_names(v: &View) -> HashMap<&str, f64> {
    let mut defs: HashMap<&str, usize> = HashMap::new();
    for s in &v.syms {
        *defs.entry(&s.name).or_default() += 1;
    }
    let mut inc: HashMap<&str, f64> = HashMap::new();
    for r in &v.refs {
        if let Some(d) = defs.get(r.name.as_str()) {
            *inc.entry(&r.name).or_default() += 1.0 / *d as f64;
        }
    }
    // one propagation step: a symbol used by popular symbols is popular too
    let base = inc.clone();
    for r in &v.refs {
        if r.from.is_empty() || !defs.contains_key(r.name.as_str()) {
            continue;
        }
        if let Some(fr) = base.get(r.from.as_str()) {
            *inc.entry(&r.name).or_default() += 0.15 * fr.min(50.0) / *defs.get(r.name.as_str()).unwrap() as f64;
        }
    }
    inc
}

struct FileEntry {
    rel: String,
    rank: f64,
    syms: Vec<(String, String)>,
}

fn render_dir(entries: Vec<FileEntry>, indent: usize, depth: usize, out: &mut Out) {
    let mut files: Vec<FileEntry> = vec![];
    let mut dirs: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();
    for e in entries {
        match e.rel.split_once('/') {
            Some((d, rest)) => dirs.entry(d.to_string()).or_default().push(FileEntry { rel: rest.to_string(), rank: e.rank, syms: e.syms }),
            None => files.push(e),
        }
    }
    enum Item {
        F(FileEntry),
        D(String, Vec<FileEntry>, f64),
    }
    let mut items: Vec<Item> = files.into_iter().map(Item::F).collect();
    for (d, es) in dirs {
        let r = es.iter().map(|e| e.rank).sum();
        items.push(Item::D(d, es, r));
    }
    let rk = |i: &Item| match i {
        Item::F(f) => f.rank,
        Item::D(_, _, r) => *r,
    };
    items.sort_by(|a, b| rk(b).partial_cmp(&rk(a)).unwrap_or(std::cmp::Ordering::Equal));
    let pad = "  ".repeat(indent);
    for it in items {
        match it {
            Item::F(f) => {
                let shown: Vec<String> = f.syms.iter().take(8).map(|(k, n)| format!("{k} {n}")).collect();
                let more = if f.syms.len() > 8 { format!(" +{}", f.syms.len() - 8) } else { String::new() };
                out.line(format!("{pad}{}  {}{more}", f.rel, shown.join(", ")));
            }
            Item::D(d, mut es, _) => {
                if depth > 1 {
                    out.line(format!("{pad}{d}/  ({} files)", es.len()));
                    render_dir(es, indent + 1, depth - 1, out);
                } else {
                    es.sort_by(|a, b| b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal));
                    let top: Vec<&str> = es.iter().take(3).map(|e| e.rel.as_str()).collect();
                    out.line(format!("{pad}{d}/  ({} files) top: {}", es.len(), top.join(", ")));
                }
            }
        }
    }
}

pub fn render_map(v: &View, path: Option<&str>, depth: usize, cap: usize) -> String {
    let prefix = path.unwrap_or("").trim_matches('/').to_string();
    let rank = rank_names(v);
    let mut by_file: BTreeMap<&str, Vec<&Sym>> = BTreeMap::new();
    for s in &v.syms {
        by_file.entry(&s.path).or_default().push(s);
    }
    let mut entries = vec![];
    let mut nsyms = 0;
    for (p, ss) in by_file {
        let rel = if prefix.is_empty() {
            p.to_string()
        } else if p == prefix {
            p.rsplit('/').next().unwrap_or(p).to_string()
        } else if let Some(r) = p.strip_prefix(&format!("{prefix}/")) {
            r.to_string()
        } else {
            continue;
        };
        let mut top: Vec<&&Sym> = ss.iter().filter(|s| s.parent.is_empty()).collect();
        top.sort_by(|a, b| rank.get(b.name.as_str()).unwrap_or(&0.0).partial_cmp(rank.get(a.name.as_str()).unwrap_or(&0.0)).unwrap_or(std::cmp::Ordering::Equal));
        nsyms += ss.len();
        let r = ss.iter().map(|s| rank.get(s.name.as_str()).copied().unwrap_or(0.0)).sum();
        entries.push(FileEntry { rel, rank: r, syms: top.iter().map(|s| (s.kind.clone(), s.name.clone())).collect() });
    }
    let mut out = Out::new(cap);
    out.line(format!("map {}: {} files, {} symbols, ranked by use", if prefix.is_empty() { "." } else { &prefix }, entries.len(), nsyms));
    if entries.is_empty() {
        out.line("no indexed files here. Check the path, or use code_find.");
    }
    render_dir(entries, 0, depth.max(1), &mut out);
    out.finish("path=<dir> or depth=<n>")
}

fn fuzzy(q: &str, s: &str) -> bool {
    let mut it = s.chars();
    q.chars().all(|c| it.any(|x| x == c))
}

pub fn find_items<'a>(v: &'a View, q: &str, kind: Option<&str>) -> Vec<&'a Sym> {
    let rank = rank_names(v);
    let ql = q.to_lowercase();
    let mut hits: Vec<(f64, &Sym)> = vec![];
    for s in &v.syms {
        if kind.map(|k| k != s.kind).unwrap_or(false) {
            continue;
        }
        let nl = s.name.to_lowercase();
        let score = if s.name == q {
            100.0
        } else if nl == ql {
            90.0
        } else if nl.starts_with(&ql) {
            80.0
        } else if nl.contains(&ql) {
            60.0
        } else if s.path.to_lowercase().contains(&ql) {
            20.0
        } else if ql.len() >= 3 && fuzzy(&ql, &nl) {
            10.0
        } else {
            continue;
        };
        hits.push((score + rank.get(s.name.as_str()).copied().unwrap_or(0.0).min(9.0), s));
    }
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.path.cmp(&b.1.path)).then(a.1.line.cmp(&b.1.line)));
    hits.into_iter().map(|h| h.1).collect()
}

pub fn render_find(v: &View, q: &str, kind: Option<&str>, cap: usize) -> String {
    let items = find_items(v, q, kind);
    let mut out = Out::new(cap);
    out.line(format!("find \"{q}\": {} matches", items.len()));
    let mut last = "";
    for s in items {
        let par = if s.parent.is_empty() { String::new() } else { format!("{}.", s.parent) };
        if s.path != last {
            out.line(format!("{}:", s.path));
            last = &s.path;
        }
        out.line(format!("  {} {} {}{}  {}", s.line, s.kind, par, s.name, s.sig));
    }
    out.finish("a longer query or kind=<fn|class|struct|...>")
}

fn split_symbol(sym: &str) -> (Option<&str>, &str) {
    let s = sym.trim();
    for sep in ["::", "."] {
        if let Some((p, n)) = s.rsplit_once(sep) {
            return (Some(p.rsplit(|c| c == '.' || c == ':').next().unwrap_or(p)), n);
        }
    }
    (None, s)
}

/// Resolution of name matches to a definition. A reference is `exact` when it can be tied to
/// the definition through its own file, an import/use line naming the defining module, a
/// qualified path (`mod::name`, `Type::name`, `mod.name`) or a `self.`/`this.` receiver inside
/// the defining file. Everything else is a name match only ("possible").
pub struct Resolver<'a> {
    imports: HashMap<(&'a str, &'a str), Vec<&'a str>>,
}

fn stems(path: &str) -> Vec<String> {
    let mut v = vec![];
    let mut it = path.rsplit('/');
    let file = it.next().unwrap_or("");
    let stem = file.rsplit_once('.').map(|x| x.0).unwrap_or(file);
    if !matches!(stem, "mod" | "index" | "__init__" | "lib" | "main") {
        v.push(stem.to_string());
    }
    if matches!(stem, "mod" | "index" | "__init__" | "lib" | "main") {
        if let Some(d) = it.next() {
            v.push(d.to_string());
        }
    }
    v
}

fn has_word(text: &str, w: &str) -> bool {
    text.match_indices(w).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + w.len()..].chars().next();
        !before.map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false) && !after.map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false)
    })
}

impl<'a> Resolver<'a> {
    pub fn new(v: &'a View) -> Resolver<'a> {
        let mut imports: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
        for r in &v.refs {
            if r.kind == "import" {
                imports.entry((r.path.as_str(), r.name.as_str())).or_default().push(r.text.as_str());
            }
        }
        Resolver { imports }
    }

    /// Is `r` (a name match for `d.name`) provably a use of definition `d`?
    pub fn exact(&self, d: &Sym, r: &Ref) -> bool {
        if r.name != d.name {
            return false;
        }
        if r.path == d.path {
            if d.parent.is_empty() {
                return true;
            }
            return r.text.contains(&format!("self.{}", d.name)) || r.text.contains(&format!("this.{}", d.name)) || r.text.contains(&format!("{}::{}", d.parent, d.name)) || r.text.contains(&format!("{}.{}", d.parent, d.name));
        }
        if !d.parent.is_empty() && (r.text.contains(&format!("{}::{}", d.parent, d.name)) || r.text.contains(&format!("{}.{}", d.parent, d.name))) {
            return true;
        }
        let st = stems(&d.path);
        for m in &st {
            if r.text.contains(&format!("{m}::{}", d.name)) || r.text.contains(&format!("{m}.{}", d.name)) {
                return true;
            }
        }
        if d.parent.is_empty() || r.kind == "import" {
            if let Some(texts) = self.imports.get(&(r.path.as_str(), r.name.as_str())) {
                if texts.iter().any(|t| st.iter().any(|m| has_word(t, m))) {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RefHit<'a> {
    pub r: &'a Ref,
    pub exact: bool,
}

/// Definitions matching `sym` and every name match, exact ones first.
pub fn refs_of<'a>(v: &'a View, sym: &str) -> (Vec<&'a Sym>, Vec<RefHit<'a>>) {
    let (parent, name) = split_symbol(sym);
    let defs: Vec<&Sym> = v.syms.iter().filter(|s| s.name == name && parent.map(|p| s.parent == p).unwrap_or(true)).collect();
    let def_lines: HashSet<(&str, u32)> = v.syms.iter().filter(|s| s.name == name).map(|s| (s.path.as_str(), s.line)).collect();
    let res = Resolver::new(v);
    let mut hits: Vec<RefHit> = v
        .refs
        .iter()
        .filter(|r| r.name == name && !def_lines.contains(&(r.path.as_str(), r.line)))
        .map(|r| RefHit { r, exact: defs.iter().any(|d| res.exact(d, r)) })
        .collect();
    hits.sort_by(|a, b| b.exact.cmp(&a.exact).then(a.r.path.cmp(&b.r.path)).then(a.r.line.cmp(&b.r.line)));
    (defs, hits)
}

pub fn render_refs(v: &View, sym: &str, cap: usize) -> String {
    let (defs, hits) = refs_of(v, sym);
    let mut out = Out::new(cap);
    if defs.is_empty() && hits.is_empty() {
        return format!("no symbol named \"{sym}\". Try code_find with part of the name.\n");
    }
    for d in &defs {
        out.line(format!("def {}:{} {} {}", d.path, d.line, d.kind, d.sig));
    }
    if defs.len() > 1 {
        out.line(format!("note: {} definitions share this name. Qualify as Parent.name to separate them.", defs.len()));
    }
    for (label, exact, note) in [("exact", true, "resolved through the file, an import or a qualified path"), ("possible", false, "same name only, NOT confirmed. Verify before relying on it")] {
        let list: Vec<&RefHit> = hits.iter().filter(|h| h.exact == exact).collect();
        let files: HashSet<&str> = list.iter().map(|h| h.r.path.as_str()).collect();
        out.line(format!("{label}: {} refs in {} files ({note})", list.len(), files.len()));
        let mut last = "";
        for h in list {
            if h.r.path != last {
                out.line(format!("{}:", h.r.path));
                last = &h.r.path;
            }
            let inn = if h.r.from.is_empty() { String::new() } else { format!(" [{}]", h.r.from) };
            out.line(format!("  {}{} {}", h.r.line, inn, h.r.text));
        }
    }
    out.finish("Parent.name or a more specific symbol")
}

fn is_noise(kind: &str, name: &str) -> bool {
    kind == "method" && STOP_METHODS.contains(&name)
}

#[derive(Clone, Debug)]
pub struct Dependent {
    pub path: String,
    pub name: String,
    pub line: u32,
    pub via: String,
    pub depth: u8,
    pub exact: bool,
}

pub fn dependents(v: &View, changed: &[&Sym]) -> Vec<Dependent> {
    let res = Resolver::new(v);
    let mut by_name: HashMap<&str, Vec<&Ref>> = HashMap::new();
    for r in &v.refs {
        by_name.entry(&r.name).or_default().push(r);
    }
    let mut frontier: Vec<(Sym, bool)> = changed.iter().filter(|s| !is_noise(&s.kind, &s.name)).map(|s| ((*s).clone(), true)).collect();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for s in changed {
        seen.insert((s.path.clone(), s.name.clone()), usize::MAX);
    }
    let mut out: Vec<Dependent> = vec![];
    for depth in 1..=2u8 {
        let mut next = vec![];
        for (d, d_exact) in &frontier {
            for r in by_name.get(d.name.as_str()).map(|v| v.as_slice()).unwrap_or(&[]) {
                if r.from.is_empty() {
                    continue;
                }
                let Some(from) = v.syms.iter().find(|s| s.path == r.path && s.name == r.from && s.line == r.from_line) else { continue };
                if is_noise(&from.kind, &from.name) {
                    continue;
                }
                let exact = *d_exact && res.exact(d, r);
                let key = (r.path.clone(), r.from.clone());
                match seen.get(&key) {
                    Some(&i) if i == usize::MAX => continue,
                    Some(&i) => {
                        if exact && !out[i].exact {
                            out[i].exact = true;
                        }
                        continue;
                    }
                    None => {}
                }
                seen.insert(key, out.len());
                out.push(Dependent { path: r.path.clone(), name: r.from.clone(), line: r.from_line, via: d.name.clone(), depth, exact });
                next.push((from.clone(), exact));
            }
        }
        frontier = next;
    }
    out
}

pub fn render_impact(v: &View, cs: &ChangeSet, cap: usize) -> String {
    if cs.is_empty() {
        return "no changes vs the merge-base yet.\n".into();
    }
    let ch = changed_syms(v, cs);
    let mut out = Out::new(cap);
    out.line(format!("changed: {} symbols in {} files", ch.len(), cs.len()));
    for s in &ch {
        out.line(format!("  {}:{} {} {}", s.path, s.line, s.kind, s.name));
    }
    let deps = dependents(v, &ch);
    for (label, exact) in [("exact", true), ("possible (name match only, verify)", false)] {
        let list: Vec<&Dependent> = deps.iter().filter(|d| d.exact == exact).collect();
        let (d1, d2) = (list.iter().filter(|d| d.depth == 1).count(), list.iter().filter(|d| d.depth == 2).count());
        out.line(format!("dependents {label}: {d1} direct, {d2} indirect"));
        let mut by_file: BTreeMap<&str, Vec<&&Dependent>> = BTreeMap::new();
        for d in &list {
            by_file.entry(&d.path).or_default().push(d);
        }
        for (p, ds) in by_file {
            let l: Vec<String> = ds.iter().map(|d| format!("{}:{} d{} <- {}", d.name, d.line, d.depth, d.via)).collect();
            out.line(format!("  {p}  {}", l.join(", ")));
        }
    }
    out.finish("a smaller change set or code_refs <symbol>")
}

// ---------------------------------------------------------------- clashes

#[derive(Clone, Debug)]
pub struct WtChange {
    pub label: String,
    pub wt_id: String,
    pub files: BTreeSet<String>,
    pub syms: BTreeSet<(String, String)>,
    pub used: BTreeSet<String>,
}

pub fn wt_change(v: &View, cs: &ChangeSet, label: &str, wt_id: &str) -> WtChange {
    let ch = changed_syms(v, cs);
    let used = v
        .refs
        .iter()
        .filter(|r| cs.get(&r.path).map(|rg| overlaps(rg, r.line, r.line)).unwrap_or(false))
        .map(|r| r.name.clone())
        .collect();
    WtChange {
        label: label.into(),
        wt_id: wt_id.into(),
        files: cs.keys().cloned().collect(),
        syms: ch.iter().map(|s| (s.path.clone(), s.name.clone())).collect(),
        used,
    }
}

#[derive(Clone, Debug)]
pub struct Clash {
    pub a: String,
    pub b: String,
    pub severity: &'static str,
    pub detail: Vec<String>,
}

fn sev_rank(s: &str) -> u8 {
    match s {
        "same_symbol" => 3,
        "dependent" => 2,
        _ => 1,
    }
}

pub fn clashes_from(w: &[WtChange]) -> Vec<Clash> {
    let mut out = vec![];
    for i in 0..w.len() {
        for j in i + 1..w.len() {
            let (a, b) = (&w[i], &w[j]);
            let same: Vec<String> = a.syms.intersection(&b.syms).map(|(p, n)| format!("{p}: {n}")).collect();
            let mut dep = vec![];
            for (x, y) in [(a, b), (b, a)] {
                for (p, n) in &x.syms {
                    if y.used.contains(n) && !y.syms.contains(&(p.clone(), n.clone())) && n.len() > 3 && !STOP_METHODS.contains(&n.as_str()) {
                        dep.push(format!("{} changes {n} ({p}), {} uses it", x.label, y.label));
                    }
                }
            }
            let files: Vec<String> = a.files.intersection(&b.files).cloned().collect();
            let (sev, detail) = if !same.is_empty() {
                ("same_symbol", same)
            } else if !dep.is_empty() {
                ("dependent", dep)
            } else if !files.is_empty() {
                ("same_file", files)
            } else {
                continue;
            };
            let mut detail = detail;
            detail.sort();
            detail.dedup();
            out.push(Clash { a: a.label.clone(), b: b.label.clone(), severity: sev, detail });
        }
    }
    out.sort_by(|x, y| sev_rank(y.severity).cmp(&sev_rank(x.severity)));
    out
}

fn clash_json(c: &Clash) -> Value {
    json!({ "a": c.a, "b": c.b, "severity": c.severity, "detail": c.detail })
}

pub fn render_clashes(cl: &[Clash], cap: usize) -> String {
    if cl.is_empty() {
        return "no clashes between active worktrees.\n".into();
    }
    let mut out = Out::new(cap);
    out.line(format!("{} clashes", cl.len()));
    for c in cl {
        out.line(format!("{} {} x {}: {}", c.severity, c.a, c.b, c.detail.iter().take(3).cloned().collect::<Vec<_>>().join("; ")));
    }
    out.finish("repo_id=<id>")
}

// ---------------------------------------------------------------- targets

#[derive(Clone, Debug)]
pub struct Wt {
    pub id: String,
    pub path: PathBuf,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub repo_id: String,
    pub repo_path: PathBuf,
    pub branch: String,
    pub wt: Option<Wt>,
}

fn wt_row(ctx: &Ctx, w: &Value) -> Wt {
    let agent = w["agent_id"].as_str().and_then(|a| ctx.db.query("SELECT name FROM agents WHERE id=?1", [a]).ok()).and_then(|r| r.first().and_then(|x| x["name"].as_str().map(str::to_string)));
    Wt { id: w["id"].as_str().unwrap_or("").into(), path: PathBuf::from(w["path"].as_str().unwrap_or("")), label: agent.unwrap_or_else(|| w["branch"].as_str().unwrap_or("worktree").to_string()) }
}

fn target_for_repo(ctx: &Ctx, repo_id: &str, wt: Option<Wt>) -> Result<Target, String> {
    let r = ctx.db.query("SELECT * FROM repos WHERE id=?1", [repo_id])?.into_iter().next().ok_or("unknown repo")?;
    Ok(Target {
        repo_id: repo_id.into(),
        repo_path: PathBuf::from(r["path"].as_str().unwrap_or("")),
        branch: r["default_branch"].as_str().unwrap_or("main").into(),
        wt,
    })
}

pub fn resolve(ctx: &Ctx, args: &Value) -> Result<Target, String> {
    if let Some(id) = args["worktree_id"].as_str() {
        if let Some(repo) = id.strip_prefix("local:") {
            let mut t = target_for_repo(ctx, repo, None)?;
            t.wt = Some(Wt { id: id.into(), path: t.repo_path.clone(), label: "Local checkout".into() });
            return Ok(t);
        }
        let w = ctx.db.query("SELECT * FROM worktrees WHERE id=?1", [id])?.into_iter().next().ok_or("unknown worktree")?;
        return target_for_repo(ctx, w["repo_id"].as_str().unwrap_or(""), Some(wt_row(ctx, &w)));
    }
    if let Some(a) = args["agent"].as_str() {
        return resolve_agent(ctx, a);
    }
    let repo = match args["repo_id"].as_str() {
        Some(r) => r.to_string(),
        None => ctx.db.query("SELECT id FROM repos ORDER BY added_at LIMIT 1", [])?.first().and_then(|r| r["id"].as_str().map(str::to_string)).ok_or("no repos yet. Add a repo first.")?,
    };
    let mut target = target_for_repo(ctx, &repo, None)?;
    if args["working_tree"].as_bool() == Some(true) {
        target.wt = Some(Wt { id: format!("local:{repo}"), path: target.repo_path.clone(), label: "Local checkout".into() });
    }
    Ok(target)
}

pub fn resolve_agent(ctx: &Ctx, agent: &str) -> Result<Target, String> {
    let a = ctx.db.query("SELECT id,repo_id FROM agents WHERE id=?1 OR name=?1", [agent])?.into_iter().next().ok_or("unknown agent")?;
    let aid = a["id"].as_str().unwrap_or("");
    if let Some(w) = ctx.db.query("SELECT * FROM worktrees WHERE agent_id=?1 AND state='active' ORDER BY created_at DESC LIMIT 1", [aid])?.into_iter().next() {
        return target_for_repo(ctx, w["repo_id"].as_str().unwrap_or(""), Some(wt_row(ctx, &w)));
    }
    let repo = a["repo_id"].as_str().ok_or("agent has no repo")?;
    target_for_repo(ctx, repo, None)
}

pub struct Loaded {
    pub header: String,
    pub t: Target,
    pub view: View,
    pub base: String,
}

async fn load(ctx: &Arc<Ctx>, t: Target) -> Result<Loaded, String> {
    let c = ctx.clone();
    let tt = t.clone();
    let (view, base) = tokio::task::spawn_blocking(move || {
        index_target(&c, &tt.repo_id, &tt.repo_path, &tt.branch, tt.wt.as_ref().map(|w| (w.id.as_str(), w.path.as_path())), true)
    })
    .await
    .map_err(|e| e.to_string())??;
    let header = format!("index {} up to date (checked {} changed files on disk)\n", &base[..base.len().min(8)], t.wt.as_ref().map(|w| delta_files(&w.path, &base).len()).unwrap_or(0));
    Ok(Loaded { header, t, view, base })
}

fn cap_of(args: &Value) -> usize {
    args["max_tokens"].as_u64().map(|n| n as usize).unwrap_or(DEFAULT_TOKENS).clamp(100, 60000)
}

fn changes_of(l: &Loaded) -> Result<ChangeSet, String> {
    let w = l.t.wt.as_ref().ok_or("impact needs a worktree or agent. Pass worktree_id or agent.")?;
    Ok(change_set(&w.path, &l.base))
}

fn sym_json(s: &Sym) -> Value {
    json!({ "path": s.path, "line": s.line, "end": s.end, "kind": s.kind, "name": s.name, "sig": s.sig, "parent": s.parent })
}

// ---------------------------------------------------------------- clashes (all worktrees)

fn changed_worktrees(ctx: &Ctx, repo_id: Option<&str>) -> Result<Vec<(String, Wt, ChangeSet, View)>, String> {
    let rows = ctx.db.query(
        "SELECT id FROM worktrees WHERE state='active' AND (?1 IS NULL OR repo_id=?1) ORDER BY created_at DESC LIMIT 60",
        params![repo_id],
    )?;
    let mut out = vec![];
    for r in rows {
        let Some(w) = ctx.db.query("SELECT * FROM worktrees WHERE id=?1", [r["id"].as_str().unwrap_or("")])?.into_iter().next() else { continue };
        let wt = wt_row(ctx, &w);
        let Ok(t) = target_for_repo(ctx, w["repo_id"].as_str().unwrap_or(""), Some(wt.clone())) else { continue };
        let Ok(base) = ensure_base(ctx, &t.repo_id, &t.repo_path, &t.branch) else { continue };
        let cs = change_set(&wt.path, &base);
        if cs.is_empty() || ensure_delta(ctx, &t.repo_id, &base, &wt.id, &wt.path, false).is_err() {
            continue;
        }
        if let Ok(v) = load_view(ctx, &t.repo_id, &base, Some(&wt.id)) {
            out.push((t.repo_id.clone(), wt, cs, v));
        }
    }
    Ok(out)
}

fn clash_changes(ctx: &Ctx, repo_id: Option<&str>) -> Result<Vec<Clash>, String> {
    let mut groups: HashMap<String, Vec<WtChange>> = HashMap::new();
    for (rid, wt, cs, v) in changed_worktrees(ctx, repo_id)? {
        groups.entry(rid).or_default().push(wt_change(&v, &cs, &wt.label, &wt.id));
    }
    Ok(groups.values().flat_map(|g| clashes_from(g)).collect())
}

fn announced() -> &'static Mutex<HashSet<String>> {
    static L: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashSet::new()))
}

fn clash_key(c: &Clash) -> String {
    format!("{}|{}|{}|{}", c.a, c.b, c.severity, c.detail.join(","))
}

/// Emit `codeindex:clashes` and log `code_clash` for clashes not announced before.
fn publish(ctx: &Ctx, cl: &[Clash]) {
    let keys: HashSet<String> = cl.iter().map(clash_key).collect();
    let mut prev = announced().lk();
    if *prev == keys {
        return;
    }
    for c in cl {
        if !prev.contains(&clash_key(c)) {
            ctx.log_event("code_clash", None, &clash_json(c));
        }
    }
    *prev = keys;
    ctx.emit("codeindex:clashes", cl.iter().map(clash_json).collect::<Vec<_>>());
}

fn watcher(ctx: Arc<Ctx>) {
    loop {
        std::thread::sleep(Duration::from_secs(30));
        if ctx.handle().is_none() {
            continue;
        }
        let n = ctx.db.query("SELECT count(*) AS n FROM worktrees WHERE state='active' AND agent_id IS NOT NULL", []).ok().and_then(|r| r.first().and_then(|x| x["n"].as_i64())).unwrap_or(0);
        if n < 2 {
            continue;
        }
        if let Ok(cl) = clash_changes(&ctx, None) {
            publish(&ctx, &cl);
        }
    }
}

// ---------------------------------------------------------------- graph

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.0).unwrap_or(".")
}

fn build_graph(ctx: &Ctx, l: &Loaded, args: &Value) -> Value {
    let v = &l.view;
    let rank = rank_names(v);
    let symbol_level = args["level"].as_str() == Some("symbol");
    let mut def_files: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for s in &v.syms {
        def_files.entry(&s.name).or_default().insert(&s.path);
    }
    let marks = if args["worktrees"].as_bool().unwrap_or(false) { changed_worktrees(ctx, Some(&l.t.repo_id)).unwrap_or_default() } else { vec![] };
    let mark_for = |path: &str, name: Option<&str>| -> Vec<Value> {
        marks
            .iter()
            .filter(|(_, _, cs, v)| match name {
                None => cs.contains_key(path),
                Some(n) => changed_syms(v, cs).iter().any(|s| s.path == path && s.name == n),
            })
            .map(|(_, w, _, _)| json!({ "worktree_id": w.id, "agent": w.label }))
            .collect()
    };
    let by_rank = |a: &&Sym, b: &&Sym| rank.get(b.name.as_str()).unwrap_or(&0.0).partial_cmp(rank.get(a.name.as_str()).unwrap_or(&0.0)).unwrap_or(std::cmp::Ordering::Equal);
    if symbol_level {
        let focus = args["path"].as_str();
        let mut syms: Vec<&Sym> = v.syms.iter().filter(|s| match focus {
            Some(f) => s.path == f,
            None => s.parent.is_empty(),
        }).collect();
        syms.sort_by(by_rank);
        let truncated = syms.len() > 200;
        syms.truncate(200);
        let ids: HashMap<&str, String> = syms.iter().map(|s| (s.name.as_str(), format!("{}:{}", s.path, s.name))).collect();
        let nodes: Vec<Value> = syms
            .iter()
            .map(|s| json!({ "id": format!("{}:{}", s.path, s.name), "label": s.name, "kind": s.kind, "path": s.path, "line": s.line, "group": dir_of(&s.path), "rank": rank.get(s.name.as_str()).unwrap_or(&0.0), "changed_by": mark_for(&s.path, Some(&s.name)) }))
            .collect();
        // Edge weight = number of references between the two symbols.
        let mut w: BTreeMap<(String, String), (u32, String)> = BTreeMap::new();
        for r in &v.refs {
            if let (Some(to), Some(from)) = (ids.get(r.name.as_str()), ids.get(r.from.as_str())) {
                if from != to {
                    w.entry((from.clone(), to.clone())).or_insert((0, r.kind.clone())).0 += 1;
                }
            }
        }
        let mut es: Vec<_> = w.into_iter().collect();
        es.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
        es.truncate(1500);
        let mut deg: HashMap<&str, (u32, u32)> = HashMap::new();
        for ((a, b), (n, _)) in &es {
            deg.entry(a.as_str()).or_default().1 += n;
            deg.entry(b.as_str()).or_default().0 += n;
        }
        let nodes: Vec<Value> = nodes.into_iter().map(|mut n| {
            let (i, o) = n["id"].as_str().and_then(|id| deg.get(id)).copied().unwrap_or_default();
            n["refs_in"] = json!(i);
            n["refs_out"] = json!(o);
            n["weight"] = json!(i + o);
            n
        }).collect();
        let edges: Vec<Value> = es.iter().map(|((a, b), (n, k))| json!({ "source": a, "target": b, "weight": n, "kind": k })).collect();
        return json!({ "nodes": nodes, "edges": edges, "truncated": truncated, "level": "symbol" });
    }
    let mut frank: HashMap<&str, f64> = HashMap::new();
    for s in &v.syms {
        *frank.entry(&s.path).or_default() += rank.get(s.name.as_str()).copied().unwrap_or(0.0);
    }
    let mut files: Vec<(&str, f64)> = frank.into_iter().collect();
    files.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
    let truncated = files.len() > 300;
    files.truncate(300);
    let keep: HashSet<&str> = files.iter().map(|f| f.0).collect();
    let nodes: Vec<Value> = files.iter().map(|(p, r)| json!({ "id": p, "label": p.rsplit('/').next().unwrap_or(p), "kind": "file", "group": dir_of(p), "rank": r, "changed_by": mark_for(p, None) })).collect();
    let mut weight: HashMap<(&str, &str), u32> = HashMap::new();
    for r in &v.refs {
        let Some(df) = def_files.get(r.name.as_str()) else { continue };
        if df.len() > 3 || !keep.contains(r.path.as_str()) {
            continue;
        }
        for d in df {
            if *d != r.path && keep.contains(d) {
                *weight.entry((r.path.as_str(), d)).or_default() += 1;
            }
        }
    }
    let mut es: Vec<((&str, &str), u32)> = weight.into_iter().collect();
    es.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let truncated = truncated || es.len() > 1500;
    es.truncate(1500);
    let mut deg: HashMap<&str, (u32, u32)> = HashMap::new();
    for ((a, b), w) in &es {
        deg.entry(a).or_default().1 += w;
        deg.entry(b).or_default().0 += w;
    }
    let mut nsyms: HashMap<&str, u32> = HashMap::new();
    for s in &v.syms {
        *nsyms.entry(&s.path).or_default() += 1;
    }
    let nodes: Vec<Value> = nodes.into_iter().map(|mut n| {
        let id = n["id"].as_str().unwrap_or("").to_string();
        let (i, o) = deg.get(id.as_str()).copied().unwrap_or_default();
        n["refs_in"] = json!(i);
        n["refs_out"] = json!(o);
        n["weight"] = json!(i + o);
        n["syms"] = json!(nsyms.get(id.as_str()).copied().unwrap_or(0));
        n
    }).collect();
    let edges: Vec<Value> = es.iter().map(|((a, b), w)| json!({ "source": a, "target": b, "weight": w, "kind": "uses" })).collect();
    json!({ "nodes": nodes, "edges": edges, "truncated": truncated, "level": "file" })
}

// ---------------------------------------------------------------- rpc

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let cap = cap_of(&args);
    match method {
        "source" | "changes" | "file_diff" => {
            let c = ctx.clone();
            let method = method.to_string();
            tokio::task::spawn_blocking(move || {
                let t = resolve(&c, &args)?;
                inspect::call(&c, &t, &method, &args)
            }).await.map_err(|e| e.to_string())?
        }
        "clashes" => {
            let c = ctx.clone();
            let rid = args["repo_id"].as_str().map(str::to_string);
            let cl = tokio::task::spawn_blocking(move || clash_changes(&c, rid.as_deref())).await.map_err(|e| e.to_string())??;
            publish(ctx, &cl);
            Ok(json!({ "text": render_clashes(&cl, cap), "clashes": cl.iter().map(clash_json).collect::<Vec<_>>() }))
        }
        "reindex" => {
            let t = resolve(ctx, &args)?;
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || {
                index_target(&c, &t.repo_id, &t.repo_path, &t.branch, t.wt.as_ref().map(|w| (w.id.as_str(), w.path.as_path())), true).map(|_| ())
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({ "ok": true }))
        }
        "status" | "map" | "find" | "refs" | "impact" | "tree" | "graph" => {
            let t = resolve(ctx, &args)?;
            let l = load(ctx, t).await?;
            match method {
                "status" => {
                    let c = |t: &str| ctx.db.query(&format!("SELECT count(*) AS n FROM {t} WHERE scope=?1"), [base_scope(&l.t.repo_id, &l.base)]).ok().and_then(|r| r.first().and_then(|x| x["n"].as_i64())).unwrap_or(0);
                    let built = ctx.db.query("SELECT built_at FROM ci_snap WHERE repo_id=?1", [&l.t.repo_id]).ok().and_then(|r| r.first().and_then(|x| x["built_at"].as_i64()));
                    let wt = l.t.wt.as_ref().map(|w| {
                        let n = ctx.db.query("SELECT count(*) AS n FROM ci_files WHERE scope=?1", [wt_scope(&w.id)]).ok().and_then(|r| r.first().and_then(|x| x["n"].as_i64())).unwrap_or(0);
                        let at = ctx.db.query("SELECT indexed_at FROM ci_wt WHERE wt_id=?1", [&w.id]).ok().and_then(|r| r.first().and_then(|x| x["indexed_at"].as_i64()));
                        json!({ "id": w.id, "label": w.label, "delta_files": n, "indexed_at": at })
                    });
                    Ok(json!({ "repo_id": l.t.repo_id, "base_commit": l.base, "files": c("ci_files"), "syms": c("ci_syms"), "refs": c("ci_refs"), "built_at": built, "worktree": wt }))
                }
                "map" => Ok(json!({ "text": l.header.clone() + &render_map(&l.view, args["path"].as_str(), args["depth"].as_u64().unwrap_or(2) as usize, cap) })),
                "find" => {
                    let q = args["query"].as_str().ok_or("missing `query`")?;
                    let items: Vec<Value> = find_items(&l.view, q, args["kind"].as_str()).into_iter().map(sym_json).collect();
                    Ok(json!({ "text": l.header.clone() + &render_find(&l.view, q, args["kind"].as_str(), cap), "items": items }))
                }
                "refs" => {
                    let s = args["symbol"].as_str().ok_or("missing `symbol`")?;
                    let (defs, refs) = refs_of(&l.view, s);
                    Ok(json!({
                        "text": l.header.clone() + &render_refs(&l.view, s, cap),
                        "defs": defs.into_iter().map(sym_json).collect::<Vec<_>>(),
                        "refs": refs.into_iter().map(|h| json!({ "path": h.r.path, "line": h.r.line, "kind": h.r.kind, "from": h.r.from, "text": h.r.text, "exact": h.exact })).collect::<Vec<_>>(),
                    }))
                }
                "impact" => {
                    let cs = changes_of(&l)?;
                    let ch = changed_syms(&l.view, &cs);
                    let deps = dependents(&l.view, &ch);
                    Ok(json!({
                        "text": l.header.clone() + &render_impact(&l.view, &cs, cap),
                        "changed": ch.into_iter().map(sym_json).collect::<Vec<_>>(),
                        "dependents": deps.iter().map(|d| json!({ "path": d.path, "name": d.name, "line": d.line, "via": d.via, "depth": d.depth, "exact": d.exact })).collect::<Vec<_>>(),
                    }))
                }
                "tree" => {
                    let rank = rank_names(&l.view);
                    let mut by: BTreeMap<&str, Vec<&Sym>> = BTreeMap::new();
                    for s in &l.view.syms {
                        by.entry(&s.path).or_default().push(s);
                    }
                    let mut files: Vec<Value> = by
                        .into_iter()
                        .map(|(p, ss)| {
                            let r: f64 = ss.iter().map(|s| rank.get(s.name.as_str()).copied().unwrap_or(0.0)).sum();
                            json!({ "path": p, "rank": r, "syms": ss.iter().map(|s| json!({ "name": s.name, "kind": s.kind, "line": s.line, "end": s.end, "parent": s.parent })).collect::<Vec<_>>() })
                        })
                        .collect();
                    let truncated = files.len() > 3000;
                    files.truncate(3000);
                    Ok(json!({ "files": files, "truncated": truncated, "base_commit": l.base }))
                }
                _ => {
                    let c = ctx.clone();
                    let a = args.clone();
                    tokio::task::spawn_blocking(move || build_graph(&c, &l, &a)).await.map_err(|e| e.to_string())
                }
            }
        }
        _ => Err(format!("codeindex.{method}: unknown method")),
    }
}

// ---------------------------------------------------------------- MCP glue (called from mcp.rs)

pub fn mcp_tools() -> Vec<Value> {
    let s = json!({ "type": "string" });
    let i = json!({ "type": "integer" });
    let t = |name: &str, desc: &str, props: Value, req: &[&str]| {
        json!({ "name": name, "description": desc, "inputSchema": { "type": "object", "properties": props, "required": req } })
    };
    vec![
        t("code_map", "USE FIRST for 'what is in this repo/dir' questions, before ls, grep or reading files. Returns a small ranked outline of directories, files and their key symbols for your worktree. Pass `path` to zoom into a directory. Verify by reading the cited lines before editing.",
          json!({ "path": s, "depth": i }), &[]),
        t("code_find", "USE BEFORE grep to locate a function, class, struct or type by name. Returns `path:line kind name signature` lines (ranked, fuzzy). Results are index hits: read the cited lines to verify before editing.",
          json!({ "query": s, "kind": s }), &["query"]),
        t("code_refs", "USE BEFORE grep to see where a symbol is defined and who calls or uses it. Returns the definition and every caller with path:line and the source line. Accepts `Parent.name`. Each ref is labelled exact (resolved via file/import/qualified path) or possible (same name only, unconfirmed). Read the cited lines to verify before editing.",
          json!({ "symbol": s }), &["symbol"]),
        t("code_impact", "What your uncommitted and committed changes touch: changed symbols vs merge-base plus the code that depends on them (2 levels). Dependents are labelled exact or possible (name match only). Use before finishing, then read the cited lines to verify.",
          json!({}), &[]),
    ]
}

pub async fn mcp_call(ctx: &Arc<Ctx>, agent: &str, name: &str, a: Value) -> Result<Value, String> {
    let t = resolve_agent(ctx, agent)?;
    let l = load(ctx, t).await?;
    let cap = cap_of(&a);
    let text = match name {
        "code_map" => render_map(&l.view, a["path"].as_str(), a["depth"].as_u64().unwrap_or(2) as usize, cap),
        "code_find" => render_find(&l.view, a["query"].as_str().ok_or("missing `query`")?, a["kind"].as_str(), cap),
        "code_refs" => render_refs(&l.view, a["symbol"].as_str().ok_or("missing `symbol`")?, cap),
        "code_impact" => render_impact(&l.view, &changes_of(&l)?, cap),
        _ => return Err(format!("unknown tool {name}")),
    };
    Ok(Value::String(l.header.clone() + &text))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn repo() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ci-{}", crate::db::new_id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        sh(&d, &["init", "-q", "-b", "main"]);
        std::fs::write(d.join("src/a.rs"), "pub struct Cfg { x: u32 }\nimpl Cfg {\n    pub fn load(&self) -> u32 {\n        helper(self.x)\n    }\n}\npub fn helper(n: u32) -> u32 {\n    n + 1\n}\npub fn other() {}\n").unwrap();
        std::fs::write(d.join("src/b.rs"), "use crate::a::helper;\npub fn caller() -> u32 {\n    helper(2)\n}\npub fn top() -> u32 {\n    caller()\n}\n").unwrap();
        sh(&d, &["add", "."]);
        sh(&d, &["commit", "-q", "-m", "init"]);
        d
    }

    fn worktree(repo: &Path, name: &str) -> PathBuf {
        let p = repo.with_file_name(format!("{}-{name}", repo.file_name().unwrap().to_string_lossy()));
        sh(repo, &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()]);
        p
    }

    fn fresh_ctx() -> Arc<Ctx> {
        let ctx = Ctx::test();
        ctx.db.with(|c| c.execute_batch(SCHEMA)).unwrap();
        ctx
    }

    #[test]
    fn parses_languages() {
        let (s, r) = parse("a.rs", "struct S;\nimpl S { fn m(&self) { foo(); } }\nfn foo() {}\nconst K: u32 = 1;\n");
        let names: Vec<(&str, &str, &str)> = s.iter().map(|x| (x.kind.as_str(), x.name.as_str(), x.parent.as_str())).collect();
        assert!(names.contains(&("struct", "S", "")), "{names:?}");
        assert!(names.contains(&("method", "m", "S")), "{names:?}");
        assert!(names.contains(&("fn", "foo", "")));
        assert!(names.contains(&("const", "K", "")));
        assert!(r.iter().any(|x| x.name == "foo" && x.kind == "call" && x.from == "m"), "{r:?}");

        let (s, r) = parse("a.ts", "import { dep } from './d';\nexport interface Item { a: number }\nexport class C extends B {\n  run() { return dep(1); }\n}\nexport const f = (x: Item) => x;\nexport const LIMIT = 3;\n");
        let n: Vec<(&str, &str)> = s.iter().map(|x| (x.kind.as_str(), x.name.as_str())).collect();
        for w in [("interface", "Item"), ("class", "C"), ("method", "run"), ("fn", "f"), ("const", "LIMIT")] {
            assert!(n.contains(&w), "{w:?} in {n:?}");
        }
        assert!(r.iter().any(|x| x.name == "dep" && x.kind == "import"));
        assert!(r.iter().any(|x| x.name == "dep" && x.kind == "call" && x.from == "run"));
        assert!(r.iter().any(|x| x.name == "Item" && x.kind == "type"));

        let (s, _) = parse("m.py", "MAX = 3\nclass A:\n    def go(self):\n        pass\ndef f():\n    pass\n");
        let n: Vec<(&str, &str)> = s.iter().map(|x| (x.kind.as_str(), x.name.as_str())).collect();
        for w in [("const", "MAX"), ("class", "A"), ("method", "go"), ("fn", "f")] {
            assert!(n.contains(&w), "{w:?} in {n:?}");
        }
        let (s, r) = parse("m.go", "package m\ntype T struct{}\nfunc (t *T) Run() { helper() }\nfunc helper() {}\n");
        assert!(s.iter().any(|x| x.kind == "method" && x.name == "Run" && x.parent == "T"), "{s:?}");
        assert!(s.iter().any(|x| x.kind == "struct" && x.name == "T"));
        assert!(r.iter().any(|x| x.name == "helper" && x.kind == "call"));
    }

    #[test]
    fn base_delta_shadow_and_impact() {
        let ctx = fresh_ctx();
        let r = repo();
        let (v, base) = index_target(&ctx, "r1", &r, "main", None, true).unwrap();
        assert!(v.syms.iter().any(|s| s.name == "helper"));
        let (defs, refs) = refs_of(&v, "helper");
        assert_eq!(defs.len(), 1);
        assert!(refs.iter().any(|x| x.r.path == "src/b.rs" && x.r.from == "caller" && x.exact));

        let w = worktree(&r, "feat");
        std::fs::write(w.join("src/a.rs"), "pub struct Cfg { x: u32 }\nimpl Cfg {\n    pub fn load(&self) -> u32 {\n        helper(self.x)\n    }\n}\npub fn helper(n: u32) -> u32 {\n    n + 2\n}\n").unwrap();
        std::fs::write(w.join("src/c.rs"), "pub fn fresh() { crate::a::helper(1); }\n").unwrap();
        let (v2, base2) = index_target(&ctx, "r1", &r, "main", Some(("w1", &w)), true).unwrap();
        assert_eq!(base, base2);
        assert!(!v2.syms.iter().any(|s| s.name == "other"), "delta shadows a.rs (other removed)");
        assert!(v2.syms.iter().any(|s| s.name == "fresh"));
        assert!(v2.syms.iter().any(|s| s.name == "caller"), "unchanged file still from base");
        let (v1, _) = index_target(&ctx, "r1", &r, "main", None, true).unwrap();
        assert!(v1.syms.iter().any(|s| s.name == "other"), "base untouched by delta");
        assert_eq!(ensure_delta(&ctx, "r1", &base, "w1", &w, true).unwrap(), 0, "incremental: nothing to reparse");

        let cs = change_set(&w, &base);
        let chs = changed_syms(&v2, &cs);
        let ch: Vec<&str> = chs.iter().map(|s| s.name.as_str()).collect();
        assert!(ch.contains(&"helper") && ch.contains(&"fresh") && !ch.contains(&"top"), "{ch:?}");
        let deps = dependents(&v2, &chs);
        assert!(deps.iter().any(|d| d.name == "caller" && d.depth == 1), "{deps:?}");
        assert!(deps.iter().any(|d| d.name == "top" && d.depth == 2), "{deps:?}");
        let txt = render_impact(&v2, &cs, 1500);
        assert!(txt.contains("src/b.rs") && txt.contains("helper"), "{txt}");
    }

    #[test]
    fn graph_weights() {
        let ctx = fresh_ctx();
        let r = repo();
        let (view, base) = index_target(&ctx, "r1", &r, "main", None, true).unwrap();
        let l = Loaded { header: String::new(), t: Target { repo_id: "r1".into(), repo_path: r.clone(), branch: "main".into(), wt: None }, view, base };
        let g = build_graph(&ctx, &l, &json!({}));
        let e = g["edges"].as_array().unwrap().iter().find(|e| e["source"] == "src/b.rs" && e["target"] == "src/a.rs").expect("b uses a");
        assert!(e["weight"].as_u64().unwrap() >= 2, "import + call: {e}");
        let a = g["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "src/a.rs").unwrap();
        assert!(a["refs_in"].as_u64().unwrap() >= 2 && a["weight"].as_u64().unwrap() >= 2, "{a}");
        assert!(a["syms"].as_u64().unwrap() >= 4, "{a}");
        let g = build_graph(&ctx, &l, &json!({ "level": "symbol" }));
        let e = g["edges"].as_array().unwrap().iter().find(|e| e["source"] == "src/b.rs:top" && e["target"] == "src/b.rs:caller").expect("top calls caller");
        assert_eq!(e["weight"], 1);
        let c = g["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "src/b.rs:caller").unwrap();
        assert!(c["refs_in"].as_u64().unwrap() >= 1 && c["refs_out"].as_u64().unwrap() >= 1, "{c}");
    }

    #[test]
    fn clash_detection() {
        let ctx = fresh_ctx();
        let r = repo();
        let mk = |name: &str, a: &str, b: Option<&str>| {
            let w = worktree(&r, name);
            std::fs::write(w.join("src/a.rs"), a).unwrap();
            if let Some(b) = b {
                std::fs::write(w.join("src/b.rs"), b).unwrap();
            }
            let (v, base) = index_target(&ctx, "r1", &r, "main", Some((name, &w)), true).unwrap();
            let cs = change_set(&w, &base);
            wt_change(&v, &cs, name, name)
        };
        let base_a = std::fs::read_to_string(r.join("src/a.rs")).unwrap();
        let w1 = mk("one", &base_a.replace("n + 1", "n + 5"), None);
        let w2 = mk("two", &base_a.replace("n + 1", "n * 9"), None);
        let w3 = mk("three", &base_a, Some("pub fn caller() -> u32 {\n    helper(3) + 1\n}\npub fn top() -> u32 {\n    caller()\n}\n"));
        let cl = clashes_from(&[w1.clone(), w2.clone(), w3.clone()]);
        let get = |a: &str, b: &str| cl.iter().find(|c| (c.a == a && c.b == b) || (c.a == b && c.b == a)).map(|c| c.severity);
        assert_eq!(get("one", "two"), Some("same_symbol"), "{cl:?}");
        assert_eq!(get("one", "three"), Some("dependent"), "{cl:?}");
        assert_eq!(cl[0].severity, "same_symbol", "sorted by severity");
        let w4 = mk("four", &format!("{base_a}pub fn zed() {{}}\n"), None);
        let cl = clashes_from(&[w1, w4]);
        assert_eq!(cl[0].severity, "same_file", "{cl:?}");
    }

    #[test]
    fn exact_vs_possible() {
        let mut v = View::default();
        for (p, src) in [
            ("src/a.rs", "pub fn load() {}\n"),
            ("src/other.rs", "pub fn load() {}\n"),
            ("src/b.rs", "use crate::a::load;\npub fn go() {\n    load();\n}\n"),
            ("src/c.rs", "pub fn y() {\n    load();\n}\n"),
            ("src/d.rs", "pub fn z() {\n    a::load();\n}\n"),
        ] {
            let (s, r) = parse(p, src);
            v.syms.extend(s);
            v.refs.extend(r);
        }
        let (_, hits) = refs_of(&v, "load");
        let ex = |p: &str| hits.iter().find(|h| h.r.path == p && h.r.kind != "import").map(|h| h.exact);
        assert_eq!(ex("src/d.rs"), Some(true));
        assert_eq!(ex("src/c.rs"), Some(false), "no import: possible only");
        let t = render_refs(&v, "load", 100000);
        assert!(t.find("exact:").unwrap() < t.find("possible:").unwrap(), "{t}");
    }

    #[test]
    fn queries_are_budgeted() {
        let ctx = fresh_ctx();
        let r = repo();
        let (v, _) = index_target(&ctx, "r1", &r, "main", None, true).unwrap();
        let m = render_map(&v, None, 2, 1500);
        assert!(m.contains("src/") && m.contains("a.rs"), "{m}");
        let m = render_map(&v, None, 2, 20);
        assert!(m.contains("more, narrow with"), "{m}");
        let f = render_find(&v, "helpr", None, 1500);
        assert!(f.contains("src/a.rs:") && f.contains("7 fn helper"), "{f}");
        let x = render_refs(&v, "Cfg.load", 1500);
        assert!(x.contains("def src/a.rs:3"), "{x}");
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `BENCH_REPO=/path BENCH_SYMS=a,b,c cargo test bench_repo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_repo() {
        let Ok(repo) = std::env::var("BENCH_REPO") else { return };
        let syms: Vec<String> = std::env::var("BENCH_SYMS").unwrap_or_default().split(',').map(str::to_string).collect();
        let ctx = Ctx::test();
        ctx.db.with(|c| c.execute_batch(SCHEMA)).unwrap();
        let repo = PathBuf::from(repo);
        let br = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        let t = Instant::now();
        let (v, _) = index_target(&ctx, "b", &repo, &br, None, true).unwrap();
        println!("index: {} files-syms {} refs {} in {:?}", v.syms.len(), v.syms.len(), v.refs.len(), t.elapsed());
        let map = render_map(&v, None, 2, 60000);
        println!("MAP tokens {}", map.len() / 4);
        let (mut tp, mut tr, mut tn) = (0usize, 0usize, 0usize);
        for s in syms.iter().filter(|s| !s.is_empty()) {
            let out = render_refs(&v, s, 60000);
            let (defs, hits) = refs_of(&v, s);
            let ex: Vec<&RefHit> = hits.iter().filter(|h| h.exact).collect();
            // ground truth proxy: rg -w lines excluding definitions
            let rg = Command::new("rg").args(["-n", "-w", "--no-heading", "-g", "*.{rs,ts,tsx,js,jsx,py,go}", "-g", "!node_modules", "-g", "!target", s]).current_dir(&repo).output().unwrap();
            let rgs = String::from_utf8_lossy(&rg.stdout).to_string();
            let rg_lines = rgs.lines().count();
            let rg_files: HashSet<&str> = rgs.lines().filter_map(|l| l.split(':').next()).collect();
            let mut read_chars = 0usize;
            for f in &rg_files {
                read_chars += std::fs::metadata(repo.join(f)).map(|m| m.len() as usize).unwrap_or(0);
            }
            let rgset: HashSet<(String, u32)> = rgs.lines().filter_map(|l| { let mut p = l.splitn(3, ':'); Some((p.next()?.to_string(), p.next()?.parse().ok()?)) }).collect();
            let idx: HashSet<(String, u32)> = hits.iter().map(|h| (h.r.path.clone(), h.r.line)).chain(defs.iter().map(|d| (d.path.clone(), d.line))).collect();
            let recall_hit = rgset.iter().filter(|x| idx.contains(*x)).count();
            let extra = idx.iter().filter(|x| !rgset.contains(*x)).count();
            println!("{s}: defs {} exact {} possible {} | rg lines {rg_lines} files {} | rg-recall {}/{} idx-not-in-rg {extra} | refs tokens {} vs grep {} / read-all {}", defs.len(), ex.len(), hits.len() - ex.len(), rg_files.len(), recall_hit, rgset.len(), out.len() / 4, rgs.len() / 4, read_chars / 4);
            tp += ex.len();
            tr += rg_lines;
            tn += out.len() / 4;
        }
        println!("TOTAL exact {tp} rg lines {tr} refs tokens {tn}");
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
