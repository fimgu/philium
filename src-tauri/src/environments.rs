//! Human-operated, read-only Docker inspection across local container runtimes
//! (OrbStack, Colima, Docker, Lima). Never use ambient Docker/kube contexts (`DOCKER_HOST`).
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn valid_profile(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

pub(crate) fn valid_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Resolve a direct local Unix socket under `base_dir` along `rel_segments`.
/// Canonicalizes `base_dir` (so macOS `/var/run` -> `/private/var/run` works cleanly),
/// while rejecting symlinked intermediate directories and symlinked socket files.
#[cfg(unix)]
fn direct_socket(base_dir: &Path, rel_segments: &[&str]) -> Result<PathBuf, String> {
    use std::os::unix::fs::FileTypeExt;
    if rel_segments.is_empty() {
        return Err("No direct local Docker socket".into());
    }
    let mut cur = base_dir
        .canonicalize()
        .map_err(|_| "No direct local Docker socket".to_string())?;
    let (dirs, sock_name) = rel_segments.split_at(rel_segments.len() - 1);
    for seg in dirs {
        if !valid_profile(seg) {
            return Err("Invalid runtime path segment".into());
        }
        cur = cur.join(seg);
        let meta = std::fs::symlink_metadata(&cur)
            .map_err(|_| "No direct local Docker socket".to_string())?;
        if !meta.file_type().is_dir() {
            return Err("No direct local Docker socket".into());
        }
    }
    cur = cur.join(sock_name[0]);
    let meta = std::fs::symlink_metadata(&cur)
        .map_err(|_| "No direct local Docker socket".to_string())?;
    if !meta.file_type().is_socket() || cur.canonicalize().ok().as_ref() != Some(&cur) {
        return Err("No direct local Docker socket".into());
    }
    Ok(cur)
}

#[cfg(unix)]
pub(crate) fn socket(root: &Path, profile: &str) -> Result<PathBuf, String> {
    if !valid_profile(profile) {
        return Err("Invalid profile".into());
    }
    direct_socket(root, &[profile, "docker.sock"])
        .map_err(|_| "No direct local Docker socket for this profile".into())
}

#[cfg(unix)]
struct DiscoveryRoots {
    orbstack_run: PathBuf,
    colima: PathBuf,
    docker_user_run: PathBuf,
    docker_sys_run: PathBuf,
    lima: PathBuf,
}

#[cfg(unix)]
impl DiscoveryRoots {
    fn from_host() -> Option<Self> {
        let home = crate::pilot::real_home().or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
        Some(Self {
            orbstack_run: home.join(".orbstack").join("run"),
            colima: home.join(".colima"),
            docker_user_run: home.join(".docker").join("run"),
            docker_sys_run: PathBuf::from("/var/run"),
            lima: home.join(".lima"),
        })
    }

    #[cfg(test)]
    fn from_home(home: &Path, docker_sys_run: PathBuf) -> Self {
        Self {
            orbstack_run: home.join(".orbstack").join("run"),
            colima: home.join(".colima"),
            docker_user_run: home.join(".docker").join("run"),
            docker_sys_run,
            lima: home.join(".lima"),
        }
    }
}

#[cfg(unix)]
fn scan_colima_profiles(root: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Cannot read Colima profile directory".into()),
    };
    let mut names = Vec::new();
    for (i, entry) in entries.enumerate() {
        if i >= 256 {
            return Err("Profile directory exceeds inspection limit".into());
        }
        let e = entry.map_err(|_| "Incomplete profile directory scan")?;
        if let Ok(p) = e.file_name().into_string() {
            if !valid_profile(&p)
                || !e
                    .file_type()
                    .map_err(|_| "Incomplete profile directory scan")?
                    .is_dir()
            {
                continue;
            }
            if socket(root, &p).is_ok() {
                names.push(p);
            }
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn scan_lima_instances(root: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Cannot read Lima instance directory".into()),
    };
    let mut names = Vec::new();
    for (i, entry) in entries.enumerate() {
        if i >= 256 {
            return Err("Lima directory exceeds inspection limit".into());
        }
        let e = entry.map_err(|_| "Incomplete Lima directory scan")?;
        if let Ok(inst) = e.file_name().into_string() {
            if !valid_profile(&inst)
                || !e
                    .file_type()
                    .map_err(|_| "Incomplete Lima directory scan")?
                    .is_dir()
            {
                continue;
            }
            if direct_socket(root, &[&inst, "sock", "docker.sock"]).is_ok() {
                names.push(inst);
            }
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn discover_runtimes(roots: &DiscoveryRoots) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    if direct_socket(&roots.orbstack_run, &["docker.sock"]).is_ok() {
        out.push(json!({
            "id": "orbstack",
            "runtime": "orbstack",
            "label": "OrbStack",
            "profile": "default",
        }));
    }
    for profile in scan_colima_profiles(&roots.colima)? {
        out.push(json!({
            "id": format!("colima:{profile}"),
            "runtime": "colima",
            "label": format!("Colima ({profile})"),
            "profile": profile,
        }));
    }
    if direct_socket(&roots.docker_user_run, &["docker.sock"]).is_ok()
        || direct_socket(&roots.docker_sys_run, &["docker.sock"]).is_ok()
    {
        out.push(json!({
            "id": "docker",
            "runtime": "docker",
            "label": "Docker",
            "profile": "default",
        }));
    }
    for instance in scan_lima_instances(&roots.lima)? {
        out.push(json!({
            "id": format!("lima:{instance}"),
            "runtime": "lima",
            "label": format!("Lima ({instance})"),
            "profile": instance,
        }));
    }
    Ok(out)
}

#[cfg(unix)]
fn resolve_socket(roots: &DiscoveryRoots, selector: &str) -> Result<PathBuf, String> {
    if selector == "orbstack" {
        return direct_socket(&roots.orbstack_run, &["docker.sock"])
            .map_err(|_| "No direct local OrbStack Docker socket".into());
    }
    if selector == "docker" {
        return direct_socket(&roots.docker_user_run, &["docker.sock"])
            .or_else(|_| direct_socket(&roots.docker_sys_run, &["docker.sock"]))
            .map_err(|_| "No direct local Docker socket".into());
    }
    if let Some(profile) = selector.strip_prefix("colima:") {
        return socket(&roots.colima, profile);
    }
    if let Some(instance) = selector.strip_prefix("lima:") {
        if !valid_profile(instance) {
            return Err("Invalid Lima instance".into());
        }
        return direct_socket(&roots.lima, &[instance, "sock", "docker.sock"])
            .map_err(|_| "No direct local Lima Docker socket for this instance".into());
    }
    // Legacy bare Colima profile name (e.g. "default").
    socket(&roots.colima, selector)
}

#[cfg(unix)]
async fn get(socket: PathBuf, path: &str, cap: usize) -> Result<(Vec<u8>, bool), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .unix_socket(socket)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| "Could not open local client")?;
    let mut r = client
        .get(format!("http://localhost{path}"))
        .send()
        .await
        .map_err(|_| "Local runtime unavailable or timed out")?;
    if !r.status().is_success() {
        return Err(format!("Runtime returned {}", r.status()));
    }
    let mut out = Vec::new();
    while let Some(chunk) = r
        .chunk()
        .await
        .map_err(|_| "Runtime response interrupted")?
    {
        let room = cap - out.len();
        out.extend_from_slice(&chunk[..room.min(chunk.len())]);
        if chunk.len() > room {
            return Ok((out, true));
        }
    }
    Ok((out, false))
}

fn logs(bytes: &[u8]) -> String {
    // Docker multiplexes non-TTY stdout/stderr with 8-byte headers. TTY output is raw.
    let mut b = bytes;
    let mut out = Vec::new();
    if b.len() >= 8 && matches!(b[0], 1 | 2) && b[1..4] == [0, 0, 0] {
        while b.len() >= 8 && matches!(b[0], 1 | 2) && b[1..4] == [0, 0, 0] {
            let n = u32::from_be_bytes(b[4..8].try_into().unwrap()) as usize;
            b = &b[8..];
            let k = n.min(b.len());
            out.extend_from_slice(&b[..k]);
            b = &b[k..];
        }
    } else {
        out.extend_from_slice(b);
    }
    let mut text: String = String::from_utf8_lossy(&out)
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n'))
        .collect();
    crate::guard::scan_text(&mut text, true, &mut crate::guard::Findings::new());
    text
}

#[cfg(unix)]
async fn inspect_roots(roots: &DiscoveryRoots, method: &str, args: Value) -> Result<Value, String> {
    if method == "runtimes" {
        return Ok(json!(discover_runtimes(roots)?));
    }
    if method == "profiles" {
        let ids: Vec<String> = discover_runtimes(roots)?
            .into_iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect();
        return Ok(json!(ids));
    }
    let selector = args["profile"].as_str().ok_or("Select a runtime")?;
    let sock = resolve_socket(roots, selector)?;
    match method {
        "containers" => {
            let (buf, cut) = get(sock, "/containers/json?all=1", 2 * 1024 * 1024).await?;
            if cut {
                return Err("Inventory exceeds 2 MiB; not showing an incomplete list".into());
            }
            let rows: Vec<Value> =
                serde_json::from_slice(&buf).map_err(|_| "Invalid Docker inventory")?;
            Ok(json!(rows
                .iter()
                .filter(|r| r["Id"].as_str().is_some_and(valid_id))
                .map(|r| json!({
                    "id": r["Id"],
                    "name": r["Names"][0].as_str().unwrap_or("Unnamed").trim_start_matches('/'),
                    "image": r["Image"],
                    "status": r["Status"],
                    "state": r["State"],
                    "cluster": r["Labels"]["io.x-k8s.kind.cluster"],
                    "project": r["Labels"]["com.docker.compose.project"],
                }))
                .collect::<Vec<_>>()))
        }
        "logs" => {
            let id = args["id"]
                .as_str()
                .filter(|s| valid_id(s))
                .ok_or("Select a full container ID")?;
            let (b, cut) = get(
                sock,
                &format!("/containers/{id}/logs?stdout=1&stderr=1&timestamps=1&tail=100"),
                65536,
            )
            .await?;
            Ok(json!({ "text": logs(&b), "truncated": cut }))
        }
        _ => Err("Unknown read-only environment action".into()),
    }
}

pub async fn call(method: &str, args: Value) -> Result<Value, String> {
    #[cfg(unix)]
    {
        inspect_roots(
            &DiscoveryRoots::from_host().ok_or("No local runtime directory")?,
            method,
            args,
        )
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (method, args);
        Err("Local container runtime sockets are not supported on this platform".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(prefix: &str) -> PathBuf {
        // Keep Unix socket paths well under macOS SUN_LEN (104 bytes).
        Path::new("/tmp")
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir().canonicalize().unwrap())
            .join(format!("{prefix}-{}", &crate::db::new_id()[..8]))
    }

    #[tokio::test]
    async fn direct_socket_only_and_bounded_tail() {
        let home = temp_root("env");
        let colima = home.join(".colima");
        std::fs::create_dir_all(colima.join("one")).unwrap();
        let _cleanup = Cleanup(home.clone());
        let roots = DiscoveryRoots::from_home(&home, home.join("no-sys-run"));
        let server = tokio::net::UnixListener::bind(colima.join("one/docker.sock")).unwrap();
        std::os::unix::fs::symlink(colima.join("one"), colima.join("peer")).unwrap();

        assert_eq!(
            inspect_roots(&roots, "profiles", json!({})).await.unwrap(),
            json!(["colima:one"])
        );
        assert_eq!(
            inspect_roots(&roots, "runtimes", json!({})).await.unwrap(),
            json!([{
                "id": "colima:one",
                "runtime": "colima",
                "label": "Colima (one)",
                "profile": "one"
            }])
        );
        // Backwards-compatible socket(root, profile) helper.
        assert_eq!(socket(&colima, "one").unwrap(), colima.join("one/docker.sock"));
        assert!(socket(&colima, "peer").is_err());

        for p in ["peer", "colima:peer", "missing", "../one", "colima:../one"] {
            assert!(inspect_roots(&roots, "containers", json!({"profile": p}))
                .await
                .is_err());
        }
        assert!(inspect_roots(&roots, "delete", json!({"profile": "one"}))
            .await
            .is_err());
        assert!(
            inspect_roots(&roots, "logs", json!({"profile": "one", "id": "latest"}))
                .await
                .is_err()
        );

        let id = "a".repeat(64);
        let target = id.clone();
        let mock = tokio::spawn(async move {
            for (i, expected) in [
                "/containers/json?all=1".to_string(),
                format!("/containers/{target}/logs?stdout=1&stderr=1&timestamps=1&tail=100"),
                "/containers/json?all=1".to_string(),
            ]
            .into_iter()
            .enumerate()
            {
                let (mut s, _) = server.accept().await.unwrap();
                let mut req = [0; 2048];
                let n = s.read(&mut req).await.unwrap();
                assert!(String::from_utf8_lossy(&req[..n])
                    .starts_with(&format!("GET {expected} HTTP/")));
                let body = if i == 1 {
                    vec![b'a'; 70000]
                } else {
                    json!([{"Id": target, "Names": ["/fixture"], "Labels": {"io.x-k8s.kind.cluster": "test", "other": "not-for-ui"}}])
                        .to_string()
                        .into_bytes()
                };
                s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
                s.write_all(&body).await.unwrap();
            }
        });
        // Legacy bare Colima profile name still resolves.
        let r = inspect_roots(&roots, "containers", json!({"profile": "one"}))
            .await
            .unwrap();
        assert_eq!(r[0]["cluster"], "test");
        assert!(!r.to_string().contains("not-for-ui"));
        let t = inspect_roots(&roots, "logs", json!({"profile": "one", "id": id}))
            .await
            .unwrap();
        assert_eq!(t["truncated"], true);
        assert_eq!(t["text"].as_str().unwrap().len(), 65536);
        // New namespaced Colima ID also resolves.
        let r2 = inspect_roots(&roots, "containers", json!({"profile": "colima:one"}))
            .await
            .unwrap();
        assert_eq!(r2[0]["cluster"], "test");
        mock.await.unwrap();
        assert_eq!(
            logs(b"\x01\0\0\0\0\0\0\x04one\n\x02\0\0\0\0\0\0\x09two"),
            "one\ntwo"
        );
    }

    #[tokio::test]
    async fn discovers_all_four_runtimes_and_rejects_symlinked_sockets() {
        let home = temp_root("env-all");
        let _cleanup = Cleanup(home.clone());

        // OrbStack
        let orb_dir = home.join(".orbstack/run");
        std::fs::create_dir_all(&orb_dir).unwrap();
        let _orb_sock = tokio::net::UnixListener::bind(orb_dir.join("docker.sock")).unwrap();

        // Colima (two profiles, plus one symlinked profile that must be ignored)
        let colima_def = home.join(".colima/default");
        let colima_alt = home.join(".colima/work");
        std::fs::create_dir_all(&colima_def).unwrap();
        std::fs::create_dir_all(&colima_alt).unwrap();
        let _colima_def_sock = tokio::net::UnixListener::bind(colima_def.join("docker.sock")).unwrap();
        let _colima_alt_sock = tokio::net::UnixListener::bind(colima_alt.join("docker.sock")).unwrap();
        std::os::unix::fs::symlink(&colima_def, home.join(".colima/alias")).unwrap();

        // Docker via symlinked system root dir (simulating macOS /var/run -> /private/var/run)
        let real_sys_run = home.join("private-var-run");
        let symlink_sys_run = home.join("var-run-link");
        std::fs::create_dir_all(&real_sys_run).unwrap();
        std::os::unix::fs::symlink(&real_sys_run, &symlink_sys_run).unwrap();
        let _docker_sock = tokio::net::UnixListener::bind(real_sys_run.join("docker.sock")).unwrap();

        // Lima (real instance + symlinked instance that must be ignored)
        let lima_sock_dir = home.join(".lima/dev/sock");
        std::fs::create_dir_all(&lima_sock_dir).unwrap();
        let _lima_sock = tokio::net::UnixListener::bind(lima_sock_dir.join("docker.sock")).unwrap();
        std::os::unix::fs::symlink(home.join(".lima/dev"), home.join(".lima/link")).unwrap();

        let roots = DiscoveryRoots::from_home(&home, symlink_sys_run.clone());
        let runtimes = inspect_roots(&roots, "runtimes", json!({})).await.unwrap();
        assert_eq!(
            runtimes,
            json!([
                { "id": "orbstack", "runtime": "orbstack", "label": "OrbStack", "profile": "default" },
                { "id": "colima:default", "runtime": "colima", "label": "Colima (default)", "profile": "default" },
                { "id": "colima:work", "runtime": "colima", "label": "Colima (work)", "profile": "work" },
                { "id": "docker", "runtime": "docker", "label": "Docker", "profile": "default" },
                { "id": "lima:dev", "runtime": "lima", "label": "Lima (dev)", "profile": "dev" },
            ])
        );

        let profiles = inspect_roots(&roots, "profiles", json!({})).await.unwrap();
        assert_eq!(
            profiles,
            json!(["orbstack", "colima:default", "colima:work", "docker", "lima:dev"])
        );

        // Socket resolution covers every runtime ID and legacy bare Colima profile name.
        assert_eq!(resolve_socket(&roots, "orbstack").unwrap(), orb_dir.join("docker.sock"));
        assert_eq!(resolve_socket(&roots, "colima:default").unwrap(), colima_def.join("docker.sock"));
        assert_eq!(resolve_socket(&roots, "default").unwrap(), colima_def.join("docker.sock"));
        assert_eq!(resolve_socket(&roots, "docker").unwrap(), real_sys_run.join("docker.sock"));
        assert_eq!(resolve_socket(&roots, "lima:dev").unwrap(), lima_sock_dir.join("docker.sock"));
        assert!(resolve_socket(&roots, "lima:link").is_err());
        assert!(resolve_socket(&roots, "colima:alias").is_err());

        // Symlinked docker.sock file itself must ALWAYS be rejected even inside a canonicalized dir.
        let sym_home = temp_root("env-sym-sock");
        let _cleanup2 = Cleanup(sym_home.clone());
        let sym_docker = sym_home.join(".docker/run");
        std::fs::create_dir_all(&sym_docker).unwrap();
        std::os::unix::fs::symlink(real_sys_run.join("docker.sock"), sym_docker.join("docker.sock")).unwrap();
        let sym_roots = DiscoveryRoots::from_home(&sym_home, sym_home.join("no-sys"));
        assert_eq!(inspect_roots(&sym_roots, "runtimes", json!({})).await.unwrap(), json!([]));
        assert!(resolve_socket(&sym_roots, "docker").is_err());
    }

    #[tokio::test]
    #[ignore = "explicit local metadata only; never fetch logs"]
    async fn selected_runtime_metadata() {
        let p = std::env::var("PHILIUM_TEST_PROFILE").unwrap();
        let r = call("containers", json!({"profile": p})).await.unwrap();
        let rows = r.as_array().unwrap();
        println!(
            "{} containers, {} distinct kind clusters",
            rows.len(),
            rows.iter()
                .filter_map(|v| v["cluster"].as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }
}
