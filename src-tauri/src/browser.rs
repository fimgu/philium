//! Built-in browser (SPEC 6/10/12). One child webview per tab, created lazily
//! inside the main window with Tauri multiwebview (`unstable` feature), placed
//! over a placeholder div in the Browser screen.
//!
//! Security: tabs load remote pages and are NOT listed in any capability, and
//! remote origins are never granted Philium IPC, so pages cannot call `rpc`.
//! Agent snapshots do not use IPC either: the page text travels back through
//! `document.title` (nonce protected) and `on_document_title_changed`.
//!
//! rpc methods (`browser.*`):
//! - `open {url, agent?}` -> tab; `navigate {tab,url}`; `back|forward|reload {tab}`; `close {tab}`
//! - `tabs {}` -> `[{id,url,title,agent,visible}]`
//! - `set_bounds {tab,x,y,w,h}` (CSS px, relative to the window content); `hide {tab?}`, `show {tab?}` (no tab = all)
//! - `snapshot {tab}` -> page text; `external {url}` opens in the system browser
//!
//! Events: `browser:tab` `{id,url,title,agent}` on any tab change, `browser:closed` `{id}`.
//! Rust API for agents: `agent_open(ctx, agent, url)` -> tab id, `agent_snapshot(ctx, tab)` -> text.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::webview::WebviewBuilder;
use tauri::{LogicalPosition, LogicalSize, Manager, WebviewUrl};
use tokio::sync::oneshot;

use crate::ctx::{arg_opt_str, arg_str, Ctx};

#[derive(Clone, Serialize, Debug)]
pub struct Tab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub agent: Option<String>,
    pub visible: bool,
}

#[derive(Default)]
struct State {
    tabs: Vec<Tab>,
    snaps: HashMap<String, oneshot::Sender<String>>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

const SNAP_PREFIX: &str = "__philium_snap:";
const MAX_TEXT: usize = 200_000;

pub fn init(_ctx: &Arc<Ctx>) -> Result<(), String> {
    Ok(())
}

/// Only http(s) (and about:blank) are loadable. No file:, javascript:, data:, tauri:.
pub fn normalize_url(input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("empty url".into());
    }
    let lower = t.to_lowercase();
    if lower == "about:blank" {
        return Ok(lower);
    }
    let with = if lower.starts_with("http://") || lower.starts_with("https://") {
        t.to_string()
    } else if t.contains("://") || lower.starts_with("javascript:") || lower.starts_with("data:") || lower.starts_with("file:") {
        return Err("only http and https urls are allowed".into());
    } else if !t.contains(' ') && (t.contains('.') || t.starts_with("localhost")) {
        format!("https://{t}")
    } else {
        format!("https://duckduckgo.com/?q={}", t.replace(' ', "+"))
    };
    let u: tauri::Url = with.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?;
    Ok(u.to_string())
}

fn label(id: &str) -> String {
    format!("browser-{id}")
}

fn update(ctx: &Ctx, id: &str, f: impl FnOnce(&mut Tab)) {
    let t = {
        let mut s = state().lock().unwrap();
        s.tabs.iter_mut().find(|t| t.id == id).map(|t| {
            f(t);
            t.clone()
        })
    };
    if let Some(t) = t {
        ctx.emit("browser:tab", t);
    }
}

fn webview(ctx: &Ctx, id: &str) -> Result<tauri::Webview, String> {
    let h = ctx.handle().ok_or("no app handle")?;
    h.get_webview(&label(id)).ok_or_else(|| format!("no such tab `{id}`"))
}

async fn open(ctx: &Arc<Ctx>, url: &str, agent: Option<String>) -> Result<Tab, String> {
    let url = normalize_url(url)?;
    let h = ctx.handle().ok_or("no app handle")?.clone();
    let win = h.get_window("main").ok_or("main window not found")?;
    let id = crate::db::new_id()[..8].to_string();
    let parsed: tauri::Url = url.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?;
    let (c_nav, c_title, id_nav, id_title) = (ctx.clone(), ctx.clone(), id.clone(), id.clone());
    let b = WebviewBuilder::new(label(&id), WebviewUrl::External(parsed))
        .on_navigation(move |u| {
            let ok = matches!(u.scheme(), "http" | "https" | "about");
            if ok {
                let s = u.to_string();
                update(&c_nav, &id_nav, |t| t.url = s);
            }
            ok
        })
        .on_document_title_changed(move |_, title| {
            if let Some(rest) = title.strip_prefix(SNAP_PREFIX) {
                if let Some((nonce, text)) = rest.split_once(':') {
                    if let Some(tx) = state().lock().unwrap().snaps.remove(nonce) {
                        let _ = tx.send(text.to_string());
                    }
                }
                return;
            }
            update(&c_title, &id_title, |t| t.title = title);
        });
    // Start tiny and hidden until the UI sends bounds.
    win.add_child(b, LogicalPosition::new(0.0, 0.0), LogicalSize::new(1.0, 1.0)).map_err(|e| e.to_string())?;
    let tab = Tab { id: id.clone(), url, title: String::new(), agent, visible: false };
    if let Ok(w) = webview(ctx, &id) {
        let _ = w.hide();
    }
    state().lock().unwrap().tabs.push(tab.clone());
    ctx.emit("browser:tab", tab.clone());
    Ok(tab)
}

