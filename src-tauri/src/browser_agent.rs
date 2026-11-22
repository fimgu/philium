//! Agent browsers: every agent owns its own tabs (shown live in the agent pane)
//! with a persistent profile isolated from other agents and from the shared
//! Browser screen. This file holds the MCP `browser_*` tools, the page scripts
//! they inject, the per-agent profile paths and native result plumbing.
//!
//! Results come back through `Webview::eval_with_callback`, which on macOS is
//! WKWebView `evaluateJavaScript:completionHandler:` (JSON serialized natively),
//! with the nonce-protected `document.title` channel as fallback. Screenshots use
//! `takeSnapshotWithConfiguration:completionHandler:` on macOS. Remote pages
//! never get Philium IPC: none of this needs a capability.
//!
//! Events: `browser:agent_action` `{owner, tab, agent, text}` when an agent acts (the pane shows it).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use super::{normalize_url, state, webview, Tab, MAX_TEXT, SNAP_PREFIX};
use crate::ctx::Ctx;

// ---------- profiles ----------

/// Profile folder for an agent: `<data>/browser-profiles/<safe id>`.
pub fn profile_dir(data_dir: &Path, agent_id: &str) -> PathBuf {
    let safe: String = agent_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let safe = if safe.trim_matches('_').is_empty() { format!("x{}", hex16(&profile_uuid(agent_id))) } else { safe };
    data_dir.join("browser-profiles").join(safe)
}

/// Stable WKWebsiteDataStore identifier for an agent (UUID v4 shaped).
pub fn profile_uuid(agent_id: &str) -> [u8; 16] {
    let h = Sha256::digest(format!("philium-browser-profile:{agent_id}").as_bytes());
    let mut u = [0u8; 16];
    u.copy_from_slice(&h[..16]);
    u[6] = (u[6] & 0x0f) | 0x40;
    u[8] = (u[8] & 0x3f) | 0x80;
    u
}

fn hex16(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Close an agent's tabs and delete its browser profile (call when the agent is removed).
pub fn forget_agent(ctx: &Arc<Ctx>, agent_id: &str) {
    let ids: Vec<String> = state().lock().unwrap().tabs.iter().filter(|t| t.owner.as_deref() == Some(agent_id)).map(|t| t.id.clone()).collect();
    for id in ids {
        if let Ok(w) = webview(ctx, &id) {
            let _ = w.close();
        }
        super::forget(&id);
        ctx.emit("browser:closed", json!({"id": id}));
    }
    state().lock().unwrap().current.remove(agent_id);
    let _ = std::fs::remove_dir_all(profile_dir(&ctx.data_dir, agent_id));
    #[cfg(target_os = "macos")]
    if let Some(h) = ctx.handle() {
        let (h, uuid) = (h.clone(), profile_uuid(agent_id));
        tauri::async_runtime::spawn(async move {
            // Give closing webviews a moment to release the store.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = h.remove_data_store(uuid).await;
        });
    }
}

// ---------- page scripts ----------

/// Records console errors, uncaught errors and failed network requests.
/// Runs in the page world (no IPC); agents read it with `browser_console`.
pub const CONSOLE_JS: &str = r#"(()=>{if(window.__philium_log)return;const L=window.__philium_log=[];const push=(k,m)=>{try{L.push({k,m:String(m).slice(0,2000),t:Date.now()});if(L.length>200)L.shift()}catch(e){}};
const s=(a)=>Array.from(a).map(x=>{try{return typeof x==='string'?x:(x&&x.stack)||JSON.stringify(x)}catch(e){return String(x)}}).join(' ');
for(const lv of ['error','warn']){const o=console[lv];console[lv]=function(){push(lv,s(arguments));return o.apply(this,arguments)}}
addEventListener('error',e=>{if(e.target&&e.target!==window&&(e.target.src||e.target.href))push('network','failed to load '+(e.target.src||e.target.href));else push('error',(e.message||'error')+(e.filename?' at '+e.filename+':'+e.lineno:''))},true);
addEventListener('unhandledrejection',e=>push('error','unhandled rejection: '+s([e.reason])));
const f=window.fetch;if(f)window.fetch=function(i,o){const u=(i&&i.url)||String(i);return f.apply(this,arguments).then(r=>{if(!r.ok)push('network',r.status+' '+(o&&o.method||'GET')+' '+u);return r},e=>{push('network','failed '+u+': '+e);throw e})};
const X=XMLHttpRequest.prototype,op=X.open,se=X.send;X.open=function(m,u){this.__p=[m,u];return op.apply(this,arguments)};X.send=function(){this.addEventListener('loadend',()=>{if(!this.status||this.status>=400)push('network',(this.status||'failed')+' '+(this.__p||[]).join(' '))});return se.apply(this,arguments)};})()"#;

