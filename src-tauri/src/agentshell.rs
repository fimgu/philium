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
type Slots = Mutex<HashMap<String, Arc<tokio::sync::Mutex<Slot>>>>;
fn slot(agent: &str, shell: &str) -> Arc<tokio::sync::Mutex<Slot>> {
    static S: OnceLock<Slots> = OnceLock::new();
    S.get_or_init(Default::default).lk().entry(format!("{agent}\0{shell}")).or_default().clone()
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
    s.id = Some(id.clone());
    s.pending = None;
    Ok(id)
}

/// Open (or return) a named shell. Errors explain why it could not start.
pub async fn open(ctx: &Arc<Ctx>, agent: &str, shell: &str) -> Result<String, String> {
    let sl = slot(agent, shell);
    let mut s = sl.lock().await;
    ensure(ctx, agent, shell, &mut s).await
}

/// The agent's main shell, opening it if the agent runs. None when stopped.
pub async fn current(ctx: &Arc<Ctx>, agent: &str, shell: &str, open: bool) -> Option<String> {
    let sl = slot(agent, shell);
    let mut s = sl.lock().await;
    if let Some(id) = s.id.as_ref().filter(|id| crate::terminals::alive(ctx, id)) {
        return Some(id.clone());
    }
    if !open {
        return None;
    }
    ensure(ctx, agent, shell, &mut s).await.ok()
}

pub async fn run(ctx: &Arc<Ctx>, agent: &str, shell: &str, command: &str, cwd: Option<PathBuf>, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let sl = slot(agent, shell);
    let mut s = tokio::time::timeout(timeout, sl.lock())
        .await
        .map_err(|_| "this shell is busy with another command. Use another shell (shell_open), or background=true for long-running commands.")?;
    let id = ensure(ctx, agent, shell, &mut s).await?;
    // A previous command timed out: wait for it before typing a new one.
    if let Some(from) = s.pending {
        loop {
            if crate::terminals::since(ctx, &id, from).and_then(|b| parse(&b)).is_some() {
                s.pending = None;
                break;
            }
            if Instant::now() >= deadline || !crate::terminals::alive(ctx, &id) {
                return Err(format!(
                    "shell \"{shell}\" is still running the previous command (terminal {id}). Run elsewhere with shell_open, or use shell_read to watch it, shell_write to answer it or \"\\u0003\" to interrupt, or shell_kill to reset the shell."
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let from = crate::terminals::total(ctx, &id).ok_or("the shell is gone")?;
    let mut line = match &cwd {
        Some(d) => format!("cd {} && {}", quote(d), command.trim_end()),
        None => command.trim_end().to_string(),
    };
    line.push('\n');
    crate::terminals::write(ctx, &id, line.as_bytes())?;
    loop {
        let buf = crate::terminals::since(ctx, &id, from).unwrap_or_default();
        if let Some((out, code)) = parse(&buf) {
            return Ok(json!({
                "terminal": id,
                "exit_code": code,
                "finished": true,
                "output": crate::mcp::trim_output(&text(&out), 12000),
            }));
        }
        let alive = crate::terminals::alive(ctx, &id);
        if !alive || Instant::now() >= deadline {
            if alive {
                s.pending = Some(from);
            } else {
                s.id = None;
            }
            let partial = buf.windows(START.len()).position(|w| w == START).map_or(&buf[..], |i| &buf[i + START.len()..]);
            return Ok(json!({
                "terminal": id,
                "exit_code": Value::Null,
                "finished": false,
                "output": crate::mcp::trim_output(&text(partial), 12000),
                "note": if alive { "still running in your shell: use shell_read to follow it, shell_write to answer prompts or send \"\\u0003\" to interrupt" } else { "the shell exited; the next shell_run opens a new one" },
            }));
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        crate::terminals::kill(&ctx, &id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
