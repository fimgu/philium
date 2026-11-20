//! Runs declared checks. Every run gets its own clone at an exact SHA, its own
//! HOME and temp folder, a scrubbed environment, fresh services, and the OS
//! sandbox denying the real home folder (invariant 4). A check that cannot run
//! as declared is `not_run`, never passed (invariant 5).
use super::config::{self, Check, Pipeline, Service};
use super::records::Status;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

const OUTPUT_CAP: usize = 64 * 1024;
const READY_SECS: u64 = 180;
/// Toolchains under a home folder that hold programs, not secrets.
const TOOL_DIRS: [&str; 11] = [".rustup", ".cargo/bin", "go/bin", "go/pkg/mod", ".nvm", ".bun/bin", ".volta", ".local/bin", ".pyenv", ".asdf", ".sdkman"];

/// What the machine provides to a run. Production reads the Keychain and the
/// enrolment table; tests pass their own.
pub struct Host {
    /// Where run folders go. Docker must be able to mount it (Colima shares the
    /// home folder), so it may sit under `home`: the sandbox re-allows only the run's own folders.
    pub scratch: PathBuf,
    /// Philium's own caches, kept between runs.
    pub caches: PathBuf,
    /// The real home folder the sandbox hides.
    pub home: PathBuf,
    pub secret: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub enrolled: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// A lend's folder on this machine, when a person has lent it.
    pub lent: Box<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>,
    pub platform: String,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: Status,
    pub reason: Option<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: i64,
    pub output: String,
}

fn not_run(why: impl Into<String>) -> Outcome {
    Outcome { status: Status::NotRun, reason: Some(why.into()), exit_code: None, duration_ms: 0, output: String::new() }
}

/// A disposable clone of `repo` at `sha`. The source checkout is only read.
pub fn checkout(repo: &Path, sha: &str, dest: &Path) -> Result<(), String> {
    if !super::records::is_sha(sha) {
        return Err(format!("a run checks out a full SHA, not {sha}"));
    }
    let git = |args: &[&str], dir: Option<&Path>| -> Result<(), String> {
        let mut c = std::process::Command::new("git");
        if let Some(d) = dir {
            c.arg("-C").arg(d);
        }
        let o = c.args(["-c", "core.hooksPath=/dev/null", "-c", "advice.detachedHead=false"]).args(args).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| format!("git: {e}"))?;
        if o.status.success() {
            Ok(())
        } else {
            Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim()))
        }
    };
    // A local clone hard-links objects; nothing in the run points back into the source.
    git(&["clone", "--quiet", "--local", "--no-checkout", &repo.to_string_lossy(), &dest.to_string_lossy()], None)?;
    git(&["checkout", "--quiet", "--detach", sha], Some(dest))
}

