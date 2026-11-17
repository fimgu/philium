use super::*;
use rusqlite::params;

struct Fixture {
    ctx: Arc<Ctx>,
}
impl Fixture {
    fn new() -> Self {
        Self { ctx: Ctx::test() }
    }
    fn agent(&self, id: &str, name: &str, session: Option<&str>) {
        self.ctx.db.exec("INSERT INTO agents(id,name,model_ref,state,hal_session_id,created_at,last_active_at) VALUES(?1,?2,'fixture/model','stopped',?3,0,0)", params![id,name,session]).unwrap();
    }
    fn path(&self, name: &str) -> PathBuf {
        self.ctx.agent_dir(name).join("hal/hal/hal.db")
    }
    fn db(&self, name: &str) -> Connection {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY); CREATE TABLE events(session_id TEXT,seq INTEGER,ts TEXT,type TEXT,payload TEXT,PRIMARY KEY(session_id,seq)); INSERT INTO sessions VALUES('session');").unwrap();
        db
    }
    async fn get(&self, id: &str, after: i64, limit: i64) -> Result<Value, String> {
        super::super::call(
            &self.ctx,
            "events",
            json!({"id":id,"after":after,"limit":limit}),
        )
        .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.ctx.data_dir.parent().unwrap());
    }
}
fn event(seq: i64, session: &str) -> Value {
    json!({"seq":seq,"session_id":session,"ts":"2026-11-16T00:00:00Z","type":"session.created","workspace":"/fixture"})
}

