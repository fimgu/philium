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
        let _ = self.db.exec(
            "INSERT INTO events(ts,kind,agent_id,body) VALUES(?1,?2,?3,?4)",
            rusqlite::params![crate::db::now(), kind, agent_id, text],
        );
        self.emit("event", serde_json::json!({ "kind": kind, "agent_id": agent_id, "body": body }));
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
        let dir = std::env::temp_dir().join(format!("philium-test-{}", crate::db::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        Ctx::new(
            Db::memory(),
            dir.join("data"),
            dir.join("root"),
            Gateway::new("test-token".into(), None),
            "http://127.0.0.1:9".into(),
        )
    }
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
