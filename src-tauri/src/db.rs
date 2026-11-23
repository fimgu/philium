//! SQLite store (`philium.db` in the app data dir). One connection behind a
//! mutex: every query here is tiny, and WAL keeps readers cheap.
//!
//! Schema is the contract between modules (SPEC section 4). Modules may add
//! their own tables with `CREATE TABLE IF NOT EXISTS` in their `init`, but the
//! shared tables below are owned here so every module agrees on them.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};

pub struct Db {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
}

const READ_POOL_SIZE: usize = 3;

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
  created_at INTEGER NOT NULL, last_active_at INTEGER NOT NULL, base_ref TEXT);
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
  pr_url TEXT, playbook TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  board_cleared INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS task_deps (
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  depends_on TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  PRIMARY KEY (task_id, depends_on));
CREATE TABLE IF NOT EXISTS agent_queue (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  text TEXT NOT NULL, images TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS agent_mail_delivery (
  message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
  agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  repo_id TEXT NOT NULL, model_ref TEXT NOT NULL);
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
CREATE TABLE IF NOT EXISTS teardown_proposals (
  id TEXT PRIMARY KEY, resource_id TEXT NOT NULL, reason TEXT NOT NULL,
  unsaved INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'pending',
  created_at INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS proposal_pending ON teardown_proposals(resource_id) WHERE state='pending';
CREATE TABLE IF NOT EXISTS terminals (
  id TEXT PRIMARY KEY, agent_id TEXT, repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL, title TEXT NOT NULL, command TEXT NOT NULL,
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
CREATE INDEX IF NOT EXISTS worktrees_agent_state ON worktrees(agent_id, state);
CREATE INDEX IF NOT EXISTS worktrees_repo_state ON worktrees(repo_id, state);
CREATE INDEX IF NOT EXISTS tasks_repo_status ON tasks(repo_id, status);
CREATE INDEX IF NOT EXISTS tasks_status_lease ON tasks(status, lease_expires_at);
CREATE INDEX IF NOT EXISTS resources_agent_state ON resources(agent_id, state);
CREATE INDEX IF NOT EXISTS resources_kind_handle_state ON resources(kind, handle, state);
CREATE INDEX IF NOT EXISTS terminals_agent_state ON terminals(agent_id, state);
CREATE INDEX IF NOT EXISTS agents_parent ON agents(parent_id);
CREATE INDEX IF NOT EXISTS agents_repo_state ON agents(repo_id, state);
CREATE INDEX IF NOT EXISTS agent_queue_agent ON agent_queue(agent_id);
CREATE INDEX IF NOT EXISTS events_kind_id ON events(kind, id);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
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

fn apply_tuning_pragmas(c: &Connection) -> Result<(), String> {
    c.execute_batch("PRAGMA cache_size=-16000; PRAGMA temp_store=MEMORY;")
        .map_err(|e| e.to_string())?;
    let _ = c.execute_batch("PRAGMA mmap_size=268435456;");
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        let c = Connection::open(path).map_err(|e| e.to_string())?;
        // Other processes (previews, sqlite3 CLI, a second instance) may hold
        // the write lock briefly: wait instead of failing with SQLITE_BUSY.
        c.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        c.execute_batch("PRAGMA synchronous=NORMAL;").map_err(|e| e.to_string())?;
        apply_tuning_pragmas(&c)?;
        if c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='terminals')", [], |r| r.get::<_, bool>(0)).unwrap_or(false) {
            for (name, ddl) in [("agent_id", "agent_id TEXT"), ("state", "state TEXT NOT NULL DEFAULT 'running'")] {
                if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('terminals') WHERE name=?1)", [name], |r| r.get::<_, bool>(0)).unwrap_or(true) {
                    let _ = c.execute_batch(&format!("ALTER TABLE terminals ADD COLUMN {ddl}"));
                }
            }
        }
        c.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('terminals') WHERE name='repo_id')", [], |r| r.get::<_, bool>(0)).map_err(|e| e.to_string())? {
            c.execute_batch("ALTER TABLE terminals ADD COLUMN repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL").map_err(|e|e.to_string())?;
        }
        // Where an agent's worktree starts when it is not the checkout's usual branch: a release
        // review starts on the pull request's head, and its helpers with it.
        if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('agents') WHERE name='base_ref')", [], |r| r.get::<_, bool>(0)).map_err(|e| e.to_string())? {
            c.execute_batch("ALTER TABLE agents ADD COLUMN base_ref TEXT").map_err(|e|e.to_string())?;
        }
        let mut readers = Vec::with_capacity(READ_POOL_SIZE);
        let ro_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        for _ in 0..READ_POOL_SIZE {
            if let Ok(r) = Connection::open_with_flags(path, ro_flags) {
                let _ = r.busy_timeout(std::time::Duration::from_secs(5));
                let _ = r.execute_batch("PRAGMA foreign_keys=ON;");
                let _ = apply_tuning_pragmas(&r);
                readers.push(Mutex::new(r));
            }
        }
        Ok(Db {
            writer: Mutex::new(c),
            readers,
            next_reader: AtomicUsize::new(0),
        })
    }

    pub fn memory() -> Db {
        let c = Connection::open_in_memory().unwrap();
        let _ = apply_tuning_pragmas(&c);
        c.execute_batch(SCHEMA).unwrap();
        Db {
            writer: Mutex::new(c),
            readers: Vec::new(),
            next_reader: AtomicUsize::new(0),
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        f(&self.writer.lock().unwrap_or_else(|e| e.into_inner())).map_err(|e| e.to_string())
    }

    fn with_reader<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let len = self.readers.len();
        if len == 0 {
            return self.with(f);
        }
        let start = self.next_reader.fetch_add(1, Ordering::Relaxed);
        for offset in 0..len {
            if let Ok(guard) = self.readers[(start + offset) % len].try_lock() {
                return f(&guard).map_err(|e| e.to_string());
            }
        }
        let guard = self.readers[start % len].lock().unwrap_or_else(|e| e.into_inner());
        f(&guard).map_err(|e| e.to_string())
    }

    pub fn exec(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize, String> {
        self.with(|c| {
            let mut st = c.prepare_cached(sql)?;
            st.execute(params)
        })
    }

    /// Rows as JSON objects keyed by column name.
    pub fn query(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Value>, String> {
        let use_reader = !self.readers.is_empty() && {
            let s = sql.trim_start().as_bytes();
            (s.len() >= 6 && s[..6].eq_ignore_ascii_case(b"SELECT") && !sql.contains("RETURNING"))
                || (s.len() >= 4 && s[..4].eq_ignore_ascii_case(b"WITH") && !sql.contains("INSERT") && !sql.contains("UPDATE") && !sql.contains("DELETE"))
        };
        let run = |c: &Connection| -> rusqlite::Result<Vec<Value>> {
            let mut st = c.prepare_cached(sql)?;
            let names: Vec<String> = st.column_names().into_iter().map(str::to_string).collect();
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
        };
        if use_reader {
            self.with_reader(run)
        } else {
            self.with(run)
        }
    }

    pub fn kv_get(&self, k: &str) -> Option<String> {
        self.with_reader(|c| {
            let mut st = c.prepare_cached("SELECT v FROM kv WHERE k=?1")?;
            st.query_row([k], |r| r.get(0))
        })
        .ok()
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
    fn old_terminal_history_is_preserved_unassigned() {
        let path = std::env::temp_dir().join(format!("philium-migration-{}.db",new_id()));
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE terminals(id TEXT PRIMARY KEY,cwd TEXT); INSERT INTO terminals VALUES('old','/same');").unwrap();
        drop(old);
        for _ in 0..2 {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.query("SELECT cwd,repo_id FROM terminals WHERE id='old'",[]).unwrap()[0],serde_json::json!({"cwd":"/same","repo_id":null}));
        }
        std::fs::remove_file(&path).unwrap();
    }
    #[test]
    fn schema_and_query() {
        let db = Db::memory();
        db.kv_set("a", "1").unwrap();
        assert_eq!(db.kv_get("a").as_deref(), Some("1"));
        db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('r','n','/p',1)", []).unwrap();
        let rows = db.query("SELECT * FROM repos", []).unwrap();
        assert_eq!(rows[0]["name"], "n");
        assert_eq!(db.kv_get("missing"), None);
    }

    #[test]
    fn reopen_is_idempotent_and_concurrent_writers_wait() {
        let dir = std::env::temp_dir().join(format!("philium-db-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let a = std::sync::Arc::new(Db::open(&path).unwrap());
        let b = std::sync::Arc::new(Db::open(&path).unwrap()); // schema runs twice: fine
        let mut hs = vec![];
        for (i, db) in [a.clone(), b.clone(), a.clone(), b.clone()].into_iter().enumerate() {
            hs.push(std::thread::spawn(move || {
                for j in 0..50 {
                    db.with(|c| {
                        c.execute_batch("BEGIN IMMEDIATE")?;
                        c.execute("INSERT INTO kv(k,v) VALUES(?1,'x')", [format!("{i}.{j}")])?;
                        c.execute_batch("COMMIT")
                    })
                    .unwrap();
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        let n = a.query("SELECT COUNT(*) AS n FROM kv", []).unwrap()[0]["n"].as_i64();
        assert_eq!(n, Some(200));
        drop((a, b));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn poisoned_connection_lock_still_serves() {
        let db = std::sync::Arc::new(Db::memory());
        let d2 = db.clone();
        let _ = std::thread::spawn(move || {
            let _ = d2.with(|_| -> rusqlite::Result<()> { panic!("boom") });
        })
        .join();
        db.kv_set("a", "1").unwrap();
        assert_eq!(db.kv_get("a").as_deref(), Some("1"));
    }

    #[test]
    fn read_pool_concurrency_and_secondary_indexes() {
        let dir = std::env::temp_dir().join(format!("philium-db-pool-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pool.db");
        let db = std::sync::Arc::new(Db::open(&path).unwrap());
        assert_eq!(db.readers.len(), READ_POOL_SIZE);
        db.kv_set("k1", "v1").unwrap();
        assert_eq!(db.kv_get("k1").as_deref(), Some("v1"));

        // While the writer lock is held, read-only queries and kv_get proceed without blocking on writer.
        let db_reader = db.clone();
        db.with(|_| {
            let val = std::thread::spawn(move || {
                assert_eq!(db_reader.kv_get("k1").as_deref(), Some("v1"));
                let rows = db_reader.query("SELECT v FROM kv WHERE k=?1", ["k1"]).unwrap();
                rows[0]["v"].as_str().unwrap().to_string()
            })
            .join()
            .unwrap();
            assert_eq!(val, "v1");
            Ok(())
        })
        .unwrap();

        let idx_rows = db
            .query("SELECT name FROM sqlite_master WHERE type='index'", [])
            .unwrap();
        let names: Vec<&str> = idx_rows.iter().filter_map(|r| r["name"].as_str()).collect();
        for expected in [
            "worktrees_agent_state",
            "worktrees_repo_state",
            "tasks_repo_status",
            "tasks_status_lease",
            "resources_agent_state",
            "resources_kind_handle_state",
            "terminals_agent_state",
            "agents_parent",
            "agents_repo_state",
            "agent_queue_agent",
            "events_kind_id",
            "events_ts",
        ] {
            assert!(names.contains(&expected), "missing index {expected}");
        }
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
}
