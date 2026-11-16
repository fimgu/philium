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
//! - `open {url, agent?}` -> tab; `navigate {tab,url}`; `back|forward|reload|stop {tab}`; `close {tab}`
//! - `tabs {}` -> `[{id,url,title,agent,visible}]`
//! - `set_bounds {tab,x,y,w,h}` (CSS px, relative to the window content); `hide {tab?|owner?}`, `show {tab?|owner?}` (no tab = all tabs of owner, none = shared)
//! - `snapshot {tab}` -> page text; `external {url}` opens in the system browser
//!
//! Events: `browser:tab` `{id,url,title,agent,loading,error}` on any tab change, `browser:closed` `{id}`,
//! `browser:opened` `{id,opener}` (target=_blank / window.open became a new tab),
//! `browser:download` `{tab,name,path,state:"started"|"done"|"failed"}`.
//! Stale tab ids (webview gone) are pruned from state and reported via `browser:closed`.
//! Agents get their own tabs (`owner` = agent id) driven by the MCP `browser_*` tools in browser_agent.rs.
//! `open {url, owner}` / `tabs {owner}` / `select {tab}` serve the agent pane.

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
    /// Agent id owning this tab (agent browser pane). None = shared Browser screen.
    pub owner: Option<String>,
    pub visible: bool,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    tabs: Vec<Tab>,
    snaps: HashMap<String, oneshot::Sender<String>>,
    /// Owner (agent id) -> the tab its tools act on by default.
    current: HashMap<String, String>,
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
    match h.get_webview(&label(id)) {
        Some(w) => Ok(w),
        None => {
            // Stale id: the native webview is gone. Drop any leftover state.
            if forget(id) {
                ctx.emit("browser:closed", json!({"id": id}));
            }
            Err(format!("tab `{id}` is closed"))
        }
    }
}

/// Remove a tab from state. Returns true if it existed.
fn forget(id: &str) -> bool {
    let mut s = state().lock().unwrap();
    let n = s.tabs.len();
    s.tabs.retain(|t| t.id != id);
    s.current.retain(|_, t| t != id);
    s.tabs.len() != n
}

/// Schemes a tab may navigate to. Everything else is blocked.
pub fn allowed_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "about")
}

