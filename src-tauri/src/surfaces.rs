//! Embedded App Surfaces & Zero-Interference Host Guard.
//!
//! Two responsibilities:
//! 1. Zero-Interference Shims (`<data_dir>/bin/open` and `xdg-open`): placed first on every agent's
//!    `PATH` and exported as `BROWSER`. Stops shell commands (`open https://...`, `vite --open`,
//!    `gh auth`, `open -a KiCad`) from ever popping open host Firefox or stealing macOS desktop focus.
//! 2. Isolated App Surfaces (KiCad, DBeaver, Chromium, Wireshark, FreeCAD, custom containers):
//!    Detects the best available single Linux container runtime (OrbStack > Colima > Docker > Lima),
//!    launches single-window Xpra/noVNC containers bound strictly to `127.0.0.1` + the active
//!    workspace/worktree + a persistent per-app config volume, and registers every container with
//!    Philium's Reaper so teardown is guaranteed.
//!
//! rpc (`surfaces.*`):
//!   status {} -> {runtime, engines, shim_active, shim_path, pilot}
//!   catalog {} -> [recipe]
//!   list {repo_id?, agent_id?} -> [surface]
//!   add_recipe {id, name, category, image, command?, description?, icon?, default_port?} -> recipe
//!   remove_recipe {id} -> null
//!   launch {recipe_id, repo_id?, agent_id?, workdir?, network?} -> surface
//!   stop {id} -> null
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use rusqlite::params;
use serde_json::{json, Value};

use crate::ctx::{arg_opt_str, arg_str, Ctx};
use crate::db::{new_id, now};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_surface_recipes (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  category TEXT NOT NULL,
  description TEXT NOT NULL,
  image TEXT NOT NULL,
  command TEXT NOT NULL DEFAULT '',
  internal_port INTEGER NOT NULL DEFAULT 10000,
  protocol TEXT NOT NULL DEFAULT 'xpra',
  headless_cli TEXT NOT NULL DEFAULT '',
  builtin INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_surfaces (
  id TEXT PRIMARY KEY,
  recipe_id TEXT NOT NULL,
  name TEXT NOT NULL,
  repo_id TEXT,
  agent_id TEXT,
  workdir TEXT,
  runtime TEXT NOT NULL,
  container_name TEXT NOT NULL,
  container_id TEXT,
  host_port INTEGER NOT NULL,
  url TEXT NOT NULL,
  state TEXT NOT NULL,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
";

const OPEN_SHIM: &str = r#"#!/bin/sh
# Philium Zero-Interference Guard: prevents agent shell commands from opening
# host Firefox, Safari, or macOS desktop apps on the user's screen.
set -eu
agent="${PHILIUM_AGENT:-agent}"
first=""
for arg in "$@"; do
  case "$arg" in
    -*) ;;
    *) first="$arg"; break ;;
  esac
done
case "$first" in
  http://*|https://*)
    printf "Philium Zero-Interference: blocked opening %s in host browser (agent: %s).\nUse your isolated Philium browser tool (browser_open) instead so you never touch the user's Firefox or desktop.\n" "$first" "$agent" >&2
    exit 0
    ;;
  *)
    printf "Philium Zero-Interference: blocked launching host GUI app/file '%s' (agent: %s).\nRun tools headlessly or open an isolated Philium App Surface (e.g. KiCad, DBeaver) instead.\n" "${first:-$*}" "$agent" >&2
    exit 1
    ;;
esac
"#;

pub fn shim_bin_dir(ctx: &Ctx) -> PathBuf {
    ctx.data_dir.join("bin")
}

pub fn open_shim_path(ctx: &Ctx) -> PathBuf {
    shim_bin_dir(ctx).join("open")
}

