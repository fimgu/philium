//! Human-operated, read-only Docker inspection. Never use ambient Docker/kube contexts.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn valid_profile(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}
fn valid_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
#[cfg(unix)]
fn socket(root: &Path, profile: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::FileTypeExt;
    if !valid_profile(profile) {
        return Err("Invalid profile".into());
    }
    let p = root.join(profile).join("docker.sock");
    if p.canonicalize().ok().as_ref() != Some(&p)
        || !std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_socket())
    {
        return Err("No direct local Docker socket for this profile".into());
    }
    Ok(p)
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
async fn inspect(root: &Path, method: &str, args: Value) -> Result<Value, String> {
    if method == "profiles" {
        let entries = match std::fs::read_dir(root) {
            Ok(x) => x,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!([])),
            Err(_) => return Err("Cannot read Colima profile directory".into()),
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|p| socket(root, p).is_ok())
            .collect();
        names.sort();
        return Ok(json!(names));
    }
    let sock = socket(root, args["profile"].as_str().ok_or("Select a profile")?)?;
    match method {
        "containers" => {
            let (buf, cut) = get(sock, "/containers/json?all=1", 2 * 1024 * 1024).await?;
            if cut {
                return Err("Inventory exceeds 2 MiB; not showing an incomplete list".into());
            }
            let rows: Vec<Value> =
                serde_json::from_slice(&buf).map_err(|_| "Invalid Docker inventory")?;
            Ok(json!(rows.iter().filter(|r|r["Id"].as_str().is_some_and(valid_id)).map(|r|json!({"id":r["Id"],"name":r["Names"][0].as_str().unwrap_or("Unnamed").trim_start_matches('/'),"image":r["Image"],"status":r["Status"],"state":r["State"],"cluster":r["Labels"]["io.x-k8s.kind.cluster"],"project":r["Labels"]["com.docker.compose.project"]})).collect::<Vec<_>>()))
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
            Ok(json!({"text":logs(&b),"truncated":cut}))
        }
        _ => Err("Unknown read-only environment action".into()),
    }
}
pub async fn call(method: &str, args: Value) -> Result<Value, String> {
    #[cfg(unix)]
    {
        inspect(
            &crate::pilot::colima_dir().ok_or("No local profile directory")?,
            method,
            args,
        )
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (method, args);
        Err("Local Colima sockets are not supported on this platform".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn direct_socket_only_and_bounded_tail() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("env-{}", crate::db::new_id()));
        std::fs::create_dir_all(root.join("one")).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let server = tokio::net::UnixListener::bind(root.join("one/docker.sock")).unwrap();
        std::os::unix::fs::symlink(root.join("one"), root.join("peer")).unwrap();
        assert_eq!(
            inspect(&root, "profiles", json!({})).await.unwrap(),
            json!(["one"])
        );
        for p in ["peer", "missing", "../one"] {
            assert!(inspect(&root, "containers", json!({"profile":p}))
                .await
                .is_err());
        }
        assert!(inspect(&root, "delete", json!({"profile":"one"}))
            .await
            .is_err());
        assert!(
            inspect(&root, "logs", json!({"profile":"one","id":"latest"}))
                .await
                .is_err()
        );
        let id = "a".repeat(64);
        let target = id.clone();
        let mock = tokio::spawn(async move {
            for (i, expected) in [
                "/containers/json?all=1".to_string(),
                format!("/containers/{target}/logs?stdout=1&stderr=1&timestamps=1&tail=100"),
            ]
            .into_iter()
            .enumerate()
            {
                let (mut s, _) = server.accept().await.unwrap();
                let mut req = [0; 2048];
                let n = s.read(&mut req).await.unwrap();
                assert!(String::from_utf8_lossy(&req[..n])
                    .starts_with(&format!("GET {expected} HTTP/")));
                let body = if i == 0 {
                    json!([{"Id":target,"Names":["/fixture"],"Labels":{"io.x-k8s.kind.cluster":"test","other":"not-for-ui"}}]).to_string().into_bytes()
                } else {
                    vec![b'a'; 70000]
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
        let r = inspect(&root, "containers", json!({"profile":"one"}))
            .await
            .unwrap();
        assert_eq!(r[0]["cluster"], "test");
        assert!(!r.to_string().contains("not-for-ui"));
        let t = inspect(&root, "logs", json!({"profile":"one","id":id}))
            .await
            .unwrap();
        assert_eq!(t["truncated"], true);
        assert_eq!(t["text"].as_str().unwrap().len(), 65536);
        mock.await.unwrap();
        assert_eq!(
            logs(b"\x01\0\0\0\0\0\0\x04one\n\x02\0\0\0\0\0\0\x09two"),
            "one\ntwo"
        );
    }
    #[tokio::test]
    #[ignore = "explicit local metadata only; never fetch logs"]
    async fn selected_runtime_metadata() {
        let p = std::env::var("PHILIUM_TEST_PROFILE").unwrap();
        let r = call("containers", json!({"profile":p})).await.unwrap();
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