/// Shared helpers: element lookup, labels, highlight flash.
const PRELUDE: &str = r#"const P=window.__philium=window.__philium||(()=>{
const SEL='a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
const vis=e=>{const r=e.getBoundingClientRect();if(r.width<1||r.height<1)return false;const s=getComputedStyle(e);return s.visibility!=='hidden'&&s.display!=='none'&&s.opacity!=='0'};
const clean=t=>(t||'').replace(/\s+/g,' ').trim().slice(0,80);
const label=e=>{let t=e.getAttribute('aria-label')||'';if(!t&&e.labels&&e.labels.length)t=e.labels[0].innerText;if(!t&&e.getAttribute('aria-labelledby')){const l=document.getElementById(e.getAttribute('aria-labelledby'));if(l)t=l.innerText}
if(!t)t=e.placeholder||'';if(!t&&e.tagName!=='INPUT'&&e.tagName!=='TEXTAREA')t=e.innerText||'';if(!t)t=e.title||e.alt||'';if(!t&&e.tagName==='INPUT'&&['submit','button'].includes(e.type))t=e.value;if(!t)t=e.name||e.id||'';return clean(t)};
const flash=e=>{try{const r=e.getBoundingClientRect();const d=document.createElement('div');d.style.cssText='position:fixed;z-index:2147483647;pointer-events:none;border:2px solid #8b9cff;border-radius:6px;box-shadow:0 0 0 4px rgba(139,156,255,.3);transition:opacity .5s;left:'+(r.left-3)+'px;top:'+(r.top-3)+'px;width:'+(r.width+2)+'px;height:'+(r.height+2)+'px';document.documentElement.appendChild(d);setTimeout(()=>{d.style.opacity='0'},450);setTimeout(()=>d.remove(),1000)}catch(x){}};
const all=()=>Array.from(document.querySelectorAll(SEL)).filter(vis);
const find=t=>{if(t.ref!=null){const e=document.querySelector('[data-philium-ref="'+t.ref+'"]');if(!e)throw new Error('ref '+t.ref+' not found, take a new browser_snapshot');return e}
if(t.selector){const e=document.querySelector(t.selector);if(!e)throw new Error('no element matches '+t.selector);return e}
if(t.text){const q=t.text.toLowerCase();const c=all();const e=c.find(e=>label(e).toLowerCase()===q)||c.find(e=>label(e).toLowerCase().includes(q));if(!e)throw new Error('no interactive element with text "'+t.text+'"');return e}
throw new Error('give ref, selector or text')};
return {SEL,vis,label,flash,all,find,clean}})();"#;

fn wrap(body: &str) -> String {
    // Exceptions are not reported by every platform's eval callback, so return them as values.
    format!("(()=>{{try{{{PRELUDE}return {{ok:true,v:(()=>{{{body}}})()}}}}catch(e){{return {{ok:false,e:String(e&&e.message||e)}}}}}})()")
}

const SNAPSHOT_JS: &str = r#"document.querySelectorAll('[data-philium-ref]').forEach(e=>e.removeAttribute('data-philium-ref'));
const els=P.all().slice(0,400).map((e,i)=>{e.setAttribute('data-philium-ref',String(i+1));const tag=e.tagName.toLowerCase();
return {ref:i+1,tag,type:e.getAttribute('role')||(tag==='input'?(e.type||'text'):''),label:P.label(e),href:tag==='a'?e.href:'',value:('value' in e&&tag!=='button'&&e.type!=='password')?String(e.value||'').slice(0,60):'',disabled:!!e.disabled}});
return {url:location.href,title:document.title,text:((document.body&&document.body.innerText)||'').slice(0,MAXT),elements:els}"#;

