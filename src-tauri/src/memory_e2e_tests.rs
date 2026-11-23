//! Real memory pipeline, end to end, with no fakes: the bundled HAL runs a real agent turn
//! against a real subscription provider, the session is observed into memories, the local
//! embedder and reranker are downloaded into a temp data dir and loaded, a later lookup recalls
//! the memory, and one real dream runs and is undone.
//!
//! Opt in: `PHILIUM_E2E_CODEX_AUTH=<path to a Codex auth.json> cargo test memory_e2e -- --ignored --nocapture`.
//! Everything lives under a temp dir that is removed at the end.

use crate::ctx::Ctx;
use crate::providers::Kind;
use serde_json::{json, Value};
use std::time::Duration;

fn jwt_exp_ms(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let part = jwt.split('.').nth(1)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Value>(&raw).ok()?["exp"].as_i64().map(|s| s * 1000)
}

async fn wait_idle(ctx: &Ctx, id: &str, secs: u64) {
    for _ in 0..secs * 4 {
        if let Some(e) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) {
            panic!("agent failed: {e}");
        }
        let row = crate::agents::get_row(ctx, id).unwrap_or_default();
        if row["state"] == "idle" && row["hal_session_id"].as_str().is_some_and(|s| !s.is_empty()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("agent never went idle: {}", crate::agents::get_row(ctx, id).unwrap_or_default());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn memory_e2e_real_agent_models_recall_and_dream() {
    let Ok(auth) = std::env::var("PHILIUM_E2E_CODEX_AUTH") else {
        eprintln!("skipping: set PHILIUM_E2E_CODEX_AUTH");
        return;
    };
    assert!(crate::hal::server_path().exists(), "no HAL sidecar at {}", crate::hal::server_path().display());
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = Scratch(std::env::temp_dir().join(format!("philium-mem-e2e-{}", crate::hal::random_token())));
    let data = dir.0.join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::secrets::init(&data);

    // Real gateway, real on-disk DB in the temp dir.
    let gw = crate::gateway::Gateway::new(crate::hal::random_token(), None);
    let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_url = format!("http://{}", gl.local_addr().unwrap());
    let router = gw.router();
    tokio::spawn(async move { axum::serve(gl, router).await.unwrap() });
    let db = crate::db::Db::open(&data.join("philium.db")).unwrap();
    let ctx = Ctx::new(db, data.clone(), dir.0.join("root"), gw.clone(), gw_url);
    crate::library::init(&ctx).unwrap();
    crate::providers::init(&ctx).unwrap();
    crate::workspace::init(&ctx).unwrap();
    crate::agents::init(&ctx).unwrap();
    crate::codeindex::init(&ctx).unwrap();
    crate::dream::init(&ctx).unwrap();
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    ctx.mcp_url.set(format!("http://{}/mcp", ml.local_addr().unwrap())).unwrap();
    let mr = crate::mcp::router(ctx.clone());
    tokio::spawn(async move { axum::serve(ml, mr).await.unwrap() });

    // Real ChatGPT subscription credentials, stored the way sign-in stores them.
    let a: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    let t = &a["tokens"];
    let access = t["access_token"].as_str().unwrap().to_string();
    let creds = crate::oauth::Creds {
        expires_at: jwt_exp_ms(&access).unwrap(),
        access,
        refresh: t["refresh_token"].as_str().unwrap_or("").into(),
        account_id: t["account_id"].as_str().map(str::to_string),
        ..Default::default()
    };
    crate::secrets::set("oauth:chatgpt", &serde_json::to_string(&creds).unwrap()).unwrap();
    crate::providers::ensure_oauth_provider(&ctx, "chatgpt", Kind::ChatgptOauth).unwrap();
    crate::providers::sync_routes(&ctx);
    let models = crate::providers::refresh_models(&ctx, "chatgpt").await.expect("live model list");
    assert!(!models.is_empty());
    let (_, mid) = crate::providers::resolve(&ctx, Some("chatgpt")).unwrap();
    let model_ref = format!("chatgpt/{mid}");
    eprintln!("STEP provider: {model_ref} ({} models)", models.len());

    // Step 1: local models download into the empty data dir and load.
    assert!(!crate::local_models::installed(&data, &crate::local_models::EMBED));
    let st = crate::local_models::ensure(&ctx).await;
    for s in crate::local_models::SPECS {
        assert!(crate::local_models::installed(&data, s), "{} not installed: {st}", s.label);
    }
    let v = crate::local_models::embed(&data, &["hello".into()]).unwrap();
    assert!(!v[0].is_empty());
    let rr = crate::local_models::rerank(&data, "airplane", &["no network".into(), "tokens".into()]).unwrap();
    assert_eq!(rr.len(), 2);
    eprintln!("STEP models: downloaded and loaded ({} dims)", v[0].len());

    // Step 2: a real agent session.
    let repo = dir.0.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "app\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["add", "."], &["-c", "user.name=T", "-c", "user.email=t@example.invalid", "commit", "-qm", "init"]] {
        crate::workspace::git(&repo, args).unwrap();
    }
    ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at) VALUES('r1','app',?1,'main',0)", [repo.to_str().unwrap()]).unwrap();
    let id = crate::agents::spawn(&ctx, crate::agents::SpawnReq {
        name: Some("memtest".into()), role: None, model: Some(model_ref.clone()), repo_id: Some("r1".into()),
        task_id: None, brief: None, parent_id: None, strict: false, base: None,
    }).unwrap()["id"].as_str().unwrap().to_string();
    for _ in 0..240 {
        if crate::agents::runtime(&id).is_some() { break; }
        if let Some(e) = ctx.db.kv_get(&format!("agent_error.{id}")).filter(|e| !e.is_empty()) { panic!("agent failed: {e}"); }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    crate::agents::send(&ctx, &id, "Project rule, please remember it for future sessions: this repo deploys only with the `zephyr-ship` script, never with `make deploy`, because make deploy skips the database migration step. Just acknowledge in one sentence, do not run any tools.").await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    wait_idle(&ctx, &id, 240).await;

    // Step 3: observations are stored.
    let mut obs = Value::Null;
    for _ in 0..120 {
        obs = crate::library::observe_session(&ctx, &id).await.unwrap();
        let n = ctx.db.query("SELECT id FROM memories WHERE scope='repo:r1' AND status='active'", []).unwrap().len();
        if n > 0 { break; }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let mems = ctx.db.query("SELECT id,content FROM memories WHERE scope='repo:r1' AND status='active'", []).unwrap();
    eprintln!("STEP observe: {obs} -> {mems:?}");
    assert!(mems.iter().any(|m| m["content"].as_str().unwrap_or("").contains("zephyr")), "{mems:?}");
    let vecs = ctx.db.query("SELECT DISTINCT model_version AS v FROM memory_vectors", []).unwrap();
    assert_eq!(vecs, vec![json!({ "v": crate::local_models::EMBED.version })], "observed memory embedded locally");
    crate::agents::stop(&ctx, &id, "stopped");

    // Step 4: recall in a later session by meaning, not words.
    let hits = crate::library::agent_memories_async(&ctx, "how do I ship a release to production?", "r1", 5).await.unwrap();
    eprintln!("STEP recall: {hits}");
    assert!(hits.as_array().unwrap().first().is_some_and(|h| h["content"].as_str().unwrap_or("").contains("zephyr")), "{hits}");
    let id2 = crate::agents::spawn(&ctx, crate::agents::SpawnReq {
        name: Some("memlater".into()), role: None, model: Some(model_ref.clone()), repo_id: Some("r1".into()),
        task_id: None, brief: None, parent_id: None, strict: false, base: None,
    }).unwrap()["id"].as_str().unwrap().to_string();
    for _ in 0..240 {
        if crate::agents::runtime(&id2).is_some() { break; }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    crate::agents::send(&ctx, &id2, "How should I deploy this repo? Answer in one sentence from what you already know, do not run any tools.").await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    wait_idle(&ctx, &id2, 240).await;
    let sid2 = crate::agents::get_row(&ctx, &id2).unwrap()["hal_session_id"].as_str().unwrap().to_string();
    let conn = rusqlite::Connection::open(ctx.agent_dir("memlater").join("hal/hal/hal.db")).unwrap();
    let answer: String = conn.query_row("SELECT group_concat(payload,' ') FROM events WHERE session_id=?1 AND type='assistant.completed'", [&sid2], |r| r.get(0)).unwrap();
    eprintln!("STEP later session answer: {}", answer.chars().take(400).collect::<String>());
    assert!(answer.contains("zephyr-ship"), "later session did not recall: {answer}");
    crate::agents::stop(&ctx, &id2, "stopped");

    // Step 5: one real dream through the real provider, then undo.
    let before = ctx.db.query("SELECT id,content,status FROM memories ORDER BY id", []).unwrap();
    let out = crate::dream::run(&ctx, "r1", true).await.unwrap();
    eprintln!("STEP dream: {out}");
    let run = ctx.db.query("SELECT * FROM dream_runs", []).unwrap();
    eprintln!("STEP dream_runs: {run:?}");
    assert!(out["skipped"] != json!(true), "dream skipped: {out}");
    let run_id = out["run_id"].as_str().unwrap();
    let applied = ctx.db.query("SELECT id FROM dream_actions WHERE run_id=?1", [run_id]).unwrap().len();
    let undone = crate::dream::undo(&ctx, &json!({ "run_id": run_id })).unwrap();
    assert_eq!(undone["undone"].as_i64().unwrap() as usize, applied);
    let after = ctx.db.query("SELECT id,content,status FROM memories WHERE id IN (SELECT id FROM memories) ORDER BY id", []).unwrap();
    let after_orig: Vec<&Value> = after.iter().filter(|r| before.iter().any(|b| b["id"] == r["id"])).collect();
    assert_eq!(after_orig.len(), before.len());
    for (b, a) in before.iter().zip(after_orig) { assert_eq!(b, a, "undo restores state"); }
    eprintln!("STEP undo: {applied} actions applied and undone");
}
