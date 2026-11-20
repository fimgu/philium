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
// The comparison lane runs one comparison at a time by design. Tests that use it take turns.
static LANE_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[tokio::test]
async fn reviewed_disposable_change() {
    let _turn = LANE_TESTS.lock().await;
    let tmp = std::env::var_os("PHILIUM_SCRATCH_DIR")
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
            json!({"state":"clean"}),
        )
        .unwrap()
    };
    let ran = compare().await.unwrap();
    let initial = result(ran.clone(), observe(&head));
    assert_eq!(initial["contract_decision"], "scoped_pass");
    assert_eq!(initial["decision"], "incomplete");
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert!(nothing_answers(port).await, "the stand-in service still answers after it was stopped");
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
    assert!(nothing_answers(port).await, "the stand-in service still answers after it was stopped");
}

#[test]
fn every_cause_gets_its_own_sentence() {
    let digest = format!("sha256:{}", "b".repeat(64));
    let attested = json!({"recorded":true,"all_changes_accounted_for":true,"code_review_complete":true,"cleanup_verified":true,"unresolved_findings":0,"required_cases":["child"]});
    let receipt = json!({"artifact":digest,"release_records":true,
        "cases":[
            {"id":"child","state":"release_range_regression","landed_evidence":[{"pr":31,"oid":"x","result":"same_failure_as_candidate"},{"pr":32,"oid":"y","result":"pass"}]},
            {"id":"merge","state":"composed_head_regression","landed_evidence":[{"pr":31,"oid":"x","result":"pass"}]},
            {"id":"old","state":"same_failure_on_target"},
            {"id":"gone","state":"incomplete"}],
        "live_cases":[{"id":"view","state":"unknown"}],
        "deployment_identity":{"state":"mismatch"},"deployment_after":{"state":"mismatch"},
        "review_attestation":attested,"verified_evidence":{"child":"h"},
        "evidence_status":{"merge":"altered","old":"missing"},"review_scope_verified":false});
    let moved = json!({"head":"c".repeat(40)});
    let why = explain(Some(false), Some(&moved), &json!({"missing":2,"ambiguous":0}), &receipt, None).join("\n");
    for needle in [
        "moved during verification",
        "GitHub did not say which pull request 2 of the included commits came from",
        "child: regression inside this release",
        "Landed #31 fails the same way on its own",
        "merge: merge-only regression",
        "old: already failing on the target",
        "gone: missing evidence",
        "Wrong deployment before the checks",
        "merge (altered",
        "old (missing",
        "gone (not recorded",
    ] {
        assert!(why.contains(needle), "{needle}\n{why}");
    }
    // The merge-only case names no child: every landed change passes on its own.
    assert!(!why.lines().any(|l| l.starts_with("merge:") && l.contains("#31")), "{why}");
    let clean = json!({"artifact":digest,"release_records":true,
        "cases":[{"id":"child","state":"candidate_pass"}],"live_cases":[{"id":"view","state":"pass"}],
        "deployment_identity":{"state":"match"},"deployment_after":{"state":"match"},
        "review_attestation":{"recorded":true,"all_changes_accounted_for":true,"code_review_complete":true,"cleanup_verified":true,"unresolved_findings":0,"required_cases":["child","view"]},
        "verified_evidence":{"child":"h","view":"h"},"evidence_status":{"child":"verified","view":"verified"},"review_scope_verified":true});
    let members = json!({"missing":0,"ambiguous":0});
    assert_eq!(explain(Some(true), None, &members, &clean, None), Vec::<String>::new());
    // The same passing checks on a newer head: no record for that head, so nothing carries over.
    let mut fresh = clean.clone();
    fresh["review_attestation"] = json!({"recorded":false});
    fresh["artifact"] = Value::Null;
    fresh["deployment_identity"] = json!({"state":"unknown"});
    let why = explain(Some(true), None, &members, &fresh, None).join("\n");
    assert!(why.contains("An earlier head's record does not carry over") && why.contains("No independent artifact record exists for this exact head"), "{why}");
    // No observer, an observer that is down, and an origin that cannot be reached all read differently.
    let mut absent = clean.clone();
    absent["deployment_identity"] = json!({"state":"absent"});
    let mut down = clean.clone();
    down["deployment_identity"] = json!({"state":"unknown"});
    let a = explain(Some(true), None, &members, &absent, None).join("\n");
    let d = explain(Some(true), None, &members, &down, None).join("\n");
    let o = explain(None, None, &Value::Null, &Value::Null, Some("Membership discovery: Origin unavailable")).join("\n");
    assert!(a.contains("No deployment observer is configured") && d.contains("Deployment unavailable before the checks") && o.contains("Origin could not be reconfirmed") && o.contains("Landed pull requests were not established"), "{a}\n{d}\n{o}");
}

