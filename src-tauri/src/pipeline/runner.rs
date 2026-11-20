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
    /// Where run folders go. Must not be inside `home`.
    pub scratch: PathBuf,
    /// Philium's own caches, kept between runs.
    pub caches: PathBuf,
    /// The real home folder the sandbox hides.
    pub home: PathBuf,
    pub secret: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub enrolled: Box<dyn Fn(&str) -> bool + Send + Sync>,
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
pub async fn up(p: &Pipeline, needed: &BTreeSet<String>, label: &str, workdir: &Path, rundir: &Path, host: &Host) -> Services {
    let mut s = Services::none();
    s.vars = base_vars(label, workdir, rundir);
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
pub fn profile(home: &Path, allow: &[PathBuf]) -> String {
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
    for a in allow {
        for v in [a.clone(), std::fs::canonicalize(a).unwrap_or_else(|_| a.clone())] {
            s.push_str(&format!("(allow file-read* file-write* (subpath \"{}\"))\n", sbpl(&v)));
        }
    }
    s
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
    let prof = profile(&host.home, &[rundir.to_path_buf(), workdir.to_path_buf(), host.caches.clone()]);
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

/// Run one check in a prepared checkout with its services up.
pub async fn run_check(p: &Pipeline, c: &Check, workdir: &Path, rundir: &Path, services: &Services, host: &Host) -> Outcome {
    if !p.platforms.contains(&host.platform) {
        return not_run(format!("this machine is {}, which is not one of the declared platforms ({})", host.platform, p.platforms.join(", ")));
    }
    if let Some(pl) = c.platform.as_ref().filter(|pl| **pl != host.platform) {
        return not_run(format!("needs {pl}; this machine is {}", host.platform));
    }
    for n in &c.needs {
        if let Some(why) = services.failed.get(n) {
            return not_run(format!("service {n}: {why}"));
        }
    }
    let mut env: HashMap<String, String> = HashMap::new();
    let mut hidden: Vec<String> = vec![];
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
    for (k, dir) in &p.caches {
        let path = host.caches.join(dir);
        let _ = std::fs::create_dir_all(&path);
        env.insert(k.clone(), path.display().to_string());
    }
    let timeout = c.timeout_secs().unwrap_or(600);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut cmd = match command(&refs, workdir, rundir, host, &env) {
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
            if let Some(pid) = pid {
                kill_group(pid).await;
            }
            (Status::Timeout, None, format!("timed out after {timeout} s"))
        }
    };
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
        let d = PathBuf::from("/private/tmp").join(format!("philium-{tag}-{}", crate::db::new_id()));
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
        let s = up(p, &needed, "philium-t", &work, &rundir, &h).await;
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
        for (id, why) in [("plat", "needs"), ("sec", "secret MISSING is not in the Keychain"), ("svc", "service gone: nope.yaml is not in this commit"), ("env", "not enrolled")] {
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
