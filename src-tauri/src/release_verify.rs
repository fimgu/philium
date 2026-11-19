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
    let mergeable = release_intake::call(ctx, "mergeable", a.clone()).await.unwrap_or_else(|_| json!({"state":"unknown"}));
    let reason = match (reason, membership_error) {
        (existing, Some(error)) => Some(format!(
            "{}Membership discovery: {error}",
            existing.map(|r| format!("{r}. ")).unwrap_or_default()
        )),
        (existing, None) => existing,
    };
    finish(ctx, repo, first, last, members, receipt, reason, mergeable)
}

fn finish(
    ctx: &Ctx,
    repo: &str,
    first: Value,
    last: Option<Value>,
    members: Option<Value>,
    receipt: Value,
    reason: Option<String>,
    mergeable: Value,
) -> Result<Value, String> {
    let current = last.as_ref().map(|v| {
        v["generation"] == first["generation"]
            && v["head"] == first["head"]
            && v["base"] == first["base"]
    });
    let targets = receipt["targets"].as_array().cloned().unwrap_or_default();
    let loopback: Vec<Value> = receipt["live_cases"].as_array().into_iter().flatten().filter(|c| c.get("target").is_none()).cloned().collect();
    let outcome = match current {
        None => "origin_unconfirmed",
        Some(ok) => {
            let base = if targets.is_empty() || !loopback.is_empty() {
                crate::release_scope::scoped_result(
                    &receipt["cases"],
                    &json!(loopback),
                    receipt["deployment_identity"]["state"].as_str().unwrap_or(""),
                    receipt["deployment_after"]["state"].as_str().unwrap_or(""),
                    ok,
                )
            } else {
                // Command targets stand in for the loopback lane: judge the local contracts alone.
                crate::release_scope::scoped_result(&receipt["cases"], &json!([{"state":"pass"}]), "match", "match", ok)
            };
            with_targets(base, &targets, &receipt["live_cases"])
        }
    };
    let m = members.as_ref().unwrap_or(&Value::Null);
    let decision = match outcome {
        "blocked" => "not_ready",
        "superseded" => "superseded",
        "scoped_pass"
            if m["missing"] == 0
                && m["ambiguous"] == 0
                && receipt["review_scope_verified"] == true
                // Nothing counts as ready until the person confirmed these exact checks by pinning.
                && receipt["checks_confirmed_ms"].as_i64().is_some()
                // A head that cannot merge onto the target as it is now has to change first.
                && mergeable["state"] == "clean" =>
        {
            "ready_for_reviewed_scope"
        }
        _ => "incomplete",
    };
    let mut why = explain(current, last.as_ref(), m, &receipt, reason.as_deref());
    match mergeable["state"].as_str() {
        Some("clean") => {}
        Some("conflicts") => {
            let files: Vec<&str> = mergeable["files"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let n = mergeable["count"].as_u64().unwrap_or(files.len() as u64);
            let shown = files.iter().take(4).copied().collect::<Vec<_>>().join(", ");
            let more = if n as usize > files.len().min(4) { format!(" and {} more", n as usize - files.len().min(4)) } else { String::new() };
            why.push(format!("Does not merge onto the target as it is now: conflicts in {shown}{more}. The head has to change, and a new head starts a new verification."));
        }
        _ => why.push("Whether this head merges onto the target could not be established locally.".into()),
    }
    let v = json!({"pr":first["pr"],"generation":first["generation"],"head":first["head"],"base":first["base"],"artifact":receipt["artifact"],"checked_ms":crate::db::now(),"origin_current":current,"decision":decision,"contract_decision":outcome,"membership":members,"receipt":receipt,"reason":reason,"why":why,"mergeable":mergeable});
    ctx.db.with(|c|c.execute_batch("CREATE TABLE IF NOT EXISTS release_decisions(repo TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,seq INTEGER NOT NULL REFERENCES release_refs(seq),summary TEXT NOT NULL,PRIMARY KEY(repo,seq))"))?;
    ctx.db.exec(
        "INSERT OR REPLACE INTO release_decisions(repo,seq,summary) VALUES(?,?,?)",
        rusqlite::params![repo, first["generation"].as_i64(), v.to_string()],
    )?;
    Ok(v)
}
/// A target's checks count only with that target's own identity matching before and after.
/// A mismatch on a matching deployment blocks. Anything unidentified or unobserved is inconclusive.
fn with_targets(base: &'static str, targets: &[Value], live: &Value) -> &'static str {
    if targets.is_empty() || matches!(base, "superseded" | "blocked") {
        return base;
    }
    let mut all_pass = true;
    for t in targets {
        let matched = t["identity_before"]["state"] == "match" && t["identity_after"]["state"] == "match";
        let cases: Vec<&Value> = live.as_array().into_iter().flatten().filter(|c| c["target"] == t["name"]).collect();
        if matched && cases.iter().any(|c| c["state"] == "mismatch") {
            return "blocked";
        }
        all_pass &= matched && !cases.is_empty() && cases.iter().all(|c| c["state"] == "pass");
    }
    if base == "scoped_pass" && all_pass {
        "scoped_pass"
    } else {
        "inconclusive"
    }
}
/// Every cause that keeps this from being a ship decision, in the operator's words. One line
/// per cause, so a moved head, missing or altered evidence, a wrong or absent deployment and an
/// unreachable origin never look alike. Empty only when nothing stands in the way.
fn explain(
    current: Option<bool>,
    last: Option<&Value>,
    m: &Value,
    receipt: &Value,
    reason: Option<&str>,
) -> Vec<String> {
    let mut why = vec![];
    match current {
        None => why.push("Origin could not be reconfirmed after the checks. There is no verdict for the current head.".to_owned()),
        Some(false) => why.push(format!(
            "The head or target moved during verification (origin now reports head {}). This result belongs to the earlier revision only.",
            last.and_then(|v| v["head"].as_str()).unwrap_or("unknown")
        )),
        Some(true) => {}
    }
    if let Some(r) = reason {
        why.push(r.to_owned());
    }
    if m.is_null() {
        why.push("Landed pull requests were not established: the origin lookup did not complete.".to_owned());
    } else {
        if m["missing"].as_u64().unwrap_or(0) > 0 {
            why.push(format!("{} included commits have no completed pull request lookup. The origin did not answer for them.", m["missing"]));
        }
        if m["ambiguous"].as_u64().unwrap_or(0) > 0 {
            why.push(format!("{} included commits map to more than one pull request.", m["ambiguous"]));
        }
    }
    if receipt.is_null() {
        return why;
    }
    for c in receipt["cases"].as_array().into_iter().flatten() {
        let id = c["id"].as_str().unwrap_or("?");
        let children: Vec<String> = c["landed_evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["result"] == "same_failure_as_candidate")
            .map(|e| format!("#{}", e["pr"]))
            .collect();
        let child = if children.is_empty() {
            String::new()
        } else {
            format!(" Landed {} fails the same way on its own.", children.join(", "))
        };
        match c["state"].as_str() {
            Some("composed_head_regression") => why.push(format!("{id}: merge-only regression. The target and both merged sides pass; the merged head fails.{child}")),
            Some("release_range_regression") => why.push(format!("{id}: regression inside this release. The target passes and the head fails.{child}")),
            Some("same_failure_on_target") => why.push(format!("{id}: already failing on the target in the same way. Not introduced by this release.")),
            Some("failure_unattributed") => why.push(format!("{id}: fails on the head; the comparison does not establish where it started.{child}")),
            Some("incomplete") => why.push(format!("{id}: missing evidence. The check could not be observed on the pinned revisions.")),
            _ => {}
        }
    }
    let (before, after) = (
        receipt["deployment_identity"]["state"].as_str().unwrap_or(""),
        receipt["deployment_after"]["state"].as_str().unwrap_or(""),
    );
    let targets: Vec<&Value> = receipt["targets"].as_array().into_iter().flatten().collect();
    let all_live: Vec<&Value> = receipt["live_cases"].as_array().into_iter().flatten().collect();
    let loopback: Vec<&Value> = all_live.iter().copied().filter(|c| c.get("target").is_none()).collect();
    let no_record = receipt["artifact"].is_null();
    let deployment = |state: &str, when: &str, place: &str| match state {
        "match" => None,
        "absent" if place.is_empty() => Some("No deployment observer is configured for this checkout, so the running product was not observed.".to_owned()),
        "absent" => Some(format!("{place}the confirmed checks include no identity command, so the revision and artifact this environment runs are unknown. Its behavior is shown but cannot count toward a decision.")),
        "mismatch" => Some(format!("{place}Wrong deployment {when}: the running replicas report a different revision or artifact than this head.")),
        "mixed" => Some(format!("{place}Mixed deployment {when}: replicas report different revisions or artifacts.")),
        "not_ready" => Some(format!("{place}Deployment {when}: the matching replicas are not ready.")),
        _ if no_record => Some(format!("{place}No independent artifact record exists for this exact head, so the deployment cannot be matched to it.")),
        _ if !place.is_empty() => Some(format!("{place}its identity command did not return a complete, well-formed answer {when}. Check that you are signed in to this environment.")),
        _ => Some(format!("{place}Deployment unavailable {when}: the observer did not answer, or its report was stale or incomplete.")),
    };
    // The loopback observer lane, unless the verifier uses command targets only.
    if targets.is_empty() || !loopback.is_empty() {
        if let Some(line) = deployment(before, "before the checks", "") {
            why.push(line);
        } else if !loopback.is_empty() {
            if let Some(line) = deployment(after, "after the checks", "") {
                why.push(line);
            }
        }
        for c in &loopback {
            let id = c["id"].as_str().unwrap_or("?");
            match c["state"].as_str() {
                Some("mismatch") => why.push(format!("{id}: the running product returned a different value than the contract expects.")),
                Some("unknown") if before == "match" => why.push(format!("{id}: the running product could not be observed twice with the pinned revision and artifact.")),
                _ => {}
            }
        }
    }
    // Each command target is one environment with its own identity.
    for t in &targets {
        let name = t["name"].as_str().unwrap_or("?");
        let place = format!("{name}: ");
        let state = t["identity_before"]["state"].as_str().unwrap_or("");
        let sampled = matches!(state, "match" | "absent");
        if let Some(line) = deployment(state, "before the checks", &place) {
            why.push(line);
        } else if let Some(line) = deployment(t["identity_after"]["state"].as_str().unwrap_or(""), "after the checks", &place) {
            why.push(line);
        }
        for c in all_live.iter().filter(|c| c["target"] == t["name"]) {
            let id = c["id"].as_str().unwrap_or("?");
            match c["state"].as_str() {
                Some("mismatch") => why.push(format!("{id} ({name}): the environment returned a different value than the confirmed check expects.")),
                Some("unknown") if sampled => why.push(format!("{id} ({name}): the command did not return the same readable output twice. Check that you are signed in and that the endpoint exists.")),
                _ => {}
            }
        }
    }
    if receipt.get("checks_confirmed_ms").is_some_and(Value::is_null) {
        why.push("These checks were pinned before confirmations were recorded. Open the setup, read them and pin again.".to_owned());
    }
    if all_live.is_empty() && receipt["release_records"] == true {
        why.push("The verifier declares no running-product checks, so product behavior was not exercised.".to_owned());
    }
    let att = &receipt["review_attestation"];
    if receipt["release_records"] != true {
        why.push("The verifier does not record reviews, so review coverage cannot be established.".to_owned());
    } else if att["recorded"] != true {
        why.push("No independent release record exists for this exact base, head and verifier pin. An earlier head's record does not carry over.".to_owned());
    } else {
        for (flag, text) in [
            ("all_changes_accounted_for", "not every change is accounted for"),
            ("code_review_complete", "code review is not recorded as complete"),
            ("cleanup_verified", "cleanup is not recorded as verified"),
        ] {
            if att[flag] != true {
                why.push(format!("Review record: {text}."));
            }
        }
        match att["unresolved_findings"].as_u64() {
            Some(0) => {}
            Some(n) => why.push(format!("Review record: {n} unresolved findings.")),
            None => why.push("Review record: unresolved findings are not listed.".to_owned()),
        }
        let mut gaps = vec![];
        for c in receipt["cases"].as_array().into_iter().flatten().chain(receipt["live_cases"].as_array().into_iter().flatten()) {
            let Some(id) = c["id"].as_str() else { continue };
            if receipt["verified_evidence"][id].is_string() {
                continue;
            }
            gaps.push(format!(
                "{id} ({})",
                match receipt["evidence_status"][id].as_str() {
                    Some("altered") => "altered: the file no longer matches its recorded hash",
                    Some("missing") => "missing: the recorded file is not in the verifier",
                    Some("not_for_this_head") => "not a passing observation of this head",
                    Some("invalid") => "unreadable",
                    _ => "not recorded",
                }
            ));
        }
        if !gaps.is_empty() {
            why.push(format!("Evidence not accepted for {}.", gaps.join("; ")));
        }
        if receipt["review_scope_verified"] != true && att["required_cases"].is_array() && gaps.is_empty() && !why.iter().any(|w| w.starts_with("Review record")) {
            why.push("Review record: the required case list does not match the cases that ran.".to_owned());
        }
    }
    why
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
    crate::secrets::init(&dir);
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
