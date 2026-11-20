//! The repo contract: `.philium/pipeline.yaml`. Philium code knows only these
//! concepts; every repo name, branch, service, port, namespace and command
//! comes from this file.
//!
//! The gate's definition is read from the base, never from the change being
//! judged: a pull request cannot weaken the checks it is held to.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const FILE: &str = ".philium/pipeline.yaml";
const MAX_TIMEOUT_SECS: u64 = 4 * 3600;
const DEFAULT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    pub version: u32,
    pub branch: String,
    #[serde(default)]
    pub land: Land,
    /// Where checks may run, as `<os>/<arch>`. A check that would run anywhere else is `not_run`.
    #[serde(default = "host_only")]
    pub platforms: Vec<String>,
    /// The container image for each platform other than this machine's
    /// (for example linux/amd64: golang:1.26). A check may name its own.
    #[serde(default)]
    pub images: BTreeMap<String, String>,
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    /// Secret names only. Values come from the Keychain and never reach a log.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Environment variables for every check, as templates (`${port.db}`, `${run}`).
    #[serde(default)]
    pub fixtures: BTreeMap<String, String>,
    /// Environment variable -> cache folder name, kept by Philium between runs
    /// (for example GOMODCACHE: go-mod). Never the host's own caches.
    #[serde(default)]
    pub caches: BTreeMap<String, String>,
    pub tiers: Tiers,
    #[serde(default)]
    pub requirements: Requirements,
    #[serde(default)]
    pub environments: BTreeMap<String, Environment>,
}

