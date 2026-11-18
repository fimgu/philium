//! One noninteractive review operation. Neither UI nor CLI delegates to a coordinator.
use crate::{ctx::Ctx, db::Db, gateway::Gateway, hal, release_intake, workspace};
use serde_json::{json, Value};
use std::path::PathBuf;
pub async fn run(ctx: &Ctx, repo: &str, url: &str) -> Result<Value, String> {
    let req = json!({"repo_id":repo,"url":url});
    let first = release_intake::call(ctx, "resolve", req.clone()).await?;
    let a = json!({"repo_id":repo,"generation":first["generation"]});
    let membership = release_intake::call(ctx, "discover_members", a.clone()).await;
    let membership_error = membership.as_ref().err().cloned();
    let members = membership.ok();
    let profile = release_intake::call(ctx, "sql_profile", a.clone()).await?;
    let (receipt, reason) = if profile.is_null() {
        (
            Value::Null,
            Some("No independent driver enrolled".to_owned()),
        )
    } else {
        match release_intake::call(ctx, "sql_compare", a.clone()).await {
            Ok(v) => (v, None),
            Err(e) => (Value::Null, Some(e)),
        }
    };
    let last = release_intake::call(ctx, "resolve", req).await.ok();
    let reason = match (reason, membership_error) {
        (existing, Some(error)) => Some(format!(
            "{}Membership discovery: {error}",
            existing.map(|r| format!("{r}. ")).unwrap_or_default()
        )),
        (existing, None) => existing,
    };
    finish(ctx, repo, first, last, members, receipt, reason)
}

fn finish(
    ctx: &Ctx,
    repo: &str,
    first: Value,
    last: Option<Value>,
    members: Option<Value>,
    receipt: Value,
    reason: Option<String>,
) -> Result<Value, String> {
    let current = last.as_ref().map(|v| {
        v["generation"] == first["generation"]
            && v["head"] == first["head"]
            && v["base"] == first["base"]
    });
    let outcome = match current {
        None => "origin_unconfirmed",
        Some(ok) => crate::release_scope::scoped_result(
            &receipt["cases"],
            &receipt["live_cases"],
            receipt["deployment_identity"]["state"]
                .as_str()
                .unwrap_or(""),
            receipt["deployment_after"]["state"].as_str().unwrap_or(""),
            ok,
        ),
    };
    let m = members.as_ref().unwrap_or(&Value::Null);
    let decision = match outcome {
        "blocked" => "not_ready",
        "superseded" => "superseded",
        "scoped_pass"
            if m["missing"] == 0
                && m["ambiguous"] == 0
                && receipt["review_scope_verified"] == true =>
        {
            "ready_for_reviewed_scope"
        }
        _ => "incomplete",
    };
    let v = json!({"pr":first["pr"],"generation":first["generation"],"head":first["head"],"base":first["base"],"checked_ms":crate::db::now(),"origin_current":current,"decision":decision,"contract_decision":outcome,"membership":members,"receipt":receipt,"reason":reason});
    ctx.db.with(|c|c.execute_batch("CREATE TABLE IF NOT EXISTS release_decisions(repo TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,seq INTEGER NOT NULL REFERENCES release_refs(seq),summary TEXT NOT NULL,PRIMARY KEY(repo,seq))"))?;
    ctx.db.exec(
        "INSERT OR REPLACE INTO release_decisions(repo,seq,summary) VALUES(?,?,?)",
        rusqlite::params![repo, first["generation"].as_i64(), v.to_string()],
    )?;
    Ok(v)
}
pub(crate) fn reviewed(scope: &Value, refs: &Value, receipt: &Value) -> bool {
    use std::collections::BTreeSet;
    if !scope.is_object()
        || refs["base"].as_str().is_none()
        || refs["head"].as_str().is_none()
        || scope["base"] != refs["base"]
        || scope["head"] != refs["head"]
        || scope["pr"] != refs["pr"]
        || scope["all_changes_accounted_for"] != true
        || scope["code_review_complete"] != true
        || scope["cleanup_verified"] != true
        || !scope["unresolved_findings"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return false;
    }
    let Some(required) = scope["required_cases"].as_array() else {
        return false;
    };
    let ids = receipt["cases"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(receipt["live_cases"].as_array().into_iter().flatten())
        .filter_map(|r| r["id"].as_str())
        .collect::<BTreeSet<_>>();
    !ids.is_empty()
        && ids
            .iter()
            .all(|id| receipt["verified_evidence"][*id].is_string())
        && required.len() == ids.len()
        && required
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>()
            == ids
}
#[cfg(test)]
#[path = "release_verify_test.rs"]
mod tests;

pub async fn cli() -> Result<Value, String> {
    let argv: Vec<_> = std::env::args().skip(1).collect();
    let verify = argv.first().is_some_and(|s| s == "--review-url");
    if (verify && argv.len() != 3) || (!verify && !(3..=4).contains(&argv.len())) {
        return Err("Usage: --review-url CHECKOUT PR_URL | --pin-review-driver CHECKOUT DRIVER [OBSERVER_PORT]".into());
    }
    let dir = PathBuf::from(std::env::var_os("PHILIUM_DATA_DIR").ok_or("Missing Pilot profile")?);
    let ctx = Ctx::new(
        Db::open(&dir.join("philium.db"))?,
        dir,
        crate::philium_root(),
        Gateway::new(hal::random_token(), None),
        String::new(),
    );
    workspace::init(&ctx)?;
    let register = |p: &str| workspace::call(&ctx, "repo_add", json!({"path":p}));
    let product = register(&argv[1]).await?;
    let repo = product["id"].as_str().ok_or("Missing checkout")?;
    if verify {
        return run(&ctx, repo, &argv[2]).await;
    }
    let driver = register(&argv[2]).await?;
    let port = argv
        .get(3)
        .map(|p| p.parse::<u16>())
        .transpose()
        .map_err(|_| "Invalid port")?;
    release_intake::call(
        &ctx,
        "sql_enroll",
        json!({"repo_id":repo,"driver":driver["id"],"stage_port":port}),
    )
    .await
}
