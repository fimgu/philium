//! Review's checks: the repo's gate on a pull request's exact head and base,
//! started from the app next to the agents' review.
//!
//! rpc `pipeline.*`: `status {repo_id}`, `run {repo_id, generation, tier}`,
//! `results {repo_id, generation, tier}`, `lend {repo_id, name, path}`.
//! Events: `pipeline:progress {repo_id, line}`, `pipeline:done {repo_id, state}`.
//!
//! The person's checkout is only read. Pull request commits come into a mirror
//! Philium owns, fetched read-only with the same GitHub access Review uses.
use super::config::{self, Tier};
use super::{gate, records, runner, verdict};
use crate::ctx::{arg_str, Ctx};
use crate::db::Db;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default, Clone)]
struct RunState {
    running: bool,
    generation: i64,
    tier: String,
    lines: Vec<String>,
    error: Option<String>,
}

fn runs() -> &'static Mutex<HashMap<String, RunState>> {
    static R: OnceLock<Mutex<HashMap<String, RunState>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// The account's own home: hidden from checks, and where Docker keeps its settings.
fn account_home() -> PathBuf {
    crate::pilot::real_home().or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("/var/empty"))
}

/// Under the home folder so Docker (Colima shares home) can mount run folders.
fn root() -> PathBuf {
    account_home().join("Library/Caches/philium-pipeline")
}

fn slug(origin: &str) -> String {
    origin.replace('/', "-")
}

/// Where a person keeps a gate for a repo that has none committed yet.
pub fn local_gate(ctx: &Ctx, origin: &str) -> PathBuf {
    ctx.data_dir.join("pipelines").join(slug(origin)).join(".philium").join("pipeline.yaml")
}

fn db(ctx: &Ctx) -> Result<Db, String> {
    std::fs::create_dir_all(&ctx.data_dir).map_err(|e| e.to_string())?;
    let db = Db::open(&ctx.data_dir.join("pipeline.db"))?;
    records::init(&db)?;
    Ok(db)
}

fn host(ctx: &Ctx, origin: &str) -> runner::Host {
    let path = ctx.data_dir.join("pipeline.db");
    let (o1, o2, o3) = (origin.to_string(), origin.to_string(), origin.to_string());
    let (p1, p2) = (path.clone(), path);
    runner::Host {
        scratch: root().join("runs").join(format!("app-{}", std::process::id())),
        caches: root().join("caches").join(slug(origin)),
        home: account_home(),
        tools_home: account_home(),
        secret: Box::new(move |n| crate::secrets::get(&format!("pipeline.{o1}.{n}"))),
        enrolled: Box::new(move |e| Db::open(&p1).ok().is_some_and(|db| records::enrolled(&db, &o2, e))),
        lent: Box::new(move |l| Db::open(&p2).ok().and_then(|db| records::lent(&db, &o3, l))),
        platform: config::host_platform(),
    }
}

fn git(dir: &Path, args: &[&str], env: &[(&str, String)]) -> Result<String, String> {
    let mut c = std::process::Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null"]).args(args).env("GIT_TERMINAL_PROMPT", "0").env("HOME", account_home());
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().chars().take(400).collect())
    }
}

fn has(mirror: &Path, sha: &str) -> bool {
    git(mirror, &["cat-file", "-e", &format!("{sha}^{{commit}}")], &[]).is_ok()
}