fn host_only() -> Vec<String> {
    vec![host_platform()]
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Land {
    #[serde(default)]
    pub mode: LandMode,
    #[serde(default)]
    pub method: LandMethod,
    #[serde(default = "one")]
    pub batch_max: u32,
}
fn one() -> u32 {
    1
}
impl Default for Land {
    fn default() -> Self {
        Land { mode: LandMode::Tested, method: LandMethod::Squash, batch_max: 1 }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LandMode {
    #[default]
    Tested,
    Optimistic,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LandMethod {
    #[default]
    Squash,
    Merge,
    Rebase,
}

/// A service a check needs, brought up fresh for each run and torn down after.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged, deny_unknown_fields)]
pub enum Service {
    /// A docker compose service. `port` is its container port; `${port.<name>}`
    /// becomes the loopback port Philium publishes it on.
    Compose { compose: String, service: String, #[serde(default)] port: Option<u16>, #[serde(default)] ready: Option<String> },
    /// A disposable kind cluster with these manifests applied. Checks get
    /// `${kubeconfig.<name>}` and `${context.<name>}`.
    Kind { kind: KindService },
    /// Any command. `${port.<name>}` is a free loopback port Philium picks;
    /// `ready` is polled until it exits 0.
    Run { run: String, ready: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KindService {
    pub manifests: String,
    #[serde(default)]
    pub ready: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tiers {
    #[serde(default)]
    pub fast: Vec<Check>,
    #[serde(default)]
    pub full: Vec<Check>,
    #[serde(default)]
    pub release: Vec<Check>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Fast,
    Full,
    Release,
}

impl Tier {
    pub fn parse(s: &str) -> Result<Tier, String> {
        match s {
            "fast" => Ok(Tier::Fast),
            "full" => Ok(Tier::Full),
            "release" => Ok(Tier::Release),
            _ => Err(format!("unknown tier {s}: fast, full or release")),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Full => "full",
            Tier::Release => "release",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    /// A shell command, run in the checkout. For an environment check, a
    /// plain `<client> <verb> ...` line, run without a shell.
    pub run: String,
    #[serde(default)]
    pub needs: Vec<String>,
    /// Seconds, or `90s`, `10m`, `1h`.
    #[serde(default)]
    pub timeout: Option<Timeout>,
    #[serde(default)]
    pub platform: Option<String>,
    /// The container image when the check runs on another platform than this machine.
    #[serde(default)]
    pub image: Option<String>,
    /// Declared secret names this check receives.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// A read-only environment this check observes. It runs only while a person
    /// has that environment enrolled.
    #[serde(default)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Timeout {
    Secs(u64),
    Text(String),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    #[serde(default = "all_sources")]
    pub sources: Vec<Source>,
    #[serde(default = "p0_p1")]
    pub block_on_unmapped: Vec<String>,
}
impl Default for Requirements {
    fn default() -> Self {
        Requirements { sources: all_sources(), block_on_unmapped: p0_p1() }
    }
}
fn all_sources() -> Vec<Source> {
    vec![Source::PrBody, Source::LinkedIssues, Source::PrComments, Source::ReviewThreads, Source::ChildPrs]
}
fn p0_p1() -> Vec<String> {
    vec!["P0".into(), "P1".into()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    PrBody,
    LinkedIssues,
    PrComments,
    ReviewThreads,
    ChildPrs,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub client: String,
    pub allowed_verbs: Vec<String>,
}

pub fn host_platform() -> String {
    let os = if cfg!(target_os = "macos") { "darwin" } else if cfg!(windows) { "windows" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    format!("{os}/{arch}")
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn env_name_ok(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_uppercase() || f == '_') && c.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn platform_ok(p: &str) -> bool {
    matches!(p.split_once('/'), Some(("darwin" | "linux" | "windows", "amd64" | "arm64")))
}

impl Check {
    pub fn timeout_secs(&self) -> Result<u64, String> {
        let secs = match &self.timeout {
            None => DEFAULT_TIMEOUT_SECS,
            Some(Timeout::Secs(s)) => *s,
            Some(Timeout::Text(t)) => {
                let t = t.trim();
                let (n, mult) = match t.chars().last() {
                    Some('s') => (&t[..t.len() - 1], 1),
                    Some('m') => (&t[..t.len() - 1], 60),
                    Some('h') => (&t[..t.len() - 1], 3600),
                    _ => (t, 1),
                };
                n.trim().parse::<u64>().map_err(|_| format!("check {}: timeout {t} is not a duration such as 90s, 10m or 1h", self.id))? * mult
            }
        };
        if secs == 0 || secs > MAX_TIMEOUT_SECS {
            return Err(format!("check {}: timeout must be between 1 s and 4 h", self.id));
        }
        Ok(secs)
    }
}

impl Pipeline {
    pub fn parse(text: &str) -> Result<Pipeline, String> {
        let p: Pipeline = serde_yaml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
        p.validate()?;
        Ok(p)
    }

    pub fn tier(&self, t: Tier) -> &[Check] {
        match t {
            Tier::Fast => &self.tiers.fast,
            Tier::Full => &self.tiers.full,
            Tier::Release => &self.tiers.release,
        }
    }

    /// Every check, wherever it is declared, by id.
    pub fn check(&self, id: &str) -> Option<&Check> {
        [Tier::Fast, Tier::Full, Tier::Release].iter().flat_map(|t| self.tier(*t)).find(|c| c.id == id)
    }

    /// Where a check runs: its own platform, else this machine when it is declared,
    /// else the first declared platform (in a container).
    pub fn platform_for(&self, c: &Check, host: &str) -> String {
        match &c.platform {
            Some(p) => p.clone(),
            None if self.platforms.iter().any(|p| p == host) => host.to_string(),
            None => self.platforms[0].clone(),
        }
    }

    pub fn image_for(&self, c: &Check, platform: &str) -> Option<String> {
        c.image.clone().or_else(|| self.images.get(platform).cloned())
    }

    /// Every reference resolves, before anything runs.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!("{FILE}: version {} is not supported; use version: 1", self.version));
        }
        if self.branch.trim().is_empty() || self.branch.contains("..") || self.branch.starts_with('-') {
            return Err(format!("{FILE}: branch must name the default branch"));
        }
        if !(1..=32).contains(&self.land.batch_max) {
            return Err(format!("{FILE}: land.batch_max must be between 1 and 32"));
        }
        if self.platforms.is_empty() {
            return Err(format!("{FILE}: platforms lists where checks may run"));
        }
        for p in &self.platforms {
            if !platform_ok(p) {
                return Err(format!("{FILE}: platform {p} is not <os>/<arch> (darwin, linux or windows; amd64 or arm64)"));
            }
        }
        for (name, s) in &self.services {
            if !name_ok(name) {
                return Err(format!("{FILE}: service name {name} may use letters, digits, - _ and ."));
            }
            if let Service::Compose { compose, .. } = s {
                relative(compose, &format!("service {name}: compose"))?;
            }
            if let Service::Kind { kind } = s {
                relative(&kind.manifests, &format!("service {name}: kind.manifests"))?;
            }
        }
        for (pl, img) in &self.images {
            if !platform_ok(pl) || img.trim().is_empty() || img.contains(char::is_whitespace) {
                return Err(format!("{FILE}: images maps a platform such as linux/amd64 to one image name"));
            }
        }
        for s in &self.secrets {
            if !env_name_ok(s) {
                return Err(format!("{FILE}: secret {s} must be an UPPER_CASE name"));
            }
        }
        for (k, v) in &self.fixtures {
            if !env_name_ok(k) {
                return Err(format!("{FILE}: fixture {k} must be an UPPER_CASE environment variable name"));
            }
            self.check_template(v, &format!("fixture {k}"))?;
        }
        for (k, v) in &self.caches {
            if !env_name_ok(k) || !name_ok(v) {
                return Err(format!("{FILE}: cache {k}: {v} must map an UPPER_CASE variable to a plain folder name"));
            }
        }
        for (name, e) in &self.environments {
            if !name_ok(name) || !name_ok(&e.client) || e.allowed_verbs.is_empty() || !e.allowed_verbs.iter().all(|v| name_ok(v)) {
                return Err(format!("{FILE}: environment {name} needs a client program and at least one allowed verb"));
            }
        }
        let mut seen: HashMap<&str, Tier> = HashMap::new();
        for t in [Tier::Fast, Tier::Full, Tier::Release] {
            for c in self.tier(t) {
                if !name_ok(&c.id) {
                    return Err(format!("{FILE}: check id {} may use letters, digits, - _ and .", c.id));
                }
                if let Some(prev) = seen.insert(&c.id, t) {
                    if prev == t {
                        return Err(format!("{FILE}: check {} appears twice in tier {}", c.id, t.as_str()));
                    }
                    if self.tier(prev).iter().find(|x| x.id == c.id) != Some(c) {
                        return Err(format!("{FILE}: check {} differs between tiers {} and {}; one id means one check", c.id, prev.as_str(), t.as_str()));
                    }
                }
                if c.run.trim().is_empty() {
                    return Err(format!("{FILE}: check {} has nothing to run", c.id));
                }
                c.timeout_secs()?;
                for n in &c.needs {
                    if !self.services.contains_key(n) {
                        return Err(format!("{FILE}: check {} needs service {n}, which is not declared", c.id));
                    }
                }
                for s in &c.secrets {
                    if !self.secrets.contains(s) {
                        return Err(format!("{FILE}: check {} uses secret {s}, which is not declared", c.id));
                    }
                }
                if let Some(p) = &c.platform {
                    if !platform_ok(p) {
                        return Err(format!("{FILE}: check {}: platform {p} is not <os>/<arch>", c.id));
                    }
                }
                match &c.environment {
                    Some(e) => {
                        let env = self.environments.get(e).ok_or_else(|| format!("{FILE}: check {} observes environment {e}, which is not declared", c.id))?;
                        environment_command(&c.run, env).map_err(|m| format!("{FILE}: check {}: {m}", c.id))?;
                        if !c.needs.is_empty() {
                            return Err(format!("{FILE}: check {} observes an environment and cannot also need services", c.id));
                        }
                    }
                    None => self.check_template(&c.run, &format!("check {}", c.id))?,
                }
            }
        }
        if seen.is_empty() {
            return Err(format!("{FILE}: declare at least one check under tiers"));
        }
        Ok(())
    }

    fn check_template(&self, t: &str, what: &str) -> Result<(), String> {
        for var in template_vars(t)? {
            let ok = match var.split_once('.') {
                None => matches!(var.as_str(), "run" | "workdir" | "tmp" | "sha" | "base" | "gate"),
                Some(("port", s)) => matches!(self.services.get(s), Some(Service::Run { .. }) | Some(Service::Compose { port: Some(_), .. })),
                Some(("kubeconfig" | "context", s)) => matches!(self.services.get(s), Some(Service::Kind { .. })),
                Some(("fixture", k)) => self.fixtures.contains_key(k),
                _ => false,
            };
            if !ok {
                return Err(format!("{FILE}: {what} uses ${{{var}}}, which nothing declares"));
            }
        }
        Ok(())
    }
}

fn relative(p: &str, what: &str) -> Result<(), String> {
    if p.is_empty() || p.starts_with('/') || p.starts_with('~') || p.split('/').any(|s| s == "..") {
        return Err(format!("{FILE}: {what} must be a path inside the repo"));
    }
    Ok(())
}

/// Philium's own template names. Any other `${...}` belongs to the shell and is left alone.
pub fn is_philium_var(name: &str) -> bool {
    matches!(name, "run" | "workdir" | "tmp" | "sha" | "base" | "gate") || ["port.", "kubeconfig.", "context.", "fixture."].iter().any(|p| name.starts_with(p))
}

/// Philium names used as `${...}` in a template. `$${` is a literal `${`.
pub fn template_vars(t: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut rest = t;
    while let Some(i) = rest.find("${") {
        if rest[..i].ends_with('$') {
            rest = &rest[i + 2..];
            continue;
        }
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            rest = after;
            continue;
        };
        if is_philium_var(&after[..end]) {
            out.push(after[..end].to_string());
        }
        rest = &after[end + 1..];
    }
    Ok(out)
}

/// Fill a template. Every name must be known.
pub fn expand(t: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find("${") {
        if rest[..i].ends_with('$') {
            out.push_str(&rest[..i - 1]);
            out.push_str("${");
            rest = &rest[i + 2..];
            continue;
        }
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            out.push_str("${");
            rest = after;
            continue;
        };
        let name = &after[..end];
        if is_philium_var(name) {
            out.push_str(vars.get(name).ok_or_else(|| format!("${{{name}}} has no value in this run"))?);
        } else {
            out.push_str(&rest[i..i + 2 + end + 1]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// An environment check is `<client> <verb> ...`, run without a shell, and
/// only with a verb the repo allows for that environment.
pub fn environment_command(run: &str, env: &Environment) -> Result<Vec<String>, String> {
    let words = split_words(run)?;
    if words.first().map(String::as_str) != Some(env.client.as_str()) {
        return Err(format!("an environment check runs {} directly", env.client));
    }
    let verb = words.get(1).ok_or("an environment check names a verb")?;
    if !env.allowed_verbs.contains(verb) {
        return Err(format!("{verb} is not an allowed verb for this environment ({})", env.allowed_verbs.join(", ")));
    }
    if words.iter().any(|w| w.contains(['|', ';', '&', '`', '>', '<']) || w.contains("$(")) {
        return Err("an environment check is one plain command, without pipes or redirection".into());
    }
    Ok(words)
}

/// Whitespace-separated words, with single or double quotes grouping.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// A gate definition and where it came from. Only a gate read from the base
/// commit is pinned; a local file is for trying a gate out and never lands.
#[derive(Debug, Clone)]
pub struct Gate {
    pub pipeline: Pipeline,
    pub from: String,
    pub pinned: bool,
    /// Where the gate's own `.philium/` folder comes from: checks reach it as
    /// `${gate}`, so scripts and regression tests belong to the gate, not to the
    /// commit under test.
    pub source: GateSource,
}

#[derive(Debug, Clone)]
pub enum GateSource {
    Commit { repo: std::path::PathBuf, sha: String },
    Folder(std::path::PathBuf),
}

impl Gate {
    /// A read-only copy of the gate's `.philium/` folder under `dest`.
    pub fn materialize(&self, dest: &Path) -> Result<std::path::PathBuf, String> {
        match &self.source {
            GateSource::Folder(f) => Ok(f.clone()),
            GateSource::Commit { repo, sha } => {
                std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
                let archive = std::process::Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", sha, ".philium"]).output().map_err(|e| format!("git: {e}"))?;
                if !archive.status.success() {
                    return Err(format!("could not read .philium from {}", &sha[..sha.len().min(12)]));
                }
                let mut tar = std::process::Command::new("tar").arg("-x").arg("-C").arg(dest).stdin(std::process::Stdio::piped()).spawn().map_err(|e| format!("tar: {e}"))?;
                use std::io::Write;
                tar.stdin.take().ok_or("tar")?.write_all(&archive.stdout).map_err(|e| e.to_string())?;
                if !tar.wait().map_err(|e| e.to_string())?.success() {
                    return Err("could not unpack the gate folder".into());
                }
                Ok(dest.join(".philium"))
            }
        }
    }
}

/// The gate definition for a change: the base commit's own file, else a file
/// a person registered for the repo. Never the head's copy.
pub fn load(repo: &Path, base_sha: &str, local: Option<&Path>) -> Result<Gate, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.hooksPath=/dev/null", "show", &format!("{base_sha}:{FILE}")])
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        return Ok(Gate { pipeline: Pipeline::parse(&text)?, from: format!("{base_sha}:{FILE}"), pinned: true, source: GateSource::Commit { repo: repo.to_path_buf(), sha: base_sha.to_string() } });
    }
    match local {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let folder = p.parent().map(Path::to_path_buf).unwrap_or_default();
            Ok(Gate { pipeline: Pipeline::parse(&text)?, from: p.display().to_string(), pinned: false, source: GateSource::Folder(folder) })
        }
        None => Err(format!("the base {} has no {FILE}, and no local pipeline file is registered for this repo", &base_sha[..base_sha.len().min(12)])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
version: 1
branch: main
land: { mode: tested, method: squash, batch_max: 4 }
platforms: [darwin/arm64, linux/amd64]
services:
  db: { compose: deploy/compose.yaml, service: postgres, port: 5432 }
  api: { run: "./serve --port ${port.api}", ready: "curl -sf http://127.0.0.1:${port.api}/health" }
  cluster: { kind: { manifests: deploy/k8s } }
secrets: [API_TOKEN]
fixtures:
  DATABASE_URL: "postgres://u@127.0.0.1:${port.db}/t_${run}"
caches: { GOMODCACHE: go-mod }
tiers:
  fast:
    - { id: unit, run: "go test ./...", timeout: 10m }
  full:
    - { id: unit, run: "go test ./...", timeout: 10m }
    - { id: migrate, run: "make migrate DB=${fixture.DATABASE_URL}", needs: [db], timeout: 1200 }
    - { id: k8s, run: "kubectl --kubeconfig ${kubeconfig.cluster} get pods", needs: [cluster] }
  release:
    - { id: smoke, run: "prod-cli get health", environment: staging, secrets: [API_TOKEN] }
requirements: { sources: [pr_body, pr_comments], block_on_unmapped: [P0] }
environments:
  staging: { client: prod-cli, allowed_verbs: [get, describe] }
"#;

    #[test]
    fn a_full_contract_parses_and_every_reference_resolves() {
        let p = Pipeline::parse(GOOD).unwrap();
        assert_eq!((p.branch.as_str(), p.land.batch_max, p.land.method), ("main", 4, LandMethod::Squash));
        assert_eq!(p.tier(Tier::Full).len(), 3);
        assert_eq!(p.check("migrate").unwrap().timeout_secs().unwrap(), 1200);
        assert_eq!(p.check("unit").unwrap().timeout_secs().unwrap(), 600);
        assert!(matches!(p.services["cluster"], Service::Kind { .. }));
        assert_eq!(p.requirements.sources, vec![Source::PrBody, Source::PrComments]);
        // Defaults: tested landing, one at a time, the host as the only platform.
        let min = Pipeline::parse("version: 1\nbranch: dev\ntiers: { fast: [ { id: t, run: make test } ] }").unwrap();
        assert_eq!((min.land.mode, min.land.batch_max, min.platforms.clone()), (LandMode::Tested, 1, vec![host_platform()]));
        assert_eq!(min.requirements.block_on_unmapped, vec!["P0", "P1"]);
    }

    #[test]
    fn a_contract_that_does_not_add_up_is_refused_before_anything_runs() {
        let bad = [
            ("version: 2", "version 2"),
            ("branch: main\nbranch: x", "duplicate"),
            ("surprise: 1", "unknown field"),
            ("platforms: [macos]", "not <os>/<arch>"),
            ("services: { db: { compose: /etc/x.yaml, service: p } }", "inside the repo"),
            ("secrets: [lower]", "UPPER_CASE"),
            ("fixtures: { URL: \"${port.nope}\" }", "nothing declares"),
        ];
        let base = "version: 1\nbranch: main\ntiers: { fast: [ { id: t, run: make test } ] }\n";
        for (extra, want) in bad {
            let text = if extra.starts_with("version") { format!("{extra}\nbranch: main\ntiers: {{ fast: [ {{ id: t, run: x }} ] }}") } else { format!("{base}{extra}") };
            let e = Pipeline::parse(&text).unwrap_err();
            assert!(e.contains(want), "{extra}: {e}");
        }
        let checks = [
            ("{ id: a, run: x, needs: [db] }", "not declared"),
            ("{ id: a, run: x, timeout: 5h }", "between 1 s and 4 h"),
            ("{ id: a, run: x, timeout: soon }", "not a duration"),
            ("{ id: a, run: x, secrets: [TOKEN] }", "not declared"),
            ("{ id: a, run: \"echo ${fixture.X}\" }", "nothing declares"),
            ("{ id: a, run: x, environment: prod }", "not declared"),
            ("{ id: a, run: '' }", "nothing to run"),
        ];
        for (c, want) in checks {
            let e = Pipeline::parse(&format!("version: 1\nbranch: main\ntiers: {{ fast: [ {c} ] }}")).unwrap_err();
            assert!(e.contains(want), "{c}: {e}");
        }
        let e = Pipeline::parse("version: 1\nbranch: main\ntiers: { fast: [ { id: a, run: x } ], full: [ { id: a, run: y } ] }").unwrap_err();
        assert!(e.contains("differs between tiers"), "{e}");
        assert!(Pipeline::parse("version: 1\nbranch: main\ntiers: {}").unwrap_err().contains("at least one check"));
    }

    #[test]
    fn environment_checks_are_one_allowed_client_verb_without_a_shell() {
        let env = Environment { client: "prod-cli".into(), allowed_verbs: vec!["get".into()] };
        assert_eq!(environment_command("prod-cli get 'two words' x", &env).unwrap(), vec!["prod-cli", "get", "two words", "x"]);
        for bad in ["prod-cli delete x", "other get x", "prod-cli get x | sh", "prod-cli get $(rm)", "prod-cli"] {
            assert!(environment_command(bad, &env).is_err(), "{bad}");
        }
        let base = "version: 1\nbranch: main\nenvironments: { s: { client: c, allowed_verbs: [get] } }\ntiers: { release: [ ";
        assert!(Pipeline::parse(&format!("{base}{{ id: a, run: c delete x, environment: s }} ] }}")).unwrap_err().contains("not an allowed verb"));
    }

    #[test]
    fn a_check_runs_on_its_platform_and_the_image_comes_from_the_file() {
        let p = Pipeline::parse("version: 1\nbranch: main\nplatforms: [darwin/arm64, linux/amd64]\nimages: { linux/amd64: 'alpine:3.20' }\ntiers: { fast: [ { id: a, run: x }, { id: b, run: x, platform: linux/amd64 }, { id: c, run: x, platform: linux/amd64, image: 'debian:12' } ] }").unwrap();
        let (a, b, c) = (p.check("a").unwrap(), p.check("b").unwrap(), p.check("c").unwrap());
        assert_eq!(p.platform_for(a, "darwin/arm64"), "darwin/arm64");
        assert_eq!(p.platform_for(a, "linux/arm64"), "darwin/arm64", "a machine outside the list runs the first declared platform");
        assert_eq!(p.platform_for(b, "darwin/arm64"), "linux/amd64");
        assert_eq!(p.image_for(b, "linux/amd64").as_deref(), Some("alpine:3.20"));
        assert_eq!(p.image_for(c, "linux/amd64").as_deref(), Some("debian:12"));
        assert!(Pipeline::parse("version: 1\nbranch: main\nimages: { linux: x }\ntiers: { fast: [ { id: a, run: x } ] }").is_err());
    }

    #[test]
    fn templates_fill_only_known_names() {
        let vars: HashMap<String, String> = [("port.db".to_string(), "5501".to_string()), ("run".to_string(), "r1".to_string())].into();
        assert_eq!(expand("pg://h:${port.db}/t_${run} costs $${run}", &vars).unwrap(), "pg://h:5501/t_r1 costs ${run}");
        // The shell's own variables pass through untouched.
        assert_eq!(expand("echo ${HOME} ${X:-none} ${run}", &vars).unwrap(), "echo ${HOME} ${X:-none} r1");
        assert!(expand("${port.other}", &vars).is_err());
        assert_eq!(template_vars("a ${port.x} $${run} ${fixture.Z} ${SHELL_VAR}").unwrap(), vec!["port.x", "fixture.Z"]);
        assert_eq!(split_words(r#"a "b c" '' d"#).unwrap(), vec!["a", "b c", "", "d"]);
    }

    #[test]
    fn the_gate_is_read_from_the_base_never_the_head() {
        let dir = std::env::temp_dir().join(format!("philium-cfg-{}", crate::db::new_id()));
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success(), "{args:?}");
        std::fs::create_dir_all(dir.join(".philium")).unwrap();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join(FILE), "version: 1\nbranch: main\ntiers: { fast: [ { id: strict, run: make test } ] }\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
        // The change rewrites its own gate to something that always passes.
        std::fs::write(dir.join(FILE), "version: 1\nbranch: main\ntiers: { fast: [ { id: strict, run: \"true\" } ] }\n").unwrap();
        git(&["commit", "-qam", "weaken"]);
        let g = load(&dir, &base, None).unwrap();
        assert_eq!(g.pipeline.check("strict").unwrap().run, "make test");
        assert!(g.from.starts_with(&base) && g.pinned);
        // A local file is accepted only where the base has none, and is never pinned.
        let local = dir.join("local.yaml");
        std::fs::write(&local, "version: 1\nbranch: main\ntiers: { fast: [ { id: t, run: x } ] }\n").unwrap();
        let g = load(&dir, &base, Some(&local)).unwrap();
        assert!(g.pinned, "the base's own file wins over a local one");
        let g = load(&dir, "4b825dc642cb6eb9a060e54bf8d69288fbee4904", Some(&local)).unwrap();
        assert!(!g.pinned && g.from.ends_with("local.yaml"));
        // A base without the file falls back to the person's registered file, else refuses.
        let e = load(&dir, "4b825dc642cb6eb9a060e54bf8d69288fbee4904", None).unwrap_err();
        assert!(e.contains("no local pipeline file"), "{e}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
