//! Side-by-side trial. Selected by the compiled bundle identifier, not user settings.
use crate::{board, ctx::Ctx, db::Db, gateway::Gateway, hal, workspace};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub const ID: &str = "dev.gunnarsson.philium.pilot";
static BASE: OnceLock<PathBuf> = OnceLock::new();
pub fn active() -> bool {
    BASE.get().is_some()
}

// Only the human-operated Environments explorer uses this fixed host directory.
// Do not restore the real HOME to children or expose an arbitrary host path.
pub(crate) fn colima_dir() -> Option<PathBuf> {
    BASE.get()
        .and_then(|base| base.parent().map(|p| p.join(".colima")))
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".colima")))
}

// Refuse links rather than accidentally following a pilot path into live data.
fn child_dir(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let p = parent.join(name);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(&p) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.to_string()),
    }
    if !fs::symlink_metadata(&p)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err(format!("Not a private directory: {}", p.display()));
    }
    Ok(p)
}

pub fn prepare() -> Result<(), String> {
    let home = PathBuf::from(env::var_os("HOME").ok_or("HOME missing")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let base = child_dir(&home, "Philium Pilot")?;
    let data = child_dir(&base, "data")?;
    let root = child_dir(&base, "workspace")?;
    let private_home = child_dir(&base, "home")?;
    // Before the desktop/runtime threads start. Never inherit preview overrides from a parent app.
    env::set_var("PHILIUM_DATA_DIR", data);
    env::set_var("PHILIUM_ROOT", root);
    env::set_var("PHILIUM_PREVIEW", "1");
    env::set_var("HOME", &private_home);
    for (key, dir) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("CODEX_HOME", "codex"),
        ("CLAUDE_CONFIG_DIR", "claude"),
    ] {
        env::set_var(key, private_home.join(dir));
    }
    BASE.set(base)
        .map_err(|_| "Pilot already initialized".into())
}

