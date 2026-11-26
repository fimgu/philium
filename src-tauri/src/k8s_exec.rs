//! Remote Kubernetes execution target (k3s / EKS / generic clusters).
//!
//! RPC (`k8s_exec.*`):
//!   get {}                                           -> Config
//!   set {enabled?, provider?, kubeconfig?, context?, namespace?, image?, storage_class?, cpu_request?, memory_request?, eks_region?, eks_cluster?} -> Config
//!   contexts {kubeconfig?}                           -> {available, current, contexts[]}
//!   eks_clusters {region?}                           -> {available, region, clusters[]}
//!   eks_connect {region, cluster}                    -> {ok, context, output}
//!   test {}                                          -> {ok, server_version?, namespace_exists, can_create_pods, error?}
//!   pods {}                                          -> {namespace, pods[]}
//!   pod_manifest {agent, image?, repo_url?, branch?, gh_user?} -> Value (Pod JSON)
//!   create_pod {agent, agent_id?, image?, repo_url?, branch?, gh_user?} -> {pod, namespace}
//!   delete_pod {pod, namespace?}                     -> {ok}
//!   exec {pod, command, namespace?}                  -> {ok, code, stdout, stderr}
//!   logs {pod, tail?, namespace?}                    -> {logs}
//!
//! Off by default and fail-closed: invalid config or missing kubectl never touches any cluster.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ctx::{arg_opt_str, arg_str, Ctx};

const KV_KEY: &str = "setting.k8s_exec";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
    /// "k3s" | "eks" | "generic"
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub kubeconfig: String,
    #[serde(default)]
    pub context: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub storage_class: String,
    #[serde(default = "default_cpu")]
    pub cpu_request: String,
    #[serde(default = "default_memory")]
    pub memory_request: String,
    #[serde(default)]
    pub eks_region: String,
    #[serde(default)]
    pub eks_cluster: String,
}

fn default_provider() -> String { "k3s".into() }
fn default_namespace() -> String { "philium-agents".into() }
fn default_cpu() -> String { "500m".into() }
fn default_memory() -> String { "1Gi".into() }

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_provider(),
            kubeconfig: String::new(),
            context: String::new(),
            namespace: default_namespace(),
            image: String::new(),
            storage_class: String::new(),
            cpu_request: default_cpu(),
            memory_request: default_memory(),
            eks_region: String::new(),
            eks_cluster: String::new(),
        }
    }
}

pub fn load(ctx: &Ctx) -> Config {
    ctx.db
        .kv_get(KV_KEY)
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_default()
}

fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn safe_arg(s: &str, field: &str) -> Result<(), String> {
    if s.starts_with('-') || s.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(format!("Invalid {field}"));
    }
    Ok(())
}

pub fn validate(cfg: &Config) -> Result<(), String> {
    if !matches!(cfg.provider.as_str(), "k3s" | "eks" | "generic") {
        return Err("provider must be k3s, eks, or generic".into());
    }
    if !is_dns_label(&cfg.namespace) {
        return Err("namespace must be a lowercase DNS-1123 label (a-z, 0-9, '-')".into());
    }
    safe_arg(&cfg.context, "context")?;
    safe_arg(&cfg.kubeconfig, "kubeconfig")?;
    safe_arg(&cfg.image, "image")?;
    safe_arg(&cfg.storage_class, "storage_class")?;
    safe_arg(&cfg.cpu_request, "cpu_request")?;
    safe_arg(&cfg.memory_request, "memory_request")?;
    safe_arg(&cfg.eks_region, "eks_region")?;
    safe_arg(&cfg.eks_cluster, "eks_cluster")?;
    Ok(())
}

pub fn save(ctx: &Ctx, cfg: &Config) -> Result<Config, String> {
    validate(cfg)?;
    let raw = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
    ctx.db.kv_set(KV_KEY, &raw)?;
    ctx.emit("k8s_exec:changed", json!({ "enabled": cfg.enabled, "context": cfg.context, "namespace": cfg.namespace }));
    Ok(cfg.clone())
}

