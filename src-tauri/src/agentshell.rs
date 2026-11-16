//! One persistent, visible shell per agent.
//!
//! `shell_run` types the command into the agent's own bash (a normal Philium
//! terminal, so the human watches it live and can type too). State carries
//! over between commands (cd, exports, venvs), like a real terminal.
//! Completion is detected with private OSC markers emitted by the shell:
//! a DEBUG trap prints START before the first command of a line and
//! PROMPT_COMMAND prints DONE;<exit code> when the prompt returns. Terminal
//! renderers ignore unknown OSC strings, so the human never sees them.
//! Background commands (servers, watchers) still get their own terminal.
use crate::ctx::{Ctx, LockExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const START: &[u8] = b"\x1b]6973;s\x07";
const DONE: &[u8] = b"\x1b]6973;d;";

#[derive(Default)]
struct Slot {
    id: Option<String>,
    /// Stream offset of a command that timed out and may still be running.
    pending: Option<u64>,
}
#[derive(Default)]
struct Shell {
    // UI metadata must never wait for a running command.
    current: Mutex<Option<String>>,
    command: tokio::sync::Mutex<Slot>,
}
type Slots = Mutex<HashMap<String, Arc<Shell>>>;
static S: OnceLock<Slots> = OnceLock::new();
fn slot(agent: &str, shell: &str) -> Arc<Shell> {
    S.get_or_init(Default::default).lk().entry(format!("{agent}\0{shell}")).or_default().clone()
}

/// Poll the command, not the lifetime of its persistent bash process.
pub fn command_status(ctx: &Arc<Ctx>, agent: &str, id: &str) -> Option<Value> {
    let prefix = format!("{agent}\0");
    let shells: Vec<_> = S.get_or_init(Default::default).lk().iter()
        .filter(|(key, _)| key.starts_with(&prefix)).map(|(_, s)| s.clone()).collect();
    for shell in shells {
        if shell.current.lk().as_deref() != Some(id) { continue; }
        let Ok(s) = shell.command.try_lock() else {
            return Some(json!({"state":"running", "running":true, "finished":false, "exit_code":null}));
        };
        if let Some(from) = s.pending {
            if let Some((_, code)) = crate::terminals::since(ctx, id, from).and_then(|b| parse(&b)) {
                return Some(json!({"state":"finished", "running":false, "finished":true, "exit_code":code}));
            }
            let alive = crate::terminals::alive(ctx, id);
            return Some(json!({"state":if alive {"running"} else {"interrupted"}, "running":alive, "finished":false, "exit_code":null}));
        }
        return Some(json!({"state":"idle", "running":false, "finished":null, "exit_code":null}));
    }
    None
}

/// Shell names: short, lowercase words. "main" is the default shell.
pub fn shell_name(s: Option<&str>) -> Result<String, String> {
    let n = s.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(MAIN).to_lowercase();
    if n.len() > 24 || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("shell names are short words (letters, digits, - and _)".into());
    }
    Ok(n)
}
pub const MAIN: &str = "main";

fn rc(name: &str) -> String {
    let name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    format!(
        "export BASH_SILENCE_DEPRECATION_WARNING=1\n\
         PS1='\\[\\e[38;5;110m\\]{name}\\[\\e[38;5;244m\\] \\W \\$\\[\\e[0m\\] '\n\
         __ph_s=\n\
         __ph_prompt() {{ local rc=$?; printf \"\\033]6973;d;%s\\007\" \"$rc\"; __ph_s=; }}\n\
         trap '[ -z \"$__ph_s\" ] && [ \"$BASH_COMMAND\" != __ph_prompt ] && {{ __ph_s=1; printf \"\\033]6973;s\\007\"; }}' DEBUG\n\
         PROMPT_COMMAND=__ph_prompt\n"
    )
}

/// Find a finished command in `buf` (bytes written after the command was
/// typed): returns (output bytes, exit code). Output starts after START when
/// present (so the echoed command line is skipped), else after the first
/// newline (empty line or syntax error, where no command ran).
pub fn parse(buf: &[u8]) -> Option<(Vec<u8>, i32)> {
    let find = |hay: &[u8], pat: &[u8], from: usize| {
        hay.get(from..)?.windows(pat.len()).position(|w| w == pat).map(|i| i + from)
    };
    let start = find(buf, START, 0);
    let d = find(buf, DONE, start.map_or(0, |s| s + START.len()))?;
    let code_end = find(buf, b"\x07", d)?;
    let code = std::str::from_utf8(&buf[d + DONE.len()..code_end]).ok()?.trim().parse().unwrap_or(-1);
    let from = match start {
        Some(s) => s + START.len(),
        None => buf.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1).min(d),
    };
    Some((buf[from..d].to_vec(), code))
}

