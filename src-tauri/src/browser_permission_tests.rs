//! Representative integration regression: real bundled HAL, localhost model/MCP.
//! This is not live desktop/browser acceptance and requires no real credentials.
use super::*;
use crate::gateway::Route;
use crate::guard::GuardMode;
use crate::providers::{Kind, Provider};
use std::sync::Mutex;
use std::time::Duration;

async fn serve(app: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, task)
}

async fn events(h: &Hal, sid: &str) -> Vec<Value> {
    h.call(
        reqwest::Method::GET,
        &format!("/sessions/{sid}/events?after=0&limit=1000"),
        None,
    )
    .await
    .unwrap()
    .as_array()
    .unwrap()
    .clone()
}

async fn pending(
    h: &Hal,
    sid: &str,
    resource: &str,
    dispatched: &Arc<Mutex<Vec<String>>>,
    count: usize,
) -> String {
    for _ in 0..150 {
        let p = h
            .call(
                reqwest::Method::GET,
                &format!("/sessions/{sid}/permissions"),
                None,
            )
            .await
            .unwrap();
        let p = p.as_array().expect("pending permissions array");
        if let Some(request) = p.first() {
            assert_eq!(p.len(), 1, "{p:?}");
            assert_eq!(request["action"], "mcp");
            assert_eq!(request["resource"], resource);
            // Give any accidental asynchronous dispatch a chance to become visible.
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(
                dispatched.lk().len(),
                count,
                "browser dispatched before consent"
            );
            return request["request_id"].as_str().unwrap().to_string();
        }
        let ev = events(h, sid).await;
        assert!(!ev.iter().any(|e| e["type"] == "turn.failed"), "{ev:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("permission never appeared: {:?}", events(h, sid).await);
}

async fn finish(h: &Hal, sid: &str, completed_before: usize) {
    for _ in 0..150 {
        let ev = events(h, sid).await;
        assert!(!ev.iter().any(|e| e["type"] == "turn.failed"), "{ev:?}");
        if ev
            .iter()
            .filter(|e| e["type"] == "assistant.completed")
            .count()
            > completed_before
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("turn never completed: {:?}", events(h, sid).await);
}

async fn send_browser(h: &Hal, sid: &str, tool: &str) -> usize {
    let before = events(h, sid)
        .await
        .iter()
        .filter(|e| e["type"] == "assistant.completed")
        .count();
    h.call(
        reqwest::Method::POST,
        &format!("/sessions/{sid}/messages"),
        Some(agents::message_body(tool, json!([]))),
    )
    .await
    .unwrap();
    before
}

async fn decide(h: &Hal, sid: &str, rid: &str, scope: Option<&str>) {
    let action = if scope.is_some() { "approve" } else { "deny" };
    h.call(
        reqwest::Method::POST,
        &format!("/sessions/{sid}/permissions/{rid}/{action}"),
        scope.map(|scope| json!({"scope": scope})),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_mcp_requires_consent_and_scopes_grants() {
    assert!(
        hal::server_path().is_file(),
        "required bundled HAL missing: {}",
        hal::server_path().display()
    );
    let dir = std::env::temp_dir().join(format!(
        "philium-browser-permissions-{}",
        hal::random_token()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // A guard ensures the child is stopped and private state removed on assertion failures too.
    struct Fixture {
        hal: Option<Hal>,
        dir: std::path::PathBuf,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(h) = &self.hal {
                h.stop();
            }
            for task in &self.tasks {
                task.abort();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    let mut fixture = Fixture {
        hal: None,
        dir: dir.clone(),
        tasks: vec![],
    };
    let dispatched: Arc<Mutex<Vec<String>>> = Default::default();
    let calls = dispatched.clone();
    let mcp = axum::Router::new().route("/mcp", axum::routing::post(move |axum::Json(req): axum::Json<Value>| {
        let calls = calls.clone();
        async move {
            let result = match req["method"].as_str().unwrap_or("") {
                "initialize" => json!({"protocolVersion":req["params"]["protocolVersion"], "capabilities":{"tools":{}}, "serverInfo":{"name":"philium-test","version":"1"}}),
                "tools/list" => json!({"tools": (["browser_open", "browser_snapshot"].map(|name| json!({"name":name,"description":"Fake browser tool", "inputSchema":{"type":"object","properties":{}}})))}),
                "tools/call" => {
                    let name = req["params"]["name"].as_str().unwrap().to_string();
                    assert!(["browser_open", "browser_snapshot"].contains(&name.as_str()));
                    calls.lk().push(name);
                    json!({"content":[{"type":"text","text":"fake browser dispatched"}],"isError":false})
                },
                _ => json!({}),
            };
            if req.get("id").is_none() {
                return (axum::http::StatusCode::ACCEPTED, axum::Json(Value::Null));
            }
            (axum::http::StatusCode::OK, axum::Json(json!({"jsonrpc":"2.0","id":req["id"],"result":result})))
        }
    }));
    let (mcp_url, task) = serve(mcp).await;
    fixture.tasks.push(task);
    let model = axum::Router::new().route("/v1/{*rest}", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
        let messages = body["messages"].as_array().unwrap();
        let done = messages.last().unwrap()["role"] == "tool";
        let tool = messages.iter().rev().find(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap();
        let name = format!("mcp_philium_{tool}");
        if !done {
            assert!(body["tools"].as_array().expect("HAL exposes tools").iter().any(|t| t["function"]["name"] == name), "requested browser tool missing: {body}");
        }
        let delta = if done { json!({"role":"assistant","content":"done"}) } else {
            json!({"tool_calls":[{"index":0,"id":format!("call_{}", messages.len()),"type":"function","function":{"name":name,"arguments":"{}"}}]})
        };
        let sse = format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":if done {"stop"} else {"tool_calls"}}],"usage":{"prompt_tokens":10,"completion_tokens":2}}));
        ([("content-type","text/event-stream")], sse)
    }));
    let (model_url, task) = serve(model).await;
    fixture.tasks.push(task);
    let gw = Gateway::new(hal::random_token(), None);
    let (gateway_url, task) = serve(gw.router()).await;
    fixture.tasks.push(task);
    let ctx = Ctx::new(
        db::Db::memory(),
        dir.join("data"),
        dir.join("root"),
        gw.clone(),
        gateway_url,
    );
    let provider = Provider {
        id: "test".into(),
        name: "Test".into(),
        kind: Kind::OpenaiChat,
        base_url: format!("{model_url}/v1"),
        trusted: false,
        guard: GuardMode::Redact,
        enabled: true,
        default_model: Some("test-model".into()),
    };
    providers::upsert(&ctx, &provider).unwrap();
    gw.set_routes(vec![Route {
        provider,
        api_key: Some("fake-localhost-only".into()),
    }]);
    // Exercise both production policies without cloning their rules into the test.
    for strict in [false, true] {
        let launch_dir = dir.join(if strict { "strict" } else { "standard" });
        std::fs::create_dir_all(&launch_dir).unwrap();
        let setup = providers::hal_setup(
            &ctx,
            &launch_dir.join("hal"),
            &providers::ConfigOpts::default(),
        );
        let mut config = setup.config;
        config["permissions"] = agents::permission_rules(strict);
        config["server"]["workspace"] = json!(launch_dir);
        config["mcp"] = json!({"servers":{"philium":{"transport":"streamable_http","url":format!("{mcp_url}/mcp")}}});
        let ld = launch_dir.clone();
        let h = tokio::task::spawn_blocking(move || {
            Hal::start(hal::Launch {
                dir: &ld,
                config,
                cwd: Some(&ld),
                env: setup.env,
                allow_logins: false,
                wrap: None,
            })
        })
        .await
        .unwrap()
        .expect("bundled HAL starts");
        fixture.hal = Some(h);
        let h = fixture.hal.as_ref().unwrap();
        let mut sessions = vec![];
        for title in ["first", "isolated"] {
            let s = h
                .call(
                    reqwest::Method::POST,
                    "/sessions",
                    Some(json!({"workspace":launch_dir,"title":title,"permission_mode":"ask"})),
                )
                .await
                .unwrap();
            sessions.push(s["id"].as_str().unwrap().to_string());
        }
        let sid = &sessions[0];
        let base = dispatched.lk().len();
        let before = send_browser(h, sid, "browser_open").await;
        let rid = pending(h, sid, "philium/browser_open", &dispatched, base).await;
        decide(h, sid, &rid, Some("once")).await;
        finish(h, sid, before).await;
        assert_eq!(&dispatched.lk()[base..], &["browser_open"]);
        let before = send_browser(h, sid, "browser_open").await;
        let rid = pending(h, sid, "philium/browser_open", &dispatched, base + 1).await;
        decide(h, sid, &rid, None).await;
        finish(h, sid, before).await;
        assert_eq!(dispatched.lk().len(), base + 1, "denial dispatched browser");
        let before = send_browser(h, sid, "browser_open").await;
        let rid = pending(h, sid, "philium/browser_open", &dispatched, base + 1).await;
        decide(h, sid, &rid, Some("session")).await;
        finish(h, sid, before).await;
        let before = send_browser(h, sid, "browser_open").await;
        finish(h, sid, before).await;
        assert_eq!(
            &dispatched.lk()[base..],
            &["browser_open", "browser_open", "browser_open"]
        );
        assert!(h
            .call(
                reqwest::Method::GET,
                &format!("/sessions/{sid}/permissions"),
                None
            )
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        let before = send_browser(h, sid, "browser_snapshot").await;
        let rid = pending(h, sid, "philium/browser_snapshot", &dispatched, base + 3).await;
        decide(h, sid, &rid, None).await;
        finish(h, sid, before).await;
        let other = &sessions[1];
        let before = send_browser(h, other, "browser_open").await;
        let rid = pending(h, other, "philium/browser_open", &dispatched, base + 3).await;
        decide(h, other, &rid, None).await;
        finish(h, other, before).await;
        assert_eq!(dispatched.lk().len(), base + 3);
        fixture.hal.take().unwrap().stop();
    }
}
