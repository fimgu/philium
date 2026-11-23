use super::*;
use axum::{routing::post, Router};
use std::sync::Mutex as StdMutex;

type Seen = Arc<StdMutex<Vec<String>>>;

/// Real gateway in front of a fake Claude upstream whose reply is swapped per test step.
pub(crate) struct Rig {
    pub(crate) ctx: Arc<Ctx>,
    pub(crate) seen: Seen,
    pub(crate) reply: Arc<StdMutex<String>>,
}

pub(crate) fn hal_db(ctx: &Ctx, name: &str, sid: &str, msgs: &[(&str, &str)]) {
    let db_path = ctx.agent_dir(name).join("hal/hal/hal.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS events(session_id TEXT,seq INTEGER,ts TEXT,type TEXT,payload TEXT,PRIMARY KEY(session_id,seq));").unwrap();
    conn.execute("INSERT OR IGNORE INTO sessions VALUES(?1)", [sid]).unwrap();
    for (i, (role, text)) in msgs.iter().enumerate() {
        let seq = i as i64 + 1;
        let ev = if *role == "user" {
            json!({"seq":seq,"session_id":sid,"ts":"t","type":"message.created","role":"user","content":text})
        } else {
            json!({"seq":seq,"session_id":sid,"ts":"t","type":"assistant.completed","text":text})
        };
        conn.execute("INSERT INTO events VALUES(?1,?2,'t',?3,?4)", params![sid, seq, ev["type"].as_str().unwrap(), ev.to_string()]).unwrap();
    }
}