/// A way to point at an element, from tool arguments.
#[derive(Debug, PartialEq)]
pub enum Target {
    Ref(u64),
    Selector(String),
    Text(String),
}

pub fn parse_target(a: &Value) -> Result<Target, String> {
    if let Some(r) = a.get("ref").filter(|v| !v.is_null()) {
        let n = r.as_u64().or_else(|| r.as_str().and_then(|s| s.trim().trim_start_matches(['[', '#']).trim_end_matches(']').parse().ok()));
        return n.filter(|n| *n > 0).map(Target::Ref).ok_or_else(|| "`ref` must be a number from browser_snapshot".into());
    }
    if let Some(s) = a["selector"].as_str().filter(|s| !s.trim().is_empty()) {
        return Ok(Target::Selector(s.to_string()));
    }
    if let Some(s) = a["text"].as_str().filter(|s| !s.trim().is_empty()) {
        return Ok(Target::Text(s.to_string()));
    }
    Err("give `ref` (from browser_snapshot), `selector` or `text`".into())
}

impl Target {
    /// JS object literal consumed by `P.find`. Strings are JSON-escaped.
    pub fn js(&self) -> String {
        match self {
            Target::Ref(n) => format!("{{ref:{n}}}"),
            Target::Selector(s) => format!("{{selector:{}}}", serde_json::to_string(s).unwrap()),
            Target::Text(s) => format!("{{text:{}}}", serde_json::to_string(s).unwrap()),
        }
    }
}

/// Render a page snapshot as compact text: page text then numbered elements.
pub fn format_snapshot(v: &Value, max: usize) -> String {
    let mut out = format!("URL: {}\nTitle: {}\n\nInteractive elements (use ref with browser_click / browser_type):\n", v["url"].as_str().unwrap_or(""), v["title"].as_str().unwrap_or(""));
    let els = v["elements"].as_array().cloned().unwrap_or_default();
    if els.is_empty() {
        out.push_str("(none)\n");
    }
    for e in &els {
        let mut line = format!("[{}] {}", e["ref"], e["tag"].as_str().unwrap_or(""));
        if let Some(t) = e["type"].as_str().filter(|t| !t.is_empty()) {
            line.push_str(&format!(" {t}"));
        }
        let label = e["label"].as_str().unwrap_or("");
        if !label.is_empty() {
            line.push_str(&format!(" \"{label}\""));
        }
        if let Some(h) = e["href"].as_str().filter(|h| !h.is_empty()) {
            line.push_str(&format!(" -> {h}"));
        }
        if let Some(val) = e["value"].as_str().filter(|h| !h.is_empty()) {
            line.push_str(&format!(" value=\"{val}\""));
        }
        if e["disabled"].as_bool() == Some(true) {
            line.push_str(" (disabled)");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("\nPage text:\n");
    out.push_str(v["text"].as_str().unwrap_or(""));
    crate::mcp::trim_output(&out, max)
}

/// Short human description of an element for the pane status line.
fn short(label: &str) -> String {
    let l: String = label.chars().take(40).collect();
    if l.is_empty() { "an element".into() } else { l }
}

// ---------- native plumbing ----------

/// Evaluate JS that returns a JSON-serializable value. Uses the native callback
/// (WKWebView evaluateJavaScript:completionHandler: on macOS) and falls back to
/// the nonce-protected title channel.
pub async fn eval_json(ctx: &Ctx, tab: &str, js: &str) -> Result<Value, String> {
    let w = webview(ctx, tab)?;
    let (tx, rx) = oneshot::channel::<String>();
    let tx = Mutex::new(Some(tx));
    let native = w.eval_with_callback(js.to_string(), move |r| {
        if let Some(t) = tx.lock().unwrap().take() {
            let _ = t.send(r);
        }
    });
    if native.is_ok() {
        if let Ok(Ok(r)) = tokio::time::timeout(Duration::from_secs(15), rx).await {
            return Ok(if r.trim().is_empty() { Value::Null } else { serde_json::from_str(&r).unwrap_or(Value::String(r)) });
        }
    }
    eval_title(ctx, tab, js).await
}

async fn eval_title(ctx: &Ctx, tab: &str, js: &str) -> Result<Value, String> {
    let w = webview(ctx, tab)?;
    let nonce = crate::db::new_id();
    let (tx, rx) = oneshot::channel();
    state().lock().unwrap().snaps.insert(nonce.clone(), tx);
    let script = format!("(()=>{{const old=document.title;let r;try{{r=JSON.stringify({js})}}catch(e){{r=JSON.stringify({{ok:false,e:String(e)}})}};document.title='{SNAP_PREFIX}{nonce}:'+r;setTimeout(()=>{{document.title=old}},50)}})()");
    w.eval(&script).map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(t)) => Ok(serde_json::from_str(&t).unwrap_or(Value::String(t))),
        _ => {
            state().lock().unwrap().snaps.remove(&nonce);
            Err("the page did not answer (still loading?)".into())
        }
    }
}

