//! Offline origin adapter only in test builds. The verifier, Git, SQLite, and HTTP are real.
use super::*;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn hash(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn git(p: &Path, a: &[&str]) -> String {
    workspace::git(p, a).unwrap()
}
fn commit(p: &Path) -> String {
    git(p, &["add", "."]);
    git(
        p,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    git(p, &["rev-parse", "HEAD"])
}
#[tokio::test]
async fn reviewed_disposable_change() {
    let tmp = std::env::var_os("JCODE_SCRATCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("review-{}", hal::random_token()));
    fs::create_dir(&tmp).unwrap();
    let _cleanup = Scratch(tmp.clone());
    let p = tmp.join("product");
    let d = tmp.join("driver");
    for dir in [&p, &d] {
        fs::create_dir(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "core.hooksPath", "/dev/null"]);
    }
    let origin = "philium-fixture/disposable";
    git(
        &p,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/philium-fixture/disposable.git",
        ],
    );
    let ctx = Ctx::new(
        Db::open(&tmp.join("db")).unwrap(),
        tmp.clone(),
        tmp.clone(),
        Gateway::new(hal::random_token(), None),
        String::new(),
    );
    workspace::init(&ctx).unwrap();
    let reg = |dir: &Path| workspace::call(&ctx, "repo_add", json!({"path":dir}));
    let product = reg(&p).await.unwrap();
    let repo = product["id"].as_str().unwrap();
    let driver = reg(&d).await.unwrap();
    let file = "view.json";
    fs::write(p.join(file), r#"{"column":"name"}"#).unwrap();
    let base = commit(&p);
    let expected = r#"{"column":"status"}"#;
    fs::write(p.join(file), expected).unwrap();
    let head = commit(&p);
    // Review the entire candidate: only one regular non-executable blob changes, from name to status. No code, dependencies, secrets or cleanup actions.
    assert_eq!(
        git(&p, &["diff", "--name-status", &base, &head]),
        "M\tview.json"
    );
    assert!(git(&p, &["ls-tree", &head]).starts_with("100644 blob "));
    assert_eq!(
        git(&p, &["show", &format!("{base}:{file}")]),
        r#"{"column":"name"}"#
    );
    let blob = git(&p, &["show", &format!("{head}:{file}")]);
    assert_eq!(blob, expected);
    let artifact = format!("sha256:{}", hash(blob.as_bytes()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let serve = |listener: tokio::net::TcpListener| {
        let (h, a, blob) = (head.clone(), artifact.clone(), blob.clone());
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut b = Vec::new();
                while !b.ends_with(b"\r\n\r\n") && b.len() < 8192 {
                    if let Ok(x) = s.read_u8().await {
                        b.push(x)
                    } else {
                        break;
                    }
                }
                let body = if b.starts_with(b"GET /view ") {
                    blob.clone()
                } else if b.starts_with(b"GET /.well-known/philium/deployment ") {
                    json!({"observed_ms":crate::db::now(),"complete":true,"replicas":[{"ready":true,"revision":h,"digest":a}]}).to_string()
                } else {
                    continue;
                };
                let msg=format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Philium-Revision: {h}\r\nX-Philium-Artifact: {a}\r\n\r\n{body}",body.len());
                s.write_all(msg.as_bytes()).await.unwrap();
            }
        })
    };
    let server = serve(listener);
    fs::create_dir_all(d.join("philium/releases")).unwrap();
    fs::create_dir_all(d.join("philium/evidence")).unwrap();
    fs::write(d.join("philium/sql-contracts.json"),json!({"release_records":true,"cases":[{"id":"source","document":file,"pointer":"/column","expect":"status"}],"live_cases":[{"id":"view","path":"/view","pointer":"/column","expect":"status"}]}).to_string()).unwrap();
    let pin = commit(&d);
    let call = |op: &'static str, args| release_intake::call(&ctx, op, args);
    call(
        "sql_enroll",
        json!({"repo_id":repo,"driver":driver["id"],"stage_port":port}),
    )
    .await
    .unwrap();
    let observe = |rev: &str| {
        release_intake::record(&ctx.db,repo,origin,1,&json!({"number":1,"url":"https://github.com/philium-fixture/disposable/pull/1","baseRefOid":base,"headRefOid":rev})).unwrap()
    };
    let first = observe(&head);
    let args = json!({"repo_id":repo,"generation":first["generation"]});
    call("members", args.clone()).await.unwrap();
    let graph = call("graph", args.clone()).await.unwrap();
    let membership = crate::release_members::save(
        &ctx,
        &first,
        first["generation"].as_i64().unwrap(),
        &graph["commits"],
        &json!({head.clone():[]}),
    )
    .unwrap();
    let mut att = json!({"base":base,"head":head,"origin":origin,"pr":1,"driver_pin":pin,"artifact":artifact});
    let record = d.join(format!("philium/releases/{head}.json"));
    fs::write(&record, att.to_string()).unwrap();
    commit(&d);
    let compare = || call("sql_compare", args.clone());
    let result = |receipt, last| {
        finish(
            &ctx,
            repo,
            first.clone(),
            Some(last),
            Some(membership.clone()),
            receipt,
            None,
        )
        .unwrap()
    };
    let ran = compare().await.unwrap();
    let initial = result(ran.clone(), observe(&head));
    assert_eq!(initial["contract_decision"], "scoped_pass");
    assert_eq!(initial["decision"], "incomplete");
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
    let server = serve(
        tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap(),
    );
    // Create independent evidence only after observing the exact head's committed and served data.
    for (id,msg) in [("source","Only tracked regular blob view.json changes its column from name to status; the exact-head committed check passed."),("view","The loopback service served the reviewed blob twice and the exact revision and artifact matched before and after.")]{let path=format!("philium/evidence/{id}.json");let bytes=json!({"case":id,"head":head,"status":"pass","observation":msg}).to_string();fs::write(d.join(&path),&bytes).unwrap();att["evidence"][id]=json!({"path":path,"sha256":hash(bytes.as_bytes())});}
    for flag in [
        "code_review_complete",
        "cleanup_verified",
        "all_changes_accounted_for",
    ] {
        att[flag] = json!(true);
    }
    att["unresolved_findings"] = json!([]);
    att["required_cases"] = json!(["source", "view"]);
    fs::write(&record, att.to_string()).unwrap();
    commit(&d);
    let receipt = compare().await.unwrap();
    assert_eq!(
        result(receipt.clone(), observe(&head))["decision"],
        "ready_for_reviewed_scope"
    );
    fs::remove_file(d.join("philium/evidence/view.json")).unwrap();
    commit(&d);
    let partial = compare().await.unwrap();
    assert!(partial["verified_evidence"]["view"].is_null());
    assert_eq!(result(partial, observe(&head))["decision"], "incomplete");
    fs::write(p.join(file), r#"{"column":"later"}"#).unwrap();
    let moved = commit(&p);
    assert_eq!(result(receipt, observe(&moved))["decision"], "superseded");
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err());
}
