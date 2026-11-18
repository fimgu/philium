//! Agents: named workers, one HAL process each (SPEC 5.1, 6.1, 7.2).
//!
//! Every agent gets: its own HAL (own data dir and session), its own gateway
//! token (attribution + provider allowlist), its own MCP token (Philium's agent
//! tools), its own worktree when it works on a repo, a port block, a scratch dir
//! and a TMPDIR. Agents on untrusted providers always run Strict: Seatbelt
//! sandbox, egress proxy, no subscriptions, only that provider.
//!
//! HAL's built-in shell is denied by permission rules, so every command goes
//! through Philium's `shell_run` MCP tool (visible terminal, attributed, reapable).
//!
//! rpc:
//!   list {}                                   -> [agent]
//!   get {id}                                  -> agent + live info
//!   spawn {name?, role?, model?, repo_id?, task_id?, brief?, parent_id?, strict?} -> agent
//!   send {id, text}                           -> HAL turn ids
//!   cancel {id}                               -> stop current turn
//!   stop {id}                                 -> stop HAL, keep agent + worktree
//!   resume {id}                               -> restart a stopped agent's HAL, same session
//!   remove {id, force?}                       -> teardown (may need confirmation) + delete
//!   events {id, after?, limit?}               -> HAL event replay
//!   messages {id}                             -> HAL conversation
//!   permissions {id}                          -> pending approvals
//!   permission {id, request_id, decision: approve|deny, scope?}
//!   set_model {id, model}
//!   set_effort {id, effort} / get_effort {id}  -> reasoning effort (default|low|medium|high|...), persisted
//!   diff {id}                                 -> git diff of the agent's workspace
//!   names {}                                  -> suggested free names
//! events: `agents:changed` {id}, `agents:events` {id, events:[...]} (batched ~10 Hz)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

use crate::ctx::{self, Ctx};
use crate::db::{new_id, now};
use crate::hal::{self, Hal};
use crate::providers::{self, ConfigOpts};

#[path = "agent_history.rs"]
pub(crate) mod history;

/// UI-only: image bytes the user sent in this agent's own conversation.
pub fn sent_attachment(ctx: &Ctx, id: &str, sha256: &str) -> Result<(String, Vec<u8>), String> {
    history::attachment(ctx, id, sha256)
}

/// Live state for one running agent (not persisted).
pub struct Runtime {
    pub hal: Arc<Hal>,
    pub session: String,
    pub name: String,
    pub cwd: PathBuf,
    pub strict: bool,
    /// Environment for commands the agent runs (shell_run terminals).
    pub env: Vec<(String, String)>,
    /// Seatbelt policy for Strict agents; commands they run are wrapped too.
    pub policy: Option<crate::sandbox::Policy>,
    pump: tokio::task::JoinHandle<()>,
}

type Runtimes = Mutex<HashMap<String, Arc<Runtime>>>;

fn runtimes() -> &'static Runtimes {
    static R: OnceLock<Runtimes> = OnceLock::new();
    R.get_or_init(Default::default)
}

// Admission must happen before spawning: an unfinished provision has no Runtime yet.
fn provisioning() -> &'static Mutex<HashSet<String>> {
    static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

struct ProvisionAdmission(String);

impl ProvisionAdmission {
    fn acquire(id: &str) -> Result<Self, String> {
        if !provisioning().lk().insert(id.to_string()) {
            return Err("agent is already provisioning".into());
        }
        Ok(Self(id.to_string()))
    }
}

impl Drop for ProvisionAdmission {
    fn drop(&mut self) {
        provisioning().lk().remove(&self.0);
    }
}

pub fn runtime(id: &str) -> Option<Arc<Runtime>> {
    runtimes().lk().get(id).cloned()
}

const NAMES: &[&str] = &[
    "lion", "otter", "falcon", "lynx", "heron", "orca", "badger", "raven", "ibis", "puma", "wren", "bison",
    "gecko", "manta", "koala", "panda", "tapir", "okapi", "egret", "dingo", "marten", "condor", "jaguar",
    "narwhal", "osprey", "quokka", "stoat", "toucan", "walrus", "yak", "zebra", "beaver", "cobra", "dove",
    "eagle", "ferret", "gazelle", "hare", "impala", "jackal", "kestrel", "lemur", "moose", "newt", "owl",
    "pelican", "robin", "seal", "tiger", "vole", "wolf", "fox", "crane", "finch", "mole", "moth", "swan",
];

/// Agent names are lowercase slugs: they appear in paths, branches, env vars.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !n.starts_with('-')
        && !matches!(n, "you" | "all" | "philium" | "user" | "system")
}

pub fn free_names(ctx: &Ctx, n: usize) -> Vec<String> {
    let taken: Vec<String> = ctx
        .db
        .query("SELECT name FROM agents", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    let mut out = vec![];
    for round in 0.. {
        for base in NAMES {
            let name = if round == 0 { base.to_string() } else { format!("{base}-{}", round + 1) };
            if !taken.contains(&name) && !out.contains(&name) {
                out.push(name);
                if out.len() >= n {
                    return out;
                }
            }
        }
    }
    out
}

pub fn get_row(ctx: &Ctx, id_or_name: &str) -> Option<Value> {
    ctx.db
        .query("SELECT * FROM agents WHERE id=?1 OR name=?1", [id_or_name])
        .ok()?
        .into_iter()
        .next()
}

fn set_state(ctx: &Ctx, id: &str, state: &str) {
    let _ = ctx.db.exec(
        "UPDATE agents SET state=?2, last_active_at=?3 WHERE id=?1",
        rusqlite::params![id, state, now()],
    );
    ctx.emit("agents:changed", json!({ "id": id, "state": state }));
}


/// Pick a free block of 20 ports, starting at 42000.
fn port_base(ctx: &Ctx) -> i64 {
    let used: Vec<i64> = ctx
        .db
        .query("SELECT port_base FROM agents WHERE port_base IS NOT NULL", [])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r["port_base"].as_i64())
        .collect();
    (0..).map(|i| 42000 + i * 20).find(|p| !used.contains(p)).unwrap()
}

