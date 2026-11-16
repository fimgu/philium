//! SQLite store (`philium.db` in the app data dir). One connection behind a
//! mutex: every query here is tiny, and WAL keeps readers cheap.
//!
//! Schema is the contract between modules (SPEC section 4). Modules may add
//! their own tables with `CREATE TABLE IF NOT EXISTS` in their `init`, but the
//! shared tables below are owned here so every module agrees on them.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::{Map, Value};

pub struct Db(Mutex<Connection>);

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS providers (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
  base_url TEXT NOT NULL DEFAULT '', trust TEXT NOT NULL DEFAULT 'trusted',
  guard_mode TEXT NOT NULL DEFAULT 'redact', enabled INTEGER NOT NULL DEFAULT 1,
  models_cached_at INTEGER, default_model TEXT, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS models (
  provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  model_id TEXT NOT NULL, display TEXT, context_window INTEGER,
  price_in REAL, price_out REAL, enabled INTEGER NOT NULL DEFAULT 1,
  discovered_at INTEGER NOT NULL, PRIMARY KEY (provider_id, model_id));
CREATE TABLE IF NOT EXISTS repos (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
  default_branch TEXT NOT NULL DEFAULT 'main', integration_branch TEXT,
  remote_url TEXT, added_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, role TEXT NOT NULL DEFAULT 'worker',
  model_ref TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'idle',
  parent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL,
  task_id TEXT, strict INTEGER NOT NULL DEFAULT 0,
  hal_session_id TEXT, token_hash TEXT, port_base INTEGER,
  created_at INTEGER NOT NULL, last_active_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS worktrees (
  id TEXT PRIMARY KEY, repo_id TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
  agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL, base_commit TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'active', created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'todo', priority TEXT NOT NULL DEFAULT 'normal',
  owner_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  lease_expires_at INTEGER, repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL,
  pr_url TEXT, playbook TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS task_deps (
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  depends_on TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  PRIMARY KEY (task_id, depends_on));
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT, from_agent TEXT NOT NULL, to_target TEXT NOT NULL,
  body TEXT NOT NULL, guard_findings TEXT, read_at INTEGER, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS leases (
  id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL,
  expires_at INTEGER NOT NULL, UNIQUE (kind, key));
CREATE TABLE IF NOT EXISTS resources (
  id TEXT PRIMARY KEY, agent_id TEXT, kind TEXT NOT NULL, handle TEXT NOT NULL,
  meta TEXT, state TEXT NOT NULL DEFAULT 'live', created_at INTEGER NOT NULL,
  released_at INTEGER);
CREATE TABLE IF NOT EXISTS terminals (
  id TEXT PRIMARY KEY, agent_id TEXT, title TEXT NOT NULL, command TEXT NOT NULL,
  cwd TEXT NOT NULL, pid INTEGER, exit_code INTEGER, state TEXT NOT NULL DEFAULT 'running',
  created_at INTEGER NOT NULL, ended_at INTEGER);
CREATE TABLE IF NOT EXISTS requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, agent_id TEXT,
  provider_id TEXT NOT NULL, model TEXT NOT NULL, status INTEGER,
  tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0,
  tokens_cached INTEGER NOT NULL DEFAULT 0, ttft_ms INTEGER, duration_ms INTEGER,
  tps REAL, cost REAL, guard_action TEXT, repo_id TEXT);
CREATE INDEX IF NOT EXISTS requests_ts ON requests(ts);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER NOT NULL, kind TEXT NOT NULL,
  agent_id TEXT, body TEXT NOT NULL);
"#;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    use rand::Rng;
    let n: u128 = rand::thread_rng().gen();
    format!("{:x}", n)[..16].to_string()
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        let c = Connection::open(path).map_err(|e| e.to_string())?;
        c.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Db(Mutex::new(c)))
    }

    pub fn memory() -> Db {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        Db(Mutex::new(c))
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        f(&self.0.lock().unwrap()).map_err(|e| e.to_string())
    }

    pub fn exec(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize, String> {
        self.with(|c| c.execute(sql, params))
    }

    /// Rows as JSON objects keyed by column name.
    pub fn query(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Value>, String> {
        self.with(|c| {
            let mut st = c.prepare(sql)?;
            let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
            let rows = st.query_map(params, |r| {
                let mut m = Map::new();
                for (i, n) in names.iter().enumerate() {
                    use rusqlite::types::ValueRef::*;
                    let v = match r.get_ref(i)? {
                        Null => Value::Null,
                        Integer(x) => Value::from(x),
                        Real(x) => Value::from(x),
                        Text(t) => Value::from(String::from_utf8_lossy(t).to_string()),
                        Blob(_) => Value::Null,
                    };
                    m.insert(n.clone(), v);
                }
                Ok(Value::Object(m))
            })?;
            rows.collect()
        })
    }

    pub fn kv_get(&self, k: &str) -> Option<String> {
        self.with(|c| c.query_row("SELECT v FROM kv WHERE k=?1", [k], |r| r.get(0))).ok()
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<(), String> {
        self.exec("INSERT INTO kv(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=excluded.v", [k, v])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_and_query() {
        let db = Db::memory();
        db.kv_set("a", "1").unwrap();
        assert_eq!(db.kv_get("a").as_deref(), Some("1"));
        db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('r','n','/p',1)", []).unwrap();
        let rows = db.query("SELECT * FROM repos", []).unwrap();
        assert_eq!(rows[0]["name"], "n");
    }
}