pub(crate) async fn rig(name: &str) -> Rig {
    let seen: Seen = Default::default();
    let reply: Arc<StdMutex<String>> = Arc::new(StdMutex::new("{\"actions\":[]}".into()));
    let (s, r) = (seen.clone(), reply.clone());
    let app = Router::new().route(
        "/{*rest}",
        post(move |headers: axum::http::HeaderMap, body: String| {
            let (s, r) = (s.clone(), r.clone());
            async move {
                assert_eq!(headers.get("authorization").and_then(|v| v.to_str().ok()), Some("Bearer SUB-ACCESS"), "gateway attaches OAuth");
                s.lock().unwrap().push(body);
                let text = r.lock().unwrap().clone();
                ([("content-type", "application/json")], json!({"type":"message","role":"assistant","content":[{"type":"text","text":text}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}).to_string())
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let gw = crate::gateway::Gateway::new("g".repeat(48), None);
    gw.tokens.set(Arc::new(|_, _| Box::pin(async { Ok(crate::oauth::Creds { access: "SUB-ACCESS".into(), ..Default::default() }) })));
    let gl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_url = format!("http://{}", gl.local_addr().unwrap());
    let router = gw.router();
    tokio::spawn(async move { axum::serve(gl, router).await.unwrap() });
    let dir = crate::ctx::test_dir(name);
    let ctx = Ctx::new(crate::db::Db::memory(), dir.join("data"), dir.join("root"), gw.clone(), gw_url);
    ctx.db.kv_set("library.imported_at", "fixture").unwrap();
    crate::library::init(&ctx).unwrap();
    crate::local_models::set_enabled(&ctx, false).unwrap();
    let p = crate::providers::Provider {
        id: "claude".into(), name: "claude".into(), kind: crate::providers::Kind::ClaudeOauth, base_url: up,
        trusted: true, guard: crate::guard::GuardMode::Redact, enabled: true, default_model: None,
    };
    crate::providers::upsert(&ctx, &p).unwrap();
    gw.set_routes(crate::providers::all(&ctx).into_iter().map(|p| crate::gateway::Route { provider: p, api_key: None }).collect());
    ctx.db.exec("INSERT INTO models(provider_id,model_id,enabled,discovered_at) VALUES ('claude','claude-opus-5-5',1,0),('claude','claude-haiku-5-5',1,0)", []).unwrap();
    ctx.db.exec("INSERT INTO repos(id,name,path,added_at) VALUES('ra','alpha','/tmp/alpha',0),('rb','alpha2','/tmp/alpha2',0)", []).unwrap();
    ctx.db.exec(
        "INSERT INTO agents(id,name,repo_id,model_ref,strict,hal_session_id,created_at,last_active_at) VALUES
         ('a1','otter','ra','claude/claude-opus-5-5',0,'sess_a1',10,10),
         ('a2','heron','ra','claude/claude-opus-5-5',0,'sess_a2',10,10),
         ('b1','fox','rb','claude/claude-opus-5-5',0,'sess_b1',10,10),
         ('s1','stricta','ra','claude/claude-opus-5-5',1,'sess_s1',10,10)",
        [],
    )
    .unwrap();
    hal_db(&ctx, "otter", "sess_a1", &[("user", "We switched from npm to pnpm last week, the lockfile is pnpm-lock.yaml now."), ("assistant", "Understood, using pnpm install from now on.")]);
    hal_db(&ctx, "heron", "sess_a2", &[("user", "Remember the dev proxy runs on port 5174 and CI uses pnpm too."), ("assistant", "Noted the proxy port.")]);
    hal_db(&ctx, "fox", "sess_b1", &[("user", "SIBLING_TRANSCRIPT_SENTINEL use yarn here")]);
    hal_db(&ctx, "stricta", "sess_s1", &[("user", "STRICT_TRANSCRIPT_SENTINEL secret plan")]);
    Rig { ctx, seen, reply }
}

pub(crate) fn save(ctx: &Ctx, scope: &str, content: &str, tier: &str, cat: &str, fk: Option<&str>) -> String {
    crate::library::save_structured(ctx, crate::library::SaveOpts {
        content, category: cat, scope, source: "agent:x", trust_tier: tier, fact_key: fk, supersedes_id: None, source_event_id: None, provenance: None,
    })
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn row(ctx: &Ctx, id: &str) -> Value {
    ctx.db.query("SELECT * FROM memories WHERE id=?1", [id]).unwrap().remove(0)
}

pub(crate) fn set_reply(r: &Rig, actions: Value) {
    *r.reply.lock().unwrap() = json!({ "actions": actions }).to_string();
}

#[tokio::test]
async fn corrects_wrong_memory_by_supersession_and_undo_restores_exact_state() {
    let r = rig("dream-correct").await;
    let wrong = save(&r.ctx, "repo:ra", "The repo uses npm for installs.", "agent_inferred", "decision", Some("tooling.package_manager"));
    let before = row(&r.ctx, &wrong);
    set_reply(&r, json!([{ "type": "correct", "id": wrong, "new_content": "The repo uses pnpm; the lockfile is pnpm-lock.yaml.", "evidence": ["We switched from npm to pnpm last week"], "reason": "user said so" }]));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    assert_eq!(out["applied"].as_array().unwrap().len(), 1, "{out}");
    let old = row(&r.ctx, &wrong);
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["content"], "The repo uses npm for installs.", "old row kept as history");
    let new = r.ctx.db.query("SELECT * FROM memories WHERE supersedes_id=?1", [&wrong]).unwrap();
    assert_eq!(new[0]["status"], "active");
    assert_eq!(new[0]["fact_key"], "tooling.package_manager");
    assert!(new[0]["content"].as_str().unwrap().contains("pnpm"));
    // Model sees the gateway-routed request with the repo's normal agent model.
    let body: Value = serde_json::from_str(&r.seen.lock().unwrap()[0]).unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");

    let st = status(&r.ctx, "ra").unwrap();
    assert!(st["last_dreamed_at"].is_string());
    assert_eq!(st["actions"].as_array().unwrap().len(), 1);
    let runs = r.ctx.db.query("SELECT * FROM dream_runs WHERE repo_id='ra'", []).unwrap();
    assert!(runs[0]["tokens_in"].as_i64().unwrap() > 0 && runs[0]["model"] == "claude/claude-opus-5-5");

    let run_id = out["run_id"].as_str().unwrap();
    assert_eq!(undo(&r.ctx, &json!({ "run_id": run_id })).unwrap()["undone"], 1);
    assert_eq!(row(&r.ctx, &wrong), before, "undo restores the exact previous row");
    assert_eq!(r.ctx.db.query("SELECT status FROM memories WHERE supersedes_id=?1", [&wrong]).unwrap()[0]["status"], "deleted");
    assert_eq!(undo(&r.ctx, &json!({ "run_id": run_id })).unwrap()["undone"], 0, "undo is idempotent");
}

#[tokio::test]
async fn merges_duplicates_and_links_them() {
    let r = rig("dream-merge").await;
    let a = save(&r.ctx, "repo:ra", "Dev proxy listens on 5174.", "agent_inferred", "fact", None);
    let b = save(&r.ctx, "repo:ra", "The development proxy port is 5174.", "agent_inferred", "fact", None);
    set_reply(&r, json!([{ "type": "merge", "ids": [a, b], "merged": { "title": "Dev proxy port", "content": "The dev proxy runs on port 5174.", "category": "fact" }, "reason": "duplicates" }]));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    assert_eq!(out["applied"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(row(&r.ctx, &a)["status"], "superseded");
    assert_eq!(row(&r.ctx, &b)["status"], "superseded");
    let merged = r.ctx.db.query("SELECT * FROM memories WHERE source='pipeline:dream' AND status='active'", []).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0]["supersedes_id"], a);
    assert!(merged[0]["provenance"].as_str().unwrap().contains(&b), "both linked");
}

#[tokio::test]
async fn user_direct_only_gets_a_proposed_correction() {
    let r = rig("dream-user").await;
    let u = save(&r.ctx, "repo:ra", "Use npm.", "user_direct", "preference", Some("tooling.package_manager"));
    let proc_id = save(&r.ctx, "repo:ra", "Run cargo test from src-tauri.", "verified_run", "procedure", None);
    let before = row(&r.ctx, &u);
    let proc_before = row(&r.ctx, &proc_id);
    set_reply(&r, json!([
        { "type": "correct", "id": u, "new_content": "Use pnpm.", "evidence": ["We switched from npm to pnpm last week"], "reason": "x" },
        { "type": "retire", "id": u, "reason": "stale" },
        { "type": "demote", "id": u, "reason": "noise" },
        { "type": "retire", "id": proc_id, "reason": "stale" }
    ]));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    assert_eq!(row(&r.ctx, &u), before, "user_direct row unchanged");
    assert_eq!(row(&r.ctx, &proc_id), proc_before, "verified procedure never auto-retired");
    let prop = r.ctx.db.query("SELECT * FROM memories WHERE supersedes_id=?1", [&u]).unwrap();
    assert_eq!(prop.len(), 1);
    assert_eq!(prop[0]["status"], "proposed");
    let rej: Vec<String> = out["rejected"].as_array().unwrap().iter().map(|x| x["reason"].as_str().unwrap().to_string()).collect();
    assert!(rej.iter().any(|s| s.contains("already changed") || s.contains("user_direct")), "{rej:?}");
    assert!(out["applied"].as_array().unwrap().iter().any(|a| a["type"] == "flag"), "{out}");
}

#[tokio::test]
async fn repo_isolation_and_strict_transcripts_never_reach_the_prompt() {
    let r = rig("dream-iso").await;
    let mine = save(&r.ctx, "repo:ra", "Alpha uses pnpm workspaces.", "agent_inferred", "fact", None);
    let sib = save(&r.ctx, "repo:rb", "SIBLING_MEMORY_SENTINEL uses yarn.", "agent_inferred", "fact", None);
    let glob = save(&r.ctx, "global", "GLOBAL_MEMORY_SENTINEL prefers tabs.", "agent_inferred", "preference", None);
    let sib_before = row(&r.ctx, &sib);
    let glob_before = row(&r.ctx, &glob);
    set_reply(&r, json!([
        { "type": "retire", "id": sib, "reason": "foreign" },
        { "type": "promote", "id": glob, "reason": "global" },
        { "type": "confirm", "id": mine, "evidence": ["SIBLING_TRANSCRIPT_SENTINEL use yarn here"], "reason": "x" },
        { "type": "confirm", "id": mine, "evidence": ["STRICT_TRANSCRIPT_SENTINEL secret plan"], "reason": "x" }
    ]));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    let body = r.seen.lock().unwrap().join("\n");
    for s in ["SIBLING_TRANSCRIPT_SENTINEL", "STRICT_TRANSCRIPT_SENTINEL", "SIBLING_MEMORY_SENTINEL", "GLOBAL_MEMORY_SENTINEL"] {
        assert!(!body.contains(s), "{s} leaked into the dream prompt");
    }
    assert!(body.contains("pnpm workspaces") && body.contains("pnpm-lock.yaml"));
    assert_eq!(row(&r.ctx, &sib), sib_before);
    assert_eq!(row(&r.ctx, &glob), glob_before);
    assert_eq!(out["applied"].as_array().unwrap().len(), 0, "{out}");
    let rej = out["rejected"].as_array().unwrap();
    assert_eq!(rej.len(), 4);
    assert!(rej[0]["reason"].as_str().unwrap().contains("outside this repo"));
    assert!(rej[2]["reason"].as_str().unwrap().contains("not found"));
    // Rejections are logged on the run.
    let logged: Value = serde_json::from_str(r.ctx.db.query("SELECT rejected FROM dream_runs", []).unwrap()[0]["rejected"].as_str().unwrap()).unwrap();
    assert_eq!(logged.as_array().unwrap().len(), 4);
    // Repo B's dream never sees A.
    r.seen.lock().unwrap().clear();
    set_reply(&r, json!([{ "type": "retire", "id": mine, "reason": "foreign" }]));
    let out_b = run(&r.ctx, "rb", true).await.unwrap();
    let body_b = r.seen.lock().unwrap().join("\n");
    assert!(!body_b.contains("pnpm") && body_b.contains("SIBLING_MEMORY_SENTINEL"));
    assert_eq!(row(&r.ctx, &mine)["status"], "active");
    assert_eq!(out_b["rejected"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_actions_are_rejected_with_reasons() {
    let r = rig("dream-invalid").await;
    let m = save(&r.ctx, "repo:ra", "Alpha uses pnpm.", "agent_inferred", "fact", None);
    set_reply(&r, json!([
        { "type": "confirm", "id": "mem_does_not_exist", "evidence": ["We switched from npm to pnpm"] },
        { "type": "correct", "id": m, "new_content": "Alpha uses bun.", "evidence": ["the user clearly asked for bun everywhere"], "reason": "made up" },
        { "type": "new", "content": "Single session pattern.", "category": "fact", "evidence": ["We switched from npm to pnpm"], "reason": "x" },
        { "type": "new", "content": "x", "category": "nonsense", "evidence": ["We switched from npm to pnpm", "dev proxy runs on port 5174"] },
        { "type": "explode", "id": m },
        { "type": "new", "title": "pnpm everywhere", "content": "Both local installs and CI use pnpm.", "category": "decision", "evidence": ["We switched from npm to pnpm", "CI uses pnpm too"], "reason": "two sessions" }
    ]));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    let rej: Vec<String> = out["rejected"].as_array().unwrap().iter().map(|x| x["reason"].as_str().unwrap().to_string()).collect();
    assert_eq!(rej.len(), 5, "{rej:?}");
    assert!(rej[0].contains("unknown id"));
    assert!(rej[1].contains("not found in transcripts"));
    assert!(rej[2].contains("two sessions"));
    assert!(rej[3].contains("bad category"));
    assert!(rej[4].contains("unknown action"));
    assert_eq!(row(&r.ctx, &m)["content"], "Alpha uses pnpm.");
    assert_eq!(out["applied"].as_array().unwrap().len(), 1, "cross-session new accepted");
}

#[tokio::test]
async fn budget_cap_toggle_and_scheduler_gates() {
    let r = rig("dream-budget").await;
    save(&r.ctx, "repo:ra", "Alpha uses pnpm.", "agent_inferred", "fact", None);
    r.ctx.db.kv_set("setting.dream.enabled", "off").unwrap();
    assert_eq!(run(&r.ctx, "ra", true).await.unwrap()["reason"], "dreaming is off");
    assert!(tick(&r.ctx).await.is_empty());
    set_config(&r.ctx, &json!({ "enabled": true, "daily_tokens": 1000 })).unwrap();
    assert_eq!(run(&r.ctx, "ra", true).await.unwrap()["reason"], "daily token budget used");
    assert!(r.seen.lock().unwrap().is_empty(), "no model call over budget");
    set_config(&r.ctx, &json!({ "daily_tokens": 50000 })).unwrap();

    let t = now();
    assert_eq!(ineligible_reason(&r.ctx, "ra", t).as_deref(), Some("not enough new transcript"));
    note_observed(&r.ctx, "ra", 25);
    assert_eq!(ineligible_reason(&r.ctx, "ra", t), None, "agents idle since epoch");
    r.ctx.db.exec("UPDATE agents SET state='running' WHERE id='a1'", []).unwrap();
    assert_eq!(ineligible_reason(&r.ctx, "ra", t).as_deref(), Some("an agent is running"));
    r.ctx.db.exec("UPDATE agents SET state='idle', last_active_at=?1 WHERE id='a1'", [t - 60_000]).unwrap();
    assert_eq!(ineligible_reason(&r.ctx, "ra", t).as_deref(), Some("repo was active recently"));
    r.ctx.db.exec("UPDATE agents SET last_active_at=10", []).unwrap();
    run(&r.ctx, "ra", false).await.unwrap();
    assert_eq!(ineligible_reason(&r.ctx, "ra", now()).as_deref(), Some("dreamed in the last 6h"));
    // Busy set: a second concurrent dream for the same repo is refused.
    busy().lock().unwrap().insert(key(&r.ctx, "rb"));
    assert_eq!(run(&r.ctx, "rb", true).await.unwrap()["reason"], "already dreaming");
    busy().lock().unwrap().remove(&key(&r.ctx, "rb"));
}

#[tokio::test]
async fn cancelled_when_an_agent_starts_and_nothing_is_written() {
    let r = rig("dream-cancel").await;
    let m = save(&r.ctx, "repo:ra", "Alpha uses npm.", "agent_inferred", "fact", None);
    set_reply(&r, json!([{ "type": "retire", "id": m, "reason": "stale" }]));
    let flag = Arc::new(AtomicBool::new(true));
    let res = apply(&r.ctx, "ra", "dream_x", &[json!({ "type": "retire", "id": m, "reason": "stale" })], &[], &[], &flag).unwrap();
    assert_eq!(res["cancelled"], true);
    assert_eq!(row(&r.ctx, &m)["status"], "active", "rolled back");
    // The real hook: an agent in this repo going to running flips the dream's flag.
    let f = Arc::new(AtomicBool::new(false));
    cancel_flags().lock().unwrap().insert(key(&r.ctx, "ra"), f.clone());
    crate::agents::next_state("idle", "turn.started").unwrap();
    on_agent_state(&r.ctx, "a1", "running");
    assert!(f.load(Ordering::SeqCst));
    cancel_flags().lock().unwrap().remove(&key(&r.ctx, "ra"));
}

#[tokio::test]
async fn index_stays_under_budget_after_promote_and_demote() {
    let r = rig("dream-index").await;
    let mut ids = vec![];
    for i in 0..30 {
        ids.push(save(&r.ctx, "repo:ra", &format!("Index candidate memory number {i}."), "agent_inferred", "fact", None));
    }
    let mut acts: Vec<Value> = ids.iter().map(|id| json!({ "type": "promote", "id": id, "reason": "core" })).collect();
    set_reply(&r, json!(acts.clone()));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    assert_eq!(out["applied"].as_array().unwrap().len(), 24, "MAX_ACTIONS caps one run");
    let pinned = |c: &Ctx| c.db.query("SELECT COUNT(*) AS n FROM memories WHERE index_rank>0 AND active=1", []).unwrap()[0]["n"].as_i64().unwrap();
    assert_eq!(pinned(&r.ctx), 24);
    acts = ids[24..].iter().map(|id| json!({ "type": "promote", "id": id, "reason": "core" })).collect();
    acts.push(json!({ "type": "demote", "id": ids[0], "reason": "rarely useful" }));
    set_reply(&r, json!(acts));
    let out = run(&r.ctx, "ra", true).await.unwrap();
    assert!(pinned(&r.ctx) <= INDEX_BUDGET as i64, "{out}");
    assert!(out["rejected"].as_array().unwrap().iter().any(|x| x["reason"].as_str().unwrap().contains("index budget")));
    let idx = crate::library::memory_index_prompt(&r.ctx, "ra", &[], INDEX_BUDGET, 4000).unwrap();
    assert!(idx.lines().filter(|l| l.starts_with("- ")).count() <= INDEX_BUDGET);
    assert!(!idx.contains(&ids[0]), "demoted memory leaves the index");
}