/// HAL permission rules for an agent. Last matching rule wins.
pub fn permission_rules(strict: bool) -> Value {
    let mut rules = vec![
        json!({"action": "read", "resource": "*", "effect": "allow"}),
        // Secrets never readable: they could end up in a provider request.
        json!({"action": "read", "resource": "**/.env*", "effect": "deny"}),
        json!({"action": "read", "resource": "**/*.pem", "effect": "deny"}),
        json!({"action": "read", "resource": "**/*.key", "effect": "deny"}),
        json!({"action": "read", "resource": "**/id_*", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.ssh/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.aws/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.kube/**", "effect": "deny"}),
        json!({"action": "read", "resource": "**/.config/gh/**", "effect": "deny"}),
        // Edits inside the workspace (relative paths) are fine; absolute
        // paths are outside it and ask; parent escapes are denied.
        json!({"action": "edit", "resource": "**", "effect": "allow"}),
        json!({"action": "edit", "resource": "/**", "effect": "ask"}),
        json!({"action": "edit", "resource": "../**", "effect": "deny"}),
        json!({"action": "edit", "resource": "**/.env*", "effect": "deny"}),
        // All execution goes through Philium (shell_run): visible and reapable.
        json!({"action": "shell", "resource": "*", "effect": "deny"}),
        json!({"action": "mcp", "resource": "philium/*", "effect": "allow"}),
        // Browser capability makes tools callable, not pre-approved. The
        // specific rule must follow the broad MCP allow (last match wins).
        json!({"action": "mcp", "resource": "philium/browser_*", "effect": "ask"}),
        json!({"action": "subagent", "resource": "*", "effect": "allow"}),
        json!({"action": "browser", "resource": "*", "effect": "deny"}),
    ];
    if strict {
        // Untrusted provider: the OS sandbox already hides private data, so
        // reads stay open (tooling, libraries). No nested agents.
        rules.push(json!({"action": "subagent", "resource": "*", "effect": "deny"}));
    }
    json!({ "default": "ask", "ask_timeout_secs": 3600, "rules": rules })
}

pub fn desk_instructions() -> String {
    "You are running inside Philium. Philium's conversation UI natively renders rich Markdown blocks:\n\
     - Fenced `mermaid` blocks (` ```mermaid `) render live as interactive vector diagrams (flowcharts, sequence, state, class, ER, architecture, mindmap, gantt). Use `mermaid` diagrams proactively whenever explaining architecture, control flow, hierarchies, state machines, or execution plans.\n\
     - Fenced `diff` or `patch` blocks render with syntax-colored +/- diff highlighting.\n\
     - Fenced `svg` blocks render inline as sanitized vector graphics with a source toggle.\n\
     - Fenced `math` / `latex` blocks and `$$...$$` blocks render formatted mathematical notation.\n\
     - Fenced `csv` or `json-table` blocks render as interactive data tables.\n\
     - Standard GFM tables, task lists (`- [x]`), and blockquotes render with native styling.\n"
        .to_string()
}

/// Shared invariant instructions placed FIRST in the system prompt so all agents on the same
/// repo share an identical token prefix for LLM KV caching (OpenAI prefix cache & Anthropic ephemeral cache).
fn shared_instructions(strict: bool) -> String {
    let mut s = String::from(
        "You are an autonomous software engineering agent inside Philium, a multi-agent workstation where a human engineer commands a hierarchy of Orchestrators, Reviewers, and Worker agents in parallel. \
         The human sees your conversation, your interactive terminal, your browser, and your AST code footprint live. \
         Philium's MCP tools (prefixed `mcp_philium_`) are already connected: call them directly, no setup needed.\n\
         \n\
         ## Hierarchy Architecture (Human -> Orchestrator -> Workers / Reviewers)\n\
         - **Human (`\"you\"`)**: Top of the food chain. Gives high-level goals, resolves genuine blockers, and approves final PRs. Keep human interruptions minimal and high-signal.\n\
         - **Orchestrator (`role = \"orchestrator\"`)**: Owns a cluster/mission. Decomposes the goal into board tasks (`board_create`), spawns a bounded pool of child workers/reviewers (`agent_spawn`), monitors progress (`agent_status`, `mail_read`), checks cross-worktree AST clashes (`code_impact`, `repo_context`), verifies worker branches (`repo_context {branch}`), and sends a single consolidated synthesis to `\"you\"`.\n\
         - **Worker (`role = \"worker\"` / `\"helper\"`)**: Owns an isolated git worktree and branch. Executes its assigned task end-to-end, runs focused tests (`shell_run`), checks `code_impact`, and reports completion or blockers to its parent Orchestrator via `mail_send` (or to `\"you\"` if it has no parent).\n\
         - **Reviewer (`role = \"reviewer\"`)**: Audits a branch or GitHub PR using `repo_context {branch/pr}`, `code_impact`, `code_symbol`, and test execution, then reports a structured verdict.\n\
         \n\
         ## Rich Rendering (Native in Philium UI)\n\
         - **Mermaid diagrams (` ```mermaid `)**: Rendered live into interactive SVG diagrams with zoom and source toggles. Use `mermaid` blocks for architecture overviews, cluster/task plans, call graphs, sequence flows, and PR impact summaries.\n\
         - **Diffs (` ```diff `)**, **SVGs (` ```svg `)**, **Math (` ```math `)**, **Data Tables (` ```csv ` / GFM tables)**: Rendered natively. Prefer structured tables and `mermaid` diagrams over walls of prose.\n\
         \n\
         ## Pick the Right Tool\n\
         - Web pages (\"open X\", \"go to google.com\", check a site, sign-in flows, test a web app): use YOUR browser. \
         `browser_open {url}`, then `browser_snapshot` to read the page and get numbered refs, then `browser_click` / `browser_type` / `browser_press` with those refs. \
         Snapshot again after every navigation. `browser_screenshot` when layout matters. Philium asks the human to approve browser tool calls; wait for that decision, and respect a denial. Never use curl or wget to \"visit\" a page for the human.\n\
         - Commands: `shell_run` types into your persistent shell (cd and exports persist). Dev servers and watchers: `shell_run` with `background=true`, then open the URL in your browser. \
         A second interactive session: `shell_open {name}`, then `shell_run` with `shell={name}`. The built-in shell tool is disabled.\n\
         - Code questions: check the `## Codebase map` in your prompt first, then use `code_outline` / `code_map` (file or directory structure with line spans), `code_symbol` (read a symbol's exact AST source + callers/callees in one call), `code_find` (locate a symbol), `code_refs` (bidirectional call graph: callees and callers), and `code_impact` (what your change touches + cross-agent clashes). Use these before grep or reading whole files.\n\
         - Branches, worktrees, GitHub PRs & past sessions: check `## Repo landscape` in your prompt first, then use `repo_context` (all git worktrees including external checkouts, local/remote branches with ahead/behind, cross-worktree clashes, open/merged GitHub PRs, or pass `branch` / `pr` for a structural AST symbol diff of any branch or PR) and `session_search` (search Philium session records bound to your repository for prior decisions or unfinished work).\n\
         - Other agents and the human: `board_*` for tasks (claim before working, keep the lease alive), `mail_send` / `mail_read` for messages. `mail_send` to `\"you\"` reaches the human.\n\
         - Jobs like commit, open_pr, review_pr, release: `playbook_get` first and follow it. Finished work lands as a GitHub pull request.\n\
         \n\
         - Run builds and test suites with `background=true` and poll `shell_read`. Foreground calls return after at most 30 seconds without killing the command. A running result or transport error does not mean the command stopped: inspect the existing terminal before retrying. Never enqueue duplicate tests. Keep progress visible rather than piping live output to tail.\n\
         - Minimize model round trips: batch independent inspections, use bounded file ranges and focused tests before broad suites. Do not repeatedly sleep in foreground tools while waiting for a job.\n\
         - Before reporting completion, inspect every started job's finished state and `exit_code`. Report changed files, checks actually run, failures and remaining blockers. Idle is not proof that the task succeeded.\n\
         ## Rules\n\
         - Do what was asked with the most direct tool. Do not inspect Philium itself (its source, config, env or proxy) unless the task is about Philium.\n\
         - If a tool returns an error you cannot fix from your side, tell the human the exact error in one line and stop. Do not work around Philium's limits.\n\
         - Never add Co-authored-by trailers, \"Generated with\" lines or agent signatures to commits or PRs.\n\
         - Servers use the 20 ports starting at $PHILIUM_PORT_BASE. Throwaway files go in $TMPDIR.\n\
         - Register anything you create outside Philium's tools (containers, clusters, processes) with `resource_register` so it is cleaned up.\n"
    );
    if strict {
        s.push_str("- You run in Strict mode. You can read and run every tool on this machine except private data (credentials, personal folders). You can write your worktree, $TMPDIR and folders granted for this repo. \
            Shell network goes through Philium's proxy, which only allows approved hosts (package registries and similar): a 403 \"host not on allowlist\" is expected, so use your browser for websites instead.\n");
    }
    s
}

/// Human-configured standing rules are intentional instructions. Never append archived memories here.
pub(crate) fn prompt_rules(ctx: &Ctx, strict: bool) -> String {
    format!("{}\n{}", shared_instructions(strict), crate::library::rules(ctx))
}

/// Per-agent identity tail placed after the shared repo cache prefix.
fn agent_identity_tail(name: &str, role: &str, parent_name: Option<&str>, worktree: Option<&Path>) -> String {
    let mut s = format!("## Agent Identity & Assignment\n- Your agent name is \"{name}\" (role: `{role}`).\n");
    if let Some(p) = parent_name {
        s.push_str(&format!(
            "- Your parent Orchestrator is \"{p}\". Send progress updates, questions, and completion reports to \"{p}\" via `mail_send {{to: \"{p}\", body: ...}}` rather than interrupting the human directly.\n"
        ));
    }
    match role {
        "orchestrator" | "coordinator" => {
            s.push_str(
                "- **Orchestrator Protocol**:\n\
                 1. Inspect `## Repo landscape` and `## Codebase map` (and `repo_context` / `session_search` if needed).\n\
                 2. Break multi-part missions into clear board tasks (`board_create`) and spawn focused child workers (`agent_spawn` with `role: \"worker\"` or `\"reviewer\"` and `task_id`). Keep worker concurrency bounded (2-6 workers per cluster).\n\
                 3. Coordinate workers via `agent_status` and `mail_read`. Before finishing, verify worker branches with `repo_context {branch}` and `code_impact` to ensure zero unresolved AST clashes.\n\
                 4. Present the human with a clean executive summary (use a `mermaid` diagram + GFM summary table).\n",
            );
        }
        "reviewer" => {
            s.push_str(
                "- **Reviewer Protocol**: Use `repo_context {pr}` or `repo_context {branch}` to inspect the structural AST symbol diff, `code_impact` to check blast radius and clashes, and run real verification checks via `shell_run` before delivering a concise verdict table.\n",
            );
        }
        _ => {}
    }
    if let Some(w) = worktree {
        s.push_str(&format!("- Your git worktree is {} on your own branch. Stay inside it.\n", w.display()));
    }
    s
}

pub struct SpawnReq {
    pub name: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub repo_id: Option<String>,
    pub task_id: Option<String>,
    pub brief: Option<String>,
    pub parent_id: Option<String>,
    pub strict: bool,
}

/// Create the agent row and start provisioning in the background.
pub fn spawn(ctx: &Arc<Ctx>, req: SpawnReq) -> Result<Value, String> {
    if let Some(parent) = req.parent_id.as_deref() {
        helper_scope(ctx, parent, req.repo_id.as_deref(), req.model.as_deref(), req.task_id.as_deref(), req.strict)?;
    }
    task_scope(ctx, req.repo_id.as_deref(), req.task_id.as_deref())?;
    let (provider, model) = providers::resolve(ctx, req.model.as_deref())?;
    let model_ref = format!("{}/{}", provider.id, model);
    let strict = req.strict || !provider.trusted;
    if strict && provider.kind.is_subscription() {
        return Err("subscriptions cannot run in Strict mode".into());
    }
    let name = match req.name.filter(|n| !n.trim().is_empty()) {
        Some(n) => {
            let n = n.trim().to_lowercase();
            if !valid_name(&n) {
                return Err("names use a-z, 0-9 and -, up to 32 characters".into());
            }
            if get_row(ctx, &n).is_some() {
                return Err(format!("an agent named {n} already exists"));
            }
            n
        }
        None => free_names(ctx, 1).remove(0),
    };
    if let Some(r) = &req.repo_id {
        if ctx.db.query("SELECT id FROM repos WHERE id=?1", [r])?.is_empty() {
            return Err("unknown repo".into());
        }
    }
    let id = new_id();
    let t = now();
    ctx.db.exec(
        "INSERT INTO agents(id,name,role,model_ref,state,parent_id,repo_id,task_id,strict,port_base,created_at,last_active_at)
         VALUES(?1,?2,?3,?4,'provisioning',?5,?6,?7,?8,?9,?10,?10)",
        rusqlite::params![
            id,
            name,
            req.role.as_deref().unwrap_or("worker"),
            model_ref,
            req.parent_id,
            req.repo_id,
            req.task_id,
            strict as i64,
            port_base(ctx),
            t
        ],
    )?;
    ctx.log_event("agent_spawned", Some(&id), &json!({ "name": name, "model": model_ref, "strict": strict }));
    ctx.emit("agents:changed", json!({ "id": id }));
    let c = ctx.clone();
    let id2 = id.clone();
    let brief = req.brief.clone();
    let admission = ProvisionAdmission::acquire(&id)?;
    tokio::spawn(async move {
        let _admission = admission;
        if let Err(e) = provision(&c, &id2, true, brief).await {
            let _ = c.db.kv_set(&format!("agent_error.{id2}"), &e);
            c.log_event("agent_failed", Some(&id2), &json!({ "error": e }));
            set_state(&c, &id2, "failed");
        }
    });
    Ok(get_row(ctx, &id).unwrap_or(Value::Null))
}

// NULL denotes the scratch workspace, not permission to link any repo's task.
fn task_scope(ctx: &Ctx, repo: Option<&str>, task: Option<&str>) -> Result<(), String> {
    if let Some(id) = task {
        if ctx.db.query("SELECT id FROM tasks WHERE id=?1 AND repo_id IS ?2", rusqlite::params![id,repo])?.is_empty() {
            return Err("Choose a task from the agent's workspace".into());
        }
    }
    Ok(())
}

fn helper_scope(ctx: &Ctx, parent: &str, repo: Option<&str>, model: Option<&str>, task: Option<&str>, strict: bool) -> Result<(), String> {
    let p = ctx.db.query("SELECT a.model_ref,a.strict,r.id AS repo FROM agents a JOIN repos r ON r.id=a.repo_id WHERE a.id=?1", [parent])?
        .into_iter().next().ok_or("Helper needs a parent with a registered workspace")?;
    if repo != p["repo"].as_str() || model.is_none() || model != p["model_ref"].as_str() || (p["strict"] == 1 && !strict) {
        return Err("Helpers must keep the parent's workspace, model recipient and restrictions".into());
    }
    if let Some(task) = task {
        if ctx.db.query("SELECT id FROM tasks WHERE id=?1 AND repo_id=?2", rusqlite::params![task,repo])?.is_empty() {
            return Err("Helper task is outside the parent's workspace".into());
        }
    }
    Ok(())
}

async fn provision(ctx: &Arc<Ctx>, id: &str, fresh: bool, brief: Option<String>) -> Result<(), String> {
    let row = get_row(ctx, id).ok_or("agent vanished")?;
    let name = row["name"].as_str().unwrap_or_default().to_string();
    let model_ref = row["model_ref"].as_str().unwrap_or_default().to_string();
    let strict = row["strict"].as_i64() == Some(1);
    task_scope(ctx, row["repo_id"].as_str(), row["task_id"].as_str())?;
    if let Some(parent) = row["parent_id"].as_str() {
        helper_scope(ctx, parent, row["repo_id"].as_str(), Some(&model_ref), row["task_id"].as_str(), strict)?;
    }
    let port = row["port_base"].as_i64().unwrap_or(42000);
    let (provider, _) = providers::resolve(ctx, Some(&model_ref))?;
    if strict {
        crate::sandbox::available()?;
    }

    // Dirs (scratch is a ledger resource deleted with the agent).
    let dir = ctx.agent_dir(&name);
    let (tmp, scratch, hal_dir) = (dir.join("tmp"), dir.join("scratch"), dir.join("hal"));
    for d in [&tmp, &scratch, &hal_dir, &scratch.join("home")] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    if fresh {
        crate::reaper::register(ctx, Some(id), "scratch_dir", &dir.to_string_lossy(), json!({ "agent": name }));
    }

    // Worktree.
    let mut worktree: Option<PathBuf> = None;
    if let Some(repo) = row["repo_id"].as_str() {
        let existing = ctx
            .db
            .query("SELECT path FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [id])?;
        worktree = match existing.first().and_then(|r| r["path"].as_str()) {
            Some(p) if !Path::new(p).is_dir() => {
                return Err(format!(
                    "The agent's worktree folder {p} is missing (deleted or moved). Remove the agent, or restore the folder and press Resume."
                ));
            }
            Some(_) => Some(crate::codeindex::resolve_agent(ctx, id)?.wt.ok_or("Worktree unavailable")?.path),
            None => {
                let (c, repo, n, aid) = (ctx.clone(), repo.to_string(), name.clone(), id.to_string());
                let wt = tokio::task::spawn_blocking(move || {
                    crate::workspace::create_worktree(&c, &repo, Some(&aid), &n, None)
                })
                .await
                .map_err(|e| e.to_string())??;
                Some(wt.path)
            }
        };
    }
    let cwd = worktree.clone().unwrap_or_else(|| scratch.clone());

    // Tokens: gateway (model traffic, attribution) and MCP (agent tools).
    let only = if strict || row["parent_id"].is_string() { Some(vec![provider.id.clone()]) } else { None };
    let gw_token = ctx.gateway.issue_token(&name, only.clone());
    let mcp_token = crate::mcp::issue_token(id);

    let setup = providers::hal_setup(
        ctx,
        &hal_dir.join("hal"),
        &ConfigOpts { token: Some(gw_token), only, no_subscriptions: strict, default_ref: Some(model_ref.clone()) },
    );
    let mut config = setup.config;
    config["server"]["workspace"] = json!(cwd);
    config["permissions"] = permission_rules(strict);
    config["limits"] = json!({ "workspace": { "allow_outside": false } });
    let role = row["role"].as_str().unwrap_or("worker").to_string();
    let parent_name = row["parent_id"]
        .as_str()
        .and_then(|pid| get_row(ctx, pid))
        .and_then(|pr| pr["name"].as_str().map(str::to_string));
    let mut extra = prompt_rules(ctx, strict);
    // Personal/imported memories are not standing rules. Agents may query ID-bound memory only.
    if let Some(repo_id) = row["repo_id"].as_str() {
        if let Some(cmap) = crate::codeindex::prompt_map(ctx, repo_id, worktree.as_deref()) {
            extra.push_str(&cmap);
        }
    }
    extra.push_str("\n<!-- PHILIUM_CACHE_BREAK -->\n");
    extra.push_str(&agent_identity_tail(&name, &role, parent_name.as_deref(), worktree.as_deref()));
    config["prompt"] = json!({ "extra": extra });
    if let Some(url) = ctx.mcp_url.get() {
        config["mcp"] = json!({ "servers": { "philium": {
            "transport": "streamable_http",
            "url": url,
            "headers": { "Authorization": format!("Bearer {mcp_token}") },
            "timeout_secs": 900,
        }}});
    }

    let path = format!(
        "{}:{}",
        ctx.data_dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())
    );
    let mut env = vec![
        ("PHILIUM_AGENT".to_string(), name.clone()),
        ("PHILIUM_PORT_BASE".into(), port.to_string()),
        ("PORT".into(), port.to_string()),
        ("TMPDIR".into(), tmp.to_string_lossy().into()),
        ("KUBECONFIG".into(), dir.join("kubeconfig").to_string_lossy().into()),
        ("COMPOSE_PROJECT_NAME".into(), format!("philium-{name}")),
        ("PATH".into(), path),
    ];
    if strict {
        env.extend(crate::sandbox::env(ctx, &name));
    }
    let mut launch_env = env.clone();
    launch_env.extend(setup.env);

    let policy = strict.then(|| {
        let repo_id = row["repo_id"].as_str().map(str::to_string);
        let repo_path = worktree
            .as_ref()
            .and(repo_id.as_deref())
            .and_then(|r| crate::workspace::repo(ctx, r))
            .and_then(|r| r["path"].as_str().map(PathBuf::from));
        let (mut ro, mut rw) = repo_id.as_deref().map(|r| crate::sandbox::grant_paths(ctx, r)).unwrap_or_default();
        if let Some(p) = repo_path {
            // Source checkout read-only; its .git writable so worktree commits land.
            rw.push(p.join(".git"));
            ro.push(p);
        }
        crate::sandbox::Policy { workdir: cwd.clone(), scratch: dir.clone(), hal_dir: hal_dir.clone(), read_only: ro, read_write: rw, allow_hosts: vec![] }
    });

    let (cfg, hd, cwd2, allow_logins) = (config, hal_dir.clone(), cwd.clone(), setup.allow_logins);
    let wrap_policy = policy.clone();
    let hal = tokio::task::spawn_blocking(move || {
        Hal::start(hal::Launch {
            dir: &hd,
            config: cfg,
            cwd: Some(&cwd2),
            env: launch_env,
            allow_logins,
            wrap: wrap_policy.map(|p| crate::sandbox::wrap(&p)).or_else(crate::sandbox::trusted_wrap).map(|w| {
                Box::new(move |b: &Path| w(b)) as Box<dyn Fn(&Path) -> (PathBuf, Vec<String>)>
            }),
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let hal = Arc::new(hal);
    // Every start gets a ledger row (resume too), and the previous HAL's row
    // is released: that process is gone, and its PID may be reused.
    if let Ok(prev) = ctx.db.query(
        "SELECT id FROM resources WHERE agent_id=?1 AND kind='process' AND state!='released' AND meta LIKE '%\"what\":\"hal\"%'",
        [id],
    ) {
        for r in prev {
            if let Some(rid) = r["id"].as_str() {
                crate::reaper::release(ctx, rid);
            }
        }
    }
    crate::reaper::register(ctx, Some(id), "process", &hal.pid.to_string(), json!({ "what": "hal" }));

    // Session: reuse on resume (HAL keeps it in its own SQLite).
    let session = match row["hal_session_id"].as_str().filter(|_| !fresh) {
        Some(s) if hal.call(reqwest::Method::GET, &format!("/sessions/{s}"), None).await.is_ok() => s.to_string(),
        _ => {
            let s = hal
                .call(
                    reqwest::Method::POST,
                    "/sessions",
                    Some(json!({ "workspace": cwd, "title": name, "model": model_ref })),
                )
                .await?;
            s["id"].as_str().ok_or("HAL returned no session id")?.to_string()
        }
    };
    // Re-apply the saved reasoning effort after every start.
    if let Some(e) = ctx.db.kv_get(&format!("agent.effort.{id}")).filter(|e| e != "default") {
        if let Ok(valid) = providers::validate_effort_for_ref(ctx, Some(&model_ref), &e) {
            ctx.gateway.set_effort(Some(&name), Some(&valid));
            let _ = hal.call(reqwest::Method::PATCH, &format!("/sessions/{session}"), Some(json!({ "effort": valid }))).await;
        } else {
            ctx.gateway.set_effort(Some(&name), None);
            let _ = ctx.db.kv_set(&format!("agent.effort.{id}"), "default");
        }
    } else {
        ctx.gateway.set_effort(Some(&name), None);
    }
    ctx.db.exec(
        "UPDATE agents SET hal_session_id=?2, token_hash=?3 WHERE id=?1",
        rusqlite::params![id, session, short_hash(&mcp_token)],
    )?;

    let pump = tokio::spawn(pump_events(ctx.clone(), id.to_string(), hal.clone(), session.clone()));
    let rt = Arc::new(Runtime { hal: hal.clone(), session: session.clone(), name: name.clone(), cwd, strict, env, policy, pump });
    if let Some(old) = runtimes().lk().insert(id.to_string(), rt) {
        old.pump.abort();
        old.hal.stop();
    }
    let _ = ctx.db.kv_set(&format!("agent_error.{id}"), "");
    set_state(ctx, id, "idle");

    if let Some(b) = brief.filter(|b| !b.trim().is_empty()) {
        send(ctx, id, &b).await?;
    }
    Ok(())
}

fn short_hash(s: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(s.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Follow HAL's SSE stream, keep agent state current, batch events to the UI.
async fn pump_events(ctx: Arc<Ctx>, id: String, hal: Arc<Hal>, session: String) {
    use futures_util::StreamExt;
    let mut after: i64 = ctx
        .db
        .kv_get(&format!("agent_seq.{id}"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    loop {
        if let Some(reason) = hal.exit_reason() {
            on_hal_exit(&ctx, &id, &hal, &reason);
            return;
        }
        let url = format!("{}/sessions/{session}/events?stream=true&after={after}", hal.base);
        let resp = match hal.client.get(&url).bearer_auth(&hal.token).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut batch: Vec<Value> = vec![];
        let mut last_flush = std::time::Instant::now();
        loop {
            let next = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buf.find("\n\n") {
                        let block: String = buf.drain(..pos + 2).collect();
                        for ev in parse_sse(&block) {
                            if let Some(s) = ev["seq"].as_i64() {
                                after = after.max(s);
                            }
                            apply_event(&ctx, &id, &ev);
                            batch.push(ev);
                        }
                    }
                }
                Ok(Some(Err(_))) | Ok(None) => break,
                Err(_) => {} // timeout tick: flush below
            }
            if !batch.is_empty() && (last_flush.elapsed().as_millis() >= 100 || batch.len() > 200) {
                ctx.emit("agents:events", json!({ "id": id, "events": std::mem::take(&mut batch) }));
                let _ = ctx.db.kv_set(&format!("agent_seq.{id}"), &after.to_string());
                last_flush = std::time::Instant::now();
            }
        }
        if !batch.is_empty() {
            ctx.emit("agents:events", json!({ "id": id, "events": batch }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// The agent's HAL exited on its own (crash, OOM kill, signal). Clean up what
/// runs on its behalf and park the agent in `failed` with a readable reason so
/// the UI can offer Resume. No-op if the runtime was already replaced/stopped.
fn on_hal_exit(ctx: &Arc<Ctx>, id: &str, hal: &Arc<Hal>, reason: &str) {
    let owned = {
        let mut map = runtimes().lk();
        match map.get(id) {
            Some(rt) if Arc::ptr_eq(&rt.hal, hal) => map.remove(id).is_some(),
            _ => false,
        }
    };
    if !owned {
        return;
    }
    let msg = format!("{reason}. Press Resume to restart it.");
    let _ = ctx.db.kv_set(&format!("agent_error.{id}"), &msg);
    if let Some(n) = get_row(ctx, id).and_then(|r| r["name"].as_str().map(str::to_string)) {
        ctx.gateway.revoke_agent(&n);
    }
    crate::mcp::revoke(id);
    crate::terminals::kill_agent(ctx, id);
    hal.stop();
    set_state(ctx, id, "failed");
    ctx.log_event("agent_hal_exited", Some(id), &json!({ "reason": reason }));
    ctx.emit("agents:error", json!({ "id": id, "error": msg }));
}

pub fn parse_sse(block: &str) -> Vec<Value> {
    let data: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim_start)
        .collect();
    if data.is_empty() {
        return vec![];
    }
    serde_json::from_str::<Value>(&data.join("\n")).ok().into_iter().filter(|v| v.get("type").is_some()).collect()
}

/// Agent state machine driven by HAL events.
pub fn next_state(current: &str, event_type: &str) -> Option<&'static str> {
    if matches!(current, "stopped" | "failed" | "done" | "tearing_down") {
        return None;
    }
    match event_type {
        "turn.started" | "model.started" | "tool.started" | "permission.resolved" => Some("running"),
        "permission.requested" => Some("waiting"),
        "assistant.completed" | "turn.cancelled" => Some("idle"),
        "turn.failed" => Some("idle"),
        _ => None,
    }
}

fn apply_event(ctx: &Arc<Ctx>, id: &str, ev: &Value) {
    let ty = ev["type"].as_str().unwrap_or("");
    let current = get_row(ctx, id).and_then(|r| r["state"].as_str().map(str::to_string)).unwrap_or_default();
    if let Some(s) = next_state(&current, ty) {
        if s != current {
            set_state(ctx, id, s);
        }
    } else if ty != "model.delta" {
        let _ = ctx.db.exec("UPDATE agents SET last_active_at=?2 WHERE id=?1", rusqlite::params![id, now()]);
    }
    match ty {
        "permission.requested" => ctx.emit("agents:approval", json!({ "id": id, "event": ev })),
        "turn.failed" => ctx.log_event("agent_turn_failed", Some(id), &json!({ "error": ev["error"] })),
        _ => {}
    }
}

fn rt(id: &str) -> Result<Arc<Runtime>, String> {
    let r = runtime(id).ok_or_else(|| "agent is not running (resume it first)".to_string())?;
    if let Some(reason) = r.hal.exit_reason() {
        return Err(format!("{reason}. Press Resume to restart it."));
    }
    Ok(r)
}

pub async fn send(ctx: &Arc<Ctx>, id: &str, text: &str) -> Result<Value, String> {
    send_with_images(ctx, id, text, json!([])).await
}

/// Let HAL expose browser tools so its permission engine can ask the human.
/// This is not approval: permission_rules requires consent for Philium browser
/// MCP calls and still denies HAL's built-in browser implementation.
pub(crate) fn message_body(text: &str, images: Value) -> Value {
    json!({ "text": text, "images": images, "allow_browser": true })
}

pub async fn send_with_images(_ctx: &Arc<Ctx>, id: &str, text: &str, images: Value) -> Result<Value, String> {
    let r = rt(id)?;
    // Only HAL events change turn state. A rejected request is not a new turn,
    // and setting state after acceptance can race a fast completion event.
    r.hal
        .call(reqwest::Method::POST, &format!("/sessions/{}/messages", r.session), Some(message_body(text, images)))
        .await
}

/// Stop the HAL and everything that runs on the agent's behalf. Keeps the
/// worktree, scratch and record so it can resume or be reviewed.
pub fn stop(ctx: &Arc<Ctx>, id: &str, state: &str) {
    if let Some(rt) = runtimes().lk().remove(id) {
        rt.pump.abort();
        rt.hal.stop();
    }
    if let Some(row) = get_row(ctx, id) {
        if let Some(n) = row["name"].as_str() {
            ctx.gateway.revoke_agent(n);
        }
    }
    crate::mcp::revoke(id);
    crate::terminals::kill_agent(ctx, id);
    crate::board::release_agent(ctx, id);
    set_state(ctx, id, state);
}

/// Called on app exit: stop every agent HAL; containers/worktrees are kept.
pub fn shutdown(ctx: &Arc<Ctx>) {
    let ids: Vec<String> = runtimes().lk().keys().cloned().collect();
    for id in ids {
        stop(ctx, &id, "stopped");
    }
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    // A previous run's agents have no HAL any more (HAL exits when our stdin
    // pipe closes, so a crashed Philium leaves no live HALs behind).
    for r in ctx.db.query("SELECT id FROM agents WHERE state IN ('provisioning','running','waiting')", [])? {
        if let Some(id) = r["id"].as_str() {
            let _ = ctx.db.kv_set(&format!("agent_error.{id}"), "Philium quit while this agent was working. Press Resume to continue.");
        }
    }
    ctx.db.exec(
        "UPDATE agents SET state='stopped' WHERE state IN ('provisioning','running','idle','waiting')",
        [],
    )?;
    Ok(())
}

fn with_live(ctx: &Ctx, mut row: Value) -> Value {
    let id = row["id"].as_str().unwrap_or_default().to_string();
    let name = row["name"].as_str().unwrap_or_default().to_string();
    row["live"] = json!(runtime(&id).is_some());
    row["strict"] = json!(row["strict"].as_i64() == Some(1));
    if let Some(e) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) {
        row["error"] = json!(e);
    }
    if let Some(rt) = runtime(&id) {
        row["cwd"] = json!(rt.cwd);
        row["pid"] = json!(rt.hal.pid);
    }
    let stats = ctx.gateway.agent_stats();
    let kv = stats.get(&name).cloned().unwrap_or_else(|| {
        json!({
            "requests": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cached_tokens": 0,
            "cache_hit_pct": 0,
            "last_input_tokens": 0,
            "last_cached_tokens": 0,
            "last_cache_hit_pct": 0,
            "guard_alerts": 0,
            "rate_limits": 0,
            "errors": 0,
            "avg_ttft_ms": Value::Null,
        })
    });
    let clashes: Vec<Value> = crate::codeindex::cached_clashes()
        .into_iter()
        .filter(|c| c["a"].as_str() == Some(&name) || c["b"].as_str() == Some(&name))
        .collect();
    let mut warnings: Vec<Value> = vec![];
    if row["state"].as_str() == Some("waiting") {
        warnings.push(json!({ "kind": "waiting", "severity": "high", "text": "Waiting for human approval" }));
    }
    if row["state"].as_str() == Some("failed") || row.get("error").and_then(Value::as_str).is_some() {
        let msg = row["error"].as_str().unwrap_or("Agent failed");
        warnings.push(json!({ "kind": "failed", "severity": "high", "text": msg }));
    }
    for c in &clashes {
        let peer = if c["a"].as_str() == Some(&name) {
            c["b"].as_str().unwrap_or("peer")
        } else {
            c["a"].as_str().unwrap_or("peer")
        };
        let detail = c["detail"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).take(2).collect::<Vec<_>>().join("; "))
            .unwrap_or_default();
        warnings.push(json!({
            "kind": "clash",
            "severity": if c["severity"].as_str() == Some("symbol") { "high" } else { "med" },
            "text": format!("AST {} clash with {peer}: {detail}", c["severity"].as_str().unwrap_or("file")),
        }));
    }
    let reqs = kv["requests"].as_u64().unwrap_or(0);
    let inp = kv["input_tokens"].as_u64().unwrap_or(0);
    let hit = kv["cache_hit_pct"].as_u64().unwrap_or(0);
    if reqs >= 2 && inp >= 8_000 && hit < 25 {
        warnings.push(json!({
            "kind": "kv_cache",
            "severity": "med",
            "text": format!("Cold KV cache ({hit}% hit across {}k input tokens)", inp / 1000),
        }));
    }
    let guard_alerts = kv["guard_alerts"].as_u64().unwrap_or(0);
    if guard_alerts > 0 {
        warnings.push(json!({
            "kind": "guard",
            "severity": "high",
            "text": format!("Outbound secret guard triggered ({guard_alerts}x)"),
        }));
    }
    let rl = kv["rate_limits"].as_u64().unwrap_or(0);
    if rl > 0 {
        warnings.push(json!({
            "kind": "rate_limit",
            "severity": "med",
            "text": format!("Provider rate-limited 429 ({rl}x)"),
        }));
    }
    row["kv"] = kv;
    row["clashes"] = json!(clashes);
    row["warnings"] = json!(warnings);
    row
}

pub fn list(ctx: &Ctx) -> Result<Value, String> {
    let rows = ctx.db.query(
        "SELECT a.*, t.title AS task_title, r.name AS repo_name, w.path AS worktree, w.branch AS branch
         FROM agents a LEFT JOIN tasks t ON t.id=a.task_id LEFT JOIN repos r ON r.id=a.repo_id
         LEFT JOIN worktrees w ON w.agent_id=a.id AND w.state='active'
         ORDER BY a.created_at",
        [],
    )?;
    let mut by_id: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut children_count: HashMap<String, usize> = HashMap::new();
    for r in &rows {
        if let Some(id) = r["id"].as_str() {
            let name = r["name"].as_str().unwrap_or_default().to_string();
            let pid = r["parent_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
            if let Some(p) = &pid {
                *children_count.entry(p.clone()).or_default() += 1;
            }
            by_id.insert(id.to_string(), (name, pid));
        }
    }
    let root_of = |start_id: &str| -> (String, String) {
        let mut cur = start_id.to_string();
        for _ in 0..8 {
            if let Some((_, Some(p))) = by_id.get(&cur) {
                if by_id.contains_key(p) {
                    cur = p.clone();
                    continue;
                }
            }
            break;
        }
        let cname = by_id.get(&cur).map(|(n, _)| n.clone()).unwrap_or_default();
        (cur, cname)
    };
    let enriched: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut v = with_live(ctx, r);
            let id = v["id"].as_str().unwrap_or_default().to_string();
            let pid = v["parent_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
            v["parent_name"] = json!(pid.as_ref().and_then(|p| by_id.get(p)).map(|(n, _)| n.clone()));
            let (cid, cname) = root_of(&id);
            v["cluster_id"] = json!(cid);
            v["cluster_name"] = json!(cname);
            v["children_count"] = json!(children_count.get(&id).copied().unwrap_or(0));
            v
        })
        .collect();
    Ok(Value::Array(enriched))
}

async fn hal_get(id: &str, path: &str) -> Result<Value, String> {
    let r = rt(id)?;
    r.hal.call(reqwest::Method::GET, &format!("/sessions/{}{path}", r.session), None).await
}

fn local_kind_limit(ctx: &Ctx) -> u32 {
    ctx.setting("kind.local_limit", "1").parse::<u32>().unwrap_or(1).min(8)
}

fn check_kind_inventory(stdout: &str, limit: u32) -> Result<(), String> {
    let count = stdout.lines().filter(|s| !s.trim().is_empty()).count();
    if count >= limit as usize { return Err(format!("This kind runtime already has {count} cluster(s) (local limit {limit}). No cluster was started.")); }
    Ok(())
}

fn reserve_local_kind(ctx: &Ctx, aid: &str, name: &str, meta: Value) -> Result<String, String> {
    let rid = new_id();
    let limit = local_kind_limit(ctx);
    let inserted = ctx.db.exec(
        "INSERT INTO resources(id,agent_id,kind,handle,meta,created_at)
         SELECT ?1,?2,'kind_cluster',?3,?4,?5
         WHERE (SELECT count(*) FROM resources WHERE kind='kind_cluster' AND state!='released') < ?6
           AND NOT EXISTS (SELECT 1 FROM resources WHERE kind='kind_cluster' AND state!='released' AND (agent_id=?2 OR handle=?3))",
        rusqlite::params![rid, aid, name, meta.to_string(), now(), limit],
    )?;
    if inserted != 1 { return Err(format!("Local kind capacity unavailable (limit {limit}, one per agent). Reuse an existing cluster or remove it first.")); }
    ctx.log_event("reaper:register", Some(aid), &json!({"id": rid, "kind":"kind_cluster", "handle":name}));
    ctx.emit("reaper:changed", json!({"id":rid}));
    Ok(rid)
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    let id = || -> Result<String, String> {
        let raw = ctx::arg_str(&args, "id")?;
        get_row(ctx, &raw)
            .and_then(|r| r["id"].as_str().map(str::to_string))
            .ok_or_else(|| format!("no agent {raw}"))
    };
    match method {
        "list" => list(ctx),
        "names" => Ok(json!(free_names(ctx, 8))),
        "get" => {
            let id = id()?;
            let mut row = with_live(ctx, get_row(ctx, &id).ok_or("gone")?);
            // The view must remain available while HAL or a tool is busy.
            // Live session reads belong to their dedicated endpoints, not metadata.
            row["resources"] = json!(ctx.db.query(
                "SELECT * FROM resources WHERE agent_id=?1 AND state!='released' ORDER BY created_at",
                [&id]
            )?);
            row["children"] = json!(ctx.db.query("SELECT id,name,state FROM agents WHERE parent_id=?1", [&id])?);
            Ok(row)
        }
        "spawn" => spawn(
            ctx,
            SpawnReq {
                name: ctx::arg_opt_str(&args, "name"),
                role: ctx::arg_opt_str(&args, "role"),
                model: ctx::arg_opt_str(&args, "model"),
                repo_id: ctx::arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty()),
                task_id: ctx::arg_opt_str(&args, "task_id").filter(|s| !s.is_empty()),
                brief: ctx::arg_opt_str(&args, "brief"),
                parent_id: ctx::arg_opt_str(&args, "parent_id"),
                strict: args["strict"].as_bool().unwrap_or(false),
            },
        ),
        "send" => send_with_images(ctx, &id()?, &ctx::arg_str(&args, "text")?, args.get("images").cloned().unwrap_or(json!([]))).await,
        "cancel" => {
            let r = rt(&id()?)?;
            r.hal.call(reqwest::Method::POST, &format!("/sessions/{}/cancel", r.session), None).await
        }
        "stop" => {
            stop(ctx, &id()?, "stopped");
            Ok(Value::Null)
        }
        "resume" => {
            let id = id()?;
            let admission = ProvisionAdmission::acquire(&id)?;
            if let Some(r) = runtime(&id) {
                if r.hal.alive() {
                    return Ok(Value::Null);
                }
                // Dead HAL the watcher has not reaped yet: drop it and restart.
                stop(ctx, &id, "stopped");
            }
            if get_row(ctx, &id).is_none() {
                return Err("unknown agent".into());
            }
            set_state(ctx, &id, "provisioning");
            let c = ctx.clone();
            tokio::spawn(async move {
                let _admission = admission;
                if let Err(e) = provision(&c, &id, false, None).await {
                    let _ = c.db.kv_set(&format!("agent_error.{id}"), &e);
                    set_state(&c, &id, "failed");
                }
            });
            Ok(Value::Null)
        }
        "remove" => {
            let id = id()?;
            if !ctx.db.query("SELECT id FROM agents WHERE parent_id=?1 LIMIT 1", [&id])?.is_empty() {
                return Err("Remove this agent's helpers first".into());
            }
            let force = args["force"].as_bool().unwrap_or(false);
            stop(ctx, &id, "tearing_down");
            let (c, i) = (ctx.clone(), id.clone());
            let report = tokio::task::spawn_blocking(move || crate::reaper::teardown_agent(&c, &i, force))
                .await
                .map_err(|e| e.to_string())??;
            if report["needs_confirmation"].as_bool() == Some(true) {
                set_state(ctx, &id, "stopped");
                return Ok(report);
            }
            if let Some(row) = get_row(ctx, &id) {
                if let Some(n) = row["name"].as_str() {
                    let _ = std::fs::remove_dir_all(ctx.agent_dir(n));
                }
            }
            crate::browser::forget_agent(ctx, &id);
            ctx.db.exec("DELETE FROM agents WHERE id=?1", [&id])?;
            for k in ["agent_error", "agent_seq"] {
                let _ = ctx.db.exec("DELETE FROM kv WHERE k=?1", [format!("{k}.{id}")]);
            }
            ctx.log_event("agent_removed", Some(&id), &report);
            ctx.emit("agents:changed", json!({ "id": id, "removed": true }));
            Ok(report)
        }
        "events" => {
            let after = args["after"].as_i64().unwrap_or(0).max(0);
            let limit = args["limit"].as_i64().unwrap_or(2000).clamp(1, 10000);
            let raw = ctx::arg_str(&args, "id")?;
            let c = ctx.clone();
            tokio::task::spawn_blocking(move || history::events(&c, &raw, after, limit))
                .await.map_err(|e| format!("saved agent history task failed: {e}"))?
        }
        "messages" => hal_get(&id()?, "/messages").await,
        "permissions" => hal_get(&id()?, "/permissions").await,
        "permission" => {
            let r = rt(&id()?)?;
            let rid = ctx::arg_str(&args, "request_id")?;
            if !rid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("bad request id".into());
            }
            let (path, body) = match ctx::arg_str(&args, "decision")?.as_str() {
                "approve" => (
                    "approve",
                    Some(json!({ "scope": ctx::arg_opt_str(&args, "scope").unwrap_or_else(|| "once".into()) })),
                ),
                "deny" => ("deny", None),
                _ => return Err("decision must be approve or deny".into()),
            };
            r.hal
                .call(reqwest::Method::POST, &format!("/sessions/{}/permissions/{rid}/{path}", r.session), body)
                .await
        }
        "set_effort" => {
            let id = id()?;
            let raw_effort = ctx::arg_str(&args, "effort")?;
            let row = get_row(ctx, &id).ok_or("gone")?;
            let effort = providers::validate_effort_for_ref(ctx, row["model_ref"].as_str(), &raw_effort)?;
            ctx.gateway.set_effort(row["name"].as_str(), Some(&effort));
            if let Some(rt) = runtime(&id) {
                let _ = rt.hal.call(reqwest::Method::PATCH, &format!("/sessions/{}", rt.session), Some(json!({ "effort": effort }))).await;
            }
            ctx.db.kv_set(&format!("agent.effort.{id}"), &effort)?;
            ctx.emit("agents:changed", json!({ "id": id, "effort": effort }));
            Ok(json!(effort))
        }
        // The agent's main shell (opened on demand while it runs). Null when stopped.
        "shell" => Ok(json!(crate::agentshell::current(ctx, &id()?, crate::agentshell::MAIN, args["open"].as_bool() == Some(true)).await)),
        "get_effort" => Ok(json!(ctx.db.kv_get(&format!("agent.effort.{}", id()?)).unwrap_or_else(|| "default".into()))),
        "kind_list" => {
            let agent_filter = if args.get("id").is_some() { Some(id()?) } else { None };
            let rows = ctx.db.query(
                "SELECT r.*, a.name AS agent_name, a.port_base AS port_base
                 FROM resources r LEFT JOIN agents a ON a.id=r.agent_id
                 WHERE r.kind='kind_cluster' AND r.state!='released'
                   AND (?1 IS NULL OR r.agent_id=?1)
                 ORDER BY r.created_at DESC",
                rusqlite::params![agent_filter],
            )?;
            Ok(json!(rows))
        }
        "kind_create" => {
            let aid = id()?;
            let row = get_row(ctx, &aid).ok_or("gone")?;
            let aname = row["name"].as_str().ok_or("missing agent name")?.to_string();
            let port_base = row["port_base"].as_i64().unwrap_or(42000);
            let raw_suffix = ctx::arg_opt_str(&args, "suffix").unwrap_or_else(|| "test".into());
            let suffix: String = raw_suffix
                .chars()
                .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                .take(16)
                .collect();
            let suffix = if suffix.is_empty() { "test".to_string() } else { suffix };
            let cluster_name = format!("philium-{aname}-{suffix}");
            let kubeconfig = ctx.agent_dir(&aname).join("kubeconfig");
            let _op = ctx.kind_operation.try_lock().map_err(|_| "Another kind operation is in progress. Try again when it finishes.")?;
            let inventory = tokio::task::spawn_blocking(|| std::process::Command::new("kind").args(["get", "clusters"]).output())
                .await.map_err(|e| e.to_string())?.map_err(|e| format!("Cannot inspect local kind capacity: {e}"))?;
            if !inventory.status.success() { return Err(format!("Cannot inspect local kind capacity: {}", String::from_utf8_lossy(&inventory.stderr).trim())); }
            check_kind_inventory(&String::from_utf8_lossy(&inventory.stdout), local_kind_limit(ctx))?;
            std::fs::create_dir_all(ctx.agent_dir(&aname)).map_err(|e| e.to_string())?;
            let rid = reserve_local_kind(
                ctx,
                &aid,
                &cluster_name,
                json!({
                    "agent_name": aname,
                    "kubeconfig": kubeconfig.to_string_lossy(),
                    "http_port": port_base + 10,
                    "https_port": port_base + 11,
                }),
            )?;
            let cfg_path = ctx.agent_dir(&aname).join(format!("kind-{suffix}.yaml"));
            let kind_cfg = format!(
                "kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nnodes:\n- role: control-plane\n  extraPortMappings:\n  - containerPort: 30080\n    hostPort: {}\n    protocol: TCP\n  - containerPort: 30443\n    hostPort: {}\n    protocol: TCP\n",
                port_base + 10,
                port_base + 11
            );
            if let Err(e) = std::fs::write(&cfg_path, kind_cfg) {
                crate::reaper::release(ctx, &rid);
                return Err(format!("Could not prepare local kind config: {e}"));
            }
            let cname = cluster_name.clone();
            let kc = kubeconfig.clone();
            let cp = cfg_path.clone();
            let out = tokio::task::spawn_blocking(move || {
                std::process::Command::new("kind")
                    .args([
                        "create",
                        "cluster",
                        "--name",
                        &cname,
                        "--config",
                        &cp.to_string_lossy(),
                        "--kubeconfig",
                        &kc.to_string_lossy(),
                        "--wait",
                        "60s",
                    ])
                    .output()
            })
            .await
            .map_err(|e| e.to_string())?;
            match out {
                Ok(o) if o.status.success() => {
                    ctx.emit("agents:changed", json!({ "id": aid }));
                    Ok(json!({
                        "resource_id": rid,
                        "cluster": cluster_name,
                        "kubeconfig": kubeconfig.to_string_lossy(),
                        "http_port": port_base + 10,
                        "https_port": port_base + 11,
                    }))
                }
                Ok(o) => {
                    crate::reaper::release(ctx, &rid);
                    Err(format!("kind create failed: {}", String::from_utf8_lossy(&o.stderr).trim()))
                }
                Err(e) => {
                    crate::reaper::release(ctx, &rid);
                    Err(format!("kind binary not available or failed to start: {e}"))
                }
            }
        }
        "kind_delete" => {
            let aid = id()?;
            let owner = get_row(ctx, &aid).ok_or("gone")?;
            let aname = owner["name"].as_str().ok_or("missing owner")?;
            let cluster = ctx::arg_str(&args, "cluster")?;
            if !cluster.starts_with(&format!("philium-{aname}-")) {
                return Err("Cluster does not belong to this agent".into());
            }
            let _op = ctx.kind_operation.try_lock().map_err(|_| "Another kind operation is in progress. Try again when it finishes.")?;
            let rows = ctx.db.query(
                "SELECT id, agent_id FROM resources WHERE kind='kind_cluster' AND handle=?1 AND state!='released'",
                [&cluster],
            )?;
            if rows.len() != 1 || rows[0]["agent_id"].as_str() != Some(aid.as_str()) {
                return Err("No uniquely owned active cluster for this agent".into());
            }
            let kc = ctx.agent_dir(aname).join("kubeconfig");
            let cname = cluster.clone();
            let out = tokio::task::spawn_blocking(move || {
                std::process::Command::new("kind")
                    .env("KUBECONFIG", kc)
                    .args(["delete", "cluster", "--name", &cname])
                    .output()
            })
            .await
            .map_err(|e| e.to_string())?;
            let o = out.map_err(|e| format!("Could not run kind: {e}"))?;
            if !o.status.success() {
                return Err(format!("kind delete failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
            }
            for r in rows {
                if let Some(rid) = r["id"].as_str() {
                    crate::reaper::release(ctx, rid);
                }
                if let Some(aid) = r["agent_id"].as_str() {
                    ctx.emit("agents:changed", json!({ "id": aid }));
                }
            }
            Ok(json!({ "deleted": cluster }))
        }
        "set_model" => {
            let id = id()?;
            let model = ctx::arg_str(&args, "model")?;
            let row = get_row(ctx, &id).ok_or("gone")?;
            let (p, m) = providers::resolve(ctx, Some(&model))?;
            let strict = row["strict"].as_i64() == Some(1);
            if strict && !(p.trusted == false && row["model_ref"].as_str().unwrap_or("").starts_with(&format!("{}/", p.id))) {
                return Err("a Strict agent can only switch models within its provider".into());
            }
            if !strict && !p.trusted {
                return Err("switching to an untrusted provider needs a new Strict agent".into());
            }
            let r = format!("{}/{}", p.id, m);
            if r != row["model_ref"] && (row["parent_id"].is_string() || !ctx.db.query("SELECT id FROM agents WHERE parent_id=?1 LIMIT 1", [&id])?.is_empty()) {
                return Err("Start a separate agent to change the recipient of a helper family".into());
            }
            let cur_eff = ctx.db.kv_get(&format!("agent.effort.{id}")).unwrap_or_else(|| "default".into());
            let next_eff = providers::validate_effort_for_ref(ctx, Some(&r), &cur_eff).unwrap_or_else(|_| "default".into());
            if next_eff != cur_eff {
                let _ = ctx.db.kv_set(&format!("agent.effort.{id}"), &next_eff);
            }
            ctx.gateway.set_effort(row["name"].as_str(), Some(&next_eff));
            if let Some(rt) = runtime(&id) {
                rt.hal
                    .call(reqwest::Method::PATCH, &format!("/sessions/{}", rt.session), Some(json!({ "model": r })))
                    .await?;
            }
            ctx.db.exec("UPDATE agents SET model_ref=?2 WHERE id=?1", rusqlite::params![id, r])?;
            ctx.emit("agents:changed", json!({ "id": id, "effort": next_eff }));
            Ok(json!(r))
        }
        "diff" => {
            let id = id()?;
            let wt = ctx.db.query("SELECT id FROM worktrees WHERE agent_id=?1 AND state='active' LIMIT 1", [&id])?;
            match wt.first().and_then(|w| w["id"].as_str()) {
                Some(w) => crate::workspace::call(ctx, "diff", json!({ "worktree_id": w })).await,
                None => Ok(json!({ "stat": "", "patch": "" })),
            }
        }
        "broadcast" => {
            let text = ctx::arg_str(&args, "text")?;
            let cluster_id = ctx::arg_opt_str(&args, "cluster_id");
            let repo_id = ctx::arg_opt_str(&args, "repo_id");
            let only_orchestrator = args["only_orchestrator"].as_bool().unwrap_or(false);
            let all = list(ctx)?.as_array().cloned().unwrap_or_default();
            let mut sent = 0usize;
            for a in all {
                if a["live"].as_bool() != Some(true) {
                    continue;
                }
                let aid = a["id"].as_str().unwrap_or_default();
                if let Some(cid) = &cluster_id {
                    if a["cluster_id"].as_str() != Some(cid.as_str()) && aid != cid {
                        continue;
                    }
                    if only_orchestrator && aid != cid {
                        continue;
                    }
                }
                if let Some(rid) = &repo_id {
                    if a["repo_id"].as_str() != Some(rid.as_str()) {
                        continue;
                    }
                }
                if send(ctx, aid, &text).await.is_ok() {
                    sent += 1;
                }
            }
            Ok(json!({ "sent": sent }))
        }
        "batch_permission" => {
            let decision = ctx::arg_str(&args, "decision")?;
            if !matches!(decision.as_str(), "approve" | "deny") {
                return Err("decision must be approve or deny".into());
            }
            let scope = ctx::arg_opt_str(&args, "scope").unwrap_or_else(|| "session".into());
            let cluster_id = ctx::arg_opt_str(&args, "cluster_id");
            let repo_id = ctx::arg_opt_str(&args, "repo_id");
            let all = list(ctx)?.as_array().cloned().unwrap_or_default();
            let mut resolved = 0usize;
            for a in all {
                if a["state"].as_str() != Some("waiting") || a["live"].as_bool() != Some(true) {
                    continue;
                }
                let aid = a["id"].as_str().unwrap_or_default();
                if let Some(cid) = &cluster_id {
                    if a["cluster_id"].as_str() != Some(cid.as_str()) && aid != cid {
                        continue;
                    }
                }
                if let Some(rid) = &repo_id {
                    if a["repo_id"].as_str() != Some(rid.as_str()) {
                        continue;
                    }
                }
                if let Ok(r) = rt(aid) {
                    if let Ok(perms) = r.hal.call(reqwest::Method::GET, &format!("/sessions/{}/permissions", r.session), None).await {
                        if let Some(arr) = perms.as_array() {
                            for p in arr {
                                if let Some(rid) = p["request_id"].as_str().or_else(|| p["id"].as_str()) {
                                    let body = if decision == "approve" { Some(json!({ "scope": scope })) } else { None };
                                    if r.hal.call(reqwest::Method::POST, &format!("/sessions/{}/permissions/{rid}/{decision}", r.session), body).await.is_ok() {
                                        resolved += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(json!({ "resolved": resolved }))
        }
        "stop_cluster" => {
            let cid = ctx::arg_str(&args, "cluster_id")?;
            let all = list(ctx)?.as_array().cloned().unwrap_or_default();
            let mut stopped = 0usize;
            for a in all {
                let aid = a["id"].as_str().unwrap_or_default();
                if (a["cluster_id"].as_str() == Some(cid.as_str()) || aid == cid) && a["live"].as_bool() == Some(true) {
                    stop(ctx, aid, "stopped");
                    stopped += 1;
                }
            }
            Ok(json!({ "stopped": stopped }))
        }
        _ => Err(format!("unknown agents.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_admission_counts_external_clusters_and_reserves_managed_ports() {
        assert!(check_kind_inventory("outside\nother\n", 2).is_err());
        assert!(check_kind_inventory("", 0).is_err());
        assert!(check_kind_inventory("outside\n", 2).is_ok());
        let ctx = Ctx::test();
        let rid = reserve_local_kind(&ctx, "a", "philium-a-one", json!({})).unwrap();
        assert!(reserve_local_kind(&ctx, "b", "philium-b-one", json!({})).is_err());
        ctx.db.kv_set("setting.kind.local_limit", "2").unwrap();
        assert!(reserve_local_kind(&ctx, "a", "philium-a-two", json!({})).is_err());
        crate::reaper::release(&ctx, &rid);
        assert!(reserve_local_kind(&ctx, "a", "philium-a-two", json!({})).is_ok());
    }

    #[tokio::test]
    #[ignore = "requires an accessible kind runtime; uses inventory only and a zero budget"]
    async fn real_kind_runtime_refuses_at_zero_without_creating_resources() {
        let ctx = Ctx::test();
        ctx.db.kv_set("setting.kind.local_limit", "0").unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('a','fixture','p/m',0,0)", []).unwrap();
        let message = call(&ctx, "kind_create", json!({"id":"a"})).await.unwrap_err();
        assert!(message.contains("local limit 0"), "{message}");
        println!("{message}");
        assert_eq!(call(&ctx, "kind_list", json!({"id":"a"})).await.unwrap(), json!([]));
        assert!(!ctx.agent_dir("fixture").exists());
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn kind_rpcs_do_not_leak_resources_or_remove_another_owners_cluster() {
        let ctx = Ctx::test();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('a','owl','p/m',0,0),('b','owl-two','p/m',0,0)", []).unwrap();
        reserve_local_kind(&ctx, "b", "philium-owl-two-test", json!({})).unwrap();
        assert!(call(&ctx, "kind_list", json!({"id":"missing"})).await.is_err());
        assert_eq!(call(&ctx, "kind_list", json!({"id":"a"})).await.unwrap(), json!([]));
        assert!(call(&ctx, "kind_delete", json!({"id":"a","cluster":"philium-owl-two-test"})).await.unwrap_err().contains("owned"));
        assert_eq!(call(&ctx, "kind_list", json!({"id":"b"})).await.unwrap()[0]["handle"], "philium-owl-two-test");
    }

    #[test]
    fn names_are_slugs_and_unique() {
        let ctx = Ctx::test();
        assert!(valid_name("lion"));
        assert!(valid_name("lion-2"));
        for bad in ["", "Lion", "a b", "../x", "-x", "you", "all", &"x".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
        let n = free_names(&ctx, 3);
        assert_eq!(n.len(), 3);
        assert!(n.iter().all(|x| valid_name(x)));
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,created_at,last_active_at) VALUES('a','lion','p/m',0,0)",
                [],
            )
            .unwrap();
        assert!(!free_names(&ctx, NAMES.len() + 5).contains(&"lion".to_string()));
    }

    #[test]
    fn port_blocks_do_not_overlap() {
        let ctx = Ctx::test();
        let a = port_base(&ctx);
        ctx.db
            .exec(
                "INSERT INTO agents(id,name,model_ref,port_base,created_at,last_active_at) VALUES('a','a','p/m',?1,0,0)",
                [a],
            )
            .unwrap();
        let b = port_base(&ctx);
        assert_eq!(b - a, 20);
    }

    #[test]
    fn state_machine() {
        assert_eq!(next_state("idle", "turn.started"), Some("running"));
        assert_eq!(next_state("running", "permission.requested"), Some("waiting"));
        assert_eq!(next_state("waiting", "permission.resolved"), Some("running"));
        assert_eq!(next_state("running", "assistant.completed"), Some("idle"));
        assert_eq!(next_state("stopped", "turn.started"), None);
        assert_eq!(next_state("running", "model.delta"), None);
    }

    #[test]
    fn sse_parsing() {
        let ev = parse_sse("id: 7\nevent: model.delta\ndata: {\"seq\":7,\"type\":\"model.delta\",\"delta\":\"hi\"}\n\n");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["delta"], "hi");
        assert!(parse_sse(": ping\n\n").is_empty());
        assert!(parse_sse("event: ping\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn shell_is_denied_and_secrets_unreadable() {
        let p = permission_rules(false);
        let rules = p["rules"].as_array().unwrap();
        // Last matching rule wins: shell deny must not be followed by a shell allow.
        let last_shell = rules.iter().rev().find(|r| r["action"] == "shell").unwrap();
        assert_eq!(last_shell["effect"], "deny");
        assert!(rules.iter().any(|r| r["resource"] == "**/.env*" && r["effect"] == "deny"));
        let s = permission_rules(true);
        let last_sub = s["rules"].as_array().unwrap().iter().rev().find(|r| r["action"] == "subagent").unwrap();
        assert_eq!(last_sub["effect"], "deny");
        // The secret denies must be the last read rule touching .env (nothing re-opens it).
        let last_read = s["rules"].as_array().unwrap().iter().rev().find(|r| r["action"] == "read").unwrap();
        assert_eq!(last_read["effect"], "deny");
    }

    #[test]
    fn browser_capability_requires_mcp_approval() {
        for strict in [false, true] {
            let p = permission_rules(strict);
            let rules = p["rules"].as_array().unwrap();
            let broad = rules.iter().position(|r| r["action"] == "mcp" && r["resource"] == "philium/*").unwrap();
            let browser = rules.iter().position(|r| r["action"] == "mcp" && r["resource"] == "philium/browser_*").unwrap();
            assert!(browser > broad);
            assert_eq!(rules[browser]["effect"], "ask");
            assert_eq!(rules.iter().rev().find(|r| r["action"] == "browser").unwrap()["effect"], "deny");
        }
        let images = json!([{ "data": "test", "media_type": "image/png" }]);
        let body = message_body("open a page", images.clone());
        assert_eq!(body["allow_browser"], true);
        assert_eq!(body["text"], "open a page");
        assert_eq!(body["images"], images);
    }

    #[tokio::test]
    async fn provisioning_admission_rejects_duplicates_and_releases_on_failure() {
        let ctx = Ctx::test();
        let id = new_id();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES(?1,'admission-fixture','missing/model','stopped',0,0)", [&id]).unwrap();
        let admission = ProvisionAdmission::acquire(&id).unwrap();
        let error = call(&ctx, "resume", json!({"id":id})).await.unwrap_err();
        assert!(error.contains("already provisioning"));
        assert_eq!(get_row(&ctx, &id).unwrap()["state"], "stopped");
        // A peer has independent admission, and dropping a failed task's guard allows retry.
        let peer = ProvisionAdmission::acquire(&new_id()).unwrap();
        drop(peer);
        drop(admission);
        call(&ctx, "resume", json!({"id":id})).await.unwrap();
        for _ in 0..100 {
            if get_row(&ctx, &id).unwrap()["state"] == "failed" { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!(get_row(&ctx, &id).unwrap()["state"], "failed");
        assert!(ProvisionAdmission::acquire(&id).is_ok());
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the actual hal-desktop-server sidecar beside the test binary"]
    async fn real_sidecar_duplicate_resume_stop_and_reconnect() {
        assert!(hal::server_path().is_file(), "actual HAL sidecar required");
        let ctx = Ctx::test();
        let id = new_id();
        struct Fixture(Arc<Ctx>, String);
        impl Drop for Fixture {
            fn drop(&mut self) {
                stop(&self.0, &self.1, "stopped");
                let _ = std::fs::remove_dir_all(self.0.data_dir.parent().unwrap());
            }
        }
        let fixture = Fixture(ctx.clone(), id.clone());
        let fixture_dir = ctx.data_dir.parent().unwrap().to_path_buf();
        ctx.db.exec("INSERT INTO providers(id,name,kind,base_url,trust,guard_mode,enabled,default_model,created_at) VALUES('fixture','Fixture','openai_chat','http://127.0.0.1:9/v1','trusted','redact',1,'fixture-model',0)", []).unwrap();
        ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES(?1,'sidecar-fixture','fixture/fixture-model','stopped',0,0)", [&id]).unwrap();
        async fn wait_live(ctx: &Arc<Ctx>, id: &str) -> Arc<Runtime> {
            for _ in 0..400 {
                if let Some(rt) = runtime(id) { return rt; }
                if get_row(ctx, id).unwrap()["state"] == "failed" {
                    panic!("sidecar provision failed: {:?}", ctx.db.kv_get(&format!("agent_error.{id}")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("sidecar runtime did not become live");
        }
        call(&ctx, "resume", json!({"id":id})).await.unwrap();
        assert!(call(&ctx, "resume", json!({"id":id})).await.unwrap_err().contains("already provisioning"));
        let first = wait_live(&ctx, &id).await;
        assert_eq!(first.hal.call(reqwest::Method::GET, "/health", None).await.unwrap()["status"], "ok");
        let session = first.session.clone();
        let pid = first.hal.pid;
        call(&ctx, "resume", json!({"id":id})).await.unwrap();
        assert_eq!(runtime(&id).unwrap().hal.pid, pid);
        call(&ctx, "stop", json!({"id":id})).await.unwrap();
        assert!(!first.hal.alive());
        assert!(runtime(&id).is_none());
        call(&ctx, "resume", json!({"id":id})).await.unwrap();
        let second = wait_live(&ctx, &id).await;
        assert_ne!(second.hal.pid, pid);
        assert_eq!(second.session, session);
        assert_eq!(second.hal.call(reqwest::Method::GET, "/health", None).await.unwrap()["status"], "ok");
        stop(&ctx, &id, "stopped");
        assert!(!second.hal.alive());
        drop(fixture);
        assert!(!fixture_dir.exists(), "sidecar fixture must be removed");
        println!("real sidecar duplicate rejected, live resume idempotent, stopped PID {pid}, reconnected PID {}, preserved session {session}, both children stopped and fixture removed", second.hal.pid);
    }

    #[tokio::test]
    async fn spawn_validates_before_starting() {
        let ctx = Ctx::test();
        // No providers configured: resolve fails, nothing is created.
        assert!(spawn(&ctx, SpawnReq {
            name: Some("lion".into()), role: None, model: Some("nope/x".into()), repo_id: None,
            task_id: None, brief: None, parent_id: None, strict: false
        })
        .is_err());
        assert!(ctx.db.query("SELECT * FROM agents", []).unwrap().is_empty());
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