#[tokio::test]
async fn empty_errors_and_readonly_pagination_are_distinct() {
    let f = Fixture::new();
    f.agent("empty", "empty", None);
    assert_eq!(f.get("empty", 0, 10).await.unwrap(), json!([]));
    assert!(!f.ctx.agent_dir("empty").exists());
    assert!(f
        .get("unknown", 0, 10)
        .await
        .unwrap_err()
        .contains("no agent"));
    f.agent("saved", "saved", Some("session"));
    assert!(f
        .get("saved", 0, 10)
        .await
        .unwrap_err()
        .contains("cannot open"));
    assert!(!f.path("saved").exists());
    let db = f.db("saved");
    for seq in 1..=4 {
        db.execute(
            "INSERT INTO events VALUES('session',?1,'ts','session.created',?2)",
            params![seq, event(seq, "session").to_string()],
        )
        .unwrap();
    }
    db.execute("INSERT INTO sessions VALUES('peer')", [])
        .unwrap();
    db.execute(
        "INSERT INTO events VALUES('peer',1,'ts','session.created',?1)",
        [event(1, "peer").to_string()],
    )
    .unwrap();
    drop(db);
    let before = std::fs::read(f.path("saved")).unwrap();
    assert_eq!(
        f.get("saved", 1, 2).await.unwrap(),
        json!([event(2, "session"), event(3, "session")])
    );
    assert_eq!(
        f.get("saved", -1, 0).await.unwrap(),
        json!([event(1, "session")])
    );
    assert_eq!(std::fs::read(f.path("saved")).unwrap(), before);
    assert!(!f.path("saved").with_extension("db-wal").exists());
    assert!(runtime("saved").is_none());
    f.agent("other", "other", Some("session"));
    let peer = f.db("other");
    peer.execute(
        "INSERT INTO events VALUES('session',1,'ts','session.created',?1)",
        [event(1, "session").to_string()],
    )
    .unwrap();
    assert_eq!(
        f.get("other", 0, 10)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let db = Connection::open(f.path("saved")).unwrap();
    db.execute("DELETE FROM sessions WHERE id='session'", [])
        .unwrap();
    assert!(f
        .get("saved", 0, 10)
        .await
        .unwrap_err()
        .contains("session is missing"));
    drop(db);
    std::fs::write(f.path("saved"), b"not sqlite").unwrap();
    assert!(f
        .get("saved", 0, 10)
        .await
        .unwrap_err()
        .contains("Saved conversation unavailable"));
}

#[tokio::test]
async fn busy_corrupt_payload_and_size_limits() {
    let f = Fixture::new();
    f.agent("saved", "saved", Some("session"));
    let db = f.db("saved");
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let start = std::time::Instant::now();
    assert!(f.get("saved", 0, 10).await.is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    db.execute_batch("ROLLBACK").unwrap();
    db.execute(
        "INSERT INTO events VALUES('session',1,'ts','bad','not json')",
        [],
    )
    .unwrap();
    let error = f.get("saved", 0, 10).await.unwrap_err();
    assert!(error.contains("sequence 1"));
    assert!(error.contains("Retry"));
    assert!(!error.contains("not json"));
    db.execute("DELETE FROM events WHERE seq=1", []).unwrap();
    db.execute(
        "INSERT INTO events VALUES('session',2,'ts','large',?1)",
        ["x".repeat(MAX_EVENT_BYTES + 1)],
    )
    .unwrap();
    assert!(f
        .get("saved", 0, 10)
        .await
        .unwrap_err()
        .contains("safety limit"));
    assert!(runtime("saved").is_none());
}

#[test]
fn legacy_child_envelope_and_parent_isolation() {
    let mut value = event(1, "child");
    value["type"] = json!("subagent.started");
    let restored = decode(&value.to_string(), "parent", 1).unwrap();
    assert_eq!(restored["session_id"], "parent");
    assert_eq!(restored["child_session_id"], "child");
    value["child_session_id"] = json!("leaf");
    assert!(decode(&value.to_string(), "parent", 1).is_none());
    assert!(decode(&event(1, "peer").to_string(), "parent", 1).is_none());
}

#[tokio::test]
async fn malformed_saved_events_error_without_rejecting_forward_types() {
    let f = Fixture::new();
    f.agent("saved", "saved", Some("session"));
    let db = f.db("saved");
    let mut future = event(1, "session");
    future["type"] = json!("future.valid.event");
    future["data"] = json!({"custom":true});
    db.execute(
        "INSERT INTO events VALUES('session',1,'ts','future.valid.event',?1)",
        [future.to_string()],
    )
    .unwrap();
    assert_eq!(f.get("saved", 0, 10).await.unwrap(), json!([future]));
    for payload in [
        "{\"private-body\":",
        "[]",
        "{}",
        "null",
        &event(2, "wrong-parent").to_string(),
        &event(3, "session").to_string(),
    ] {
        db.execute(
            "INSERT OR REPLACE INTO events VALUES('session',2,'ts','test',?1)",
            [payload],
        )
        .unwrap();
        let error = f.get("saved", 0, 10).await.unwrap_err();
        assert!(error.contains("sequence 2"));
        assert!(!error.contains("private-body"));
        assert!(!error.contains("wrong-parent"));
    }
}

#[tokio::test]
async fn wal_reads_preserve_existing_storage_and_missing_sidecars_are_errors() {
    let f = Fixture::new();
    f.agent("saved", "saved", Some("session"));
    let db = f.db("saved");
    db.pragma_update(None, "journal_mode", "wal").unwrap();
    db.execute(
        "INSERT INTO events VALUES('session',1,'ts','session.created',?1)",
        [event(1, "session").to_string()],
    )
    .unwrap();
    let parent = f.path("saved").parent().unwrap().to_path_buf();
    let snapshot = || {
        let mut names = std::fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let names = snapshot();
    assert_eq!(
        f.get("saved", 0, 10).await.unwrap(),
        json!([event(1, "session")])
    );
    assert_eq!(snapshot(), names);
    drop(db);
    std::fs::write(parent.join("hal.db-wal"), b"fixture orphan WAL").unwrap();
    assert!(f
        .get("saved", 0, 10)
        .await
        .unwrap_err()
        .contains("refusing to create"));
    assert!(!parent.join("hal.db-shm").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn history_cannot_follow_another_agents_database_symlink() {
    let f = Fixture::new();
    f.agent("saved", "saved", Some("session"));
    f.agent("other", "other", Some("session"));
    drop(f.db("other"));
    std::fs::create_dir_all(f.path("saved").parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(f.path("other"), f.path("saved")).unwrap();
    assert!(f.get("saved", 0, 10).await.unwrap_err().contains("symlink"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_hal_persists_events_then_public_rpc_reads_without_resurrection() {
    let f = Fixture::new();
    f.agent("saved", "saved", None);
    assert!(
        hal::server_path().is_file(),
        "bundled HAL binary required for this regression"
    );
    let dir = f.ctx.agent_dir("saved").join("hal");
    let data = dir.join("hal");
    let config = json!({"server":{"data_dir":data},"default_model":"fixture/model","models":{"fixture/model":{"provider":"openai","model":"fixture","base_url":"http://127.0.0.1:9","api_key":"fixture"}}});
    let hal = Hal::start(hal::Launch {
        dir: &dir,
        config,
        cwd: None,
        env: vec![],
        allow_logins: false,
        wrap: None,
    })
    .unwrap();
    let session = hal
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"title":"isolated history fixture"})),
        )
        .await
        .unwrap();
    let sid = session["id"].as_str().unwrap();
    f.ctx
        .db
        .exec(
            "UPDATE agents SET hal_session_id=?1 WHERE id='saved'",
            [sid],
        )
        .unwrap();
    let peer = hal
        .call(
            reqwest::Method::POST,
            "/sessions",
            Some(json!({"title":"isolated peer fixture"})),
        )
        .await
        .unwrap();
    let live = hal
        .call(
            reqwest::Method::GET,
            &format!("/sessions/{sid}/events?after=0&limit=100"),
            None,
        )
        .await
        .unwrap();
    assert!(!live.as_array().unwrap().is_empty());
    assert_eq!(f.get("saved", 0, 100).await.unwrap(), live);
    let pid = hal.pid;
    hal.stop();
    assert!(!hal.alive());
    let files = || {
        let mut entries = std::fs::read_dir(&data)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let before = files();
    assert_eq!(f.get("saved", 0, 100).await.unwrap(), live);
    assert_eq!(
        files(),
        before,
        "reading stopped WAL-mode history must not create sidecars"
    );
    assert_eq!(
        f.get("saved", 0, 1).await.unwrap(),
        json!([live[0].clone()])
    );
    let last = live.as_array().unwrap().last().unwrap()["seq"]
        .as_i64()
        .unwrap();
    assert_eq!(f.get("saved", last, 100).await.unwrap(), json!([]));
    assert!(live
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["session_id"] != peer["id"]));
    assert!(runtime("saved").is_none());
    assert_eq!(
        f.ctx
            .db
            .query("SELECT state FROM agents WHERE id='saved'", [])
            .unwrap()[0]["state"],
        "stopped"
    );
    assert!(f
        .ctx
        .db
        .query("SELECT * FROM resources WHERE agent_id='saved'", [])
        .unwrap()
        .is_empty());
    #[cfg(unix)]
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
}

#[test]
fn sent_attachments_are_scoped_to_own_user_messages_and_read_only() {
    let f = Fixture::new();
    f.agent("owner", "owner", Some("session"));
    let db = f.db("owner");
    db.execute_batch(
        "CREATE TABLE messages(id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, content TEXT, created_at TEXT, compacted INTEGER DEFAULT 0);
         CREATE TABLE blobs(session_id TEXT, sha256 TEXT, media_type TEXT, data BLOB, PRIMARY KEY(session_id, sha256));
         INSERT INTO sessions VALUES('peer');",
    )
    .unwrap();
    let sent = "1".repeat(64);
    let unreferenced = "2".repeat(64);
    let assistant_only = "3".repeat(64);
    let svg = "4".repeat(64);
    let peer = "5".repeat(64);
    let png = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    for (session, sha, media) in [("session", &sent, "image/png"), ("session", &unreferenced, "image/png"), ("session", &assistant_only, "image/png"), ("session", &svg, "image/svg+xml"), ("peer", &peer, "image/png")] {
        db.execute("INSERT INTO blobs VALUES(?1,?2,?3,?4)", params![session, sha, media, png]).unwrap();
    }
    let msg = |sha: &str| json!({"role":"user","content":"x","images":[{"sha256":sha,"media_type":"image/png","name":"a.png"}]}).to_string();
    db.execute("INSERT INTO messages(session_id,role,content,created_at) VALUES('session','user',?1,'t')", [msg(&sent)]).unwrap();
    db.execute("INSERT INTO messages(session_id,role,content,created_at) VALUES('session','user',?1,'t')", [msg(&svg)]).unwrap();
    db.execute("INSERT INTO messages(session_id,role,content,created_at) VALUES('session','assistant',?1,'t')", [msg(&assistant_only)]).unwrap();
    db.execute("INSERT INTO messages(session_id,role,content,created_at) VALUES('peer','user',?1,'t')", [msg(&peer)]).unwrap();
    drop(db);
    let before = std::fs::read(f.path("owner")).unwrap();
    let (media, bytes) = attachment(&f.ctx, "owner", &sent).unwrap();
    assert_eq!((media.as_str(), bytes), ("image/png", png));
    for sha in [&unreferenced, &assistant_only, &peer] {
        assert!(attachment(&f.ctx, "owner", sha).unwrap_err().contains("not found"), "{sha}");
    }
    assert!(attachment(&f.ctx, "owner", &svg).unwrap_err().contains("cannot be previewed"));
    assert!(attachment(&f.ctx, "owner", "../hal.db").is_err());
    assert!(attachment(&f.ctx, "owner", &"A".repeat(64)).unwrap_err().contains("invalid"));
    assert!(attachment(&f.ctx, "missing", &sent).is_err());
    assert_eq!(std::fs::read(f.path("owner")).unwrap(), before);
    assert!(!f.path("owner").with_extension("db-wal").exists());
}