fn text(b: &[u8]) -> String {
    crate::terminals::clean(&String::from_utf8_lossy(b)).trim_matches('\n').to_string()
}

fn quote(p: &std::path::Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

async fn ensure(ctx: &Arc<Ctx>, agent: &str, shell: &str, s: &mut Slot) -> Result<String, String> {
    if let Some(id) = s.id.as_ref().filter(|id| crate::terminals::alive(ctx, id)) {
        return Ok(id.clone());
    }
    let rt = crate::agents::runtime(agent).ok_or("agent not running")?;
    let scratch = crate::sandbox::scratch_dir(ctx, &rt.name);
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let rcfile = scratch.join(".philium-bashrc");
    std::fs::write(&rcfile, rc(&rt.name)).map_err(|e| e.to_string())?;
    let argv = vec!["/bin/bash".to_string(), "--noprofile".into(), "--rcfile".into(), rcfile.to_string_lossy().into(), "-i".into()];
    let argv = crate::sandbox::wrap_argv(rt.policy.as_ref(), argv);
    let mut env = rt.env.clone();
    env.push(("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()));
    let id = crate::terminals::open(
        ctx,
        crate::terminals::Opts { title: if shell == MAIN { format!("{} shell", rt.name) } else { format!("{}: {shell}", rt.name) }, command: Some(argv), cwd: rt.cwd.clone(), env, agent_id: Some(agent.to_string()) },
    )?;
    if shell == MAIN {
        ctx.db.kv_set(&format!("agent.shell.{agent}"), &id)?;
    }
    // Wait for the first prompt so the first command is not typed into rc loading.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let b = crate::terminals::since(ctx, &id, 0).unwrap_or_default();
        if b.windows(DONE.len()).any(|w| w == DONE) {
            break;
        }
        if !crate::terminals::alive(ctx, &id) {
            return Err(format!("the shell exited at startup: {}", text(&b)));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    *slot(agent, shell).current.lk() = Some(id.clone());
    s.id = Some(id.clone());
    s.pending = None;
    Ok(id)
}

/// Open (or return) a named shell. Errors explain why it could not start.
pub async fn open(ctx: &Arc<Ctx>, agent: &str, shell: &str) -> Result<String, String> {
    let sl = slot(agent, shell);
    let mut s = sl.command.lock().await;
    ensure(ctx, agent, shell, &mut s).await
}

/// The agent's main shell, opening it if the agent runs. None when stopped.
pub async fn current(ctx: &Arc<Ctx>, agent: &str, shell: &str, open: bool) -> Option<String> {
    let sl = slot(agent, shell);
    let current = sl.current.lk().clone();
    if let Some(id) = current.filter(|id| crate::terminals::alive(ctx, id)) {
        return Some(id);
    }
    if !open {
        return None;
    }
    let mut s = sl.command.lock().await;
    ensure(ctx, agent, shell, &mut s).await.ok()
}

// Return a pollable terminal well before the MCP transport's 300-second cap.
// This is a response budget, not a deadline that kills the command.
fn foreground_budget(requested: Duration) -> Duration {
    requested.min(Duration::from_secs(30))
}

pub async fn run(ctx: &Arc<Ctx>, agent: &str, shell: &str, command: &str, cwd: Option<PathBuf>, timeout: Duration) -> Result<Value, String> {
    let timeout = foreground_budget(timeout);
    let deadline = Instant::now() + timeout;
    let sl = slot(agent, shell);
    let mut s = tokio::time::timeout(timeout, sl.command.lock())
        .await
        .map_err(|_| "this shell is busy with another command. Use another shell (shell_open), or background=true for long-running commands.")?;
    let id = ensure(ctx, agent, shell, &mut s).await?;
    // A previous response returned early or was cancelled. Never enqueue a
    // retry behind an unfinished command: report its terminal immediately.
    if let Some(from) = s.pending {
        if crate::terminals::since(ctx, &id, from).and_then(|b| parse(&b)).is_some() {
            s.pending = None;
        } else {
            return Err(format!(
                "shell \"{shell}\" is still running the previous command (terminal {id}). Do not retry it. Use shell_read to watch it, shell_write to answer it or \"\\u0003\" to interrupt, or shell_kill to reset the shell."
            ));
        }
    }
    let from = crate::terminals::total(ctx, &id).ok_or("the shell is gone")?;
    let mut line = match &cwd {
        Some(d) => format!("cd {} && {}", quote(d), command.trim_end()),
        None => command.trim_end().to_string(),
    };
    line.push('\n');
    crate::terminals::write(ctx, &id, line.as_bytes())?;
    // Persist before the first await so dropping this request cannot forget
    // the live command and let a retry type into its input.
    s.pending = Some(from);
    loop {
        let buf = crate::terminals::since(ctx, &id, from).unwrap_or_default();
        if let Some((out, code)) = parse(&buf) {
            s.pending = None;
            return Ok(json!({
                "terminal": id,
                "exit_code": code,
                "finished": true,
                "running": false,
                "output": crate::mcp::trim_output(&text(&out), 12000),
            }));
        }
        let alive = crate::terminals::alive(ctx, &id);
        if !alive || Instant::now() >= deadline {
            if alive {
                s.pending = Some(from);
            } else {
                s.id = None;
                s.pending = None;
            }
            let partial = buf.windows(START.len()).position(|w| w == START).map_or(&buf[..], |i| &buf[i + START.len()..]);
            return Ok(json!({
                "terminal": id,
                "exit_code": Value::Null,
                "finished": false,
                "running": alive,
                "output": crate::mcp::trim_output(&text(partial), 12000),
                "note": if alive { "still running, not failed: do not run this command again. Use shell_read with the returned terminal id to follow progress and completion, or shell_write to answer prompts or send \"\\u0003\" to interrupt" } else { "the shell exited; the next shell_run opens a new one" },
            }));
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_wait_stays_below_transport_timeout() {
        assert_eq!(foreground_budget(Duration::from_secs(900)), Duration::from_secs(30));
        assert_eq!(foreground_budget(Duration::from_secs(2)), Duration::from_secs(2));
    }

    #[test]
    fn parse_markers() {
        let b = b"ls -la\r\n\x1b]6973;s\x07a\r\nb\r\n\x1b]6973;s\x07\x1b]6973;d;0\x07\x1b[2mlion\x1b[0m ~ $ ";
        let (out, code) = parse(b).unwrap();
        assert_eq!(text(&out), "a\nb");
        assert_eq!(code, 0);
        // No command ran (syntax error): output after the echoed line.
        let (out, code) = parse(b"if\r\nbash: syntax error\r\n\x1b]6973;d;2\x07").unwrap();
        assert_eq!(text(&out), "bash: syntax error");
        assert_eq!(code, 2);
        assert!(parse(b"sleep 5\r\n\x1b]6973;s\x07").is_none());
        // DONE without its terminator yet.
        assert!(parse(b"x\r\n\x1b]6973;s\x07\x1b]6973;d;1").is_none());
    }

    /// Real bash with the real rc file: exit codes, persistent state, multi-line.
    #[tokio::test]
    async fn real_shell_roundtrip() {
        let ctx = Ctx::test();
        let dir = std::env::temp_dir().join(format!("phsh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rcfile = dir.join("rc");
        std::fs::write(&rcfile, rc("lion")).unwrap();
        let id = crate::terminals::open(
            &ctx,
            crate::terminals::Opts {
                title: "t".into(),
                command: Some(vec!["/bin/bash".into(), "--noprofile".into(), "--rcfile".into(), rcfile.to_string_lossy().into(), "-i".into()]),
                cwd: dir.clone(),
                env: vec![],
                agent_id: None,
            },
        )
        .unwrap();
        let wait = |from: u64| {
            let ctx = ctx.clone();
            let id = id.clone();
            async move {
                for _ in 0..200 {
                    if let Some(r) = crate::terminals::since(&ctx, &id, from).and_then(|b| parse(&b)) {
                        return r;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                panic!("no marker: {:?}", text(&crate::terminals::since(&ctx, &id, from).unwrap()));
            }
        };
        wait(0).await;
        let cmd = |c: &str| {
            let from = crate::terminals::total(&ctx, &id).unwrap();
            crate::terminals::write(&ctx, &id, format!("{c}\n").as_bytes()).unwrap();
            wait(from)
        };
        let (o, c) = cmd("echo hi; false").await;
        assert_eq!((text(&o).as_str(), c), ("hi", 1));
        let (_, c) = cmd("export PH_X=42; cd /tmp").await;
        assert_eq!(c, 0);
        let (o, _) = cmd("echo $PH_X $(pwd)").await;
        assert_eq!(text(&o), "42 /tmp");
        let (o, c) = cmd("for i in 1 2; do\n echo n$i\ndone").await;
        assert!(text(&o).ends_with("n1\nn2"), "{:?}", text(&o));
        assert_eq!(c, 0);
        let (_, c) = cmd("exit_code_test() { return 7; }; exit_code_test").await;
        assert_eq!(c, 7);
        // Exercise the actual command path while the UI asks for metadata.
        let agent = format!("metadata-test-{id}");
        let sl = slot(&agent, MAIN);
        *sl.current.lk() = Some(id.clone());
        sl.command.lock().await.id = Some(id.clone());
        let running = {
            let ctx = ctx.clone();
            let agent = agent.clone();
            tokio::spawn(async move {
                run(&ctx, &agent, MAIN, "sleep 0.5; echo metadata-ok", None, Duration::from_secs(5)).await
            })
        };
        for _ in 0..100 {
            if sl.command.try_lock().is_err() { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(sl.command.try_lock().is_err(), "test command must hold the command lock");
        for open in [false, true] {
            let visible = tokio::time::timeout(Duration::from_millis(100), current(&ctx, &agent, MAIN, open)).await;
            assert_eq!(visible.expect("terminal lookup blocked behind a running command"), Some(id.clone()));
        }
        assert!(running.await.unwrap().unwrap().to_string().contains("metadata-ok"));
        let started = Instant::now();
        let partial = run(&ctx, &agent, MAIN, "sleep 31; echo long-job-done", None, Duration::from_secs(900)).await.unwrap();
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_secs(29) && elapsed < Duration::from_secs(31), "foreground response took {elapsed:?}");
        println!("requested_wait=900s actual_response={elapsed:?} running={} finished={}", partial["running"], partial["finished"]);
        assert_eq!(partial["finished"], false);
        assert_eq!(partial["running"], true);
        assert_eq!(partial["terminal"], id);
        assert_eq!(command_status(&ctx, &agent, &id).unwrap()["running"], true);
        let rejected = run(&ctx, &agent, MAIN, "echo duplicate", None, Duration::from_secs(5)).await.unwrap_err();
        assert!(rejected.contains("Do not retry"));
        let from = sl.command.lock().await.pending.unwrap();
        let (out, code) = wait(from).await;
        assert_eq!(code, 0);
        assert!(text(&out).contains("long-job-done"));
        assert!(!text(&out).contains("duplicate"));
        let status = command_status(&ctx, &agent, &id).unwrap();
        assert_eq!(status["finished"], true);
        assert_eq!(status["exit_code"], 0);
        println!("long command completed after partial return: exit_code={} output={}", status["exit_code"], text(&out));
        let next = run(&ctx, &agent, MAIN, "echo follow-up-ok", None, Duration::from_secs(5)).await.unwrap();
        assert_eq!(next["finished"], true);
        assert!(next["output"].as_str().unwrap().contains("follow-up-ok"));

        // A disconnected caller must not lose ownership of the live command.
        let cancelled = {
            let ctx = ctx.clone();
            let agent = agent.clone();
            tokio::spawn(async move {
                run(&ctx, &agent, MAIN, "sleep 0.5; echo cancelled-caller-done", None, Duration::from_secs(5)).await
            })
        };
        for _ in 0..100 {
            if sl.command.try_lock().is_err() { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(sl.command.try_lock().is_err());
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        let from = sl.command.lock().await.pending.expect("cancelled request forgot its live command");
        assert!(run(&ctx, &agent, MAIN, "echo duplicate-after-cancel", None, Duration::from_secs(5)).await.is_err());
        let (out, code) = wait(from).await;
        assert_eq!(code, 0);
        assert!(text(&out).contains("cancelled-caller-done"));
        assert!(!text(&out).contains("duplicate-after-cancel"));
        crate::terminals::kill(&ctx, &id).unwrap();
        for _ in 0..100 {
            if !crate::terminals::alive(&ctx, &id) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(current(&ctx, &agent, MAIN, false).await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