/// Lay files from another commit over a checkout, such as a fix's regression
/// tests over the code that shipped the bug.
pub fn overlay(work: &Path, from: &str, paths: &[String]) -> Result<(), String> {
    if !super::records::is_sha(from) {
        return Err(format!("an overlay comes from a full SHA, not {from}"));
    }
    let o = std::process::Command::new("git").arg("-C").arg(work).args(["-c", "core.hooksPath=/dev/null", "checkout", from, "--"]).args(paths).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err(format!("overlay from {}: {}", &from[..12], String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(())
}

/// Services brought up for one side of a run.
pub struct Services {
    pub vars: HashMap<String, String>,
    pub failed: HashMap<String, String>,
    teardown: Vec<Teardown>,
}

enum Teardown {
    Compose { project: String, files: Vec<PathBuf>, dir: PathBuf },
    Process(u32),
    Kind { name: String },
}

impl Services {
    pub fn none() -> Services {
        Services { vars: HashMap::new(), failed: HashMap::new(), teardown: vec![] }
    }

    pub async fn down(mut self) {
        for t in self.teardown.drain(..).rev() {
            match t {
                Teardown::Compose { project, files, dir } => {
                    let mut c = tokio::process::Command::new("docker");
                    c.arg("compose").arg("-p").arg(&project);
                    for f in &files {
                        c.arg("-f").arg(f);
                    }
                    let _ = c.args(["down", "-v", "--remove-orphans", "--timeout", "5"]).current_dir(&dir).output().await;
                }
                Teardown::Process(pid) => kill_group(pid).await,
                Teardown::Kind { name } => {
                    let _ = tokio::process::Command::new("kind").args(["delete", "cluster", "--name", &name]).output().await;
                }
            }
        }
    }
}

async fn kill_group(pid: u32) {
    // SAFETY: plain signal sends to a process group this run created.
    unsafe {
        libc::killpg(pid as i32, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())
}

/// Base variables every template may use.
pub fn base_vars(label: &str, workdir: &Path, rundir: &Path) -> HashMap<String, String> {
    [("run", label.to_string()), ("workdir", workdir.display().to_string()), ("tmp", rundir.join("tmp").display().to_string())]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Bring up what these checks need, each service once.
pub async fn up(p: &Pipeline, needed: &BTreeSet<String>, label: &str, workdir: &Path, rundir: &Path, host: &Host, extra: &HashMap<String, String>) -> Services {
    let mut s = Services::none();
    s.vars = base_vars(label, workdir, rundir);
    s.vars.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    for l in &p.lends {
        if let Some(path) = (host.lent)(l) {
            s.vars.insert(format!("lend.{l}"), path.display().to_string());
        }
    }
    for name in needed {
        let Some(svc) = p.services.get(name) else {
            s.failed.insert(name.clone(), "not declared".into());
            continue;
        };
        let r = match svc {
            Service::Compose { compose, service, port, ready } => up_compose(&mut s, name, compose, service, *port, ready.as_deref(), label, workdir, rundir, host).await,
            Service::Run { run, ready } => up_process(&mut s, name, run, ready, workdir, rundir, host).await,
            Service::Kind { kind } => up_kind(&mut s, name, &kind.manifests, kind.ready.as_deref(), label, workdir, rundir, host).await,
        };
        if let Err(e) = r {
            s.failed.insert(name.clone(), e);
        }
    }
    s
}

fn project(label: &str, name: &str) -> String {
    format!("{label}-{name}").to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect()
}

#[allow(clippy::too_many_arguments)]
async fn up_compose(s: &mut Services, name: &str, file: &str, service: &str, port: Option<u16>, ready: Option<&str>, label: &str, workdir: &Path, rundir: &Path, host: &Host) -> Result<(), String> {
    let path = workdir.join(file);
    if !path.is_file() {
        return Err(format!("{file} is not in this commit"));
    }
    let proj = project(label, name);
    let mut files = vec![path];
    if let Some(port) = port {
        // Publish the declared port on a loopback port only this run uses.
        let over = rundir.join(format!("compose-{name}.yaml"));
        std::fs::write(&over, format!("services:\n  {service}:\n    ports: !override\n      - \"127.0.0.1::{port}\"\n")).map_err(|e| e.to_string())?;
        files.push(over);
    }
    let compose = |args: &[&str]| {
        let mut c = tokio::process::Command::new("docker");
        c.arg("compose").arg("-p").arg(&proj);
        for f in &files {
            c.arg("-f").arg(f);
        }
        c.args(args).current_dir(workdir).kill_on_drop(true);
        c
    };
    s.teardown.push(Teardown::Compose { project: proj.clone(), files: files.clone(), dir: workdir.to_path_buf() });
    let o = tokio::time::timeout(Duration::from_secs(600), compose(&["up", "-d", "--wait", service]).output()).await.map_err(|_| "compose did not come up in 10 minutes".to_string())?.map_err(|e| format!("docker is not available: {e}"))?;
    if !o.status.success() {
        return Err(format!("compose up failed: {}", tail(&String::from_utf8_lossy(&o.stderr), 600)));
    }
    if let Some(port) = port {
        let o = compose(&["port", service, &port.to_string()]).output().await.map_err(|e| e.to_string())?;
        let mapped = String::from_utf8_lossy(&o.stdout).trim().rsplit(':').next().unwrap_or("").to_string();
        if mapped.parse::<u16>().is_err() {
            return Err(format!("compose published no port for {service}:{port}"));
        }
        s.vars.insert(format!("port.{name}"), mapped);
    }
    if let Some(r) = ready {
        wait_ready(r, s, workdir, rundir, host).await?;
    }
    Ok(())
}

async fn up_process(s: &mut Services, name: &str, run: &str, ready: &str, workdir: &Path, rundir: &Path, host: &Host) -> Result<(), String> {
    s.vars.insert(format!("port.{name}"), free_port()?.to_string());
    let cmd = config::expand(run, &s.vars)?;
    let mut c = command(&["/bin/sh", "-c", &cmd], workdir, rundir, host, &HashMap::new())?;
    let log = std::fs::File::create(rundir.join(format!("service-{name}.log"))).map_err(|e| e.to_string())?;
    c.stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    let child = c.spawn().map_err(|e| format!("could not start: {e}"))?;
    let pid = child.id().ok_or("the service exited at once")?;
    s.teardown.push(Teardown::Process(pid));
    std::mem::forget(child);
    wait_ready(ready, s, workdir, rundir, host).await
}

#[allow(clippy::too_many_arguments)]
async fn up_kind(s: &mut Services, name: &str, manifests: &str, ready: Option<&str>, label: &str, workdir: &Path, rundir: &Path, host: &Host) -> Result<(), String> {
    let dir = workdir.join(manifests);
    if !dir.exists() {
        return Err(format!("{manifests} is not in this commit"));
    }
    let cluster = project(label, name);
    let kubeconfig = rundir.join(format!("kubeconfig-{name}"));
    let o = tokio::process::Command::new("kind").args(["create", "cluster", "--name", &cluster, "--kubeconfig"]).arg(&kubeconfig).output().await.map_err(|e| format!("kind is not available: {e}"))?;
    if !o.status.success() {
        return Err(format!("kind create failed: {}", tail(&String::from_utf8_lossy(&o.stderr), 600)));
    }
    // Only a cluster this run created is ever deleted.
    s.teardown.push(Teardown::Kind { name: cluster.clone() });
    let ctx = tokio::process::Command::new("kubectl").arg("--kubeconfig").arg(&kubeconfig).args(["config", "current-context"]).output().await.map_err(|e| format!("kubectl is not available: {e}"))?;
    s.vars.insert(format!("kubeconfig.{name}"), kubeconfig.display().to_string());
    s.vars.insert(format!("context.{name}"), String::from_utf8_lossy(&ctx.stdout).trim().to_string());
    let o = tokio::process::Command::new("kubectl").arg("--kubeconfig").arg(&kubeconfig).arg("apply").arg("-f").arg(&dir).arg("--recursive").output().await.map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("applying {manifests} failed: {}", tail(&String::from_utf8_lossy(&o.stderr), 600)));
    }
    if let Some(r) = ready {
        wait_ready(r, s, workdir, rundir, host).await?;
    }
    Ok(())
}

async fn wait_ready(probe: &str, s: &Services, workdir: &Path, rundir: &Path, host: &Host) -> Result<(), String> {
    let cmd = config::expand(probe, &s.vars)?;
    let until = Instant::now() + Duration::from_secs(READY_SECS);
    while Instant::now() < until {
        let ok = command(&["/bin/sh", "-c", &cmd], workdir, rundir, host, &HashMap::new())?.output().await.is_ok_and(|o| o.status.success());
        if ok {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(format!("not ready after {READY_SECS} s: {probe}"))
}

fn sbpl(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
}

/// The sandbox for one check: everything else as usual, but the real home folder
/// is out of reach except toolchains and Philium's own caches.
pub fn profile(home: &Path, allow: &[PathBuf], read_only: &[PathBuf]) -> String {
    let mut s = String::from("(version 1)\n(allow default)\n");
    let homes = [home.to_path_buf(), std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())];
    for h in &homes {
        s.push_str(&format!("(deny file-read* file-write* (subpath \"{}\"))\n", sbpl(h)));
        // Listing the home folder itself is harmless and many tools stat it.
        s.push_str(&format!("(allow file-read-metadata (literal \"{}\"))\n", sbpl(h)));
        for d in TOOL_DIRS {
            s.push_str(&format!("(allow file-read* (subpath \"{}\"))\n", sbpl(&h.join(d))));
        }
    }
    // Shells stat every parent folder on `cd`: allow the metadata of each parent of an
    // allowed folder (no listing, no reading), or `cd` fails with "Not a directory".
    let mut parents = std::collections::BTreeSet::new();
    for a in allow.iter().chain(read_only) {
        for v in [a.clone(), std::fs::canonicalize(a).unwrap_or_else(|_| a.clone())] {
            let mut d = v.parent();
            while let Some(x) = d {
                parents.insert(x.to_path_buf());
                d = x.parent();
            }
        }
    }
    for d in &parents {
        s.push_str(&format!("(allow file-read-metadata (literal \"{}\"))\n", sbpl(d)));
    }
    // Read-only areas first, so a writable run folder is never shadowed by them.
    for a in read_only {
        for v in [a.clone(), std::fs::canonicalize(a).unwrap_or_else(|_| a.clone())] {
            s.push_str(&format!("(allow file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n", sbpl(&v), sbpl(&v)));
        }
    }
    for a in allow {
        for v in [a.clone(), std::fs::canonicalize(a).unwrap_or_else(|_| a.clone())] {
            s.push_str(&format!("(allow file-read* file-write* (subpath \"{}\"))\n", sbpl(&v)));
        }
    }
    s
}

/// Before any check on a side runs: inside the sandbox, the clone is writable,
/// the gate readable and the home folder unreadable. Otherwise a check would
/// fail for the sandbox's reasons and be taken for a real failure.
pub async fn preflight(workdir: &Path, rundir: &Path, gate: Option<&Path>, host: &Host) -> Result<(), String> {
    let mut env = HashMap::new();
    if let Some(g) = gate {
        env.insert("PHILIUM_GATE".to_string(), g.display().to_string());
    }
    let script = "touch .philium-preflight && rm .philium-preflight || { echo 'the clone is not writable'; exit 1; }; \
        if [ -n \"${PHILIUM_GATE:-}\" ]; then ls \"$PHILIUM_GATE\" >/dev/null 2>&1 || { echo 'the gate folder is not readable'; exit 1; }; fi; \
        if ls \"$1/\" >/dev/null 2>&1; then echo 'the home folder is readable'; exit 1; fi";
    let home = host.home.display().to_string();
    let out = command(&["/bin/sh", "-c", script, "preflight", &home], workdir, rundir, host, &env)?.output().await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("sandbox preflight: {}", String::from_utf8_lossy(&out.stdout).trim()))
    }
}

/// A process with nothing inherited: its own HOME and temp folder, a fixed
/// PATH, only the variables given, inside the sandbox.
fn command(argv: &[&str], workdir: &Path, rundir: &Path, host: &Host, env: &HashMap<String, String>) -> Result<tokio::process::Command, String> {
    let home = rundir.join("home");
    let tmp = rundir.join("tmp");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    if !cfg!(target_os = "macos") || !Path::new("/usr/bin/sandbox-exec").exists() {
        return Err("this machine has no sandbox to keep checks away from the home folder".into());
    }
    let read_only: Vec<PathBuf> = env.iter().filter(|(k, _)| *k == "PHILIUM_GATE" || k.starts_with("PHILIUM_LEND_")).map(|(_, v)| PathBuf::from(v)).collect();
    let prof = profile(&host.home, &[rundir.to_path_buf(), workdir.to_path_buf(), host.caches.clone()], &read_only);
    let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
    c.arg("-p").arg(prof).args(argv);
    let tools: Vec<String> = [".cargo/bin", "go/bin", ".local/bin"].iter().map(|d| host.home.join(d).display().to_string()).collect();
    c.env_clear()
        .env("PATH", format!("/opt/homebrew/bin:/usr/local/bin:/usr/local/go/bin:{}:/usr/bin:/bin:/usr/sbin:/sbin", tools.join(":")))
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("LANG", "C.UTF-8")
        .env("CI", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(env)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    Ok(c)
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut i = s.len() - n;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    format!("…{}", &s[i..])
}

/// A check on another platform runs in `docker run --platform`, with only its
/// clone, its own HOME and temp folder, Philium's caches and the declared
/// variables. No home folder, no added privileges.
#[allow(clippy::too_many_arguments)]
fn container(platform: &str, image: &str, name: &str, script: &str, workdir: &Path, rundir: &Path, env: &HashMap<String, String>, caches: &[(String, PathBuf)]) -> Result<tokio::process::Command, String> {
    let home = rundir.join("home");
    let tmp = rundir.join("tmp");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let mut c = tokio::process::Command::new("docker");
    c.args(["run", "--rm", "--name", name, "--platform", platform, "--pull=missing", "--cap-drop=ALL", "--security-opt=no-new-privileges", "--network=bridge"]);
    c.arg("-v").arg(format!("{}:/philium/work", workdir.display())).args(["-w", "/philium/work"]);
    if let Some(g) = env.get("PHILIUM_GATE") {
        c.arg("-v").arg(format!("{g}:/philium/gate:ro"));
    }
    if let Some(l) = env.get("PHILIUM_CHANGED") {
        c.arg("-v").arg(format!("{l}:/philium/changed.txt:ro"));
    }
    // A lent folder appears at the same path, read-only.
    for v in env.iter().filter(|(k, _)| k.starts_with("PHILIUM_LEND_")).map(|(_, v)| v) {
        c.arg("-v").arg(format!("{v}:{v}:ro"));
    }
    c.arg("-v").arg(format!("{}:/philium/home", home.display())).arg("-v").arg(format!("{}:/philium/tmp", tmp.display()));
    c.args(["-e", "HOME=/philium/home", "-e", "TMPDIR=/philium/tmp", "-e", "CI=true", "-e", "LANG=C.UTF-8"]);
    for (var, dir) in caches {
        let _ = std::fs::create_dir_all(dir);
        let inside = format!("/philium/cache/{}", dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        c.arg("-v").arg(format!("{}:{inside}", dir.display())).arg("-e").arg(format!("{var}={inside}"));
    }
    // Values travel in the docker client's environment, never on its command line.
    c.env_clear();
    for k in ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG"] {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    if std::env::var_os("PATH").is_none() {
        c.env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    }
    for (k, v) in env {
        c.arg("-e").arg(k);
        c.env(k, v);
    }
    c.args(["--entrypoint", "/bin/sh", image, "-c", script]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    Ok(c)
}

/// Docker could not give the check its platform or start it at all: not the check's failure.
fn docker_refusal(exit: Option<i32>, text: &str) -> Option<String> {
    let low = text.to_ascii_lowercase();
    let platform = ["exec format error", "no matching manifest", "does not match the specified platform", "requested image's platform"];
    if let Some(m) = platform.iter().find(|m| low.contains(*m)) {
        return Some(format!("Docker cannot provide this platform ({m})"));
    }
    if exit == Some(125) || low.contains("error response from daemon") || low.contains("cannot connect to the docker daemon") {
        let line = text.lines().find(|l| l.to_ascii_lowercase().contains("docker")).unwrap_or("").trim();
        return Some(format!("Docker could not start the check: {}", tail(line, 300)));
    }
    None
}

/// Run one check in a prepared checkout with its services up.
pub async fn run_check(p: &Pipeline, c: &Check, workdir: &Path, rundir: &Path, services: &Services, host: &Host) -> Outcome {
    let platform = p.platform_for(c, &host.platform);
    if !p.platforms.contains(&platform) {
        return not_run(format!("{platform} is not one of the declared platforms ({})", p.platforms.join(", ")));
    }
    let image = if platform != host.platform {
        if c.environment.is_some() {
            return not_run(format!("needs {platform}, and an environment check runs only on this machine ({})", host.platform));
        }
        if !c.needs.is_empty() {
            return not_run(format!("needs {platform}; a check in a container cannot use services yet"));
        }
        match p.image_for(c, &platform) {
            Some(i) => Some(i),
            None => return not_run(format!("needs {platform}, and no image is declared for it")),
        }
    } else {
        None
    };
    for n in &c.needs {
        if let Some(why) = services.failed.get(n) {
            return not_run(format!("service {n}: {why}"));
        }
    }
    let mut env: HashMap<String, String> = HashMap::new();
    let mut hidden: Vec<String> = vec![];
    for l in &c.lends {
        match services.vars.get(&format!("lend.{l}")) {
            Some(path) => {
                env.insert(format!("PHILIUM_LEND_{l}"), path.clone());
            }
            None => return not_run(format!("{l} is not lent on this machine; a person lends it read-only for this repo")),
        }
    }
    for s in &c.secrets {
        match (host.secret)(s) {
            Some(v) if !v.is_empty() => {
                hidden.push(v.clone());
                env.insert(s.clone(), v);
            }
            _ => return not_run(format!("secret {s} is not in the Keychain")),
        }
    }
    let argv: Vec<String> = match &c.environment {
        Some(e) => {
            if !(host.enrolled)(e) {
                return not_run(format!("environment {e} is not enrolled read-only by a person"));
            }
            match p.environments.get(e).map(|env| config::environment_command(&c.run, env)) {
                Some(Ok(words)) => words,
                Some(Err(m)) => return not_run(m),
                None => return not_run(format!("environment {e} is not declared")),
            }
        }
        None => {
            // A fixture that points at a service this run did not start is simply absent;
            // a check that uses it then cannot run.
            for (var, name) in [("sha", "PHILIUM_SHA"), ("base", "PHILIUM_BASE"), ("gate", "PHILIUM_GATE"), ("changed", "PHILIUM_CHANGED")] {
                if let Some(v) = services.vars.get(var) {
                    env.insert(name.into(), v.clone());
                }
            }
            let mut vars = services.vars.clone();
            for (k, t) in &p.fixtures {
                if let Ok(v) = config::expand(t, &services.vars) {
                    vars.insert(format!("fixture.{k}"), v.clone());
                    env.insert(k.clone(), v);
                }
            }
            match config::expand(&c.run, &vars) {
                Ok(cmd) => vec!["/bin/sh".into(), "-c".into(), cmd],
                Err(m) => return not_run(format!("{m} (does the check list the service it needs?)")),
            }
        }
    };
    let timeout = c.timeout_secs().unwrap_or(600);
    let cname = format!("philium-check-{}", &crate::db::new_id()[..12]).to_ascii_lowercase();
    let built = match &image {
        Some(img) => {
            let caches: Vec<(String, PathBuf)> = p.caches.iter().map(|(k, d)| (k.clone(), host.caches.join(format!("{}-{}", d, platform.replace('/', "-"))))).collect();
            container(&platform, img, &cname, argv.last().map(String::as_str).unwrap_or("true"), workdir, rundir, &env, &caches)
        }
        None => {
            for (k, dir) in &p.caches {
                let path = host.caches.join(dir);
                let _ = std::fs::create_dir_all(&path);
                env.insert(k.clone(), path.display().to_string());
            }
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            command(&refs, workdir, rundir, host, &env)
        }
    };
    let mut cmd = match built {
        Ok(cmd) => cmd,
        Err(m) => return not_run(m),
    };
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(ch) => ch,
        Err(e) => return not_run(format!("could not start: {e}")),
    };
    let pid = child.id();
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let collect = async {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let ra = async {
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_end(&mut a).await;
            }
        };
        let rb = async {
            if let Some(e) = err.as_mut() {
                let _ = e.read_to_end(&mut b).await;
            }
        };
        tokio::join!(ra, rb);
        let status = child.wait().await;
        (a, b, status)
    };
    let (status, exit, text) = match tokio::time::timeout(Duration::from_secs(timeout), collect).await {
        Ok((a, b, st)) => {
            let code = st.ok().and_then(|s| s.code());
            let text = format!("{}{}", String::from_utf8_lossy(&a), String::from_utf8_lossy(&b));
            (if code == Some(0) { Status::Pass } else { Status::Fail }, code.map(i64::from), text)
        }
        Err(_) => {
            if image.is_some() {
                let _ = tokio::process::Command::new("docker").args(["kill", &cname]).output().await;
            }
            if let Some(pid) = pid {
                kill_group(pid).await;
            }
            (Status::Timeout, None, format!("timed out after {timeout} s"))
        }
    };
    if image.is_some() && status != Status::Pass {
        if let Some(why) = docker_refusal(exit.map(|e| e as i32), &text) {
            return Outcome { status: Status::NotRun, reason: Some(why), exit_code: exit, duration_ms: started.elapsed().as_millis() as i64, output: tail(&text, OUTPUT_CAP) };
        }
    }
    let mut text = tail(&text, OUTPUT_CAP);
    for h in hidden {
        text = text.replace(&h, "***");
    }
    Outcome { status, reason: (status == Status::Timeout).then(|| format!("timed out after {timeout} s")), exit_code: exit, duration_ms: started.elapsed().as_millis() as i64, output: text }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn tmp(tag: &str) -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/private/tmp"));
        let d = home.join("Library/Caches/philium-tests").join(format!("{tag}-{}", crate::db::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A host whose "real home" is a folder with a secret in it.
    pub fn host(root: &Path) -> Host {
        let home = root.join("realhome");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/id_test"), "PRIVATE-KEY").unwrap();
        Host {
            scratch: root.join("runs"),
            caches: root.join("caches"),
            home,
            secret: Box::new(|n| (n == "TOKEN").then(|| "s3cr3t-value".to_string())),
            enrolled: Box::new(|e| e == "enrolled"),
            lent: Box::new(|_| None),
            platform: config::host_platform(),
        }
    }

    fn pipeline(checks: &str) -> Pipeline {
        Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}]\nsecrets: [TOKEN, MISSING]\nservices:\n  api: {{ run: \"python3 -m http.server ${{port.api}} --bind 127.0.0.1\", ready: \"curl -sf http://127.0.0.1:${{port.api}}/\" }}\n  gone: {{ compose: nope.yaml, service: x, port: 1 }}\nenvironments:\n  enrolled: {{ client: echo, allowed_verbs: [status] }}\n  other: {{ client: echo, allowed_verbs: [status] }}\nfixtures: {{ API_URL: \"http://127.0.0.1:${{port.api}}\" }}\ntiers: {{ fast: [ {checks} ] }}\n",
            config::host_platform()
        ))
        .unwrap()
    }

    async fn run(p: &Pipeline, id: &str, root: &Path) -> Outcome {
        let h = host(root);
        let work = root.join("work");
        let rundir = root.join("run");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&rundir).unwrap();
        let c = p.check(id).unwrap();
        let needed: BTreeSet<String> = c.needs.iter().cloned().collect();
        let s = up(p, &needed, "philium-t", &work, &rundir, &h, &HashMap::new()).await;
        let o = run_check(p, c, &work, &rundir, &s, &h).await;
        s.down().await;
        o
    }

    #[tokio::test]
    async fn invariant_4_checks_see_neither_the_home_folder_nor_the_callers_environment() {
        let root = tmp("iso");
        std::env::set_var("PHILIUM_LEAK_TEST", "leaked");
        let real = root.join("realhome/.ssh/id_test");
        let p = pipeline(&format!(
            "{{ id: peek, run: \"cat {} ; echo HOME=$HOME ; echo LEAK=${{PHILIUM_LEAK_TEST:-none}} ; touch {}/planted\" }}, {{ id: tok, run: \"echo token=$TOKEN\", secrets: [TOKEN] }}",
            real.display(),
            root.join("realhome").display()
        ));
        let o = run(&p, "peek", &root).await;
        assert!(!o.output.contains("PRIVATE-KEY"), "{}", o.output);
        assert!(o.output.contains("LEAK=none"), "{}", o.output);
        assert!(o.output.contains(&format!("HOME={}", root.join("run/home").display())), "{}", o.output);
        assert!(!root.join("realhome/planted").exists(), "nothing written into the home folder");
        // A declared secret arrives, and never shows in the output.
        let o = run(&p, "tok", &root).await;
        assert_eq!(o.status, Status::Pass);
        assert!(o.output.contains("token=***") && !o.output.contains("s3cr3t"), "{}", o.output);
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn invariant_5_what_cannot_run_as_declared_is_not_run() {
        let root = tmp("nr");
        let other = if config::host_platform().starts_with("darwin") { "linux/amd64" } else { "darwin/arm64" };
        let p = pipeline(&format!(
            "{{ id: plat, run: \"true\", platform: {other} }}, {{ id: sec, run: \"true\", secrets: [MISSING] }}, {{ id: svc, run: \"true\", needs: [gone] }}, {{ id: env, run: \"echo status\", environment: other }}, {{ id: envok, run: \"echo status ok\", environment: enrolled }}"
        ));
        for (id, why) in [("plat", "is not one of the declared platforms"), ("sec", "secret MISSING is not in the Keychain"), ("svc", "service gone: nope.yaml is not in this commit"), ("env", "not enrolled")] {
            let o = run(&p, id, &root).await;
            assert_eq!(o.status, Status::NotRun, "{id}");
            assert!(o.reason.as_deref().unwrap_or("").contains(why), "{id}: {:?}", o.reason);
        }
        let o = run(&p, "envok", &root).await;
        assert_eq!((o.status, o.output.trim()), (Status::Pass, "status ok"));
        // A machine outside the declared platforms runs nothing.
        let mut q = p.clone();
        q.platforms = vec![other.into()];
        assert_eq!(run(&q, "envok", &root).await.status, Status::NotRun);
        std::fs::remove_dir_all(&root).ok();
    }

    fn docker_has(image: &str) -> bool {
        std::process::Command::new("docker").args(["image", "inspect", image]).output().is_ok_and(|o| o.status.success())
    }

    #[tokio::test]
    async fn a_check_for_another_platform_runs_in_docker_with_the_same_isolation() {
        if !docker_has("alpine:3.20") {
            eprintln!("skipped: Docker or the alpine:3.20 image is not available here");
            return;
        }
        let root = tmp("ctr");
        std::env::set_var("PHILIUM_LEAK_TEST", "leaked");
        let p = Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}, linux/amd64]\nimages: {{ linux/amd64: 'alpine:3.20' }}\nsecrets: [TOKEN]\ncaches: {{ GOCACHE: go-build }}\ntiers: {{ fast: [ {{ id: arch, run: \"uname -m; echo HOME=$HOME; echo LEAK=${{PHILIUM_LEAK_TEST:-none}}; echo tok=$TOKEN; echo cache=$GOCACHE; ls {} 2>&1 | head -1; touch made-inside\", platform: linux/amd64, secrets: [TOKEN] }}, {{ id: fails, run: \"exit 4\", platform: linux/amd64 }}, {{ id: noimg, run: \"true\", platform: linux/amd64, image: 'philium-no-such-image:0' }}, {{ id: svc, run: \"true\", platform: linux/amd64, needs: [x] }} ] }}\nservices: {{ x: {{ run: \"sleep 60\", ready: \"true\" }} }}\n",
            config::host_platform(),
            root.join("realhome").display()
        ))
        .unwrap();
        let o = run(&p, "arch", &root).await;
        assert_eq!(o.status, Status::Pass, "{} {:?}", o.output, o.reason);
        assert!(o.output.starts_with("x86_64"), "{}", o.output);
        assert!(o.output.contains("HOME=/philium/home") && o.output.contains("LEAK=none"), "{}", o.output);
        assert!(o.output.contains("tok=***") && !o.output.contains("s3cr3t"), "{}", o.output);
        assert!(o.output.contains("cache=/philium/cache/go-build-linux-amd64"), "{}", o.output);
        assert!(o.output.contains("No such file"), "the home folder is not in the container: {}", o.output);
        assert!(root.join("work/made-inside").exists(), "the clone is the container's working folder");
        let o = run(&p, "fails", &root).await;
        assert_eq!((o.status, o.exit_code), (Status::Fail, Some(4)));
        let o = run(&p, "noimg", &root).await;
        assert_eq!(o.status, Status::NotRun, "{} {:?}", o.output, o.reason);
        assert!(o.reason.as_deref().unwrap().contains("Docker"), "{:?}", o.reason);
        assert!(run(&p, "svc", &root).await.reason.unwrap().contains("cannot use services yet"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn docker_refusals_are_not_run_and_check_failures_are_failures() {
        assert!(docker_refusal(Some(1), "exec /bin/sh: exec format error").unwrap().contains("cannot provide this platform"));
        assert!(docker_refusal(Some(125), "docker: Error response from daemon: no matching manifest for linux/s390x").unwrap().contains("cannot provide"));
        assert!(docker_refusal(Some(125), "docker: Error response from daemon: pull access denied").unwrap().contains("could not start"));
        assert_eq!(docker_refusal(Some(127), "sh: make: not found"), None);
        assert_eq!(docker_refusal(Some(1), "--- FAIL: TestThing"), None);
    }

    #[tokio::test]
    async fn the_preflight_refuses_a_sandbox_that_could_read_the_home_folder() {
        let root = tmp("pre");
        let h = host(&root);
        let (work, rundir, gate) = (root.join("work"), root.join("run"), root.join("gate"));
        for d in [&work, &rundir, &gate] {
            std::fs::create_dir_all(d).unwrap();
        }
        assert_eq!(preflight(&work, &rundir, Some(&gate), &h).await, Ok(()));
        // A "gate" that contains the home folder would open it up: refused.
        assert!(preflight(&work, &rundir, Some(&root), &h).await.unwrap_err().contains("home folder is readable"));
        // Run folders inside the hidden home (as on a real machine): `cd` into the clone works,
        // and the home folder's other contents stay out of reach.
        let mut inside = host(&root);
        inside.home = root.clone();
        let deep = root.join("Library/Caches/runs/1/src");
        let deep_run = root.join("Library/Caches/runs/1/run");
        std::fs::create_dir_all(deep.join("pkg/sub")).unwrap();
        std::fs::create_dir_all(&deep_run).unwrap();
        let mut c = command(&["/bin/sh", "-c", "cd pkg/sub && pwd && ls ../../../../../../realhome >/dev/null 2>&1 && echo LEAK || echo private"], &deep, &deep_run, &inside, &HashMap::new()).unwrap();
        let o = c.output().await.unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(out.contains("pkg/sub") && out.contains("private") && !out.contains("LEAK"), "{out} {}", String::from_utf8_lossy(&o.stderr));
        // A gate the check cannot read is refused too, rather than failing every check.
        assert!(preflight(&work, &rundir, Some(&root.join("missing")), &h).await.unwrap_err().contains("gate folder is not readable"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_lent_folder_is_readable_never_writable_and_only_when_lent() {
        let root = tmp("lend");
        let shared = root.join("realhome/go-downloads");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("module.zip"), "zip").unwrap();
        let p = Pipeline::parse(&format!(
            "version: 1\nbranch: main\nplatforms: [{}]\nlends: [DOWNLOADS]\nfixtures: {{ PROXY: \"file://${{lend.DOWNLOADS}}\" }}\ntiers: {{ fast: [ {{ id: read, run: \"cat ${{PROXY#file://}}/module.zip; touch ${{PROXY#file://}}/planted 2>/dev/null && echo WROTE; cat {} 2>/dev/null || echo private\", lends: [DOWNLOADS] }} ] }}\n",
            config::host_platform(),
            root.join("realhome/.ssh/id_test").display()
        ))
        .unwrap();
        // Not lent on this machine: the check does not run at all.
        let o = run(&p, "read", &root).await;
        assert_eq!(o.status, Status::NotRun);
        assert!(o.reason.unwrap().contains("DOWNLOADS is not lent"));
        // Lent: readable, not writable, and the rest of the home folder stays private.
        let mut h = host(&root);
        let lent = shared.clone();
        h.lent = Box::new(move |n| (n == "DOWNLOADS").then(|| lent.clone()));
        let (work, rundir) = (root.join("work"), root.join("run"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&rundir).unwrap();
        let s = up(&p, &BTreeSet::new(), "philium-t", &work, &rundir, &h, &HashMap::new()).await;
        let o = run_check(&p, p.check("read").unwrap(), &work, &rundir, &s, &h).await;
        assert!(o.output.starts_with("zip") && !o.output.contains("WROTE") && o.output.contains("private") && !o.output.contains("PRIVATE-KEY"), "{}", o.output);
        assert!(!shared.join("planted").exists());
        // What may never be lent.
        let db = crate::pipeline::records::tests::db();
        let home = root.join("realhome");
        assert!(crate::pipeline::records::lend(&db, "r", "X", &home, "filip", &home).is_err());
        assert!(crate::pipeline::records::lend(&db, "r", "X", &root, "filip", &home).is_err());
        assert!(crate::pipeline::records::lend(&db, "r", "X", &home.join(".ssh"), "filip", &home).is_err());
        assert!(crate::pipeline::records::lend(&db, "r", "X", &shared, "", &home).is_err());
        crate::pipeline::records::lend(&db, "r", "X", &shared, "filip", &home).unwrap();
        assert_eq!(crate::pipeline::records::lent(&db, "r", "X"), Some(std::fs::canonicalize(&shared).unwrap()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn services_come_up_per_run_and_fixtures_point_at_them() {
        let root = tmp("svc");
        let p = pipeline("{ id: hit, run: \"curl -sf $API_URL/ >/dev/null && echo reached ${port.api}\", needs: [api], timeout: 60 }, { id: slow, run: \"sleep 30\", timeout: 1 }, { id: bad, run: \"exit 3\" }");
        let o = run(&p, "hit", &root).await;
        assert_eq!(o.status, Status::Pass, "{} {:?}", o.output, o.reason);
        assert!(o.output.contains("reached "));
        let o = run(&p, "slow", &root).await;
        assert_eq!(o.status, Status::Timeout);
        assert!(o.duration_ms < 10_000);
        let o = run(&p, "bad", &root).await;
        assert_eq!((o.status, o.exit_code), (Status::Fail, Some(3)));
        std::fs::remove_dir_all(&root).ok();
    }
}
