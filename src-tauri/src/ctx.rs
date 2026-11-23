//! Shared runtime context and the module contract.
//!
//! Every feature module (`providers`, `terminals`, `workspace`, `agents`,
//! `board`, `browser`, `graph`, `sandbox`, `stats`, `reaper`, `selfdev`)
//! exposes:
//!
//! ```ignore
//! pub fn init(ctx: &Arc<Ctx>) -> Result<(), String>;          // optional startup work
//! pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String>;
//! ```
//!
//! The UI reaches them through one Tauri command, `rpc("module.method", args)`.
//! Modules push live updates with `ctx.emit(topic, payload)`; the UI listens
//! with `listen(topic)`. Topic names are `module:thing` (e.g. `terminal:data`).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use tauri::Emitter;

use crate::db::Db;
use crate::gateway::Gateway;

/// The largest event body kept whole, in bytes.
pub const EVENT_MAX: usize = 64 * 1024;

pub struct Ctx {
    pub db: Db,
    /// Platform app data dir, e.g. ~/Library/Application Support/dev.gunnarsson.philium
    pub data_dir: PathBuf,
    /// User-visible Philium folder, default ~/Philium (SPEC 11.1).
    pub root: PathBuf,
    pub gateway: Arc<Gateway>,
    pub gateway_url: String,
    /// Serialize this workstation's managed kind create/delete operations.
    pub kind_operation: tokio::sync::Mutex<()>,
    /// Loopback URL of Philium's agent MCP server (`mcp.rs`), set after bind.
    pub mcp_url: OnceLock<String>,
    /// Loopback egress proxy for Strict agents (`sandbox.rs`), set after bind.
    pub egress_url: OnceLock<String>,
    handle: OnceLock<tauri::AppHandle>,
    /// In-process change notifications between modules, e.g. "providers"
    /// after any provider/model edit (desk HAL and agents reload configs).
    pub changes: tokio::sync::broadcast::Sender<String>,
    /// The desk HAL (plain chat, subscription discovery). Err = start failure.
    pub desk: std::sync::RwLock<Result<Arc<crate::hal::Hal>, String>>,
    /// Set when the app is exiting. UI emits are dropped from then on: the
    /// webview is going away, and emitting from a shutdown thread would wait
    /// on tao's event-queue mutex that the exiting main thread holds.
    exiting: std::sync::atomic::AtomicBool,
}