#[test]
fn a_target_counts_only_with_its_own_identity() {
    let t = |name: &str, before: &str, after: &str| json!({"name":name,"identity_before":{"state":before},"identity_after":{"state":after}});
    let c = |target: &str, state: &str| json!({"id":format!("{target}-{state}"),"target":target,"state":state});
    let both = [t("env01", "match", "match"), t("local", "match", "match")];
    assert_eq!(with_targets("scoped_pass", &both, &json!([c("env01", "pass"), c("local", "pass")])), "scoped_pass");
    // One environment without an identity: observed, never counted, never blocking.
    let unknown = [t("env01", "absent", "absent"), t("local", "match", "match")];
    assert_eq!(with_targets("scoped_pass", &unknown, &json!([c("env01", "pass"), c("local", "pass")])), "inconclusive");
    assert_eq!(with_targets("scoped_pass", &unknown, &json!([c("env01", "mismatch"), c("local", "pass")])), "inconclusive");
    // A mismatch on a matching deployment blocks, whatever the other target says.
    assert_eq!(with_targets("scoped_pass", &both, &json!([c("env01", "mismatch"), c("local", "pass")])), "blocked");
    assert_eq!(with_targets("inconclusive", &both, &json!([c("env01", "pass"), c("local", "mismatch")])), "blocked");
    // Wrong or changed deployment, unobserved checks, or a target with no checks: inconclusive.
    for targets in [[t("env01", "mismatch", "mismatch"), t("local", "match", "match")], [t("env01", "match", "unknown"), t("local", "match", "match")]] {
        assert_eq!(with_targets("scoped_pass", &targets, &json!([c("env01", "pass"), c("local", "pass")])), "inconclusive");
    }
    assert_eq!(with_targets("scoped_pass", &both, &json!([c("env01", "unknown"), c("local", "pass")])), "inconclusive");
    assert_eq!(with_targets("scoped_pass", &both, &json!([c("local", "pass")])), "inconclusive");
    // Targets never upgrade a failed or moved base result.
    assert_eq!(with_targets("blocked", &both, &json!([c("env01", "pass"), c("local", "pass")])), "blocked");
    assert_eq!(with_targets("superseded", &both, &json!([c("env01", "pass"), c("local", "pass")])), "superseded");
    assert_eq!(with_targets("inconclusive", &both, &json!([c("env01", "pass"), c("local", "pass")])), "inconclusive");
    assert_eq!(with_targets("scoped_pass", &[], &json!([])), "scoped_pass");
    // Each target's state gets its own sentence, named.
    let receipt = json!({"artifact":format!("sha256:{}", "b".repeat(64)),"release_records":true,"cases":[],"checks_confirmed_ms":1,
        "live_cases":[c("env01", "unknown"), c("local", "mismatch"), c("stage", "pass")],
        "targets":[t("env01", "absent", "absent"), t("local", "match", "match"), t("stage", "mismatch", "mismatch")],
        "review_attestation":{"recorded":false}});
    let why = explain(Some(true), None, &json!({"missing":0,"ambiguous":0}), &receipt, None).join("\n");
    for needle in [
        "env01: the confirmed checks include no identity command",
        "env01-unknown (env01): the command did not return the same readable output twice",
        "local-mismatch (local): the environment returned a different value",
        "stage: Wrong deployment before the checks",
    ] {
        assert!(why.contains(needle), "{needle}\n{why}");
    }
    assert!(!why.contains("No deployment observer is configured"), "{why}");
}