/// A bare mirror Philium owns, with the pull request's head and base in it.
async fn mirror(checkout: &Path, origin: &str, pr: i64, shas: &[&str]) -> Result<PathBuf, String> {
    let m = root().join("mirrors").join(format!("{}.git", slug(origin)));
    std::fs::create_dir_all(m.parent().unwrap_or(Path::new("/"))).map_err(|e| e.to_string())?;
    if !m.join("HEAD").exists() {
        let o = std::process::Command::new("git").args(["clone", "--bare", "--quiet", "--no-tags"]).arg(checkout).arg(&m).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| format!("git: {e}"))?;
        if !o.status.success() {
            return Err(format!("could not mirror the checkout: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
    } else {
        // Pick up what the checkout fetched since; its own refs are only read.
        let _ = git(&m, &["fetch", "--quiet", "--no-tags", &checkout.display().to_string(), "+refs/heads/*:refs/heads/*", "+refs/remotes/origin/*:refs/remotes/origin/*"], &[]);
    }
    let missing: Vec<&str> = shas.iter().copied().filter(|s| !has(&m, s)).collect();
    if !missing.is_empty() {
        let mut env = vec![];
        if let Some(token) = crate::release_intake::origin_bearer(origin).await {
            use base64::Engine;
            let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
            env = vec![("GIT_CONFIG_COUNT", "1".to_string()), ("GIT_CONFIG_KEY_0", "http.extraHeader".to_string()), ("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {basic}"))];
        }
        let url = format!("https://github.com/{origin}.git");
        let mut args: Vec<String> = vec!["fetch".into(), "--quiet".into(), "--no-tags".into(), url, format!("+refs/pull/{pr}/head:refs/philium/pr/{pr}")];
        args.extend(missing.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        git(&m, &refs, &env.iter().map(|(k, v)| (*k, v.clone())).collect::<Vec<_>>()).map_err(|e| format!("could not fetch the pull request from GitHub: {e}"))?;
    }
    for s in shas {
        if !has(&m, s) {
            return Err(format!("{} is not on GitHub or not readable with this account", &s[..12]));
        }
    }
    Ok(m)
}

fn checkout_and_origin(ctx: &Ctx, repo_id: &str) -> Result<(PathBuf, String), String> {
    let r = ctx.db.query("SELECT remote_url,path FROM repos WHERE id=?", [repo_id])?.pop().ok_or("Unknown checkout")?;
    let origin = crate::release_intake::origin_of(r["remote_url"].as_str().unwrap_or(""))?;
    Ok((PathBuf::from(r["path"].as_str().ok_or("Checkout unavailable")?), origin))
}

fn generation(ctx: &Ctx, repo_id: &str, origin: &str, generation: i64) -> Result<(i64, String, String), String> {
    let r = ctx
        .db
        .query("SELECT pr,base,head FROM release_refs WHERE repo=?1 AND origin=?2 AND seq=?3", params![repo_id, origin, generation])?
        .pop()
        .ok_or("Look the pull request up in Review first")?;
    Ok((r["pr"].as_i64().ok_or("Invalid pull request")?, r["base"].as_str().unwrap_or("").to_string(), r["head"].as_str().unwrap_or("").to_string()))
}

fn say(ctx: &Arc<Ctx>, key: &str, line: String) {
    if let Some(s) = runs().lock().ok().as_mut().and_then(|m| m.get_mut(key)) {
        s.lines.push(line.clone());
        if s.lines.len() > 200 {
            s.lines.remove(0);
        }
    }
    ctx.emit("pipeline:progress", json!({ "repo_id": key, "line": line }));
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, a: Value) -> Result<Value, String> {
    let repo_id = arg_str(&a, "repo_id")?;
    let (checkout, origin) = checkout_and_origin(ctx, &repo_id)?;
    match method {
        "status" => {
            let local = local_gate(ctx, &origin);
            let parsed = std::fs::read_to_string(&local).ok().map(|t| config::Pipeline::parse(&t));
            let checks = |t: Tier| parsed.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.tier(t).iter().map(|c| c.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
            let lends: Vec<Value> = parsed.as_ref().and_then(|p| p.as_ref().ok()).map(|p| {
                let d = db(ctx).ok();
                p.lends.iter().map(|l| json!({ "name": l, "path": d.as_ref().and_then(|d| records::lent(d, &origin, l)).map(|x| x.display().to_string()) })).collect()
            }).unwrap_or_default();
            let state = runs().lock().ok().and_then(|m| m.get(&repo_id).cloned()).unwrap_or_default();
            Ok(json!({
                "origin": origin,
                "local_gate": local.display().to_string(),
                "local_found": parsed.is_some(),
                "local_error": parsed.as_ref().and_then(|p| p.as_ref().err().cloned()),
                "fast": checks(Tier::Fast),
                "full": checks(Tier::Full),
                "lends": lends,
                "running": state.running,
            }))
        }
        "run" => {
            let gen = a["generation"].as_i64().ok_or("Look the pull request up in Review first")?;
            let tier = Tier::parse(a["tier"].as_str().unwrap_or("fast"))?;
            let (pr, base, head) = generation(ctx, &repo_id, &origin, gen)?;
            {
                let mut m = runs().lock().map_err(|_| "busy")?;
                if m.get(&repo_id).is_some_and(|s| s.running) {
                    return Err("Checks are already running for this checkout".into());
                }
                m.insert(repo_id.clone(), RunState { running: true, generation: gen, tier: tier.as_str().into(), lines: vec![], error: None });
            }
            let (c, key) = (ctx.clone(), repo_id.clone());
            tauri::async_runtime::spawn(async move {
                let result = async {
                    say(&c, &key, format!("getting #{pr} ({}) and its base ({})", &head[..12], &base[..12]));
                    let m = mirror(&checkout, &origin, pr, &[&head, &base]).await?;
                    let local = local_gate(&c, &origin);
                    let g = config::load(&m, &base, Some(&local))?;
                    say(&c, &key, format!("gate from {}{}", g.from, if g.pinned { "" } else { " (unpinned: a local file, not the base commit)" }));
                    let d = db(&c)?;
                    let h = host(&c, &origin);
                    let t = gate::Target { key: &origin, path: &m, pr: Some(pr), head: &head, base: &base, candidate: &head };
                    let (c2, k2) = (c.clone(), key.clone());
                    let out = gate::run_tier(&d, &t, &g, tier, &h, &move |l| say(&c2, &k2, l)).await;
                    std::fs::remove_dir_all(&h.scratch).ok();
                    out
                }
                .await;
                let state = match &result {
                    Ok((_, v)) => v.state.to_string(),
                    Err(_) => "error".into(),
                };
                if let Ok(mut m) = runs().lock() {
                    if let Some(s) = m.get_mut(&key) {
                        s.running = false;
                        s.error = result.as_ref().err().cloned();
                    }
                }
                if let Err(e) = &result {
                    say(&c, &key, format!("stopped: {e}"));
                }
                c.emit("pipeline:done", json!({ "repo_id": key, "state": state }));
            });
            Ok(json!({ "started": true, "pr": pr }))
        }
        "results" => {
            let gen = a["generation"].as_i64().ok_or("Look the pull request up in Review first")?;
            let tier = Tier::parse(a["tier"].as_str().unwrap_or("fast"))?;
            let (pr, base, head) = generation(ctx, &repo_id, &origin, gen)?;
            let state = runs().lock().ok().and_then(|m| m.get(&repo_id).cloned()).filter(|s| s.generation == gen && s.tier == tier.as_str());
            let d = db(ctx)?;
            // The checks the gate declares, from the base's own file when the mirror has it.
            let m = root().join("mirrors").join(format!("{}.git", slug(&origin)));
            let g = if m.join("HEAD").exists() { config::load(&m, &base, Some(&local_gate(ctx, &origin))).ok() } else { config::load(&checkout, &base, Some(&local_gate(ctx, &origin))).ok() };
            let Some(g) = g else {
                return Ok(json!({ "configured": false, "local_gate": local_gate(ctx, &origin).display().to_string(), "running": state.as_ref().is_some_and(|s| s.running), "lines": state.map(|s| s.lines).unwrap_or_default() }));
            };
            let ids: Vec<String> = g.pipeline.tier(tier).iter().map(|c| c.id.clone()).collect();
            let v = verdict::for_candidate(&d, &origin, Some(pr), tier.as_str(), ids.clone(), &base, &head)?;
            let res = records::results(&d, &origin, Some(pr), tier.as_str(), &head)?;
            let outputs: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let side = |s: records::Side| res.iter().find(|(c, x, _)| c == id && *x == s).map(|(_, _, r)| json!({ "status": r.status.as_str(), "reason": r.reason, "duration_ms": r.duration_ms, "sha": r.sha }));
                    let out = d
                        .query(
                            "SELECT r.side, substr(r.output, -4000) AS output FROM pipeline_results r JOIN pipeline_runs u ON u.id=r.run_id WHERE u.repo=?1 AND u.pr=?2 AND u.candidate_sha=?3 AND u.tier=?4 AND r.check_id=?5 ORDER BY r.recorded DESC",
                            params![origin, pr, head, tier.as_str(), id],
                        )
                        .unwrap_or_default();
                    let text = |s: &str| out.iter().find(|r| r["side"] == s).and_then(|r| r["output"].as_str().map(str::to_string));
                    json!({ "id": id, "candidate": side(records::Side::Candidate), "rerun": side(records::Side::Rerun), "base": side(records::Side::Base), "output": text("candidate"), "base_output": text("base") })
                })
                .collect();
            Ok(json!({
                "configured": true, "pr": pr, "head": head, "base": base, "from": g.from, "pinned_gate": g.pinned,
                "verdict": v, "checks": outputs,
                "running": state.as_ref().is_some_and(|s| s.running),
                "error": state.as_ref().and_then(|s| s.error.clone()),
                "lines": state.map(|s| s.lines).unwrap_or_default(),
            }))
        }
        "lend" => {
            let name = arg_str(&a, "name")?;
            let raw = arg_str(&a, "path")?;
            let path = match raw.strip_prefix("~/") {
                Some(rest) => account_home().join(rest),
                None => PathBuf::from(raw),
            };
            records::lend(&db(ctx)?, &origin, &name, &path, "you", &account_home())?;
            Ok(json!({ "lent": name }))
        }
        _ => Err(format!("unknown pipeline.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::gate::tests::{commit, repo};
    use crate::pipeline::runner::tests::tmp;

    #[tokio::test]
    async fn picking_a_pull_request_runs_its_checks_on_the_exact_head_and_base() {
        let scratch = tmp("app");
        let (r, _) = repo(&scratch);
        let base = commit(&r, &["broken-b"], &[]);
        let head = commit(&r, &["broken-a"], &[]);
        let ctx = Ctx::test();
        ctx.db.exec("INSERT INTO repos(id,name,path,default_branch,added_at,remote_url) VALUES('w','w',?1,'main',0,'https://github.com/acme-test/widget-test.git')", [r.display().to_string()]).unwrap();
        ctx.db.with(|c| c.execute_batch("CREATE TABLE IF NOT EXISTS release_refs(seq INTEGER PRIMARY KEY,repo TEXT NOT NULL,origin TEXT NOT NULL,pr INTEGER NOT NULL,base TEXT NOT NULL,head TEXT NOT NULL,observed INTEGER NOT NULL)")).unwrap();
        ctx.db.exec("INSERT INTO release_refs(seq,repo,origin,pr,base,head,observed) VALUES(1,'w','acme-test/widget-test',42,?1,?2,0)", [&base, &head]).unwrap();
        // No gate yet: Review says where one goes, and nothing runs.
        let none = call(&ctx, "results", json!({"repo_id": "w", "generation": 1})).await.unwrap();
        assert_eq!(none["configured"], false);
        let gate = local_gate(&ctx, "acme-test/widget-test");
        std::fs::create_dir_all(gate.parent().unwrap()).unwrap();
        std::fs::write(&gate, format!("version: 1\nbranch: main\nplatforms: [{}]\ntiers: {{ fast: [ {{ id: a, run: sh check.sh a }}, {{ id: b, run: sh check.sh b }} ] }}\n", config::host_platform())).unwrap();
        let st = call(&ctx, "status", json!({"repo_id": "w"})).await.unwrap();
        assert_eq!((st["local_found"].as_bool(), st["fast"].clone()), (Some(true), json!(["a", "b"])));
        call(&ctx, "run", json!({"repo_id": "w", "generation": 1, "tier": "fast"})).await.unwrap();
        assert!(call(&ctx, "run", json!({"repo_id": "w", "generation": 1})).await.unwrap_err().contains("already running"));
        let mut res = Value::Null;
        for _ in 0..600 {
            res = call(&ctx, "results", json!({"repo_id": "w", "generation": 1, "tier": "fast"})).await.unwrap();
            if res["running"] == false && res["verdict"]["outcomes"].as_array().is_some_and(|o| o.len() == 2) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(res["verdict"]["state"], "fail", "{res}");
        assert_eq!(res["verdict"]["pinned"], false, "a local gate is unpinned");
        let outcome = |i: usize| res["verdict"]["outcomes"][i][1]["outcome"].clone();
        assert_eq!((outcome(0), outcome(1)), (json!("introduced"), json!("pre_existing")), "{res}");
        assert_eq!(res["checks"][0]["candidate"]["sha"], head);
        assert_eq!(res["checks"][1]["base"]["sha"], base);
        assert!(res["lines"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("gate from")));
        // The person's checkout is untouched; the commits came from Philium's own mirror.
        let m = root().join("mirrors").join("acme-test-widget-test.git");
        assert!(m.join("HEAD").exists());
        std::fs::remove_dir_all(&m).ok();
        std::fs::remove_dir_all(&scratch).ok();
    }
}