fn kubectl_cmd(cfg: &Config) -> Command {
    let mut cmd = Command::new("kubectl");
    if !cfg.kubeconfig.trim().is_empty() {
        cmd.env("KUBECONFIG", cfg.kubeconfig.trim());
    }
    if !cfg.context.trim().is_empty() {
        cmd.arg("--context").arg(cfg.context.trim());
    }
    cmd
}

fn run_kubectl(cfg: &Config, args: &[&str], stdin_data: Option<&str>) -> Result<(bool, i32, String, String), String> {
    let mut cmd = kubectl_cmd(cfg);
    cmd.args(args)
        .stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("kubectl unavailable: {e}"))?;
    if let Some(body) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let code = out.status.code().unwrap_or(-1);
    Ok((
        out.status.success(),
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

pub fn sanitize_pod_name(agent: &str) -> String {
    let clean: String = agent
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' })
        .collect();
    let trimmed = clean.trim_matches('-');
    let suffix = if trimmed.is_empty() { "agent" } else { trimmed };
    let full = format!("philium-{suffix}");
    full.chars().take(63).collect::<String>().trim_end_matches('-').to_string()
}

/// Deterministic, non-root Pod manifest for one Philium agent running in k3s or EKS.
pub fn build_pod_manifest(
    cfg: &Config,
    agent: &str,
    image_override: Option<&str>,
    repo_url: Option<&str>,
    branch: Option<&str>,
    gh_user: Option<&str>,
) -> Result<Value, String> {
    validate(cfg)?;
    let image = image_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| (!cfg.image.trim().is_empty()).then_some(cfg.image.trim()))
        .ok_or("Configure a runner container image in Settings -> Remote Execution before creating a pod")?;
    safe_arg(image, "image")?;
    let pod_name = sanitize_pod_name(agent);
    let mut env = vec![
        json!({ "name": "PHILIUM_AGENT", "value": agent }),
        json!({ "name": "PHILIUM_EXEC_TARGET", "value": cfg.provider }),
    ];
    if let Some(url) = repo_url.filter(|s| !s.trim().is_empty()) {
        env.push(json!({ "name": "PHILIUM_REPO_URL", "value": url.trim() }));
    }
    if let Some(br) = branch.filter(|s| !s.trim().is_empty()) {
        env.push(json!({ "name": "PHILIUM_BRANCH", "value": br.trim() }));
    }
    if let Some(u) = gh_user.filter(|s| !s.trim().is_empty()) {
        env.push(json!({ "name": "PHILIUM_GH_USER", "value": u.trim() }));
    }
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": pod_name,
            "namespace": cfg.namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "philium",
                "philium.dev/agent": agent,
                "philium.dev/provider": cfg.provider
            }
        },
        "spec": {
            "restartPolicy": "Never",
            "automountServiceAccountToken": false,
            "containers": [{
                "name": "agent",
                "image": image,
                "command": ["sh", "-lc", "sleep infinity"],
                "workingDir": "/workspace",
                "env": env,
                "resources": {
                    "requests": {
                        "cpu": cfg.cpu_request,
                        "memory": cfg.memory_request
                    }
                },
                "volumeMounts": [{
                    "name": "workspace",
                    "mountPath": "/workspace"
                }]
            }],
            "volumes": [{
                "name": "workspace",
                "emptyDir": {}
            }]
        }
    }))
}

