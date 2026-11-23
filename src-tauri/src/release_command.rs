//! Observation through a command-line client the person enrolled, such as the product's own
//! CLI for a hosted environment or `kubectl` for a local cluster.
//!
//! The commands come from the pinned verifier, which the person reads and confirms before
//! pinning. Philium adds three limits of its own: only client programs it knows how to keep
//! read-only, only their read verbs, and no remote command that looks like a change. These are
//! guards, not proof: the confirmation is the control. Nothing here is reachable by an agent.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};

const MAX_ARGS: usize = 48;
const MAX_ARG: usize = 4096;
const MAX_OUTPUT: usize = 262_144;

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Words that change something, as whole words, in a remote command body.
fn looks_like_a_change(body: &str) -> Option<&'static str> {
    const WORDS: [&str; 34] = [
        "insert", "update", "delete", "drop", "alter", "create", "truncate", "grant", "revoke", "merge", "call", "copy", "vacuum",
        "reindex", "replace", "upsert", "rm", "mv", "cp", "chmod", "chown", "kill", "sudo", "tee", "dd", "apply", "patch", "edit",
        "scale", "rollout", "annotate", "label", "set", "restart",
    ];
    if body.contains('>') || body.contains('`') || body.contains("$(") {
        return Some("a redirect or command substitution");
    }
    let lower = body.to_ascii_lowercase();
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find_map(|w| WORDS.iter().find(|x| **x == w).copied())
}

/// A client the person declared read-only in `<data dir>/read-only-clients.json`:
///
/// ```json
/// {"clients":{"mycli":{"reads":["whoami","version","context list"],
///                      "exec":{"verb":"exec","value_options":["-x","--timeout"]}}}}
/// ```
///
/// `reads` are exact argument lists that only read. `exec` names a verb that runs one remote
/// command on one endpoint: `VERB [OPTION VALUE]... ENDPOINT (-c BODY | -- WORDS...)`, with the
/// body screened for words that change something. `kubectl` is built in and needs no entry.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct Client {
    #[serde(default)]
    reads: Vec<String>,
    #[serde(default)]
    exec: Option<Exec>,
}
#[derive(Debug, Default, Clone, serde::Deserialize)]
struct Exec {
    verb: String,
    #[serde(default)]
    value_options: Vec<String>,
}
pub type Clients = std::collections::BTreeMap<String, Client>;

pub const CLIENTS_FILE: &str = "read-only-clients.json";

/// The person's configured clients. Missing or unreadable config means only built-ins.
pub fn clients() -> Clients {
    let dir = crate::pilot::data_dir().or_else(|| std::env::var_os("PHILIUM_DATA_DIR").map(std::path::PathBuf::from));
    let Some(raw) = dir.and_then(|d| std::fs::read_to_string(d.join(CLIENTS_FILE)).ok()) else { return Clients::new() };
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        clients: Clients,
    }
    serde_json::from_str::<File>(&raw).map(|f| f.clients).unwrap_or_default().into_iter().filter(|(k, _)| name_ok(k) && k != "kubectl").collect()
}

fn configured(name: &str, client: &Client, args: &[String]) -> Result<(), String> {
    if client.reads.iter().any(|r| r.split_whitespace().eq(args.iter().map(String::as_str))) {
        return Ok(());
    }
    let Some(exec) = client.exec.as_ref().filter(|e| !e.verb.is_empty() && args[0] == e.verb) else {
        return Err(format!("{name} {} is not a read Philium runs", args[0]));
    };
    let verb = &exec.verb;
    let mut i = 1;
    let mut body: Option<String> = None;
    let mut endpoint = false;
    while i < args.len() {
        match args[i].as_str() {
            a if exec.value_options.iter().any(|o| o == a) => i += 1,
            "-c" | "--command" => {
                body = args.get(i + 1).cloned();
                i += 1;
            }
            "--" => {
                body = Some(args[i + 1..].join(" "));
                break;
            }
            a if a.starts_with('-') => return Err(format!("{verb} option {a} is not allowed for observation")),
            _ if !endpoint => endpoint = true,
            _ => return Err(format!("{verb} takes one endpoint; put the command after -c or --")),
        }
        i += 1;
    }
    let body = body.filter(|b| !b.trim().is_empty()).ok_or_else(|| format!("{verb} needs a command after -c or --"))?;
    if !endpoint {
        return Err(format!("{verb} needs an endpoint"));
    }
    match looks_like_a_change(&body) {
        Some(word) => Err(format!("the remote command contains {word}, which is not a read")),
        None => Ok(()),
    }
}