/// Run a wrapped body and unwrap `{ok,v}|{ok:false,e}`.
async fn run(ctx: &Ctx, tab: &str, body: &str) -> Result<Value, String> {
    let r = eval_json(ctx, tab, &wrap(body)).await?;
    match r["ok"].as_bool() {
        Some(true) => Ok(r["v"].clone()),
        Some(false) => Err(r["e"].as_str().unwrap_or("script error").to_string()),
        None => Ok(r),
    }
}

#[cfg(target_os = "macos")]
async fn screenshot_png(ctx: &Ctx, tab: &str) -> Result<Vec<u8>, String> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError};
    use objc2_web_kit::WKWebView;
    let w = webview(ctx, tab)?;
    let (tx, rx) = oneshot::channel::<Result<Vec<u8>, String>>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let tx2 = tx.clone();
    w.with_webview(move |pw| unsafe {
        let wk: &WKWebView = &*(pw.inner() as *const WKWebView);
        let tx = tx2.clone();
        let block = block2::RcBlock::new(move |img: *mut NSImage, err: *mut NSError| {
            let r = if img.is_null() {
                Err(if err.is_null() { "snapshot failed".to_string() } else { (*err).localizedDescription().to_string() })
            } else {
                (*img)
                    .TIFFRepresentation()
                    .and_then(|t| NSBitmapImageRep::imageRepWithData(&t))
                    .and_then(|rep| rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()))
                    .map(|d| d.to_vec())
                    .ok_or_else(|| "could not encode PNG".to_string())
            };
            if let Some(t) = tx.lock().unwrap().take() {
                let _ = t.send(r);
            }
        });
        wk.takeSnapshotWithConfiguration_completionHandler(None, &block);
    })
    .map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(r)) => r,
        _ => Err("screenshot timed out".into()),
    }
}

#[cfg(not(target_os = "macos"))]
async fn screenshot_png(_ctx: &Ctx, _tab: &str) -> Result<Vec<u8>, String> {
    Err("browser_screenshot is only available on macOS for now; use browser_snapshot".into())
}

// ---------- MCP tools ----------

fn t(name: &str, desc: &str, props: Value, required: &[&str]) -> Value {
    json!({ "name": name, "description": desc, "inputSchema": { "type": "object", "properties": props, "required": required } })
}