/// Safe teardown invoked by `reaper::destroy` for `kind == "k8s_pod"`.
pub fn delete_pod_by_handle(ctx: &Ctx, handle: &str, namespace: Option<&str>) -> Result<(), String> {
    if !handle.starts_with("philium-") || !is_dns_label(handle) {
        return Err("Refusing to delete a pod without the philium- ownership prefix".into());
    }
    let cfg = load(ctx);
    let ns = namespace.filter(|s| is_dns_label(s)).unwrap_or(cfg.namespace.as_str());
    let (ok, _, _, stderr) = run_kubectl(
        &cfg,
        &["delete", "pod", handle, "-n", ns, "--ignore-not-found", "--wait=false"],
        None,
    )?;
    if !ok {
        return Err(stderr.trim().to_string());
    }
    Ok(())
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "get" => Ok(serde_json::to_value(load(ctx)).map_err(|e| e.to_string())?),
        "set" => {
            let mut cfg = load(ctx);
            if let Some(v) = args["enabled"].as_bool() { cfg.enabled = v; }
            if let Some(v) = args["provider"].as_str() { cfg.provider = v.trim().to_ascii_lowercase(); }
            if let Some(v) = args["kubeconfig"].as_str() { cfg.kubeconfig = v.trim().to_string(); }
            if let Some(v) = args["context"].as_str() { cfg.context = v.trim().to_string(); }
            if let Some(v) = args["namespace"].as_str() { cfg.namespace = v.trim().to_string(); }
            if let Some(v) = args["image"].as_str() { cfg.image = v.trim().to_string(); }
            if let Some(v) = args["storage_class"].as_str() { cfg.storage_class = v.trim().to_string(); }
            if let Some(v) = args["cpu_request"].as_str() { cfg.cpu_request = v.trim().to_string(); }
            if let Some(v) = args["memory_request"].as_str() { cfg.memory_request = v.trim().to_string(); }
            if let Some(v) = args["eks_region"].as_str() { cfg.eks_region = v.trim().to_string(); }
            if let Some(v) = args["eks_cluster"].as_str() { cfg.eks_cluster = v.trim().to_string(); }
            let saved = save(ctx, &cfg)?;
            Ok(serde_json::to_value(saved).map_err(|e| e.to_string())?)
        }
        "contexts" => {
            let mut cfg = load(ctx);
            if let Some(kc) = arg_opt_str(&args, "kubeconfig") {
                safe_arg(&kc, "kubeconfig")?;
                cfg.kubeconfig = kc;
            }
            tokio::task::spawn_blocking(move || {
                let mut list_cmd = Command::new("kubectl");
                if !cfg.kubeconfig.trim().is_empty() {
                    list_cmd.env("KUBECONFIG", cfg.kubeconfig.trim());
                }
                let Ok(out) = list_cmd.args(["config", "get-contexts", "-o", "name"]).output() else {
                    return Ok(json!({ "available": false, "current": "", "contexts": [] }));
                };
                let contexts: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                let mut cur_cmd = Command::new("kubectl");
                if !cfg.kubeconfig.trim().is_empty() {
                    cur_cmd.env("KUBECONFIG", cfg.kubeconfig.trim());
                }
                let current = cur_cmd
                    .args(["config", "current-context"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                Ok(json!({ "available": true, "current": current, "contexts": contexts }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "eks_clusters" => {
            let cfg = load(ctx);
            let region = arg_opt_str(&args, "region").unwrap_or(cfg.eks_region);
            safe_arg(&region, "region")?;
            tokio::task::spawn_blocking(move || {
                let mut cmd = Command::new("aws");
                cmd.args(["eks", "list-clusters", "--output", "json"]);
                if !region.trim().is_empty() {
                    cmd.args(["--region", region.trim()]);
                }
                let Ok(out) = cmd.output() else {
                    return Ok(json!({ "available": false, "region": region, "clusters": [], "error": "aws CLI not found" }));
                };
                if !out.status.success() {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    return Ok(json!({ "available": true, "region": region, "clusters": [], "error": err }));
                }
                let parsed: Value = serde_json::from_slice(&out.stdout).unwrap_or(json!({}));
                let clusters = parsed["clusters"].clone();
                Ok(json!({ "available": true, "region": region, "clusters": clusters }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "eks_connect" => {
            let region = arg_str(&args, "region")?;
            let cluster = arg_str(&args, "cluster")?;
            safe_arg(&region, "region")?;
            safe_arg(&cluster, "cluster")?;
            let mut cfg = load(ctx);
            let kc = cfg.kubeconfig.clone();
            let (ok, output) = tokio::task::spawn_blocking(move || -> Result<(bool, String), String> {
                let mut cmd = Command::new("aws");
                cmd.args(["eks", "update-kubeconfig", "--region", region.trim(), "--name", cluster.trim()]);
                if !kc.trim().is_empty() {
                    cmd.args(["--kubeconfig", kc.trim()]);
                }
                let out = cmd.output().map_err(|e| format!("aws CLI unavailable: {e}"))?;
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout).trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                )
                .trim()
                .to_string();
                Ok((out.status.success(), combined))
            })
            .await
            .map_err(|e| e.to_string())??;
            if !ok {
                return Err(output);
            }
            cfg.provider = "eks".into();
            cfg.eks_region = arg_str(&args, "region")?;
            cfg.eks_cluster = arg_str(&args, "cluster")?;
            // Capture the newly selected current-context.
            let kc2 = cfg.kubeconfig.clone();
            let cur_ctx = tokio::task::spawn_blocking(move || {
                let mut cmd = Command::new("kubectl");
                if !kc2.trim().is_empty() { cmd.env("KUBECONFIG", kc2.trim()); }
                cmd.args(["config", "current-context"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            if !cur_ctx.is_empty() {
                cfg.context = cur_ctx.clone();
            }
            let _ = save(ctx, &cfg);
            Ok(json!({ "ok": true, "context": cur_ctx, "output": output }))
        }
        "test" => {
            let cfg = load(ctx);
            validate(&cfg)?;
            tokio::task::spawn_blocking(move || {
                let (ok, _, ver_out, ver_err) = match run_kubectl(&cfg, &["version", "--output=json"], None) {
                    Ok(v) => v,
                    Err(e) => return Ok(json!({ "ok": false, "namespace_exists": false, "can_create_pods": false, "error": e })),
                };
                if !ok {
                    return Ok(json!({
                        "ok": false,
                        "namespace_exists": false,
                        "can_create_pods": false,
                        "error": ver_err.trim(),
                    }));
                }
                let ver_json: Value = serde_json::from_str(&ver_out).unwrap_or(json!({}));
                let server_version = ver_json["serverVersion"]["gitVersion"].as_str().unwrap_or("").to_string();
                let (ns_ok, _, _, _) = run_kubectl(&cfg, &["get", "namespace", &cfg.namespace, "-o", "name"], None)
                    .unwrap_or((false, 1, String::new(), String::new()));
                let (can_ok, _, can_out, _) = run_kubectl(&cfg, &["auth", "can-i", "create", "pods", "-n", &cfg.namespace], None)
                    .unwrap_or((false, 1, String::new(), String::new()));
                let can_create_pods = can_ok && can_out.trim().eq_ignore_ascii_case("yes");
                Ok(json!({
                    "ok": true,
                    "server_version": server_version,
                    "namespace": cfg.namespace,
                    "namespace_exists": ns_ok,
                    "can_create_pods": can_create_pods,
                }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "pods" => {
            let cfg = load(ctx);
            validate(&cfg)?;
            tokio::task::spawn_blocking(move || {
                let (ok, _, stdout, stderr) = run_kubectl(
                    &cfg,
                    &["get", "pods", "-n", &cfg.namespace, "-l", "app.kubernetes.io/managed-by=philium", "-o", "json"],
                    None,
                )?;
                if !ok {
                    if stderr.contains("NotFound") {
                        return Ok(json!({ "namespace": cfg.namespace, "pods": [] }));
                    }
                    return Err(stderr.trim().to_string());
                }
                let v: Value = serde_json::from_str(&stdout).unwrap_or(json!({}));
                let pods: Vec<Value> = v["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        json!({
                            "name": p["metadata"]["name"].as_str().unwrap_or(""),
                            "namespace": p["metadata"]["namespace"].as_str().unwrap_or(&cfg.namespace),
                            "agent": p["metadata"]["labels"]["philium.dev/agent"].as_str().unwrap_or(""),
                            "phase": p["status"]["phase"].as_str().unwrap_or("Unknown"),
                            "node": p["spec"]["nodeName"].as_str().unwrap_or(""),
                            "created_at": p["metadata"]["creationTimestamp"].as_str().unwrap_or(""),
                        })
                    })
                    .collect();
                Ok(json!({ "namespace": cfg.namespace, "pods": pods }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "pod_manifest" => {
            let cfg = load(ctx);
            let agent = arg_str(&args, "agent")?;
            build_pod_manifest(
                &cfg,
                &agent,
                arg_opt_str(&args, "image").as_deref(),
                arg_opt_str(&args, "repo_url").as_deref(),
                arg_opt_str(&args, "branch").as_deref(),
                arg_opt_str(&args, "gh_user").as_deref(),
            )
        }
        "create_pod" => {
            let cfg = load(ctx);
            if !cfg.enabled {
                return Err("Remote Kubernetes execution is disabled. Enable it in Settings -> Remote Execution.".into());
            }
            let agent = arg_str(&args, "agent")?;
            let agent_id = arg_opt_str(&args, "agent_id");
            let manifest = build_pod_manifest(
                &cfg,
                &agent,
                arg_opt_str(&args, "image").as_deref(),
                arg_opt_str(&args, "repo_url").as_deref(),
                arg_opt_str(&args, "branch").as_deref(),
                arg_opt_str(&args, "gh_user").as_deref(),
            )?;
            let pod_name = manifest["metadata"]["name"].as_str().unwrap_or("").to_string();
            let ns = cfg.namespace.clone();
            let body = serde_json::to_string(&manifest).map_err(|e| e.to_string())?;
            let cfg_run = cfg.clone();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                // Ensure namespace exists (idempotent).
                let ns_manifest = json!({
                    "apiVersion": "v1",
                    "kind": "Namespace",
                    "metadata": { "name": cfg_run.namespace }
                })
                .to_string();
                let _ = run_kubectl(&cfg_run, &["apply", "-f", "-"], Some(&ns_manifest));
                let (ok, _, _, stderr) = run_kubectl(&cfg_run, &["apply", "-f", "-"], Some(&body))?;
                if !ok {
                    return Err(stderr.trim().to_string());
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())??;
            crate::reaper::register(
                ctx,
                agent_id.as_deref(),
                "k8s_pod",
                &pod_name,
                json!({ "namespace": ns, "agent_name": agent, "context": cfg.context }),
            );
            Ok(json!({ "pod": pod_name, "namespace": ns }))
        }
        "delete_pod" => {
            let pod = arg_str(&args, "pod")?;
            let ns = arg_opt_str(&args, "namespace");
            let c = ctx.clone();
            let p2 = pod.clone();
            let ns2 = ns.clone();
            tokio::task::spawn_blocking(move || delete_pod_by_handle(&c, &p2, ns2.as_deref()))
                .await
                .map_err(|e| e.to_string())??;
            let _ = ctx.db.exec(
                "UPDATE resources SET state='released', released_at=?1 WHERE kind='k8s_pod' AND handle=?2 AND state!='released'",
                rusqlite::params![crate::db::now(), pod],
            );
            Ok(json!({ "ok": true }))
        }
        "exec" => {
            let cfg = load(ctx);
            let pod = arg_str(&args, "pod")?;
            if !pod.starts_with("philium-") || !is_dns_label(&pod) {
                return Err("Refusing to exec in a non-Philium pod".into());
            }
            let command = arg_str(&args, "command")?;
            let ns = arg_opt_str(&args, "namespace").unwrap_or_else(|| cfg.namespace.clone());
            if !is_dns_label(&ns) {
                return Err("Invalid namespace".into());
            }
            tokio::task::spawn_blocking(move || {
                let (ok, code, stdout, stderr) = run_kubectl(
                    &cfg,
                    &["exec", "-n", &ns, &pod, "--", "sh", "-lc", &command],
                    None,
                )?;
                Ok(json!({ "ok": ok, "code": code, "stdout": stdout, "stderr": stderr }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        "logs" => {
            let cfg = load(ctx);
            let pod = arg_str(&args, "pod")?;
            if !pod.starts_with("philium-") || !is_dns_label(&pod) {
                return Err("Refusing to read logs from a non-Philium pod".into());
            }
            let ns = arg_opt_str(&args, "namespace").unwrap_or_else(|| cfg.namespace.clone());
            if !is_dns_label(&ns) {
                return Err("Invalid namespace".into());
            }
            let tail = args["tail"].as_u64().unwrap_or(200).min(2000).to_string();
            tokio::task::spawn_blocking(move || {
                let (ok, _, stdout, stderr) = run_kubectl(
                    &cfg,
                    &["logs", "-n", &ns, &pod, "--tail", &tail],
                    None,
                )?;
                if !ok {
                    return Err(stderr.trim().to_string());
                }
                Ok(json!({ "logs": stdout }))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Err(format!("unknown k8s_exec.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_and_validation() {
        let ctx = Ctx::test();
        let def = load(&ctx);
        assert!(!def.enabled);
        assert_eq!(def.namespace, "philium-agents");

        let mut cfg = def.clone();
        cfg.enabled = true;
        cfg.provider = "eks".into();
        cfg.context = "arn:aws:eks:eu-north-1:123456789012:cluster/home".into();
        cfg.namespace = "philium-work".into();
        cfg.eks_region = "eu-north-1".into();
        cfg.eks_cluster = "home".into();
        let saved = save(&ctx, &cfg).expect("valid config should save");
        assert_eq!(load(&ctx), saved);

        // Reject invalid namespace and flag injection.
        let mut bad_ns = cfg.clone();
        bad_ns.namespace = "UPPER_BAD".into();
        assert!(validate(&bad_ns).is_err());

        let mut bad_ctx = cfg.clone();
        bad_ctx.context = "--kubeconfig=/etc/passwd".into();
        assert!(validate(&bad_ctx).is_err());
    }

    #[test]
    fn pod_manifest_is_hardened_and_scoped() {
        let mut cfg = Config::default();
        cfg.provider = "k3s".into();
        cfg.namespace = "philium-agents".into();
        cfg.image = "ghcr.io/example/philium-runner".into();

        let pod = build_pod_manifest(
            &cfg,
            "Lion_01",
            None,
            Some("https://github.com/adaptive-scale/inventorize.git"),
            Some("release/v1.1.x"),
            Some("filipmgunnar"),
        )
        .expect("manifest");

        assert_eq!(pod["metadata"]["name"], "philium-lion-01");
        assert_eq!(pod["metadata"]["namespace"], "philium-agents");
        assert_eq!(pod["metadata"]["labels"]["app.kubernetes.io/managed-by"], "philium");
        assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
        let envs = pod["spec"]["containers"][0]["env"].as_array().unwrap();
        assert!(envs.iter().any(|e| e["name"] == "PHILIUM_GH_USER" && e["value"] == "filipmgunnar"));
        assert!(!envs.iter().any(|e| e["name"].as_str().unwrap_or("").contains("TOKEN")));
    }

    #[test]
    fn delete_pod_rejects_non_philium_handles() {
        let ctx = Ctx::test();
        assert!(delete_pod_by_handle(&ctx, "kube-system-coredns", Some("kube-system")).is_err());
        assert!(delete_pod_by_handle(&ctx, "--all", None).is_err());
    }
}