/// The read verbs Philium accepts, per client program. Returns the reason for a refusal.
fn read_only(program: &str, args: &[String], clients: &Clients) -> Result<(), String> {
    if args.is_empty() || args.len() > MAX_ARGS || args.iter().any(|a| a.len() > MAX_ARG || a.contains('\0')) {
        return Err("A command needs 1 to 48 plain arguments".into());
    }
    match program {
        "kubectl" => {
            // Only `get` against a local kind cluster named on the command line.
            let mut i = 0;
            let mut context = None;
            let mut verb = None;
            while i < args.len() {
                match args[i].as_str() {
                    "--context" => {
                        context = args.get(i + 1).cloned();
                        i += 1;
                    }
                    "-n" | "--namespace" | "-l" | "--selector" | "-o" | "--output" | "--raw" | "--request-timeout" if verb.is_some() || args[i] == "-n" || args[i] == "--namespace" => i += 1,
                    a if a.starts_with("--context=") => context = Some(a["--context=".len()..].to_owned()),
                    a if a.starts_with('-') && verb.is_none() => return Err(format!("kubectl option {a} is not allowed before the verb")),
                    a if verb.is_none() => verb = Some(a.to_owned()),
                    _ => {}
                }
                i += 1;
            }
            if verb.as_deref() != Some("get") {
                return Err("only kubectl get is a read Philium runs".into());
            }
            if !context.as_deref().is_some_and(|c| c.starts_with("kind-") && name_ok(&c.replace('_', "-"))) {
                return Err("kubectl needs --context naming a local kind cluster (kind-...)".into());
            }
            if args.iter().any(|a| matches!(a.as_str(), "--kubeconfig" | "--server" | "-s" | "--token" | "--as" | "-w" | "--watch" | "-f" | "--filename") || a.starts_with("--kubeconfig=") || a.starts_with("--server=")) {
                return Err("that kubectl option is not allowed for observation".into());
            }
            Ok(())
        }
        other => match clients.get(other) {
            Some(c) => configured(other, c, args),
            None => Err(format!("{other} is not a client Philium knows how to keep read-only (kubectl, or one declared in {CLIENTS_FILE})")),
        },
    }
}

fn args_of(v: &Value) -> Option<Vec<String>> {
    v.as_array()?.iter().map(|a| a.as_str().map(str::to_owned)).collect()
}

/// Validate the command targets of a verifier contract. Called when pinning and again when
/// running. Each target is one environment: its own identity command and its own checks, so an
/// observation of one environment is never credited with another environment's identity.
pub fn validate(spec: &Value, seen: &mut std::collections::BTreeSet<String>) -> Result<(), String> {
    validate_with(spec, seen, &clients())
}

