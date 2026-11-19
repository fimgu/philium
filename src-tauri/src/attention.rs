//! The Dock says when agents need the person: a badge with how many wait for an approval, have
//! failed or stopped at a rate limit, and one bounce when that number grows, or a review report
//! arrives, while Philium is in the background. No notification permission is needed. Hidden
//! check runs never touch the Dock.
use crate::ctx::Ctx;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tauri::Manager;

/// Set for a hidden check run: the Dock belongs to the person's own Philium.
pub static HIDDEN: AtomicBool = AtomicBool::new(false);
static SHOWN: AtomicI64 = AtomicI64::new(0);

/// How many agents need the person now.
pub fn needing_you(ctx: &Ctx) -> i64 {
    let n = |sql: &str| ctx.db.query(sql, []).ok().and_then(|r| r.first().and_then(|r| r["n"].as_i64())).unwrap_or(0);
    n("SELECT count(*) AS n FROM agents WHERE state IN ('waiting','failed')")
        + n("SELECT count(*) AS n FROM kv k JOIN agents a ON k.k='agent.retry.'||a.id WHERE k.v LIKE '%\"gave_up\":true%' AND a.state='idle'")
}

/// Bring the badge up to date; bounce once when more agents need the person than before.
pub fn refresh(ctx: &Ctx) {
    if HIDDEN.load(Ordering::Relaxed) {
        return;
    }
    let n = needing_you(ctx);
    let before = SHOWN.swap(n, Ordering::Relaxed);
    if n == before {
        return;
    }
    let Some(w) = ctx.handle().and_then(|h| h.get_webview_window("main")) else { return };
    let _ = w.set_badge_count(if n > 0 { Some(n) } else { None });
    if n > before {
        bounce(&w);
    }
}

/// Something the person asked for is ready, such as a review report.
pub fn announce(ctx: &Ctx) {
    if HIDDEN.load(Ordering::Relaxed) {
        return;
    }
    if let Some(w) = ctx.handle().and_then(|h| h.get_webview_window("main")) {
        bounce(&w);
    }
}

fn bounce(w: &tauri::WebviewWindow) {
    if !w.is_focused().unwrap_or(true) {
        let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_waiting_failed_or_out_of_retries_need_the_person() {
        let ctx = Ctx::test();
        for (id, state) in [("a", "running"), ("b", "waiting"), ("c", "failed"), ("d", "idle"), ("e", "idle")] {
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,created_at,last_active_at) VALUES(?1,?1,'p/m',?2,0,0)", [id, state]).unwrap();
        }
        assert_eq!(needing_you(&ctx), 2);
        ctx.db.kv_set("agent.retry.d", r#"{"attempt":6,"gave_up":true}"#).unwrap();
        ctx.db.kv_set("agent.retry.e", r#"{"attempt":1,"until":1}"#).unwrap();
        assert_eq!(needing_you(&ctx), 3);
        // Without a window nothing happens, and nothing fails.
        refresh(&ctx);
        announce(&ctx);
        std::fs::remove_dir_all(ctx.data_dir.parent().unwrap()).ok();
    }
}