pub fn mcp_tools() -> Vec<Value> {
    let s = json!({ "type": "string" });
    let i = json!({ "type": "integer" });
    let b = json!({ "type": "boolean" });
    let target = |extra: Value| {
        let mut m = json!({ "tab": s, "ref": i, "selector": s, "text": s });
        if let (Some(o), Some(e)) = (m.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        m
    };
    vec![
        t("browser_open", "Open a URL in YOUR browser, shown live to the human next to your chat. Reuses your current tab unless new_tab is true. The human may be signed in there.", json!({ "url": s, "new_tab": b }), &["url"]),
        t("browser_tabs", "Your browser tabs and which one is current.", json!({}), &[]),
        t("browser_navigate", "Go to a URL in your current tab (or `tab`).", json!({ "url": s, "tab": s }), &["url"]),
        t("browser_snapshot", "Page text plus a numbered list of interactive elements (links, buttons, inputs with labels). Use the numbers as `ref` in browser_click / browser_type. Refs change after navigation: snapshot again.", json!({ "tab": s }), &[]),
        t("browser_click", "Click an element by `ref` (from browser_snapshot), CSS `selector`, or visible `text`.", target(json!({})), &[]),
        t("browser_type", "Type into an input by `ref`, `selector` or `text` (label). Replaces the value unless append=true. submit=true submits the form / presses Enter.", target(json!({ "value": s, "submit": b, "append": b })), &["value"]),
        t("browser_press", "Press a key on the focused element (Enter, Escape, Tab, ArrowDown, Backspace, or a character).", json!({ "key": s, "tab": s }), &["key"]),
        t("browser_scroll", "Scroll the page (direction up|down|top|bottom, amount in px) or scroll an element (`ref`/`selector`) into view.", json!({ "direction": s, "amount": i, "ref": i, "selector": s, "tab": s }), &[]),
        t("browser_wait", "Wait until `text` appears, a `selector` exists, or `ms` passes. timeout_ms default 10000, max 30000.", json!({ "text": s, "selector": s, "ms": i, "timeout_ms": i, "tab": s }), &[]),
        t("browser_eval", "Run JavaScript in the page and return its JSON result. Write an expression or a function body with `return`.", json!({ "js": s, "tab": s }), &["js"]),
        t("browser_screenshot", "PNG screenshot of the visible page, returned as an image.", json!({ "tab": s }), &[]),
        t("browser_console", "Recent console errors/warnings, uncaught errors and failed network requests on the page.", json!({ "tab": s, "clear": b }), &[]),
        t("browser_back", "Go back in your current tab.", json!({ "tab": s }), &[]),
        t("browser_forward", "Go forward in your current tab.", json!({ "tab": s }), &[]),
        t("browser_reload", "Reload your current tab.", json!({ "tab": s }), &[]),
        t("browser_close", "Close one of your tabs (default current).", json!({ "tab": s }), &[]),
    ]
}

/// The tab an agent's tool acts on: explicit `tab` (must be its own) or its current tab.
pub(crate) fn tab_for(owner: &str, a: &Value) -> Result<String, String> {
    let s = state().lock().unwrap();
    let mine = |id: &str| s.tabs.iter().any(|t| t.id == id && t.owner.as_deref() == Some(owner));
    if let Some(id) = a["tab"].as_str().filter(|x| !x.is_empty()) {
        return if mine(id) { Ok(id.to_string()) } else { Err(format!("tab `{id}` is not one of your tabs")) };
    }
    if let Some(id) = s.current.get(owner).filter(|id| mine(id)) {
        return Ok(id.clone());
    }
    s.tabs.iter().rev().find(|t| t.owner.as_deref() == Some(owner)).map(|t| t.id.clone()).ok_or_else(|| "no tab open: call browser_open first".into())
}

/// Strict (untrusted provider) agents may only point their own tools at local pages
/// or hosts explicitly permitted by their Strict allowlist.
pub fn strict_url_ok(url: &str) -> bool {
    match url.parse::<tauri::Url>() {
        Ok(u) => u.scheme() == "http" && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) || url == "about:blank",
        Err(_) => false,
    }
}

/// Whether a Strict agent may navigate to `url`: always allows local pages (`strict_url_ok`),
/// plus http(s) hosts on the agent's effective Strict allowlist (settings + `Policy.allow_hosts`).
pub fn strict_browser_url_allowed(ctx: &Ctx, agent_name: &str, url: &str) -> bool {
    if strict_url_ok(url) {
        return true;
    }
    let Ok(parsed) = url.parse::<tauri::Url>() else { return false };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else { return false };
    let list = crate::sandbox::effective_allowlist(ctx, agent_name);
    crate::sandbox::host_allowed(&list, host)
}

fn status(ctx: &Ctx, owner: &str, tab: &str, me: &str, text: &str) {
    ctx.emit("browser:agent_action", json!({ "owner": owner, "tab": tab, "agent": me, "text": format!("{me} is {text}") }));
}

