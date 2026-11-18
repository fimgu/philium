use super::*;
use crate::{board, db::Db, gateway::Gateway};

#[test]
fn ownership_requires_every_identity() {
    let id = "a".repeat(64);
    let j =
        json!({"id":"nonce","repo":"one","task":"task","agent":"agent","cid":id,"image":"digest"});
    let c = json!({"Id":id,"Image":"digest","Config":{"Labels":{"dev.philium.id":"nonce","dev.philium.repo":"one","dev.philium.task":"task","dev.philium.agent":"agent"}}});
    assert!(issued(&j, &c));
    for k in ["id", "repo", "task", "agent", "cid", "image"] {
        let mut mismatch = j.clone();
        mismatch[k] = json!("different");
        assert!(!issued(&mismatch, &c), "{k}");
    }
}

#[tokio::test]
#[ignore = "explicit disposable local runtime fixture only; run scripts/check-managed.py"]
async fn runtime_phase() {
    let root = std::path::PathBuf::from(std::env::var("PHILIUM_MANAGED_FIXTURE").unwrap())
        .canonicalize()
        .unwrap();
    let scratch = std::path::PathBuf::from(std::env::var("PHILIUM_SCRATCH_DIR").unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(root.parent(), Some(scratch.as_path()));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("managed-check-"));
    assert_eq!(
        std::fs::read(root.join("disposable")).unwrap(),
        b"fixture\n"
    );
    let phase = std::env::var("PHILIUM_MANAGED_PHASE").unwrap();
    let profile = std::env::var("PHILIUM_TEST_PROFILE").unwrap();
    let ctx = Ctx::new(
        Db::open(&root.join("fixture.sqlite")).unwrap(),
        root.join("data"),
        root.join("work"),
        Gateway::new("fixture".into(), None),
        "http://127.0.0.1:9".into(),
    );
    board::init(&ctx).unwrap();
    init(&ctx).unwrap();
    let app = crate::App { ctx: ctx.clone() };
    let rpc = |method: &str, a: Value| crate::rpc_inner(&app, format!("managed.{method}"), Some(a));
    if phase == "seed" {
        for repo in ["alpha", "beta"] {
            ctx.db
                .exec(
                    "INSERT INTO repos(id,name,path,added_at) VALUES(?1,?1,?2,1)",
                    params![repo, root.join(repo).to_str().unwrap()],
                )
                .unwrap();
            ctx.db.exec("INSERT INTO agents(id,name,model_ref,repo_id,created_at,last_active_at) VALUES(?1,?1,'fixture',?1,1,1)",[repo]).unwrap();
            let task = board::create(&ctx, "you", repo, "", None, Some(repo), &[], None).unwrap();
            let tid = task["id"].as_str().unwrap();
            board::claim(&ctx, repo, tid, 300).unwrap();
            let args = json!({"repo_id":repo,"agent_id":repo,"task_id":tid,"profile":profile,"minutes":2,"command":format!("printf '{}\\n'; sleep 100",repo)});
            let mut stale = args.clone();
            stale["agent_id"] = json!("different");
            assert!(rpc("launch", stale)
                .await
                .unwrap_err()
                .contains("owner changed"));
            let mut wrong = args.clone();
            wrong["repo_id"] = json!("peer");
            assert!(rpc("launch", wrong).await.is_err());
            let job = rpc("launch", args.clone()).await.unwrap();
            assert!(rpc("launch", args).await.is_err());
            let info = Engine::open(&profile)
                .unwrap()
                .ok(
                    Method::GET,
                    &format!("/containers/{}/json", key(&job, "cid").unwrap()),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(info["HostConfig"]["NetworkMode"], "none");
            assert_eq!(info["HostConfig"]["ReadonlyRootfs"], true);
            assert_eq!(info["Config"]["User"], "65534:65534");
            assert_eq!(info["State"]["Running"], true);
        }
    } else if phase == "recover" {
        let a = rpc("list", json!({"repo_id":"alpha"})).await.unwrap()["jobs"][0].clone();
        let b = rpc("list", json!({"repo_id":"beta"})).await.unwrap()["jobs"][0].clone();
        assert_ne!(a["cid"], b["cid"]);
        assert_eq!(
            rpc("list", json!({"repo_id":"alpha","task_id":a["task"]}))
                .await
                .unwrap()["jobs"][0]["id"],
            a["id"]
        );
        assert_eq!(
            rpc("list", json!({"repo_id":"alpha","task_id":b["task"]}))
                .await
                .unwrap()["jobs"],
            json!([])
        );
        for m in ["logs", "release"] {
            assert!(rpc(
                m,
                json!({"repo_id":"alpha","task_id":b["task"],"id":a["id"]})
            )
            .await
            .unwrap_err()
            .contains("selected task"));
        }
        // Mutate only copies of fixture records. Never switch the shared engine.
        for (field, value, reason) in [
            ("engine", json!("changed"), "identity changed"),
            ("cid", b["cid"].clone(), "Ownership mismatch"),
            ("profile", json!("missing-philium-fixture"), ""),
        ] {
            let mut wrong = a.clone();
            wrong[field] = value;
            assert!(cleanup(&ctx, &wrong, "negative test")
                .await
                .unwrap_err()
                .contains(reason));
        }
        for j in [&a, &b] {
            let p = json!({"repo_id":j["repo"],"id":j["id"]});
            assert!(rpc("logs", p).await.unwrap()["text"]
                .as_str()
                .unwrap()
                .contains(key(j, "repo").unwrap()));
        }
        assert!(rpc("release", json!({"repo_id":"beta","id":a["id"]}))
            .await
            .is_err());
        board::update(
            &ctx,
            "you",
            key(&a, "task").unwrap(),
            &json!({"status":"cancelled"}),
        )
        .unwrap();
        // A failed automatic cleanup must retain its ledger for a later retry.
        ctx.db
            .exec(
                "UPDATE managed_jobs SET engine='fixture-mismatch' WHERE id=?",
                [key(&a, "id").unwrap()],
            )
            .unwrap();
        reconcile(&ctx).await.unwrap();
        assert!(
            row(&ctx, "alpha", key(&a, "id").unwrap()).unwrap()["receipt"]
                .as_str()
                .unwrap()
                .contains("identity changed")
        );
        ctx.db
            .exec(
                "UPDATE managed_jobs SET engine=? WHERE id=?",
                params![key(&a, "engine").unwrap(), key(&a, "id").unwrap()],
            )
            .unwrap();
        reconcile(&ctx).await.unwrap();
        assert_eq!(
            row(&ctx, "alpha", key(&a, "id").unwrap()).unwrap()["state"],
            "released"
        );
        assert!(rpc("logs", json!({"repo_id":"beta","id":b["id"]}))
            .await
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("beta"));
        // An expired recorded deadline (no clock changes) is picked up by the next process.
        ctx.db
            .exec(
                "UPDATE managed_jobs SET expires=? WHERE id=?",
                params![now() - 1, key(&b, "id").unwrap()],
            )
            .unwrap();
    } else if phase == "interrupt" {
        // Reconstruct the durable boundary after Docker create and before ID storage.
        ctx.db
            .exec(
                "UPDATE managed_jobs SET cid=NULL,state='issuing',created=? WHERE repo='beta'",
                [now() - 121000],
            )
            .unwrap();
    } else if phase == "expired" {
        reconcile(&ctx).await.unwrap();
        let jobs = ctx.db.query("SELECT * FROM managed_jobs", []).unwrap();
        assert_eq!(jobs.len(), 2);
        for j in jobs {
            assert_eq!(j["state"], "released");
            assert!(key(&j, "receipt").unwrap().contains("verified"));
            assert_eq!(
                Engine::open(&profile)
                    .unwrap()
                    .request(
                        Method::GET,
                        &format!("/containers/{}/json", key(&j, "cid").unwrap()),
                        None
                    )
                    .await
                    .unwrap()
                    .0,
                404
            );
        }
    } else if phase == "cleanup" {
        for j in ctx
            .db
            .query("SELECT * FROM managed_jobs WHERE state!='released'", [])
            .unwrap()
        {
            cleanup(&ctx, &j, "fixture teardown").await.unwrap();
        }
    } else {
        panic!("unrecognized phase");
    }
}