fn validate_with(spec: &Value, seen: &mut std::collections::BTreeSet<String>, clients: &Clients) -> Result<(), String> {
    let Some(targets) = spec.get("targets") else { return Ok(()) };
    let targets = targets.as_array().filter(|t| (1..=3).contains(&t.len())).ok_or("Choose 1 to 3 targets")?;
    let mut names = std::collections::BTreeSet::new();
    for t in targets {
        let name = t["name"].as_str().filter(|n| name_ok(n) && names.insert(n.to_string())).ok_or("Each target needs its own short name (a-z, 0-9, -)")?;
        let cases = t["cases"].as_array().filter(|c| (1..=8).contains(&c.len())).ok_or_else(|| format!("{name}: choose 1 to 8 checks"))?;
        for c in cases {
            let id = c["id"].as_str().unwrap_or("");
            if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) || !seen.insert(id.to_owned()) {
                return Err(format!("{name}: invalid or duplicate check id"));
            }
            let program = c["program"].as_str().filter(|p| name_ok(p)).ok_or_else(|| format!("{id}: name the client program"))?;
            read_only(program, &args_of(&c["args"]).ok_or_else(|| format!("{id}: list the arguments"))?, clients).map_err(|e| format!("{id}: {e}"))?;
            let text = c["expect_text"].is_string() || c["expect_contains"].as_array().is_some_and(|a| !a.is_empty() && a.len() <= 8 && a.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())));
            let json = c["pointer"].is_string() && c.get("expect").is_some();
            if text == json {
                return Err(format!("{id}: give either expect_text / expect_contains, or pointer with expect"));
            }
        }
        let ident = &t["identity"];
        if !ident.is_null() {
            let program = ident["program"].as_str().filter(|p| name_ok(p)).ok_or_else(|| format!("{name}: the identity command names its client program"))?;
            read_only(program, &args_of(&ident["args"]).ok_or_else(|| format!("{name}: the identity command lists its arguments"))?, clients).map_err(|e| format!("{name} identity: {e}"))?;
            for k in ["revision", "digest"] {
                if !ident[k].is_string() {
                    return Err(format!("{name}: the identity command needs a {k} pointer"));
                }
            }
            if !ident["replicas"].is_null() && !ident["replicas"].is_string() {
                return Err(format!("{name}: replicas must be a pointer to a list"));
            }
        }
    }
    Ok(())
}

/// Programs the contract uses, for the form to ask where each one is.
pub fn programs(spec: &Value) -> Vec<String> {
    let mut out: Vec<String> = spec["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["cases"].as_array().into_iter().flatten().chain(std::iter::once(&t["identity"])))
        .filter_map(|c| c["program"].as_str().map(str::to_owned))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A client program the person pointed at: an absolute path to a real file, not something a
/// checkout under review or the verifier itself could supply.
pub fn enroll_path(name: &str, path: &str, forbidden: &[&Path]) -> Result<String, String> {
    if !name_ok(name) {
        return Err("Program names use a-z, 0-9 and -".into());
    }
    let p = Path::new(path.trim());
    if !p.is_absolute() {
        return Err(format!("{name}: enter the absolute path of the program"));
    }
    let real = std::fs::canonicalize(p).map_err(|_| format!("{name}: no program at {}", p.display()))?;
    if !real.is_file() {
        return Err(format!("{name}: {} is not a program file", p.display()));
    }
    if real.file_name().and_then(|n| n.to_str()) != Some(name) {
        return Err(format!("{name}: the file must be named {name}"));
    }
    for dir in forbidden {
        if let Ok(d) = std::fs::canonicalize(dir) {
            if real.starts_with(&d) {
                return Err(format!("{name}: the program may not live inside the product or verifier checkout"));
            }
        }
    }
    Ok(real.to_string_lossy().into_owned())
}

fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    // Strip colour codes and collapse runs of whitespace: layout is not behavior.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run one read. `None` when it could not be observed (missing program, timeout, failure, too much output).
pub async fn run(path: &str, args: &[String], home: &Path) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(path)
        .args(args)
        .current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        // The person's own login for their own client. Nothing else of their environment.
        .env("HOME", home)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    tokio::time::timeout(Duration::from_secs(40), async {
        let mut buf = Vec::new();
        child.stdout.take()?.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut buf).await.ok()?;
        if buf.len() > MAX_OUTPUT || !child.wait().await.ok()?.success() {
            return None;
        }
        Some(buf)
    })
    .await
    .ok()
    .flatten()
}