/// Only http(s) pages may become new tabs via target=_blank / window.open.
pub fn popup_allowed(url: &str) -> bool {
    let l = url.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// A safe file name for a download: last path segment, no separators or
/// leading dots, and a numbered suffix if it already exists in `dir`.
pub fn download_target(dir: &std::path::Path, url: &str, exists: impl Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    let path = url.parse::<tauri::Url>().map(|u| u.path().to_string()).unwrap_or_default();
    let seg = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let seg = percent_decode(seg);
    let mut name: String = seg.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '\0') && !c.is_control()).collect();
    name = name.trim_start_matches('.').trim().to_string();
    if name.is_empty() {
        name = "download".into();
    }
    if name.len() > 120 {
        name = name.chars().take(120).collect();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut p = dir.join(&name);
    let mut n = 1;
    while exists(&p) {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn downloads_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Downloads")
}

fn open(ctx: &Arc<Ctx>, url: &str, agent: Option<String>, owner: Option<String>) -> Result<Tab, String> {
    let url = normalize_url(url)?;
    let h = ctx.handle().ok_or("no app handle")?.clone();
    let win = h.get_window("main").ok_or("main window not found")?;
    let id = crate::db::new_id()[..8].to_string();
    let parsed: tauri::Url = url.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?;
    let (c_nav, c_title, c_load, c_new, c_dl) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    let (id_nav, id_title, id_load, id_new, id_dl) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
    let (agent_new, owner_new) = (agent.clone(), owner.clone());
    let mut b = WebviewBuilder::new(label(&id), WebviewUrl::External(parsed));
    if let Some(o) = &owner {
        // Each agent gets its own persistent, isolated profile and a console recorder.
        b = b.data_directory(profile_dir(&ctx.data_dir, o)).initialization_script(CONSOLE_JS);
        #[cfg(target_os = "macos")]
        {
            b = b.data_store_identifier(profile_uuid(o));
        }
    }
    let b = b
        .on_navigation(move |u| {
            let ok = allowed_scheme(u.scheme());
            if ok {
                let s = u.to_string();
                update(&c_nav, &id_nav, |t| {
                    t.url = s;
                    t.error = None;
                });
            } else {
                let msg = format!("Blocked a `{}:` link. Only http and https pages can open here.", u.scheme());
                update(&c_nav, &id_nav, |t| {
                    t.error = Some(msg);
                    t.loading = false;
                });
            }
            ok
        })
        .on_page_load(move |_, p| {
            let started = matches!(p.event(), tauri::webview::PageLoadEvent::Started);
            let s = p.url().to_string();
            update(&c_load, &id_load, |t| {
                t.loading = started;
                if allowed_scheme(p.url().scheme()) {
                    t.url = s;
                }
                if started {
                    t.error = None;
                }
            });
        })
        .on_new_window(move |u, _| {
            if popup_allowed(u.as_str()) {
                let (c, opener, agent, owner, s) = (c_new.clone(), id_new.clone(), agent_new.clone(), owner_new.clone(), u.to_string());
                std::thread::spawn(move || {
                    if let Ok(t) = open(&c, &s, agent, owner.clone()) {
                        if let Some(o) = owner {
                            state().lock().unwrap().current.insert(o, t.id.clone());
                        }
                        c.emit("browser:opened", json!({"id": t.id, "opener": opener}));
                    }
                });
            } else {
                update(&c_new, &id_new, |t| t.error = Some(format!("Blocked a popup to `{}:`. Only http and https pages can open.", u.scheme())));
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(move |_, ev| {
            match ev {
                tauri::webview::DownloadEvent::Requested { url, destination } => {
                    let dir = downloads_dir();
                    if !dir.is_dir() {
                        c_dl.emit("browser:download", json!({"tab": id_dl, "name": "", "path": "", "state": "failed"}));
                        return false;
                    }
                    let p = download_target(&dir, url.as_str(), |p| p.exists());
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    c_dl.emit("browser:download", json!({"tab": id_dl, "name": name, "path": p, "state": "started"}));
                    *destination = p;
                    true
                }
                tauri::webview::DownloadEvent::Finished { path, success, .. } => {
                    let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    c_dl.emit(
                        "browser:download",
                        json!({"tab": id_dl, "name": name, "path": path, "state": if success { "done" } else { "failed" }}),
                    );
                    true
                }
                _ => true,
            }
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
    let tab = Tab { id: id.clone(), url, title: String::new(), agent, owner, visible: false, loading: true, error: None };
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
        // No tab: every tab of `owner` (absent = the shared Browser screen's tabs).
        None => {
            let owner = arg_opt_str(args, "owner");
            state().lock().unwrap().tabs.iter().filter(|t| t.owner == owner).map(|t| t.id.clone()).collect()
        }
    }
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
            let owner = arg_opt_str(&args, "owner");
            let agent = match &owner {
                Some(o) => crate::agents::get_row(ctx, o).and_then(|r| r["name"].as_str().map(str::to_string)),
                None => arg_opt_str(&args, "agent"),
            };
            let t = open(ctx, &arg_str(&args, "url")?, agent, owner.clone())?;
            if let Some(o) = owner {
                state().lock().unwrap().current.insert(o, t.id.clone());
            }
            Ok(json!(t))
        }
        "navigate" => {
            let (id, url) = (arg_str(&args, "tab")?, normalize_url(&arg_str(&args, "url")?)?);
            let w = webview(ctx, &id)?;
            w.navigate(url.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?).map_err(|e| e.to_string())?;
            update(ctx, &id, |t| {
                t.url = url;
                t.error = None;
                t.loading = true;
            });
            Ok(Value::Null)
        }
        "back" | "forward" | "reload" | "stop" => {
            let id = arg_str(&args, "tab")?;
            let w = webview(ctx, &id)?;
            let js = match method {
                "back" => "history.back()",
                "forward" => "history.forward()",
                "stop" => "window.stop()",
                _ => "location.reload()",
            };
            if method == "reload" && state().lock().unwrap().tabs.iter().any(|t| t.id == id && t.error.is_some()) {
                // A failed load leaves an error page: retry the last known url instead.
                let url = state().lock().unwrap().tabs.iter().find(|t| t.id == id).map(|t| t.url.clone()).unwrap_or_default();
                if let Ok(u) = normalize_url(&url).and_then(|u| u.parse::<tauri::Url>().map_err(|e| e.to_string())) {
                    w.navigate(u).map_err(|e| e.to_string())?;
                }
                update(ctx, &id, |t| {
                    t.error = None;
                    t.loading = true;
                });
                return Ok(Value::Null);
            }
            w.eval(js).map_err(|e| e.to_string())?;
            if method == "stop" {
                update(ctx, &id, |t| t.loading = false);
            }
            Ok(Value::Null)
        }
        "close" => {
            let id = arg_str(&args, "tab")?;
            if let Ok(w) = webview(ctx, &id) {
                let _ = w.close();
            }
            forget(&id);
            ctx.emit("browser:closed", json!({"id": id}));
            Ok(Value::Null)
        }
        "tabs" => {
            // Prune tabs whose native webview no longer exists (e.g. after a crash).
            if let Some(h) = ctx.handle() {
                let gone: Vec<String> = state().lock().unwrap().tabs.iter().filter(|t| h.get_webview(&label(&t.id)).is_none()).map(|t| t.id.clone()).collect();
                for id in gone {
                    forget(&id);
                }
            }
            let owner = arg_opt_str(&args, "owner");
            Ok(json!(state().lock().unwrap().tabs.iter().filter(|t| t.owner == owner).cloned().collect::<Vec<_>>()))
        }
        "select" => {
            // The human picked a tab in an agent pane: agent tools follow it.
            let id = arg_str(&args, "tab")?;
            let mut s = state().lock().unwrap();
            if let Some(o) = s.tabs.iter().find(|t| t.id == id).and_then(|t| t.owner.clone()) {
                s.current.insert(o, id);
            }
            Ok(Value::Null)
        }
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

#[path = "browser_agent.rs"]
pub mod agent;
pub use agent::{forget_agent, mcp_call, mcp_tools};
use agent::{profile_dir, profile_uuid, CONSOLE_JS};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::agent::*;

    #[test]
    fn url_rules() {
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(normalize_url("http://localhost:3000/a").unwrap(), "http://localhost:3000/a");
        assert!(normalize_url("rust lang").unwrap().starts_with("https://duckduckgo.com/?q=rust+lang"));
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,hi", "tauri://localhost", "ftp://x.y", ""] {
            assert!(normalize_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn scheme_and_popup_rules() {
        for ok in ["http", "https", "about"] {
            assert!(allowed_scheme(ok));
        }
        for bad in ["file", "javascript", "data", "tauri", "ipc", "blob", "mailto"] {
            assert!(!allowed_scheme(bad), "{bad}");
        }
        assert!(popup_allowed("https://a.b/c"));
        assert!(popup_allowed("HTTP://a.b"));
        for bad in ["about:blank", "javascript:x", "file:///x", "data:,x", ""] {
            assert!(!popup_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn download_names() {
        let d = std::path::Path::new("/dl");
        let none = |_: &std::path::Path| false;
        assert_eq!(download_target(d, "https://x.y/files/report.pdf?x=1#a", none), d.join("report.pdf"));
        assert_eq!(download_target(d, "https://x.y/my%20file.txt", none), d.join("my file.txt"));
        assert_eq!(download_target(d, "https://x.y/", none), d.join("download"));
        let p = download_target(d, "https://x.y/..%2F..%2Fetc%2Fpasswd", none);
        assert_eq!(p.parent(), Some(d));
        assert!(!p.file_name().unwrap().to_string_lossy().contains('/'));
        let taken = |p: &std::path::Path| p == d.join("a.zip") || p == d.join("a (1).zip");
        assert_eq!(download_target(d, "https://x.y/a.zip", taken), d.join("a (2).zip"));
        assert_eq!(download_target(d, "https://x.y/.bashrc", none), d.join("bashrc"));
    }

    #[test]
    fn stale_ids_are_forgotten() {
        state().lock().unwrap().tabs.push(Tab { id: "zz1".into(), url: "about:blank".into(), title: String::new(), agent: None, owner: None, visible: false, loading: false, error: None });
        assert!(forget("zz1"));
        assert!(!forget("zz1"));
        assert!(!forget("never"));
        let ctx = Ctx::test();
        assert!(webview(&ctx, "never").is_err());
    }

    #[test]
    fn profiles_are_stable_and_isolated() {
        let d = std::path::Path::new("/data");
        assert_eq!(profile_dir(d, "a1b2"), d.join("browser-profiles").join("a1b2"));
        assert_eq!(profile_dir(d, "../evil").parent(), Some(d.join("browser-profiles").as_path()));
        assert!(!profile_dir(d, "../evil").to_string_lossy().contains(".."));
        assert_ne!(profile_dir(d, "..").file_name().unwrap(), "..");
        assert_eq!(profile_uuid("a1"), profile_uuid("a1"));
        assert_ne!(profile_uuid("a1"), profile_uuid("a2"));
        let u = profile_uuid("a1");
        assert_eq!(u[6] >> 4, 4);
        assert_eq!(u[8] >> 6, 2);
    }

    #[test]
    fn element_targets() {
        assert_eq!(parse_target(&json!({"ref": 3})).unwrap(), Target::Ref(3));
        assert_eq!(parse_target(&json!({"ref": "[12]"})).unwrap(), Target::Ref(12));
        assert!(parse_target(&json!({"ref": 0})).is_err());
        assert!(parse_target(&json!({"ref": "x"})).is_err());
        assert_eq!(parse_target(&json!({"selector": "#a", "text": "b"})).unwrap(), Target::Selector("#a".into()));
        assert_eq!(parse_target(&json!({"text": "Sign in"})).unwrap(), Target::Text("Sign in".into()));
        assert!(parse_target(&json!({"text": "  "})).is_err());
        // Strings are JSON-escaped so they cannot break out of the script.
        let js = Target::Text("a\"};alert(1);//".into()).js();
        assert_eq!(js, r#"{text:"a\"};alert(1);//"}"#);
    }

    #[test]
    fn snapshot_format_numbers_elements() {
        let v = json!({"url": "https://x.y/", "title": "X", "text": "Hello", "elements": [
            {"ref": 1, "tag": "a", "type": "", "label": "Home", "href": "https://x.y/home", "value": "", "disabled": false},
            {"ref": 2, "tag": "input", "type": "email", "label": "Email", "href": "", "value": "a@b", "disabled": false},
            {"ref": 3, "tag": "button", "type": "", "label": "Sign in", "href": "", "value": "", "disabled": true},
        ]});
        let out = format_snapshot(&v, 10_000);
        assert!(out.contains("[1] a \"Home\" -> https://x.y/home"));
        assert!(out.contains("[2] input email \"Email\" value=\"a@b\""));
        assert!(out.contains("[3] button \"Sign in\" (disabled)"));
        assert!(out.ends_with("Hello"));
        assert!(format_snapshot(&json!({}), 1000).contains("(none)"));
    }

    #[test]
    fn strict_agents_stay_local() {
        assert!(strict_url_ok("http://localhost:3000/"));
        assert!(strict_url_ok("http://127.0.0.1/x"));
        assert!(!strict_url_ok("http://localhost.evil.com/"));
        assert!(!strict_url_ok("https://example.com/"));
        assert!(!strict_url_ok("http://127.0.0.1.nip.io/"));
    }

    #[test]
    fn agent_tabs_are_private() {
        {
            let mut s = state().lock().unwrap();
            for (id, o) in [("p1", "ag1"), ("p2", "ag1"), ("p3", "ag2")] {
                s.tabs.push(Tab { id: id.into(), url: "about:blank".into(), title: String::new(), agent: None, owner: Some(o.into()), visible: false, loading: false, error: None });
            }
            s.current.insert("ag1".into(), "p1".into());
        }
        assert_eq!(agent::tab_for("ag1", &json!({})).unwrap(), "p1");
        assert_eq!(agent::tab_for("ag1", &json!({"tab": "p2"})).unwrap(), "p2");
        assert!(agent::tab_for("ag1", &json!({"tab": "p3"})).is_err());
        assert_eq!(agent::tab_for("ag2", &json!({})).unwrap(), "p3");
        assert!(agent::tab_for("nobody", &json!({})).is_err());
        forget("p1");
        assert_eq!(agent::tab_for("ag1", &json!({})).unwrap(), "p2");
        for id in ["p2", "p3"] {
            forget(id);
        }
    }
}