/// Write executable `open` and `xdg-open` zero-interference shims into `<data_dir>/bin`.
pub fn ensure_shims(ctx: &Ctx) -> Result<PathBuf, String> {
    let dir = shim_bin_dir(ctx);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create shim dir: {e}"))?;
    for name in ["open", "xdg-open"] {
        let p = dir.join(name);
        let needs_write = std::fs::read_to_string(&p).map(|s| s != OPEN_SHIM).unwrap_or(true);
        if needs_write {
            std::fs::write(&p, OPEN_SHIM).map_err(|e| format!("write {name} shim: {e}"))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(dir.join("open"))
}

fn seed_builtin_recipes(ctx: &Ctx) -> Result<(), String> {
    let builtins: &[(&str, &str, &str, &str, &str, &str, i64, &str, &str)] = &[
        ("kicad", "KiCad", "hardware", "PCB & schematics", "lscr.io/linuxserver/kicad:latest", "", 3000, "selkies", "kicad-cli"),
        ("dbeaver", "DBeaver", "database", "SQL & databases", "lscr.io/linuxserver/dbeaver:latest", "", 3000, "selkies", ""),
        ("chromium", "Chromium", "browser", "Sandboxed browser", "lscr.io/linuxserver/chromium:latest", "", 3000, "selkies", ""),
        ("wireshark", "Wireshark", "network", "Packet capture", "lscr.io/linuxserver/wireshark:latest", "", 3000, "selkies", "tshark"),
        ("freecad", "FreeCAD", "hardware", "3D CAD", "lscr.io/linuxserver/freecad:latest", "", 3000, "selkies", "freecadcmd"),
    ];
    let ts = now();
    for (id, name, cat, desc, image, cmd, port, proto, cli) in builtins {
        ctx.db.exec(
            "INSERT INTO app_surface_recipes(id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)
             ON CONFLICT(id) DO NOTHING",
            params![id, name, cat, desc, image, cmd, port, proto, cli, ts],
        )?;
    }
    Ok(())
}

pub fn init(ctx: &Arc<Ctx>) -> Result<(), String> {
    ctx.db.with(|c| c.execute_batch(SCHEMA))?;
    seed_builtin_recipes(ctx)?;
    let _ = ensure_shims(ctx);
    Ok(())
}

fn find_bin(names: &[&str]) -> Option<PathBuf> {
    let candidates = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
    for name in names {
        for dir in candidates {
            let p = Path::new(dir).join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        if let Ok(path) = std::env::var("PATH") {
            for dir in path.split(':') {
                let p = Path::new(dir).join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn detect_engines() -> Value {
    let orb = find_bin(&["orb", "orbctl"]).is_some();
    let colima = find_bin(&["colima"]).is_some();
    let docker = find_bin(&["docker"]).is_some();
    let lima = find_bin(&["limactl"]).is_some();
    let recommended = if orb {
        "orbstack"
    } else if colima {
        "colima"
    } else if docker {
        "docker"
    } else if lima {
        "lima"
    } else {
        "none"
    };
    json!({
        "orbstack": orb,
        "colima": colima,
        "docker": docker,
        "lima": lima,
        "recommended": recommended,
        "docker_ready": docker,
    })
}

fn pick_free_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("allocate port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok(port)
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn build_docker_run_args(
    container_name: &str,
    surface_id: &str,
    agent_id: Option<&str>,
    host_port: u16,
    internal_port: i64,
    workdir: Option<&str>,
    config_dir: &Path,
    image: &str,
    command: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container_name.into(),
        "--label".into(),
        format!("philium.surface={surface_id}"),
        "--label".into(),
        format!("philium.agent={}", agent_id.unwrap_or("workspace")),
        "-p".into(),
        format!("127.0.0.1:{host_port}:{internal_port}"),
        "-e".into(),
        "PUID=1000".into(),
        "-e".into(),
        "PGID=1000".into(),
        "-e".into(),
        "TZ=UTC".into(),
        "-v".into(),
        format!("{}:/config", config_dir.display()),
    ];
    if let Some(wd) = workdir.filter(|w| !w.trim().is_empty()) {
        args.push("-v".into());
        args.push(format!("{wd}:/workspace"));
    }
    args.push(image.into());
    if !command.trim().is_empty() {
        args.push("/bin/sh".into());
        args.push("-c".into());
        args.push(command.trim().into());
    }
    args
}

fn resolve_workdir(ctx: &Arc<Ctx>, repo_id: Option<&str>, agent_id: Option<&str>, explicit: Option<&str>) -> Option<String> {
    if let Some(w) = explicit.filter(|s| !s.trim().is_empty()) {
        return Some(w.to_string());
    }
    if let Some(aid) = agent_id.filter(|s| !s.is_empty()) {
        if let Some(rt) = crate::agents::runtime(aid) {
            return Some(rt.cwd.to_string_lossy().into_owned());
        }
        if let Ok(rows) = ctx.db.query("SELECT path FROM worktrees WHERE agent_id=?1 AND state='active' ORDER BY created_at DESC LIMIT 1", [aid]) {
            if let Some(p) = rows.first().and_then(|r| r["path"].as_str()) {
                return Some(p.to_string());
            }
        }
    }
    if let Some(rid) = repo_id.filter(|s| !s.is_empty()) {
        if let Some(r) = crate::workspace::repo(ctx, rid) {
            if let Some(p) = r["path"].as_str() {
                return Some(p.to_string());
            }
        }
    }
    None
}

pub async fn call(ctx: &Arc<Ctx>, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "status" => {
            let engines = detect_engines();
            let shim = open_shim_path(ctx);
            Ok(json!({
                "runtime": engines["recommended"],
                "engines": engines,
                "shim_active": shim.is_file(),
                "shim_path": shim,
                "pilot": crate::pilot::active(),
                "running": ctx.db.query("SELECT count(*) AS n FROM app_surfaces WHERE state='running'", [])?[0]["n"],
            }))
        }
        "catalog" => {
            let rows = ctx.db.query(
                "SELECT id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin
                 FROM app_surface_recipes ORDER BY builtin DESC, category, name",
                [],
            )?;
            Ok(json!(rows))
        }
        "list" => {
            let repo_id = arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty());
            let agent_id = arg_opt_str(&args, "agent_id").filter(|s| !s.is_empty());
            let rows = ctx.db.query(
                "SELECT id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,error,created_at,updated_at
                 FROM app_surfaces
                 WHERE (?1 IS NULL OR repo_id=?1)
                   AND (?2 IS NULL OR agent_id=?2)
                 ORDER BY (state='running') DESC, updated_at DESC",
                params![repo_id, agent_id],
            )?;
            Ok(json!(rows))
        }
        "add_recipe" => {
            let id = arg_str(&args, "id")?.trim().to_lowercase();
            if !valid_slug(&id) {
                return Err("recipe id must use letters, digits, - and _".into());
            }
            let name = arg_str(&args, "name")?.trim().to_string();
            let image = arg_str(&args, "image")?.trim().to_string();
            if image.contains(char::is_whitespace) {
                return Err("container image cannot contain whitespace".into());
            }
            let category = args["category"].as_str().unwrap_or("custom").trim();
            let description = args["description"].as_str().unwrap_or("Custom isolated Linux app surface").trim();
            let command = args["command"].as_str().unwrap_or("").trim();
            let port = args["internal_port"].as_i64().unwrap_or(3000).clamp(1, 65535);
            let proto = args["protocol"].as_str().unwrap_or("selkies").trim();
            let cli = args["headless_cli"].as_str().unwrap_or("").trim();
            ctx.db.exec(
                "INSERT INTO app_surface_recipes(id,name,category,description,image,command,internal_port,protocol,headless_cli,builtin,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, category=excluded.category,
                 description=excluded.description, image=excluded.image, command=excluded.command,
                 internal_port=excluded.internal_port, protocol=excluded.protocol, headless_cli=excluded.headless_cli",
                params![id, name, category, description, image, command, port, proto, cli, now()],
            )?;
            ctx.notify("surfaces");
            Ok(json!({ "id": id, "name": name, "image": image }))
        }
        "remove_recipe" => {
            let id = arg_str(&args, "id")?;
            let rows = ctx.db.query("SELECT builtin FROM app_surface_recipes WHERE id=?1", [&id])?;
            if rows.first().and_then(|r| r["builtin"].as_i64()) == Some(1) {
                return Err("built-in surface recipes cannot be deleted".into());
            }
            ctx.db.exec("DELETE FROM app_surface_recipes WHERE id=?1", [&id])?;
            ctx.notify("surfaces");
            Ok(Value::Null)
        }
        "launch" => {
            if crate::pilot::active() {
                return Err("Container launches are disabled in the side-by-side pilot".into());
            }
            let recipe_id = arg_str(&args, "recipe_id")?;
            let recipe = ctx
                .db
                .query(
                    "SELECT id,name,image,command,internal_port FROM app_surface_recipes WHERE id=?1",
                    [&recipe_id],
                )?
                .into_iter()
                .next()
                .ok_or_else(|| format!("unknown app surface recipe: {recipe_id}"))?;
            let docker = find_bin(&["docker"]).ok_or_else(|| {
                "No container runtime found. Install OrbStack (recommended for 16 GiB Macs) or Colima to run isolated Linux app surfaces inside Philium.".to_string()
            })?;
            let engines = detect_engines();
            let runtime = engines["recommended"].as_str().unwrap_or("docker").to_string();
            let repo_id = arg_opt_str(&args, "repo_id").filter(|s| !s.is_empty());
            let agent_id = arg_opt_str(&args, "agent_id").filter(|s| !s.is_empty());
            let workdir = resolve_workdir(ctx, repo_id.as_deref(), agent_id.as_deref(), arg_opt_str(&args, "workdir").as_deref());

            let id = format!("surf_{}", new_id());
            let short = id.chars().take(12).collect::<String>();
            let container_name = format!("philium-surface-{recipe_id}-{short}");
            let host_port = pick_free_loopback_port()?;
            let internal_port = recipe["internal_port"].as_i64().unwrap_or(3000);
            let url = format!("http://127.0.0.1:{host_port}/");
            let config_dir = ctx.data_dir.join("surface-configs").join(&recipe_id);
            std::fs::create_dir_all(&config_dir).map_err(|e| format!("create surface config dir: {e}"))?;

            let run_args = build_docker_run_args(
                &container_name,
                &id,
                agent_id.as_deref(),
                host_port,
                internal_port,
                workdir.as_deref(),
                &config_dir,
                recipe["image"].as_str().unwrap_or(""),
                recipe["command"].as_str().unwrap_or(""),
            );

            let docker_bin = docker.clone();
            let out = tokio::task::spawn_blocking(move || Command::new(docker_bin).args(&run_args).output())
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("launch container: {e}"))?;

            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(if err.is_empty() {
                    "Container runtime failed to start surface. Ensure OrbStack or Colima is running.".into()
                } else {
                    err
                });
            }
            let container_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let ts = now();
            let name = recipe["name"].as_str().unwrap_or(&recipe_id).to_string();
            ctx.db.exec(
                "INSERT INTO app_surfaces(id,recipe_id,name,repo_id,agent_id,workdir,runtime,container_name,container_id,host_port,url,state,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'running',?12,?12)",
                params![id, recipe_id, name, repo_id, agent_id, workdir, runtime, container_name, container_id, host_port as i64, url, ts],
            )?;
            let _ = crate::reaper::register(
                ctx,
                agent_id.as_deref(),
                "container",
                &container_name,
                json!({ "surface_id": id, "recipe_id": recipe_id, "port": host_port }),
            );
            ctx.emit("surfaces:changed", json!({ "id": id, "state": "running" }));
            Ok(json!({
                "id": id,
                "recipe_id": recipe_id,
                "name": name,
                "url": url,
                "host_port": host_port,
                "container_name": container_name,
                "workdir": workdir,
                "runtime": runtime,
                "state": "running",
            }))
        }
        "probe" => {
            let id = arg_str(&args, "id")?;
            let row = ctx
                .db
                .query("SELECT host_port, state FROM app_surfaces WHERE id=?1", [&id])?
                .into_iter()
                .next()
                .ok_or_else(|| "surface not found".to_string())?;
            let port = row["host_port"].as_u64().unwrap_or(0) as u16;
            let running = row["state"].as_str() == Some("running");
            if !running || port == 0 {
                return Ok(json!({ "id": id, "ready": false, "state": row["state"] }));
            }
            let ready = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                tokio::net::TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
            Ok(json!({ "id": id, "ready": ready, "port": port, "state": "running" }))
        }
        "stop" => {
            if crate::pilot::active() {
                return Err("Container teardown is disabled in the side-by-side pilot".into());
            }
            let id = arg_str(&args, "id")?;
            let row = ctx
                .db
                .query("SELECT container_name FROM app_surfaces WHERE id=?1", [&id])?
                .into_iter()
                .next()
                .ok_or_else(|| "surface not found".to_string())?;
            let cname = row["container_name"].as_str().unwrap_or("").to_string();
            if let Some(docker) = find_bin(&["docker"]) {
                let c = cname.clone();
                let _ = tokio::task::spawn_blocking(move || Command::new(docker).args(["rm", "-f", &c]).output()).await;
            }
            ctx.db.exec(
                "UPDATE app_surfaces SET state='stopped', updated_at=?2 WHERE id=?1",
                params![id, now()],
            )?;
            if let Ok(res_rows) = ctx.db.query("SELECT id FROM resources WHERE kind='container' AND handle=?1 AND state='live'", [&cname]) {
                for r in res_rows {
                    if let Some(rid) = r["id"].as_str() {
                        crate::reaper::release(ctx, rid);
                    }
                }
            }
            ctx.emit("surfaces:changed", json!({ "id": id, "state": "stopped" }));
            Ok(Value::Null)
        }
        _ => Err(format!("unknown surfaces.{method}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_executable_zero_interference_shims_and_blocks_host_apps() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let shim = open_shim_path(&ctx);
        assert!(shim.is_file());
        assert!(shim_bin_dir(&ctx).join("xdg-open").is_file());

        // URL call exits 0 with guidance, without launching Firefox.
        let url_out = Command::new(&shim)
            .env("PHILIUM_AGENT", "otter")
            .arg("https://example.com")
            .output()
            .unwrap();
        assert!(url_out.status.success());
        assert!(String::from_utf8_lossy(&url_out.stderr).contains("browser_open"));

        // Host GUI app launch is blocked with exit 1.
        let app_out = Command::new(&shim)
            .env("PHILIUM_AGENT", "otter")
            .args(["-a", "KiCad"])
            .output()
            .unwrap();
        assert!(!app_out.status.success());
        assert!(String::from_utf8_lossy(&app_out.stderr).contains("App Surface"));
    }

    #[test]
    fn seeds_builtin_catalog_and_builds_isolated_loopback_docker_args() {
        let ctx = Ctx::test();
        init(&ctx).unwrap();
        let rows = ctx.db.query("SELECT id FROM app_surface_recipes ORDER BY id", []).unwrap();
        let ids: Vec<_> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert!(ids.contains(&"kicad"));
        assert!(ids.contains(&"dbeaver"));
        assert!(ids.contains(&"chromium"));

        let args = build_docker_run_args(
            "philium-surface-kicad-1",
            "surf_1",
            Some("agent_1"),
            43123,
            3000,
            Some("/tmp/wt"),
            Path::new("/tmp/cfg"),
            "lscr.io/linuxserver/kicad:latest",
            "",
        );
        assert!(args.contains(&"127.0.0.1:43123:3000".to_string()));
        assert!(args.contains(&"philium.surface=surf_1".to_string()));
        assert!(args.contains(&"philium.agent=agent_1".to_string()));
        assert!(args.contains(&"/tmp/wt:/workspace".to_string()));
    }
}