/// Entry from `mcp::call_tool` for every `browser_*` tool. `agent` is the agent id.
/// Image results are returned as `{"__mcp_content": [...]}`.
pub async fn mcp_call(ctx: &Arc<Ctx>, agent: &str, me: &str, strict: bool, name: &str, a: Value) -> Result<Value, String> {
    let check = |url: &str| -> Result<String, String> {
        let norm = normalize_url(url)?;
        if strict && !strict_browser_url_allowed(ctx, me, &norm) {
            return Err("Strict agents may only open local URLs (localhost/127.0.0.1) or hosts on their Strict allowlist; add the host in Sandbox allowlist to permit it".into());
        }
        Ok(norm)
    };
    match name {
        "browser_open" => {
            let url = check(super::arg_str(&a, "url")?.as_str())?;
            let reuse = if a["new_tab"].as_bool() == Some(true) { None } else { tab_for(agent, &json!({})).ok() };
            if let Some(id) = reuse {
                navigate(ctx, &id, &url)?;
                status(ctx, agent, &id, me, &format!("opening {url}"));
                return Ok(json!({ "tab": id, "url": url }));
            }
            let t: Tab = super::open(ctx, &url, Some(me.to_string()), Some(agent.to_string()))?;
            state().lock().unwrap().current.insert(agent.to_string(), t.id.clone());
            status(ctx, agent, &t.id, me, &format!("opening {url}"));
            Ok(json!({ "tab": t.id, "url": url }))
        }
        "browser_tabs" => {
            let s = state().lock().unwrap();
            let cur = s.current.get(agent).cloned();
            Ok(json!(s.tabs.iter().filter(|t| t.owner.as_deref() == Some(agent)).map(|t| json!({
                "tab": t.id, "url": t.url, "title": t.title, "loading": t.loading, "error": t.error, "current": Some(&t.id) == cur.as_ref()
            })).collect::<Vec<_>>()))
        }
        "browser_navigate" => {
            let id = tab_for(agent, &a)?;
            let url = check(super::arg_str(&a, "url")?.as_str())?;
            navigate(ctx, &id, &url)?;
            status(ctx, agent, &id, me, &format!("opening {url}"));
            Ok(json!({ "tab": id, "url": url }))
        }
        "browser_snapshot" => {
            let id = tab_for(agent, &a)?;
            status(ctx, agent, &id, me, "reading the page");
            let v = run(ctx, &id, &SNAPSHOT_JS.replace("MAXT", &MAX_TEXT.to_string())).await?;
            Ok(json!(format_snapshot(&v, 40_000)))
        }
        "browser_click" => {
            let id = tab_for(agent, &a)?;
            let tg = parse_target(&a)?;
            let lbl = run(ctx, &id, &format!("const e=P.find({});e.scrollIntoView({{block:'center',inline:'center'}});return P.label(e)", tg.js())).await?;
            status(ctx, agent, &id, me, &format!("clicking {}", short(lbl.as_str().unwrap_or(""))));
            tokio::time::sleep(Duration::from_millis(120)).await;
            let r = run(ctx, &id, &format!(
                "const e=P.find({});P.flash(e);const r=e.getBoundingClientRect(),o={{bubbles:true,cancelable:true,view:window,clientX:r.left+r.width/2,clientY:r.top+r.height/2}};\
                 for(const k of ['pointerdown','mousedown','pointerup','mouseup'])e.dispatchEvent(new (k.startsWith('pointer')&&window.PointerEvent?PointerEvent:MouseEvent)(k,o));\
                 if(e.focus)e.focus();e.click();return {{clicked:P.label(e),tag:e.tagName.toLowerCase()}}", tg.js())).await?;
            Ok(r)
        }
        "browser_type" => {
            let id = tab_for(agent, &a)?;
            let tg = parse_target(&a)?;
            let value = a["value"].as_str().or(a["text_value"].as_str()).ok_or("missing `value`")?;
            let (append, submit) = (a["append"].as_bool() == Some(true), a["submit"].as_bool() == Some(true));
            let lbl = run(ctx, &id, &format!("const e=P.find({});e.scrollIntoView({{block:'center'}});return P.label(e)", tg.js())).await?;
            status(ctx, agent, &id, me, &format!("typing into {}", short(lbl.as_str().unwrap_or(""))));
            let r = run(ctx, &id, &format!(
                "const e=P.find({t});P.flash(e);e.focus();const v={v};\
                 if(e.isContentEditable){{if(!{app}){{document.execCommand('selectAll',false)}}document.execCommand('insertText',false,v)}}\
                 else{{const pr=Object.getPrototypeOf(e);const d=Object.getOwnPropertyDescriptor(pr,'value');const nv=({app}?(e.value||''):'')+v;d&&d.set?d.set.call(e,nv):(e.value=nv);\
                 e.dispatchEvent(new InputEvent('input',{{bubbles:true,data:v,inputType:'insertText'}}));e.dispatchEvent(new Event('change',{{bubbles:true}}))}}\
                 if({sub}){{const k={{key:'Enter',code:'Enter',keyCode:13,which:13,bubbles:true,cancelable:true}};const ok=e.dispatchEvent(new KeyboardEvent('keydown',k));e.dispatchEvent(new KeyboardEvent('keyup',k));\
                 if(ok&&e.form){{e.form.requestSubmit?e.form.requestSubmit():e.form.submit()}}}}\
                 return {{typed:P.label(e),submitted:{sub}}}",
                t = tg.js(), v = serde_json::to_string(value).unwrap(), app = append, sub = submit)).await?;
            Ok(r)
        }
        "browser_press" => {
            let id = tab_for(agent, &a)?;
            let key = super::arg_str(&a, "key")?;
            status(ctx, agent, &id, me, &format!("pressing {key}"));
            run(ctx, &id, &format!(
                "const key={k};const e=document.activeElement||document.body;const codes={{Enter:13,Escape:27,Tab:9,Backspace:8,ArrowUp:38,ArrowDown:40,ArrowLeft:37,ArrowRight:39,' ':32}};\
                 const o={{key,code:key.length===1?'Key'+key.toUpperCase():key,keyCode:codes[key]||key.toUpperCase().charCodeAt(0),bubbles:true,cancelable:true}};\
                 const ok=e.dispatchEvent(new KeyboardEvent('keydown',o));if(key.length===1)e.dispatchEvent(new KeyboardEvent('keypress',o));e.dispatchEvent(new KeyboardEvent('keyup',o));\
                 if(ok&&key==='Enter'&&e.form&&e.tagName==='INPUT'){{e.form.requestSubmit?e.form.requestSubmit():e.form.submit()}}\
                 if(ok&&key.length===1&&'value' in e&&!e.readOnly){{e.value+=key;e.dispatchEvent(new InputEvent('input',{{bubbles:true,data:key}}))}}\
                 if(ok&&key==='Backspace'&&'value' in e){{e.value=e.value.slice(0,-1);e.dispatchEvent(new InputEvent('input',{{bubbles:true}}))}}\
                 if(ok&&key==='Tab'){{const f=P.all();const i=f.indexOf(e);(f[i+1]||f[0])&&(f[i+1]||f[0]).focus()}}\
                 return {{pressed:key,on:e.tagName.toLowerCase()}}", k = serde_json::to_string(&key).unwrap())).await
        }
        "browser_scroll" => {
            let id = tab_for(agent, &a)?;
            if a.get("ref").is_some_and(|v| !v.is_null()) || a["selector"].as_str().is_some() {
                let tg = parse_target(&a)?;
                status(ctx, agent, &id, me, "scrolling");
                return run(ctx, &id, &format!("const e=P.find({});e.scrollIntoView({{block:'center',behavior:'smooth'}});P.flash(e);return {{scrolled_to:P.label(e)}}", tg.js())).await;
            }
            let dir = a["direction"].as_str().unwrap_or("down");
            let amt = a["amount"].as_i64().unwrap_or(0);
            status(ctx, agent, &id, me, &format!("scrolling {dir}"));
            run(ctx, &id, &format!(
                "const d={d},h={a}||Math.round(innerHeight*0.8);const se=document.scrollingElement||document.documentElement;\
                 if(d==='top')scrollTo({{top:0,behavior:'smooth'}});else if(d==='bottom')scrollTo({{top:se.scrollHeight,behavior:'smooth'}});else scrollBy({{top:d==='up'?-h:h,behavior:'smooth'}});\
                 return {{y:Math.round(scrollY),height:se.scrollHeight,viewport:innerHeight}}", d = serde_json::to_string(dir).unwrap(), a = amt)).await
        }
        "browser_wait" => {
            let id = tab_for(agent, &a)?;
            if let Some(ms) = a["ms"].as_u64().filter(|_| a["text"].is_null() && a["selector"].is_null()) {
                tokio::time::sleep(Duration::from_millis(ms.min(30_000))).await;
                return Ok(json!({ "waited_ms": ms.min(30_000) }));
            }
            let cond = if let Some(t) = a["text"].as_str() {
                format!("return ((document.body&&document.body.innerText)||'').toLowerCase().includes({}.toLowerCase())", serde_json::to_string(t).unwrap())
            } else if let Some(s) = a["selector"].as_str() {
                format!("return !!document.querySelector({})", serde_json::to_string(s).unwrap())
            } else {
                return Err("give `text`, `selector` or `ms`".into());
            };
            let limit = a["timeout_ms"].as_u64().unwrap_or(10_000).min(30_000);
            status(ctx, agent, &id, me, "waiting for the page");
            let start = std::time::Instant::now();
            loop {
                if run(ctx, &id, &cond).await.ok().and_then(|v| v.as_bool()) == Some(true) {
                    return Ok(json!({ "found": true, "after_ms": start.elapsed().as_millis() as u64 }));
                }
                if start.elapsed().as_millis() as u64 >= limit {
                    return Err(format!("not found after {limit} ms"));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        "browser_eval" => {
            let id = tab_for(agent, &a)?;
            let js = super::arg_str(&a, "js")?;
            status(ctx, agent, &id, me, "running a script");
            let body = if js.contains("return") { js.clone() } else { format!("return ({js})") };
            let v = run(ctx, &id, &format!("const __r=(()=>{{{body}\n}})();try{{return JSON.parse(JSON.stringify(__r===undefined?null:__r))}}catch(e){{return String(__r)}}")).await?;
            Ok(json!(crate::mcp::trim_output(&serde_json::to_string_pretty(&v).unwrap_or_default(), 40_000)))
        }
        "browser_screenshot" => {
            let id = tab_for(agent, &a)?;
            status(ctx, agent, &id, me, "taking a screenshot");
            let png = screenshot_png(ctx, &id).await?;
            use base64::Engine;
            let data = base64::engine::general_purpose::STANDARD.encode(png);
            Ok(json!({ "__mcp_content": [{ "type": "image", "data": data, "mimeType": "image/png" }] }))
        }
        "browser_console" => {
            let id = tab_for(agent, &a)?;
            let clear = a["clear"].as_bool() == Some(true);
            let v = run(ctx, &id, &format!("const l=(window.__philium_log||[]).slice(-100);if({clear})(window.__philium_log||[]).length=0;return l")).await?;
            let lines: Vec<String> = v.as_array().cloned().unwrap_or_default().iter().map(|e| format!("[{}] {}", e["k"].as_str().unwrap_or(""), e["m"].as_str().unwrap_or(""))).collect();
            Ok(json!(if lines.is_empty() { "No console errors or failed requests recorded.".to_string() } else { lines.join("\n") }))
        }
        "browser_back" | "browser_forward" | "browser_reload" => {
            let id = tab_for(agent, &a)?;
            let m = &name["browser_".len()..];
            status(ctx, agent, &id, me, &format!("{}", match m { "back" => "going back", "forward" => "going forward", _ => "reloading" }));
            super::call(ctx, m, json!({ "tab": id })).await?;
            Ok(json!("ok"))
        }
        "browser_close" => {
            let id = tab_for(agent, &a)?;
            super::call(ctx, "close", json!({ "tab": id })).await?;
            Ok(json!("closed"))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn navigate(ctx: &Ctx, id: &str, url: &str) -> Result<(), String> {
    let w = webview(ctx, id)?;
    w.navigate(url.parse().map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?).map_err(|e| e.to_string())?;
    super::update(ctx, id, |t| {
        t.url = url.to_string();
        t.error = None;
        t.loading = true;
    });
    Ok(())
}