fn set_visible(ctx: &Ctx, id: &str, vis: bool) -> Result<(), String> {
    let w = webview(ctx, id)?;
    if vis { w.show() } else { w.hide() }.map_err(|e| e.to_string())?;
    update(ctx, id, |t| t.visible = vis);
    Ok(())
}

fn ids(args: &Value) -> Vec<String> {
    match arg_opt_str(args, "tab") {
        Some(t) => vec![t],
        None => state().lock().unwrap().tabs.iter().map(|t| t.id.clone()).collect(),
    }
}

pub async fn agent_open(ctx: &Arc<Ctx>, agent: &str, url: &str) -> Result<String, String> {
    Ok(open(ctx, url, Some(agent.to_string())).await?.id)
}

pub async fn agent_snapshot(ctx: &Arc<Ctx>, tab: &str) -> Result<String, String> {
    let w = webview(ctx, tab)?;
    let nonce = crate::db::new_id();
    let (tx, rx) = oneshot::channel();
    state().lock().unwrap().snaps.insert(nonce.clone(), tx);
    let js = format!(
        "(()=>{{const old=document.title;let t=(document.body&&document.body.innerText)||'';t=t.slice(0,{MAX_TEXT});document.title='{SNAP_PREFIX}{nonce}:'+t;setTimeout(()=>{{document.title=old}},50)}})()"
    );
    w.eval(&js).map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(t)) => Ok(t),
        _ => {
            state().lock().unwrap().snaps.remove(&nonce);
            Err("snapshot timed out (page still loading?)".into())
        }
    }
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "open" => {
            let t = open(ctx, &arg_str(&args, "url")?, arg_opt_str(&args, "agent")).await?;
            Ok(json!(t))
        }
        "navigate" => {
            let (id, url) = (arg_str(&args, "tab")?, normalize_url(&arg_str(&args, "url")?)?);
            let w = webview(ctx, &id)?;
            w.navigate(url.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?).map_err(|e| e.to_string())?;
            update(ctx, &id, |t| t.url = url);
            Ok(Value::Null)
        }
        "back" | "forward" | "reload" => {
            let w = webview(ctx, &arg_str(&args, "tab")?)?;
            let js = match method {
                "back" => "history.back()",
                "forward" => "history.forward()",
                _ => "location.reload()",
            };
            w.eval(js).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "close" => {
            let id = arg_str(&args, "tab")?;
            if let Ok(w) = webview(ctx, &id) {
                let _ = w.close();
            }
            state().lock().unwrap().tabs.retain(|t| t.id != id);
            ctx.emit("browser:closed", json!({"id": id}));
            Ok(Value::Null)
        }
        "tabs" => Ok(json!(state().lock().unwrap().tabs.clone())),
        "set_bounds" => {
            let id = arg_str(&args, "tab")?;
            let n = |k: &str| args.get(k).and_then(Value::as_f64).ok_or_else(|| format!("missing `{k}`"));
            let (x, y, w, h) = (n("x")?, n("y")?, n("w")?.max(1.0), n("h")?.max(1.0));
            let v = webview(ctx, &id)?;
            v.set_position(LogicalPosition::new(x, y)).map_err(|e| e.to_string())?;
            v.set_size(LogicalSize::new(w, h)).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "hide" | "show" => {
            for id in ids(&args) {
                let _ = set_visible(ctx, &id, method == "show");
            }
            Ok(Value::Null)
        }
        "snapshot" => Ok(json!(agent_snapshot(ctx, &arg_str(&args, "tab")?).await?)),
        "external" => {
            let url = normalize_url(&arg_str(&args, "url")?)?;
            let st = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" })
                .arg(&url)
                .status()
                .map_err(|e| e.to_string())?;
            if st.success() { Ok(Value::Null) } else { Err("could not open system browser".into()) }
        }
        _ => Err(format!("browser.{method}: unknown method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(normalize_url("http://localhost:3000/a").unwrap(), "http://localhost:3000/a");
        assert!(normalize_url("rust lang").unwrap().starts_with("https://duckduckgo.com/?q=rust+lang"));
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,hi", "tauri://localhost", "ftp://x.y", ""] {
            assert!(normalize_url(bad).is_err(), "{bad}");
        }
    }
}
