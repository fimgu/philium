//! RPC: list, open {title?,cwd?,command?}, write {id,data:base64}, resize {id,cols,rows},
//! kill {id}, buffer {id}, rename {id,title}, remove {id} (exited only).
//! Events: terminals:data {id,data:base64,offset} (exclusive byte offset),
//! terminals:exit {id,code}, terminals:changed {id}. Scrollback is memory-only.
use crate::{
    ctx::{arg_str, Ctx},
    db,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
const CAP: usize = 256 * 1024;
pub struct Opts {
    pub title: String,
    pub command: Option<Vec<String>>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub agent_id: Option<String>,
}
struct Output {
    ring: VecDeque<u8>,
    pending: Vec<u8>,
    total: u64,
    exit: Option<u32>,
    announced: bool,
}
struct Terminal {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    output: Mutex<Output>,
    agent: Option<String>,
}
struct Manager {
    ctx: Weak<Ctx>,
    entries: Mutex<HashMap<String, Arc<Terminal>>>,
}
static MANAGERS: OnceLock<Mutex<HashMap<usize, Arc<Manager>>>> = OnceLock::new();
fn manager(ctx: &Arc<Ctx>) -> Arc<Manager> {
    let mut all = MANAGERS.get_or_init(Default::default).lk();
    all.retain(|_, m| m.ctx.strong_count() > 0);
    all.entry(Arc::as_ptr(ctx) as usize).or_insert_with(|| {
        let m = Arc::new(Manager { ctx: Arc::downgrade(ctx), entries: Mutex::new(HashMap::new()) });
        let weak = Arc::downgrade(&m);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(16));
            let Some(m) = weak.upgrade() else { break };
            let Some(ctx) = m.ctx.upgrade() else { break };
            for (id, t) in m.entries.lk().iter() {
                let mut out = t.output.lk();
                if !out.pending.is_empty() { ctx.emit("terminals:data", json!({"id":id,"data":B64.encode(std::mem::take(&mut out.pending)),"offset":out.total})); }
                if let Some(code) = out.exit { if !out.announced { out.announced = true; ctx.emit("terminals:exit", json!({"id":id,"code":code})); ctx.notify("terminals"); } }
            }
        });
        m
    }).clone()
}
pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.exec(
        "UPDATE terminals SET state='exited', ended_at=?1 WHERE state='running'",
        [db::now()],
    )?;
    manager(ctx);
    Ok(())
}
pub fn open(ctx: &Arc<Ctx>, o: Opts) -> Result<String, String> {
    let command = o.command.unwrap_or_else(|| {
        vec![
            std::env::var("SHELL").unwrap_or_else(|_| {
                if cfg!(windows) {
                    "cmd.exe".into()
                } else {
                    "/bin/sh".into()
                }
            }),
            if cfg!(windows) {
                "/Q".into()
            } else {
                "-l".into()
            },
        ]
    });
    let program = command
        .first()
        .filter(|s| !s.is_empty())
        .ok_or("command must not be empty")?;
    let pair = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(&command[1..]);
    cmd.cwd(&o.cwd);
    cmd.env_clear();
    for (k, v) in std::env::vars().chain(o.env) {
        if !crate::hal::is_credential_var(&k) {
            cmd.env(k, v);
        }
    }
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader().map_err(|e| {
        let _ = child.kill();
        e.to_string()
    })?;
    let writer = pair.master.take_writer().map_err(|e| {
        let _ = child.kill();
        e.to_string()
    })?;
    let id = db::new_id();
    if let Err(e) = ctx.db.exec("INSERT INTO terminals(id,agent_id,title,command,cwd,pid,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", rusqlite::params![id,o.agent_id,o.title,serde_json::to_string(&command).unwrap(),o.cwd.to_string_lossy(),child.process_id(),db::now()]) { let _ = child.kill(); let _ = child.wait(); return Err(e); }
    let pid = child.process_id();
    let killer = child.clone_killer();
    // Personal shells are owned by the user, not orphaned agent resources.
    let resource = pid.filter(|_| o.agent_id.is_some()).map(|pid| {
        crate::reaper::register(
            ctx,
            o.agent_id.as_deref(),
            "terminal",
            &pid.to_string(),
            json!({"pid":pid,"pgid":pid,"terminal_id":id}),
        )
    });
    let t = Arc::new(Terminal {
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        killer: Mutex::new(killer),
        pid,
        output: Mutex::new(Output {
            ring: VecDeque::new(),
            pending: Vec::new(),
            total: 0,
            exit: None,
            announced: false,
        }),
        agent: o.agent_id,
    });
    manager(ctx)
        .entries
        .lock()
        .unwrap()
        .insert(id.clone(), t.clone());
    let weak = Arc::downgrade(ctx);
    let tid = id.clone();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut out = t.output.lk();
                    out.ring.extend(&buf[..n]);
                    out.total += n as u64;
                    while out.ring.len() > CAP {
                        out.ring.pop_front();
                    }
                    // Bound unread event data as well as retained scrollback.
                    out.pending.extend_from_slice(&buf[..n]);
                    if out.pending.len() > CAP {
                        let n = out.pending.len() - CAP;
                        out.pending.drain(..n);
                    }
                }
            }
        }
        let code = loop {
            match t.child.lk().try_wait() {
                Ok(Some(status)) => break status.exit_code(),
                Err(_) => break 1,
                Ok(None) => {}
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Some(ctx) = weak.upgrade() {
            if let Some(resource) = resource {
                crate::reaper::release(&ctx, &resource);
            }
            let _ = ctx.db.exec(
                "UPDATE terminals SET state='exited',exit_code=?2,ended_at=?3 WHERE id=?1",
                rusqlite::params![tid, code, db::now()],
            );
        }
        t.output.lk().exit = Some(code);
    });
    ctx.emit("terminals:changed", json!({"id":id}));
    ctx.notify("terminals");
    Ok(id)
}
fn get(ctx: &Arc<Ctx>, id: &str) -> Result<Arc<Terminal>, String> {
    manager(ctx)
        .entries
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| "terminal is no longer attached".into())
}
pub fn write(ctx: &Arc<Ctx>, id: &str, data: &[u8]) -> Result<(), String> {
    let t = get(ctx, id)?;
    if t.output.lk().exit.is_some() {
        return Err("terminal exited".into());
    }
    let mut w = t.writer.lk();
    w.write_all(data)
        .and_then(|_| w.flush())
        .map_err(|e| e.to_string())
}
pub fn kill(ctx: &Arc<Ctx>, id: &str) -> Result<(), String> {
    let t = get(ctx, id)?;
    if t.output.lk().exit.is_some() {
        return Ok(());
    }
    #[cfg(unix)]
    if let Some(pid) = t.pid {
        // The PTY child is a session leader. Signal only its owned process group,
        // before the child is reaped, so PID reuse cannot target an unrelated group.
        let mut child = t.child.lk();
        if child.try_wait().map_err(|e| e.to_string())?.is_none() {
            let rc = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            if rc == 0 {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }
    let result = t.killer.lk().kill().map_err(|e| e.to_string());
    result
}
pub fn kill_agent(ctx: &Arc<Ctx>, agent_id: &str) {
    let ids: Vec<_> = manager(ctx)
        .entries
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| t.agent.as_deref() == Some(agent_id))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let _ = kill(ctx, &id);
    }
}
fn bytes(ctx: &Arc<Ctx>, id: &str) -> Option<Vec<u8>> {
    Some(
        get(ctx, id)
            .ok()?
            .output
            .lock()
            .unwrap()
            .ring
            .iter()
            .copied()
            .collect(),
    )
}
/// Total bytes ever produced (the exclusive end offset of the stream).
pub fn total(ctx: &Arc<Ctx>, id: &str) -> Option<u64> {
    Some(get(ctx, id).ok()?.output.lk().total)
}
/// Retained bytes from stream offset `from` onwards (clamped to scrollback).
pub fn since(ctx: &Arc<Ctx>, id: &str, from: u64) -> Option<Vec<u8>> {
    let t = get(ctx, id).ok()?;
    let out = t.output.lk();
    let start = out.total - out.ring.len() as u64;
    let skip = from.saturating_sub(start).min(out.ring.len() as u64) as usize;
    Some(out.ring.iter().skip(skip).copied().collect())
}
/// Still attached and its process has not exited.
pub fn alive(ctx: &Arc<Ctx>, id: &str) -> bool {
    get(ctx, id).is_ok_and(|t| t.output.lk().exit.is_none())
}
/// Strip terminal escape sequences and carriage returns.
pub fn clean(text: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]").unwrap()
    });
    re.replace_all(text, "").replace("\r\n", "\n").replace('\r', "")
}
pub fn snapshot(ctx: &Arc<Ctx>, id: &str, max_bytes: usize) -> Option<String> {
    let data = bytes(ctx, id)?;
    let text = String::from_utf8_lossy(&data);
    // Remove OSC strings, CSI sequences, and remaining two-byte escapes.
    let re =
        regex::Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]")
            .unwrap();
    let clean = re.replace_all(&text, "");
    let mut start = clean.len().saturating_sub(max_bytes);
    while !clean.is_char_boundary(start) {
        start += 1;
    }
    Some(clean[start..].to_string())
}
pub fn shutdown(ctx: &Arc<Ctx>) {
    let ids: Vec<_> = manager(ctx)
        .entries
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    for id in ids {
        let _ = kill(ctx, &id);
    }
}
pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    if method == "list" {
        return Ok(json!(ctx.db.query("SELECT t.*,a.name AS agent_name FROM terminals t LEFT JOIN agents a ON a.id=t.agent_id ORDER BY t.created_at", [])?));
    }
    if method == "open" {
        let command = match args.get("command").filter(|v| !v.is_null()) {
            Some(v) => {
                Some(serde_json::from_value::<Vec<String>>(v.clone()).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        return Ok(json!(open(
            ctx,
            Opts {
                title: args["title"].as_str().unwrap_or("Terminal").into(),
                command,
                cwd: args["cwd"]
                    .as_str()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| ctx.root.clone()),
                env: vec![],
                agent_id: None
            }
        )?));
    }
    let id = arg_str(&args, "id")?;
    match method {
        "write" => {
            write(
                ctx,
                &id,
                &B64.decode(arg_str(&args, "data")?)
                    .map_err(|e| e.to_string())?,
            )?;
        }
        "kill" => kill(ctx, &id)?,
        "buffer" => {
            let (data, offset) = match get(ctx, &id) {
                Ok(t) => {
                    let out = t.output.lk();
                    (out.ring.iter().copied().collect::<Vec<u8>>(), out.total)
                }
                Err(_) => (vec![], 0),
            };
            return Ok(json!({"id":id,"data":B64.encode(data),"offset":offset}));
        }
        "resize" => {
            let cols = args["cols"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 1000)
                .ok_or("cols must be 1..1000")?;
            let rows = args["rows"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 1000)
                .ok_or("rows must be 1..1000")?;
            get(ctx, &id)?
                .master
                .lock()
                .unwrap()
                .resize(PtySize {
                    cols: cols as u16,
                    rows: rows as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string())?;
        }
        "rename" => {
            let title = arg_str(&args, "title")?;
            if title.trim().is_empty() {
                return Err("title must not be empty".into());
            }
            ctx.db.exec(
                "UPDATE terminals SET title=?2 WHERE id=?1",
                rusqlite::params![id, title],
            )?;
            ctx.emit("terminals:changed", json!({"id":id}));
            ctx.notify("terminals");
        }
        "remove" => {
            if ctx.db.exec(
                "DELETE FROM terminals WHERE id=?1 AND state='exited'",
                [&id],
            )? == 0
            {
                return Err("only exited terminals can be removed".into());
            }
            manager(ctx).entries.lk().remove(&id);
            ctx.emit("terminals:changed", json!({"id":id}));
        }
        _ => return Err(format!("unknown terminals.{method}")),
    }
    Ok(Value::Null)
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn wait(ctx: &Arc<Ctx>, id: &str) {
        for _ in 0..500 {
            if get(ctx, id).unwrap().output.lk().exit.is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        shutdown(ctx);
        panic!("PTY did not exit");
    }
    fn options(command: &str, agent: Option<&str>) -> Opts {
        Opts {
            title: "test".into(),
            command: Some(vec!["/bin/sh".into(), "-c".into(), command.into()]),
            cwd: std::env::temp_dir(),
            env: vec![],
            agent_id: agent.map(str::to_owned),
        }
    }
    #[tokio::test]
    async fn interactive_lifecycle_and_peer_isolation() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let id = open(
            &ctx,
            options(
                "read answer; printf 'received:%s' \"$answer\"",
                Some("lion"),
            ),
        )
        .unwrap();
        let peer = open(
            &ctx,
            options("read answer; printf 'peer:%s' \"$answer\"", Some("otter")),
        )
        .unwrap();
        call(&ctx, "resize", json!({"id":id,"cols":100,"rows":40}))
            .await
            .unwrap();
        call(&ctx, "rename", json!({"id":id,"title":"Renamed"}))
            .await
            .unwrap();
        assert!(call(&ctx, "remove", json!({"id":id})).await.is_err());
        write(&ctx, &id, b"hello\n").unwrap();
        wait(&ctx, &id);
        assert!(snapshot(&ctx, &id, CAP).unwrap().contains("received:hello"));
        let b = call(&ctx, "buffer", json!({"id":id})).await.unwrap();
        assert!(B64.decode(b["data"].as_str().unwrap()).unwrap().len() > 5);
        assert!(b["offset"].as_u64().unwrap() > 5);
        let active = open(&ctx, options("sleep 60", Some("lion"))).unwrap();
        kill_agent(&ctx, "lion");
        wait(&ctx, &active);
        assert!(get(&ctx, &peer)
            .unwrap()
            .output
            .lock()
            .unwrap()
            .exit
            .is_none());
        write(&ctx, &peer, b"alive\n").unwrap();
        wait(&ctx, &peer);
        assert!(snapshot(&ctx, &peer, CAP).unwrap().contains("peer:alive"));
        call(&ctx, "remove", json!({"id":id})).await.unwrap();
        assert!(snapshot(&ctx, &id, CAP).is_none());
    }
    #[test]
    fn scrubs_credentials_and_bounds_output() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let mut o = options(
            "printf '%s/%s' \"${OPENAI_API_KEY-unset}\" \"$PHILIUM_TEST_VISIBLE\"",
            None,
        );
        o.env = vec![
            ("OPENAI_API_KEY".into(), "must-not-inherit".into()),
            ("PHILIUM_TEST_VISIBLE".into(), "visible".into()),
        ];
        let id = open(&ctx, o).unwrap();
        wait(&ctx, &id);
        assert_eq!(snapshot(&ctx, &id, CAP).unwrap(), "unset/visible");
        let id = open(
            &ctx,
            options("head -c 300000 /dev/zero | tr '\\000' x", None),
        )
        .unwrap();
        wait(&ctx, &id);
        let t = get(&ctx, &id).unwrap();
        let out = t.output.lk();
        assert_eq!(out.ring.len(), CAP);
        assert!(out.pending.len() <= CAP);
        assert_eq!(out.total, 300000);
    }
    #[test]
    fn shutdown_kills_active_shell() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let id = open(&ctx, options("sleep 60", Some("lion"))).unwrap();
        shutdown(&ctx);
        wait(&ctx, &id);
        assert!(get(&ctx, &id)
            .unwrap()
            .output
            .lock()
            .unwrap()
            .exit
            .is_some());
    }
    #[test]
    fn pty_output_and_exit() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let id = open(
            &ctx,
            Opts {
                title: "test".into(),
                command: Some(vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '\\033[31mhello\\033[0m'; exit 7".into(),
                ]),
                cwd: std::env::temp_dir(),
                env: vec![],
                agent_id: None,
            },
        )
        .unwrap();
        for _ in 0..200 {
            if get(&ctx, &id)
                .unwrap()
                .output
                .lock()
                .unwrap()
                .exit
                .is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(snapshot(&ctx, &id, 100).unwrap(), "hello");
        assert_eq!(snapshot(&ctx, &id, 3).unwrap(), "llo");
        assert_eq!(
            ctx.db
                .query("SELECT exit_code FROM terminals WHERE id=?1", [id])
                .unwrap()[0]["exit_code"],
            7
        );
    }
}

#[allow(unused_imports)]
use crate::ctx::{LockExt, RwExt};