/// Packaged, non-graphical regression. Only accepts an explicitly marked disposable HOME.
/// The test harness starts an independent process for each phase. No imported real workspaces.
pub async fn check() -> Result<(), String> {
    let base = BASE.get().ok_or("Not a pilot binary")?;
    let marker = base
        .parent()
        .ok_or("No fixture home")?
        .join(".philium-pilot-fixture");
    if fs::read_to_string(marker).ok().as_deref() != Some("disposable\n") {
        return Err("A disposable test home is required".into());
    }
    let phase = env::args().nth(2).ok_or("Missing seed or recover")?;
    if !matches!(phase.as_str(), "seed" | "recover") {
        return Err("Unknown phase".into());
    }
    let dir = base.join("data");
    let db = dir.join("philium.db");
    if (phase == "seed") == db.exists() {
        return Err("Unexpected profile state".into());
    }
    let ctx = Ctx::new(
        Db::open(&db)?,
        dir,
        base.join("workspace"),
        Gateway::new(hal::random_token(), None),
        "http://127.0.0.1:9".into(),
    );
    workspace::init(&ctx)?;
    board::init(&ctx)?;
    crate::library::init(&ctx)?;
    crate::reaper::init(&ctx)?;
    crate::refresh_desk(&ctx);
    let desk = ctx.desk().ok_or("Bundled HAL did not start")?;
    let result = async {
        let app = crate::App { ctx: ctx.clone() };
        for method in ["selfdev.apply", "reaper.sweep"] {
            if !crate::rpc_inner(&app, method.into(), None)
                .await
                .unwrap_err()
                .contains("disabled in the side-by-side pilot")
            {
                return Err(format!("Missing pilot guard: {method}"));
            }
        }
        for name in ["alpha", "beta"] {
            let path = base.join(name).join("same");
            if phase == "seed" {
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
                workspace::git(&path, &["init", "-b", "main"])?;
                if name=="alpha" {fs::write(path.join("display.json"),r#"{"columns":["name","status"]}"#).map_err(|e|e.to_string())?;workspace::git(&path,&["add","--","display.json"])?;fs::write(path.join("schema.sql"),sql_fixture(false)).map_err(|e|e.to_string())?;}else{
                    fs::create_dir(path.join("philium")).map_err(|e|e.to_string())?;
                    let seed="WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<101) INSERT INTO items SELECT i FROM n";
                    let cases=json!([{"id":"page-101","seed":seed,"query":"SELECT id FROM page ORDER BY id LIMIT 1 OFFSET 100","expect":[[101]]},{"id":"legacy-101","seed":seed,"query":"SELECT id FROM legacy WHERE id=101","expect":[[101]]},{"id":"missing-view","seed":seed,"query":"SELECT * FROM absent","expect":[]},{"id":"second-column","document":"display.json","pointer":"/columns/1","expect":"status"}]);
                    fs::write(path.join("philium/sql-contracts.json"),json!({"schema":"schema.sql","cases":cases}).to_string()).map_err(|e|e.to_string())?;}
                workspace::git(&path,&["add","--",if name=="alpha"{"schema.sql"}else{"philium/sql-contracts.json"}])?;
                workspace::git(
                    &path,
                    &[
                        "-c",
                        "user.name=Pilot",
                        "-c",
                        "user.email=pilot@example.invalid",
                        "commit",
                        "--allow-empty",
                        "-m",
                        name,
                    ],
                )?;
                workspace::git(&path,&["config","remote.origin.url","https://github.com/philium-fixture/offline.git"])?;
                let r = crate::rpc_inner(
                    &app,
                    "workspace.repo_add".into(),
                    Some(json!({"path":path})),
                )
                .await?;
                crate::rpc_inner(
                    &app,
                    "board.create".into(),
                    Some(json!({"repo_id":r["id"],"title":name})),
                )
                .await?;
                ctx.db.kv_set(
                    &format!("fixture.{name}"),
                    r["id"].as_str().ok_or("Missing ID")?,
                )?;
            }
            let id = ctx
                .db
                .kv_get(&format!("fixture.{name}"))
                .ok_or("Missing saved repo")?;
            if workspace::repo(&ctx, &id)
                .as_ref()
                .and_then(|r| r["path"].as_str())
                != path.to_str()
            {
                return Err("Checkout not preserved".to_owned());
            }
            let tasks =
                crate::rpc_inner(&app, "board.list".into(), Some(json!({"repo_id":id}))).await?;
            let rows = tasks.as_array().ok_or("Missing board")?;
            if rows.len() != 1 || rows[0]["title"] != name {
                return Err("Lost task or peer task visible".into());
            }
        }
        let alpha = ctx.db.kv_get("fixture.alpha").ok_or("Missing alpha checkout")?;
        let beta = ctx.db.kv_get("fixture.beta").ok_or("Missing beta checkout")?;
        let tasks = crate::rpc_inner(&app,"board.list".into(),Some(json!({"repo_id":alpha}))).await?;
        let task = tasks[0]["id"].as_str().ok_or("Missing alpha task")?;
        let denied = crate::rpc_inner(&app,"agents.spawn".into(),Some(json!({"repo_id":beta,"task_id":task}))).await;
        if !matches!(denied, Err(ref msg) if msg.contains("Choose a task from the agent's workspace")) { return Err("Sibling task was not rejected before provisioning".into()); }
        for (rid, label) in [(&alpha,"alpha"),(&beta,"beta")] {
            if phase == "seed" {
                ctx.db.exec("INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES(?1,?1,'fixture',?2,1,1)", rusqlite::params![label,rid])?;
                crate::reaper::register(&ctx,Some(label),"kind_cluster",label,json!({}));
                let tid=crate::rpc_inner(&app,"terminals.open".into(),Some(json!({"repo_id":rid,"cwd":base,"title":label,"command":["/bin/pwd"]}))).await?;
                let tid=tid.as_str().ok_or("Missing terminal ID")?;
                let expected=base.join(label).join("same");
                let mut correct=false;
                for _ in 0..100 {
                    let bytes=crate::terminals::since(&ctx,tid,0).unwrap_or_default();
                    if String::from_utf8_lossy(&bytes).trim()==expected.to_string_lossy() { correct=true; break; }
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
                if !correct {return Err("Terminal did not start in its registered checkout".into());}
            }
            let terms=crate::rpc_inner(&app,"terminals.list".into(),None).await?;
            let scoped: Vec<_>=terms.as_array().ok_or("Invalid terminal list")?.iter().filter(|t|t["workspace_id"].as_str()==Some(rid.as_str())).collect();
            if scoped.len()!=1 || scoped[0]["title"]!=label {return Err("Terminal checkout identity not preserved".into());}
            let d = crate::rpc_inner(&app,"workspace.overview".into(),Some(json!({"repo_id":rid}))).await?;
            for (items,field) in [("tasks","title"),("agents","name"),("resources","handle")] {
                if d[items].as_array().map(Vec::len)!=Some(1) || d[items][0][field]!=label {return Err(format!("{items} escaped checkout scope"));}
            }
        }
        for method in ["agents.kind_create","agents.kind_delete"] {
            let result=crate::rpc_inner(&app,method.into(),Some(json!({"id":"alpha","cluster":"alpha"}))).await;
            if !matches!(result,Err(ref e) if e.contains("disabled in the side-by-side pilot")) {return Err(format!("Missing pilot guard for {method}"));}
        }
        if phase == "seed" {
            let session = desk
                .call(
                    reqwest::Method::POST,
                    "/sessions",
                    Some(crate::chat_session_body(
                        &base.join("workspace"),
                        "pilot-recovery",
                        None,
                    )),
                )
                .await?;
            ctx.db.kv_set(
                "fixture.session",
                session["id"].as_str().ok_or("No HAL session")?,
            )?;
        }
        let id = ctx
            .db
            .kv_get("fixture.session")
            .ok_or("Session not saved")?;
        let session = desk
            .call(reqwest::Method::GET, &format!("/sessions/{id}"), None)
            .await?;
        if session["id"] != id {
            return Err("Wrong HAL session".into());
        }
        if ctx.db.kv_get("library.imported_at").is_some()
            || crate::reaper::shutdown(&ctx)?["skipped"] != "pilot"
        {
            return Err("Pilot startup or shutdown isolation failed".into());
        }
        release_graph_fixture(&app, &base.join("alpha/same"), &alpha, &beta, phase=="seed").await?;
        Ok(())
    }
    .await;
    crate::terminals::shutdown(&ctx);
    desk.stop();
    result?;
    println!("Pilot {phase}: selected workspace tasks isolated, both IDs preserved, bundled HAL session recovered.");
    Ok(())
}

// Only reached from the packaged check with its explicit disposable-home guard.
async fn release_graph_fixture(
    app: &crate::App,
    path: &Path,
    repo: &str,
    peer: &str,
    seed: bool,
) -> Result<(), String> {
    let call =
        |method: &str, args| crate::rpc_inner(app, format!("release_intake.{method}"), Some(args));
    call("history", json!({"repo_id":repo})).await?;
    if seed {
        let root = workspace::git(path, &["rev-parse", "HEAD"])?;
        let node = |name: &str, parents: &[&str], tree: &str| {
            let mut args = vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit-tree",
                tree,
                "-m",
                name,
            ];
            for p in parents {
                args.extend(["-p", *p]);
            }
            workspace::git(path, &args)
        };
        let good = "HEAD^{tree}";
        let target = node("target only", &[&root], good)?;
        let direct = node("direct", &[&root], good)?;
        let child = node("child", &[&root], good)?;
        fs::write(path.join("schema.sql"), sql_fixture(true)).map_err(|e| e.to_string())?;
        fs::write(path.join("display.json"), r#"{"columns":["name"]}"#)
            .map_err(|e| e.to_string())?;
        workspace::git(path, &["add", "--", "schema.sql", "display.json"])?;
        let bad = workspace::git(path, &["write-tree"])?;
        workspace::git(
            path,
            &[
                "restore",
                "--staged",
                "--worktree",
                "--",
                "schema.sql",
                "display.json",
            ],
        )?;
        let merged = node("integration", &[&direct, &child], &bad)?;
        let url = "https://github.com/philium-fixture/offline/pull/1";
        for h in [&direct, &merged] {
            crate::release_intake::record(
                &app.ctx.db,
                repo,
                "philium-fixture/offline",
                1,
                &json!({"number":1,"url":url,"baseRefOid":target,"headRefOid":h}),
            )?;
        }
        for (k, v) in [
            ("root", root),
            ("target", target),
            ("direct", direct),
            ("child", child),
            ("merged", merged),
        ] {
            workspace::git(path, &["branch", &format!("fixture-{k}"), &v])?;
        }
    }
    let oid =
        |name: &str| workspace::git(path, &["rev-parse", &format!("refs/heads/fixture-{name}")]);
    let all = call("history", json!({"repo_id":repo})).await?;
    let rows = json!(all
        .as_array()
        .ok_or("Invalid history")?
        .iter()
        .filter(|r| r["pr"] == 1)
        .collect::<Vec<_>>());
    if rows.as_array().map(Vec::len) != Some(2) {
        return Err("Missing fixture generations".into());
    }
    for (i, n) in [(0, 3), (1, 1)] {
        let id = &rows[i]["seq"];
        let graph = call("graph", json!({"repo_id":repo,"generation":id})).await?;
        let commits = graph["commits"].as_array().ok_or("Missing graph")?;
        if graph["merge_base"] != oid("root")?
            || graph["target"] != oid("target")?
            || commits.len() != n
            || graph["head"] != oid(if i == 0 { "merged" } else { "direct" })?
        {
            return Err("Wrong commit range or comparator".into());
        }
        if i == 0
            && (commits[0]["parents"] != json!([oid("direct")?, oid("child")?])
                || !commits.iter().any(|v| v["oid"] == commits[0]["parents"][1]))
        {
            return Err("Wrong merge edges".into());
        }
        if !matches!(call("graph",json!({"repo_id":peer,"generation":id})).await,Err(e) if e=="Generation does not belong to this checkout")
        {
            return Err("Peer generation not isolated".into());
        }
    }
    let first = json!({"repo_id":repo,"generation":rows[1]["seq"]});
    let latest = json!({"repo_id":repo,"generation":rows[0]["seq"]});
    if seed {
        if call("followup", latest.clone()).await?["mode"] != "full" {
            return Err("Captured head became a review checkpoint".into());
        }
        call("checkpoint", first.clone()).await?;
    }
    let next = call("followup", latest).await?;
    if call("followup", first).await?["mode"] != "checkpoint"
        || next["mode"] != "followup"
        || next["from"] != oid("direct")?
        || next["to"] != oid("merged")?
        || next["commits"].as_array().map(Vec::len) != Some(2)
    {
        return Err("Follow-up lost the last reviewed head or missed a commit".into());
    }
    if seed {
        call("sql_enroll", json!({"repo_id":repo,"driver":peer})).await?;
        let fixed = workspace::git(
            path,
            &[
                "-c",
                "user.name=Pilot",
                "-c",
                "user.email=pilot@example.invalid",
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                &oid("merged")?,
                "-m",
                "restore page",
            ],
        )?;
        workspace::git(path, &["branch", "fixture-fixed", &fixed])?;
        for h in [oid("merged")?, fixed] {
            let gen = crate::release_intake::record(
                &app.ctx.db,
                repo,
                "philium-fixture/offline",
                2,
                &json!({"number":2,"url":"https://github.com/philium-fixture/offline/pull/2","baseRefOid":oid("target")?,"headRefOid":h}),
            )?;
            let args = json!({"repo_id":repo,"generation":gen["generation"]});
            if !call("sql_receipt", args.clone()).await?.is_null() {
                return Err("New head inherited evidence".into());
            }
            if !call("members", args.clone()).await?.is_null() {
                return Err("New head inherited PR associations".into());
            }
            let graph = call("graph", args.clone()).await?;
            let outer = oid("merged")?;
            let inner = graph["commits"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["oid"] == outer))
                .and_then(|c| c["parents"][1].as_str())
                .ok_or("Missing fixture child")?;
            let mut fixture = json!({});
            for (pr, pin) in [(30, outer.as_str()), (31, inner)] {
                fixture[pin] = json!([{"merged":true,"merge":pin,"number":pr}]);
            }
            let m = crate::release_members::save(
                &app.ctx,
                &json!({"pr":2,"head":h}),
                gen["generation"].as_i64().ok_or("Missing generation")?,
                &graph["commits"],
                &fixture,
            )?;
            if m["edges"] != json!([{"outer":30,"inner":31}]) {
                return Err("Nested fixture membership not established".into());
            }
            call("sql_compare", args).await?;
        }
    }
    let all = call("history", json!({"repo_id":repo})).await?;
    let found: Vec<_> = all
        .as_array()
        .ok_or("Invalid history")?
        .iter()
        .filter(|r| r["pr"] == 2)
        .collect();
    if found.len() != 2 {
        return Err("Missing SQL generations".into());
    }
    for (index, r) in found.iter().enumerate() {
        let receipt = call("sql_receipt", json!({"repo_id":repo,"generation":r["seq"]})).await?;
        let evidence = receipt["cases"][3]["landed_evidence"]
            .as_array()
            .ok_or("Missing landed document evidence")?;
        if !evidence
            .iter()
            .any(|c| c["pr"] == 31 && c["result"] == "pass")
            || !evidence.iter().any(|c| {
                c["pr"] == 30
                    && c["result"]
                        == if index == 0 {
                            "different_failure"
                        } else {
                            "same_failure_as_candidate"
                        }
            })
        {
            return Err("Landed commit evidence incorrect".into());
        }
        let expected = if index == 0 {
            "candidate_pass"
        } else {
            "composed_head_regression"
        };
        if receipt["head"] != r["head"]
            || receipt["cases"][0]["state"] != expected
            || receipt["cases"][1]["state"] != "same_failure_on_target"
            || receipt["cases"][2]["state"] != "incomplete"
            || receipt["cases"][3]["state"] != expected
            || receipt["deployment_identity"]["state"] != "absent"
            || receipt["staging"] != "not_verified"
            || receipt["release_decision"]
                != if index == 0 {
                    "incomplete"
                } else {
                    "not_ready_at_tested_head"
                }
        {
            return Err(format!(
                "SQL acceptance failed: expected {expected}, got {}",
                receipt["cases"]
            ));
        }
        if call("sql_receipt", json!({"repo_id":peer,"generation":r["seq"]}))
            .await
            .is_ok()
        {
            return Err("Peer could read private receipt".into());
        }
    }
    if !seed {
        let args = json!({"repo_id":repo,"generation":found[0]["seq"]});
        let prev = call("sql_receipt", args.clone()).await?;
        let fresh = call("sql_compare", args).await?;
        if fresh["head"] != prev["head"]
            || fresh["observed"].as_i64() <= prev["observed"].as_i64()
            || fresh["release_decision"] != "incomplete"
        {
            return Err("Restart did not produce a fresh incomplete decision".into());
        }
    }
    deployment_fixture().await?;
    println!("SQL and JSON: merged head regresses, fixed revision restores both. Nested fixture PRs differ. Historical and fresh receipts isolated. Observer: mixed, stale and match distinguished; no staging workflow credit.");
    println!("Release graph: divergent target excluded, merge parents preserved, peer denied. Follow-up covers both commits since the reviewed revision (fixture origin metadata).");
    Ok(())
}
async fn deployment_fixture() -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let head = "a".repeat(40);
    let digest = format!("sha256:{}", "b".repeat(64));
    for expected in ["mixed", "unknown", "match"] {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "Fixture listener unavailable")?;
        let port = listener.local_addr().map_err(|_| "No port")?.port();
        let replica = json!({"revision":head,"digest":digest,"ready":true});
        let mut peer = replica.clone();
        if expected == "mixed" {
            peer["revision"] = json!("c".repeat(40));
        }
        let body=json!({"observed_ms":crate::db::now()-if expected=="unknown"{180000}else{0},"complete":true,"replicas":[replica,peer]}).to_string();
        let serve = async move {
            let (mut tcp, _) = listener.accept().await.ok()?;
            let mut buf = [0; 1024];
            let n = tcp.read(&mut buf).await.ok()?;
            if !buf[..n].starts_with(b"GET /.well-known/philium/deployment HTTP/1.1\r\n") {
                return None;
            }
            tcp.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .ok()
        };
        let (served, state) = tokio::join!(
            tokio::time::timeout(std::time::Duration::from_secs(4), serve),
            crate::release_sql::stage_probe(Some(port), &head, &digest)
        );
        if !matches!(served, Ok(Some(()))) || state["state"] != expected {
            return Err(format!(
                "Deployment fixture expected {expected}, got {state}"
            ));
        }
    }
    Ok(())
}
fn sql_fixture(broken: bool) -> String {
    format!("CREATE TABLE items(id INTEGER PRIMARY KEY); CREATE VIEW page AS SELECT id FROM items {}; CREATE VIEW legacy AS SELECT id FROM items WHERE id<101;",if broken{"WHERE id<101"}else{""})
}