/// `pass:<hash>` or `mismatch:<hash>` for one observed output, the same shape other lanes use.
pub fn judge(output: &[u8], case: &Value) -> Option<String> {
    if case["pointer"].is_string() {
        return crate::release_document::observe(std::str::from_utf8(output).ok()?, case);
    }
    let text = clean(output);
    let passed = match case["expect_text"].as_str() {
        Some(want) => text == clean(want.as_bytes()),
        None => case["expect_contains"].as_array()?.iter().all(|n| n.as_str().is_some_and(|n| text.contains(&clean(n.as_bytes())))),
    };
    let hash: String = Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{}:{hash}", if passed { "pass" } else { "mismatch" }))
}

/// The field may carry more than the identity (an image reference, a label). Take the first
/// well-formed token, `prefix` followed by exactly `hex` hex digits, and nothing looser.
fn token(value: &Value, hex: usize, prefix: &str) -> Option<String> {
    let s = value.as_str()?;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if s.is_char_boundary(i) && s[i..].starts_with(prefix) {
            let from = i + prefix.len();
            let run = bytes[from..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
            if run == hex {
                return Some(format!("{prefix}{}", s[from..from + hex].to_ascii_lowercase()));
            }
            i = from + run.max(1);
        } else {
            i += 1;
        }
    }
    None
}