impl Ctx {
    pub fn new(db: Db, data_dir: PathBuf, root: PathBuf, gateway: Arc<Gateway>, gateway_url: String) -> Arc<Ctx> {
        Arc::new(Ctx {
            db,
            data_dir,
            root,
            gateway,
            gateway_url,
            kind_operation: tokio::sync::Mutex::new(()),
            mcp_url: OnceLock::new(),
            egress_url: OnceLock::new(),
            handle: OnceLock::new(),
            changes: tokio::sync::broadcast::channel(64).0,
            desk: std::sync::RwLock::new(Err("starting".into())),
            exiting: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn begin_exit(&self) {
        self.exiting.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_handle(&self, h: tauri::AppHandle) {
        let _ = self.handle.set(h);
    }

    pub fn desk(&self) -> Option<Arc<crate::hal::Hal>> {
        self.desk.rd().as_ref().ok().cloned()
    }

    pub fn handle(&self) -> Option<&tauri::AppHandle> {
        self.handle.get()
    }

    /// Push an event to the UI. No-op in tests (no app handle).
    pub fn emit(&self, topic: &str, payload: impl serde::Serialize + Clone) {
        if self.exiting.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if let Some(h) = self.handle.get() {
            let _ = h.emit(topic, payload);
        }
    }

    pub fn notify(&self, what: &str) {
        let _ = self.changes.send(what.to_string());
    }

    /// Append to the audit/event log shown on Home and in Activity.
    /// An entry is a record, not a dump: one larger than `EVENT_MAX` keeps its start and size.
    pub fn log_event(&self, kind: &str, agent_id: Option<&str>, body: &Value) {
        let _ = self.log_event_id(kind, agent_id, body);
    }

    /// Append to the audit/event log and return its durable event reference (`evt:<rowid>`).
    pub fn log_event_id(&self, kind: &str, agent_id: Option<&str>, body: &Value) -> Option<String> {
        let text = body.to_string();
        let (text, body) = if text.len() > EVENT_MAX {
            let mut cut = 4096;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let short = serde_json::json!({ "truncated": true, "bytes": text.len(), "start": &text[..cut] });
            (short.to_string(), short)
        } else {
            (text, body.clone())
        };
        let event_id = self
            .db
            .with(|c| {
                let mut st = c.prepare_cached("INSERT INTO events(ts,kind,agent_id,body) VALUES(?1,?2,?3,?4)")?;
                st.execute(rusqlite::params![crate::db::now(), kind, agent_id, text])?;
                Ok(c.last_insert_rowid())
            })
            .ok()
            .filter(|id| *id > 0)
            .map(|id| format!("evt:{id}"));
        self.emit("event", serde_json::json!({ "kind": kind, "agent_id": agent_id, "body": body }));
        event_id
    }

    pub fn agent_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join("agents").join(name)
    }

    pub fn setting(&self, key: &str, default: &str) -> String {
        self.db.kv_get(&format!("setting.{key}")).unwrap_or_else(|| default.to_string())
    }

    /// Test context: in-memory DB, temp dirs, gateway without a listener.
    #[cfg(test)]
    pub fn test() -> Arc<Ctx> {
        let dir = test_dir("philium-test");
        Ctx::new(
            Db::memory(),
            dir.join("data"),
            dir.join("root"),
            Gateway::new("test-token".into(), None),
            "http://127.0.0.1:9".into(),
        )
    }
}

#[cfg(test)]
static TEST_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// A fresh directory for one test. All of them sit under one root per test process, which
/// is removed when the process exits. Roots left by test processes that died are removed
/// the first time a test asks for a directory.
#[cfg(test)]
pub fn test_dir(prefix: &str) -> PathBuf {
    let root = TEST_ROOT.get_or_init(|| {
        let tmp = std::env::temp_dir();
        #[cfg(unix)]
        for entry in std::fs::read_dir(&tmp).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("philium-tests-")).and_then(|p| p.parse::<i32>().ok()) else { continue };
            // Signal 0 only asks whether the process exists.
            let gone = unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            if gone {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let root = tmp.join(format!("philium-tests-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            extern "C" fn remove_root() {
                if let Some(root) = TEST_ROOT.get() {
                    let _ = std::fs::remove_dir_all(root);
                }
            }
            unsafe { libc::atexit(remove_root) };
        }
        root
    });
    let dir = root.join(format!("{prefix}-{}", crate::db::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Helpers for pulling typed args out of the `rpc` JSON.
pub fn arg_str(args: &Value, k: &str) -> Result<String, String> {
    args.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("missing `{k}`"))
}

pub fn arg_opt_str(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Poison-tolerant locking. A panic on one thread while holding a lock must
/// not turn every later rpc into a panic too: the data behind our locks is
/// plain maps and buffers that stay consistent, so we recover the guard.
pub trait LockExt<T: ?Sized> {
    fn lk(&self) -> std::sync::MutexGuard<'_, T>;
}
impl<T: ?Sized> LockExt<T> for std::sync::Mutex<T> {
    fn lk(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}
pub trait RwExt<T: ?Sized> {
    fn rd(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn wr(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}
impl<T: ?Sized> RwExt<T> for std::sync::RwLock<T> {
    fn rd(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }
    fn wr(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    #[test]
    fn poisoned_locks_recover() {
        let m = Arc::new(std::sync::Mutex::new(1));
        let r = Arc::new(std::sync::RwLock::new(2));
        let (m2, r2) = (m.clone(), r.clone());
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            let _w = r2.write().unwrap();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned() && r.is_poisoned());
        assert_eq!(*m.lk(), 1);
        assert_eq!(*r.rd(), 2);
        *r.wr() = 3;
        assert_eq!(*r.rd(), 3);
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    #[test]
    fn an_event_is_a_record_not_a_dump() {
        let ctx = Ctx::test();
        ctx.log_event("small", None, &serde_json::json!({ "a": 1 }));
        ctx.log_event("huge", None, &serde_json::json!({ "detail": "é".repeat(EVENT_MAX) }));
        let rows = ctx.db.query("SELECT kind, body FROM events ORDER BY id", []).unwrap();
        assert_eq!(rows[0]["body"], "{\"a\":1}");
        let huge: Value = serde_json::from_str(rows[1]["body"].as_str().unwrap()).unwrap();
        assert_eq!(huge["truncated"], true);
        assert!(huge["bytes"].as_u64().unwrap() > EVENT_MAX as u64);
        assert!(rows[1]["body"].as_str().unwrap().len() < 10_000);
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }
}

/// Marker for a missing optional record (stale id, deleted row). The `rpc` command turns it
/// into a structured `{kind:"not_found"}` error so the UI never treats it as a load failure.
const NOT_FOUND: &str = "\u{1}not_found\u{1}";
pub fn not_found(msg: impl std::fmt::Display) -> String {
    format!("{NOT_FOUND}{msg}")
}
/// Structured IPC error: `kind` is `not_found` or `failed`.
pub fn rpc_error(e: String) -> Value {
    match e.strip_prefix(NOT_FOUND) {
        Some(m) => serde_json::json!({ "kind": "not_found", "message": m }),
        None => serde_json::json!({ "kind": "failed", "message": e }),
    }
}
#[cfg(test)]
mod rpc_error_tests {
    #[test]
    fn kinds() {
        assert_eq!(super::rpc_error(super::not_found("Task not found"))["kind"], "not_found");
        assert_eq!(super::rpc_error(super::not_found("Task not found"))["message"], "Task not found");
        assert_eq!(super::rpc_error("boom".into())["kind"], "failed");
    }
}
