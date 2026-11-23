//! U6: native OS notifications when an agent finishes, fails or asks for approval while
//! Philium is in the background. Delivery is in the backend so a hidden webview does not
//! matter. Content is generic (agent name and what happened), never model output, paths,
//! commands or error text, because the OS keeps notification history on the lock screen.
//! Off unless the person turns on `setting.notifications` ("true"). Cancellation is silent.
//! Replays are deduplicated by agent and event sequence.
use crate::ctx::Ctx;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Kind {
    Done,
    Failed,
    Approval,
}

/// What an agent event means for the person, if anything.
pub(crate) fn classify(ty: &str) -> Option<Kind> {
    match ty {
        "assistant.completed" => Some(Kind::Done),
        "turn.failed" => Some(Kind::Failed),
        "permission.requested" => Some(Kind::Approval),
        _ => None,
    }
}

pub(crate) fn text(kind: Kind, agent: &str) -> (String, String) {
    let agent = if agent.is_empty() { "An agent" } else { agent };
    match kind {
        Kind::Done => (format!("{agent} finished"), "Ready for you to look at.".into()),
        Kind::Failed => (format!("{agent} stopped with a problem"), "Open Philium to see what happened.".into()),
        Kind::Approval => (format!("{agent} is waiting for you"), "It needs your approval to continue.".into()),
    }
}

/// True once per (agent, seq). Sequences only grow within a session, so a lower or equal
/// seq is a replay. Events without a seq are always new.
pub(crate) fn first_time(seen: &mut HashMap<String, i64>, agent_id: &str, seq: Option<i64>) -> bool {
    let Some(seq) = seq else { return true };
    match seen.get(agent_id) {
        Some(&last) if seq <= last => false,
        _ => {
            seen.insert(agent_id.to_string(), seq);
            true
        }
    }
}

fn enabled(ctx: &Ctx) -> bool {
    ctx.db.kv_get("setting.notifications").as_deref() == Some("true")
}

/// Called for each applied HAL event of an agent.
pub fn on_event(ctx: &Ctx, agent_id: &str, agent_name: &str, ev: &Value) {
    let Some(kind) = classify(ev["type"].as_str().unwrap_or("")) else { return };
    if crate::attention::HIDDEN.load(std::sync::atomic::Ordering::Relaxed) || !enabled(ctx) {
        return;
    }
    static SEEN: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(Default::default).lock() else { return };
    if !first_time(&mut seen, agent_id, ev["seq"].as_i64()) {
        return;
    }
    drop(seen);
    let Some(h) = ctx.handle() else { return };
    use tauri::Manager;
    if h.get_webview_window("main").and_then(|w| w.is_focused().ok()).unwrap_or(false) {
        return;
    }
    let (title, body) = text(kind, agent_name);
    use tauri_plugin_notification::NotificationExt;
    let _ = h.notification().builder().title(title).body(body).show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_finish_fail_and_approval_notify() {
        assert_eq!(classify("assistant.completed"), Some(Kind::Done));
        assert_eq!(classify("turn.failed"), Some(Kind::Failed));
        assert_eq!(classify("permission.requested"), Some(Kind::Approval));
        assert_eq!(classify("turn.cancelled"), None);
        assert_eq!(classify("assistant.delta"), None);
    }

    #[test]
    fn text_is_generic_and_replays_are_dropped() {
        let (t, b) = text(Kind::Failed, "lion");
        assert_eq!(t, "lion stopped with a problem");
        assert!(!b.contains("error"));
        assert_eq!(text(Kind::Done, "").0, "An agent finished");
        let mut seen = HashMap::new();
        assert!(first_time(&mut seen, "a", Some(5)));
        assert!(!first_time(&mut seen, "a", Some(5)));
        assert!(!first_time(&mut seen, "a", Some(3)));
        assert!(first_time(&mut seen, "b", Some(1)));
        assert!(first_time(&mut seen, "a", None));
    }

    #[test]
    fn off_by_default() {
        let ctx = Ctx::test();
        assert!(!enabled(&ctx));
        on_event(&ctx, "a", "lion", &serde_json::json!({"type": "turn.failed", "seq": 1}));
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }
}