/// Turn the identity command's JSON into the deployment report every observer uses.
pub fn identity_report(output: &[u8], ident: &Value, now_ms: i64) -> Value {
    let Some(root) = std::str::from_utf8(output).ok().and_then(crate::release_document::strict_value) else {
        return json!({});
    };
    let items: Vec<&Value> = match ident["replicas"].as_str() {
        Some(p) => match root.pointer(p).and_then(Value::as_array) {
            Some(a) => a.iter().collect(),
            None => return json!({}),
        },
        None => vec![&root],
    };
    let mut replicas = vec![];
    for item in items {
        let revision = ident["revision"].as_str().and_then(|p| item.pointer(p)).and_then(|v| token(v, 40, ""));
        let digest = ident["digest"].as_str().and_then(|p| item.pointer(p)).and_then(|v| token(v, 64, "sha256:"));
        let ready = match ident["ready"].as_str() {
            Some(p) => item.pointer(p).and_then(Value::as_bool),
            None => Some(true),
        };
        replicas.push(json!({"revision":revision,"digest":digest,"ready":ready}));
    }
    json!({"observed_ms":now_ms,"complete":true,"replicas":replicas})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> Clients {
        serde_json::from_value(json!({"mycli":{"reads":["whoami","version","context list"],"exec":{"verb":"exec","value_options":["-x","--context-name","--timeout"]}}})).unwrap()
    }
    fn a(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn only_reads_of_known_clients_are_accepted() {
        for ok in [
            a(&["whoami"]),
            a(&["context", "list"]),
            a(&["exec", "-x", "staging", "postgres:abc", "-c", "SELECT count(*) FROM employees"]),
            a(&["exec", "ssh-1", "--", "uptime"]),
        ] {
            assert_eq!(read_only("mycli", &ok, &cfg()), Ok(()), "{ok:?}");
        }
        for bad in [
            a(&["admin", "create"]),
            a(&["cluster"]),
            a(&["stop", "x"]),
            a(&["access"]),
            a(&["s3", "ls"]),
            a(&["context", "use", "prod"]),
            a(&["context", "rm", "x"]),
            a(&["login"]),
            a(&["exec", "pg", "-c", "DELETE FROM employees"]),
            a(&["exec", "pg", "-c", "select 1; drop table x"]),
            a(&["exec", "pg", "-f", "./x.sql"]),
            a(&["exec", "pg", "--stdin-cmd"]),
            a(&["exec", "pg", "--tty", "-c", "select 1"]),
            a(&["exec", "pg", "--env", "A=B", "-c", "select 1"]),
            a(&["exec", "ssh-1", "--", "cat", "x", ">", "y"]),
            a(&["exec", "ssh-1", "--", "echo", "$(id)"]),
            a(&["exec", "-c", "select 1"]),
            a(&["exec", "pg"]),
        ] {
            assert!(read_only("mycli", &bad, &cfg()).is_err(), "{bad:?}");
        }
        assert_eq!(read_only("kubectl", &a(&["--context", "kind-review", "-n", "app", "get", "pods", "-l", "app=web", "-o", "json"]), &cfg()), Ok(()));
        assert_eq!(read_only("kubectl", &a(&["--context=kind-review", "get", "--raw", "/api/v1/namespaces/app/services/app:8080/proxy/api/health"]), &cfg()), Ok(()));
        for bad in [
            a(&["get", "pods"]),
            a(&["--context", "staging", "get", "pods"]),
            a(&["--context", "kind-review", "delete", "pod", "x"]),
            a(&["--context", "kind-review", "apply", "-f", "x.yaml"]),
            a(&["--context", "kind-review", "exec", "pod", "--", "sh"]),
            a(&["--context", "kind-review", "--kubeconfig", "/tmp/k", "get", "pods"]),
            a(&["--context", "kind-review", "get", "pods", "-w"]),
            a(&["--server", "https://x", "--context", "kind-review", "get", "pods"]),
        ] {
            assert!(read_only("kubectl", &bad, &cfg()).is_err(), "{bad:?}");
        }
        assert!(read_only("mycli", &a(&["whoami"]), &Clients::new()).is_err(), "unconfigured clients are refused");
        assert!(read_only("psql", &a(&["-c", "select 1"]), &cfg()).is_err());
    }
    #[test]
    fn contract_shape_and_text_judgement() {
        let mut seen = Default::default();
        let good = json!({"targets":[
            {"name":"env01","cases":[
                {"id":"who","program":"mycli","args":["whoami"],"expect_contains":["staging"]},
                {"id":"count","program":"mycli","args":["exec","pg","-c","select count(*) from t"],"expect_text":"count ------- 5 (1 row)"}]},
            {"name":"local","identity":{"program":"kubectl","args":["--context","kind-r","get","pods","-o","json"],"replicas":"/items","revision":"/metadata/annotations/rev","digest":"/status/containerStatuses/0/imageID","ready":"/status/containerStatuses/0/ready"},
             "cases":[{"id":"health","program":"kubectl","args":["--context","kind-r","get","--raw","/x"],"pointer":"/status","expect":"ok"}]}]});
        validate_with(&good, &mut seen, &cfg()).unwrap();
        assert_eq!(programs(&good), ["kubectl", "mycli"]);
        assert!(validate_with(&good, &mut seen, &cfg()).is_err(), "duplicate ids across lanes are refused");
        let mut bad = good.clone();
        bad["targets"][0]["cases"][0]["args"] = json!(["stop", "x"]);
        assert!(validate_with(&bad, &mut Default::default(), &cfg()).unwrap_err().starts_with("who:"));
        let mut both = good.clone();
        both["targets"][0]["cases"][0]["pointer"] = json!("/x");
        both["targets"][0]["cases"][0]["expect"] = json!(1);
        assert!(validate_with(&both, &mut Default::default(), &cfg()).is_err());
        let mut twice = good.clone();
        twice["targets"][1]["name"] = json!("env01");
        assert!(validate_with(&twice, &mut Default::default(), &cfg()).is_err());
        let mut changing = good.clone();
        changing["targets"][1]["identity"]["args"] = json!(["--context", "kind-r", "delete", "pods", "--all"]);
        assert!(validate_with(&changing, &mut Default::default(), &cfg()).unwrap_err().starts_with("local identity:"));
        let cases: Vec<Value> = good["targets"].as_array().unwrap().iter().flat_map(|t| t["cases"].as_array().unwrap().clone()).collect();
        assert!(judge(b"Logged in as x\n\x1b[0;33mContext: staging\x1b[0m\n", &cases[0]).unwrap().starts_with("pass:"));
        assert!(judge(b"Context: production\n", &cases[0]).unwrap().starts_with("mismatch:"));
        assert!(judge(b" count \n-------\n     5\n(1 row)\n", &cases[1]).unwrap().starts_with("pass:"));
        assert!(judge(b" count \n-------\n     6\n(1 row)\n", &cases[1]).unwrap().starts_with("mismatch:"));
        assert!(judge(br#"{"status":"ok"}"#, &cases[2]).unwrap().starts_with("pass:"));
        assert!(judge(b"not json", &cases[2]).is_none());
    }
    #[test]
    fn identity_is_taken_per_replica_and_only_when_well_formed() {
        let ident = json!({"replicas":"/items","revision":"/metadata/annotations/rev","digest":"/status/containerStatuses/0/imageID","ready":"/status/containerStatuses/0/ready"});
        let (rev, dig) = ("a".repeat(40), "b".repeat(64));
        let pod = |r: &str, d: &str, ready: bool| json!({"metadata":{"annotations":{"rev":r}},"status":{"containerStatuses":[{"imageID":format!("docker.io/library/app@sha256:{d}"),"ready":ready}]}});
        let out = json!({"items":[pod(&rev, &dig, true), pod(&rev, &dig, true)]}).to_string();
        let report = identity_report(out.as_bytes(), &ident, 1000);
        let state = |v: &Value| crate::release_stage::assess(&rev, &format!("sha256:{dig}"), v, 1000)["state"].clone();
        assert_eq!(state(&report), "match");
        let other = json!({"items":[pod(&rev, &dig, true), pod(&"c".repeat(40), &dig, true)]}).to_string();
        assert_eq!(state(&identity_report(other.as_bytes(), &ident, 1000)), "mixed");
        let unready = json!({"items":[pod(&rev, &dig, false)]}).to_string();
        assert_eq!(state(&identity_report(unready.as_bytes(), &ident, 1000)), "not_ready");
        // A tag instead of a digest, a short revision, or no JSON: unknown, never a match.
        let tagged = json!({"items":[{"metadata":{"annotations":{"rev":"abc123"}},"status":{"containerStatuses":[{"imageID":"app:1.2.3","ready":true}]}}]}).to_string();
        assert_eq!(state(&identity_report(tagged.as_bytes(), &ident, 1000)), "unknown");
        assert_eq!(state(&identity_report(b"Error from server", &ident, 1000)), "unknown");
        assert_eq!(state(&identity_report(br#"{"items":[]}"#, &ident, 1000)), "unknown");
    }
    #[test]
    fn programs_must_be_real_files_outside_the_checkouts() {
        let dir = std::env::temp_dir().join(format!("philium-cmd-{}", crate::hal::random_token()));
        std::fs::create_dir_all(dir.join("checkout")).unwrap();
        std::fs::write(dir.join("mycli"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("checkout/mycli"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("other"), "x").unwrap();
        let checkout = dir.join("checkout");
        let no = [checkout.as_path()];
        assert!(enroll_path("mycli", dir.join("mycli").to_str().unwrap(), &no).is_ok());
        assert!(enroll_path("mycli", "mycli", &no).is_err());
        assert!(enroll_path("mycli", dir.join("missing").to_str().unwrap(), &no).is_err());
        assert!(enroll_path("mycli", dir.join("other").to_str().unwrap(), &no).is_err());
        assert!(enroll_path("mycli", checkout.join("mycli").to_str().unwrap(), &no).is_err());
        assert!(enroll_path("mycli", dir.to_str().unwrap(), &no).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