#[tokio::test]
async fn a_check_on_a_file_new_in_the_release_is_not_missing_evidence() {
    let _turn = LANE_TESTS.lock().await;
    let tmp = std::env::var_os("PHILIUM_SCRATCH_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!("review-new-{}", hal::random_token()));
    fs::create_dir(&tmp).unwrap();
    let _cleanup = Scratch(tmp.clone());
    let (p, d) = (tmp.join("product"), tmp.join("driver"));
    for dir in [&p, &d] {
        fs::create_dir(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "core.hooksPath", "/dev/null"]);
    }
    git(&p, &["remote", "add", "origin", "https://github.com/philium-fixture/fresh.git"]);
    let ctx = Ctx::new(Db::open(&tmp.join("db")).unwrap(), tmp.clone(), tmp.clone(), Gateway::new(hal::random_token(), None), String::new());
    workspace::init(&ctx).unwrap();
    let reg = |dir: &Path| workspace::call(&ctx, "repo_add", json!({"path":dir}));
    let product = reg(&p).await.unwrap();
    let repo = product["id"].as_str().unwrap();
    let driver = reg(&d).await.unwrap();
    fs::write(p.join("kept.json"), r#"{"k":0}"#).unwrap();
    let base = commit(&p);
    fs::write(p.join("added.json"), r#"{"k":1}"#).unwrap();
    let head = commit(&p);
    fs::create_dir_all(d.join("philium")).unwrap();
    // The SQLite lane runs in the packaged executable, so this test uses committed files.
    fs::write(d.join("philium/sql-contracts.json"), json!({"cases":[
        {"id":"new-file","document":"added.json","pointer":"/k","expect":1},
        {"id":"new-file-wrong","document":"added.json","pointer":"/k","expect":2},
        {"id":"never-there","document":"never.json","pointer":"/k","expect":1},
        {"id":"kept","document":"kept.json","pointer":"/k","expect":0}]}).to_string()).unwrap();
    commit(&d);
    let call = |op: &'static str, args| release_intake::call(&ctx, op, args);
    call("sql_enroll", json!({"repo_id":repo,"driver":driver["id"]})).await.unwrap();
    let first = release_intake::record(&ctx.db, repo, "philium-fixture/fresh", 3, &json!({"number":3,"url":"https://github.com/philium-fixture/fresh/pull/3","baseRefOid":base,"headRefOid":head})).unwrap();
    let args = json!({"repo_id":repo,"generation":first["generation"]});
    let r = call("sql_compare", args).await.unwrap();
    let state = |i: usize| (r["cases"][i]["state"].as_str().unwrap().to_owned(), r["cases"][i]["new_at_head"] == true);
    assert_eq!(state(0), ("candidate_pass".to_owned(), true), "{}", r["cases"]);
    // New, but wrong on the head: not a pass, and not credited as new.
    assert_eq!(state(1), ("incomplete".to_owned(), false), "{}", r["cases"]);
    // Absent everywhere: still missing evidence.
    assert_eq!(state(2), ("incomplete".to_owned(), false), "{}", r["cases"]);
    // A file the target already had is compared as before.
    assert_eq!(state(3), ("candidate_pass".to_owned(), false), "{}", r["cases"]);
    assert!(r["checks_confirmed_ms"].as_i64().is_some());
}

/// Nothing serves on this port: the connection is refused, or nothing replies. A child
/// process another test spawned can briefly hold a copy of a closed listener, so a
/// successful connect alone does not mean a server is there.
async fn nothing_answers(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else { return true };
    let _ = s.write_all(b"GET /view HTTP/1.1\r\nHost: x\r\n\r\n").await;
    let mut b = [0u8; 1];
    !matches!(tokio::time::timeout(std::time::Duration::from_millis(500), s.read(&mut b)).await, Ok(Ok(n)) if n > 0)
}
