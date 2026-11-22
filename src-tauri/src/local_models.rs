//! In-process Memory models: a small embedder and a small cross-encoder reranker, run on ONNX
//! Runtime inside Philium. No servers, no Python, nothing leaves the machine once downloaded.
//!
//! Weights come from pinned Hugging Face revisions, are sha256-verified while streaming, and live
//! in `<data_dir>/models/<id>/`. A model is "ready" only when every file is present with its
//! pinned size and the `.verified` marker matches the pinned revision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fastembed::{
    InitOptionsUserDefined, OnnxSource, Pooling, QuantizationMode, RerankInitOptionsUserDefined, TextEmbedding,
    TextRerank, TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::ctx::Ctx;

pub struct ModelFile {
    pub remote: &'static str,
    pub local: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub struct Spec {
    pub id: &'static str,
    pub label: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    /// Stored with every vector / rerank result so versions never mix.
    pub version: &'static str,
    pub files: &'static [ModelFile],
}

impl Spec {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// BAAI bge-small-en-v1.5, int8 (Qdrant export). 384 dims, CLS pooling, MIT.
pub const EMBED: Spec = Spec {
    id: "bge-small-en-v1.5-q",
    label: "bge-small-en-v1.5",
    repo: "Qdrant/bge-small-en-v1.5-onnx-Q",
    revision: "aa8f8b060edb00e03bfdd08813a2949946c8ba55",
    version: "local:bge-small-en-v1.5-q@aa8f8b0",
    files: &[
        ModelFile { remote: "model_optimized.onnx", local: "model.onnx", sha256: "51f1bd0addd6e859e42c2c8021a5e5461385bb676a649f4b269aa445449f2431", size: 66_465_124 },
        ModelFile { remote: "tokenizer.json", local: "tokenizer.json", sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66", size: 711_396 },
        ModelFile { remote: "config.json", local: "config.json", sha256: "13582bcf2effc85b7bf3d3f5532e686bc1c9ce86bb009d10f0ec33cbe92299dd", size: 706 },
        ModelFile { remote: "special_tokens_map.json", local: "special_tokens_map.json", sha256: "5d5b662e421ea9fac075174bb0688ee0d9431699900b90662acd44b2a350503a", size: 695 },
        ModelFile { remote: "tokenizer_config.json", local: "tokenizer_config.json", sha256: "0b29c7bfc889e53b36d9dd3e686dd4300f6525110eaa98c76a5dafceb2029f53", size: 1_242 },
    ],
};

/// ms-marco-MiniLM-L-6-v2 cross-encoder, int8 (Xenova export of cross-encoder/…), Apache-2.0.
/// Picked over jina-reranker-v1-turbo int8 on a memory-shaped probe: 9/10 vs 7/10, and 23 MB.
pub const RERANK: Spec = Spec {
    id: "ms-marco-minilm-l6-v2-q",
    label: "ms-marco-MiniLM-L-6-v2",
    repo: "Xenova/ms-marco-MiniLM-L-6-v2",
    revision: "a09144355adeed5f58c8ed011d209bf8ee5a1fec",
    version: "local:ms-marco-minilm-l6-v2-q@a091443",
    files: &[
        ModelFile { remote: "onnx/model_quantized.onnx", local: "model.onnx", sha256: "e9d8ebf845c413e981c175bfe49a3bfa9b3dcce2a3ba54875ee5df5a58639fbe", size: 23_143_499 },
        ModelFile { remote: "tokenizer.json", local: "tokenizer.json", sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66", size: 711_396 },
        ModelFile { remote: "config.json", local: "config.json", sha256: "d827779a72d27ae68cf878a6fc2e954542663fe21ca515d9f4783fc96be2d37e", size: 824 },
        ModelFile { remote: "special_tokens_map.json", local: "special_tokens_map.json", sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3", size: 125 },
        ModelFile { remote: "tokenizer_config.json", local: "tokenizer_config.json", sha256: "0b29c7bfc889e53b36d9dd3e686dd4300f6525110eaa98c76a5dafceb2029f53", size: 1_242 },
    ],
};

pub const SPECS: [&Spec; 2] = [&EMBED, &RERANK];
const MARKER: &str = ".verified";
const OFF_KEY: &str = "setting.local_models";

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

fn spec_dir(data_dir: &Path, spec: &Spec) -> PathBuf {
    models_dir(data_dir).join(spec.id)
}

/// The user can turn local models off (Settings). On by default.
pub fn enabled(ctx: &Ctx) -> bool {
    ctx.db.kv_get(OFF_KEY).as_deref() != Some("off")
}

/// Every pinned file present at its pinned size, and the marker names the pinned revision.
pub fn installed(data_dir: &Path, spec: &Spec) -> bool {
    let dir = spec_dir(data_dir, spec);
    std::fs::read_to_string(dir.join(MARKER)).is_ok_and(|m| m.trim() == spec.revision)
        && spec
            .files
            .iter()
            .all(|f| std::fs::metadata(dir.join(f.local)).is_ok_and(|m| m.len() == f.size))
}

pub fn ready(ctx: &Ctx, spec: &Spec) -> bool {
    enabled(ctx) && installed(&ctx.data_dir, spec)
}

// ---------- Download state ----------

#[derive(Clone, Default)]
struct Progress {
    running: bool,
    done: u64,
    error: Option<String>,
}

fn progress() -> &'static Mutex<HashMap<(PathBuf, &'static str), Progress>> {
    static P: OnceLock<Mutex<HashMap<(PathBuf, &'static str), Progress>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn set_progress(data_dir: &Path, id: &'static str, f: impl FnOnce(&mut Progress)) {
    let mut g = progress().lock().unwrap_or_else(|e| e.into_inner());
    f(g.entry((data_dir.to_path_buf(), id)).or_default());
}

fn get_progress(data_dir: &Path, id: &'static str) -> Progress {
    let g = progress().lock().unwrap_or_else(|e| e.into_inner());
    g.get(&(data_dir.to_path_buf(), id)).cloned().unwrap_or_default()
}

fn spec_status(ctx: &Ctx, spec: &Spec) -> Value {
    let p = get_progress(&ctx.data_dir, spec.id);
    let state = if installed(&ctx.data_dir, spec) {
        "ready"
    } else if p.running {
        "downloading"
    } else if p.error.is_some() {
        "failed"
    } else {
        "missing"
    };
    json!({
        "id": spec.id,
        "label": spec.label,
        "version": spec.version,
        "state": state,
        "bytes_done": p.done,
        "bytes_total": spec.bytes(),
        "error": p.error,
    })
}

pub fn status(ctx: &Ctx) -> Value {
    json!({
        "enabled": enabled(ctx),
        "embed": spec_status(ctx, &EMBED),
        "rerank": spec_status(ctx, &RERANK),
    })
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .user_agent("Philium")
            .build()
            .unwrap_or_default()
    })
}

/// Stream one file to `<name>.part`, hashing as it goes; rename only when size and sha256 match.
async fn fetch_file(base: &str, dir: &Path, f: &ModelFile, on_bytes: &(dyn Fn(u64) + Sync)) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let dest = dir.join(f.local);
    if std::fs::metadata(&dest).is_ok_and(|m| m.len() == f.size) && sha256_file(&dest).as_deref() == Some(f.sha256) {
        on_bytes(f.size);
        return Ok(());
    }
    let url = format!("{base}/{}", f.remote);
    let resp = http().get(&url).send().await.map_err(|e| format!("{}: {e}", f.remote))?;
    if !resp.status().is_success() {
        return Err(format!("{}: HTTP {}", f.remote, resp.status()));
    }
    let part = dir.join(format!("{}.part", f.local));
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut n = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("{}: {e}", f.remote))?;
        n += chunk.len() as u64;
        if n > f.size {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!("{}: larger than pinned size", f.remote));
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        on_bytes(chunk.len() as u64);
    }
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    let got = hex(&hasher.finalize());
    if n != f.size || got != f.sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("{}: checksum mismatch", f.remote));
    }
    tokio::fs::rename(&part, &dest).await.map_err(|e| e.to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha256_file(p: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(hex(&h.finalize()))
}

async fn download_spec_from(base_root: &str, data_dir: &Path, spec: &'static Spec) -> Result<(), String> {
    if installed(data_dir, spec) {
        return Ok(());
    }
    {
        let mut g = progress().lock().unwrap_or_else(|e| e.into_inner());
        let p = g.entry((data_dir.to_path_buf(), spec.id)).or_default();
        if p.running {
            return Ok(());
        }
        *p = Progress { running: true, done: 0, error: None };
    }
    let dir = spec_dir(data_dir, spec);
    let base = format!("{}/{}/resolve/{}", base_root.trim_end_matches('/'), spec.repo, spec.revision);
    let dd = data_dir.to_path_buf();
    let on_bytes = move |n: u64| set_progress(&dd, spec.id, |p| p.done += n);
    let res = async {
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        let _ = tokio::fs::remove_file(dir.join(MARKER)).await;
        for f in spec.files {
            fetch_file(&base, &dir, f, &on_bytes).await?;
        }
        tokio::fs::write(dir.join(MARKER), spec.revision).await.map_err(|e| e.to_string())
    }
    .await;
    set_progress(data_dir, spec.id, |p| {
        p.running = false;
        p.error = res.as_ref().err().cloned();
    });
    res
}

const HF: &str = "https://huggingface.co";

/// Download any missing model, then index memories that lack a vector for the local embedder.
pub async fn ensure(ctx: &Arc<Ctx>) -> Value {
    if !enabled(ctx) {
        return status(ctx);
    }
    let mut errors = vec![];
    for spec in SPECS {
        let before = installed(&ctx.data_dir, spec);
        match download_spec_from(HF, &ctx.data_dir, spec).await {
            Ok(()) if !before && installed(&ctx.data_dir, spec) => {
                ctx.log_event("memory:model_ready", None, &json!({ "model": spec.version }));
            }
            Ok(()) => {}
            Err(e) => errors.push(format!("{}: {e}", spec.label)),
        }
        ctx.notify("library");
    }
    if !errors.is_empty() {
        ctx.log_event("memory:model_download_failed", None, &json!({ "errors": errors }));
    }
    if ready(ctx, &EMBED) {
        let _ = crate::library::sync_vectors_async(ctx).await;
        ctx.notify("library");
    }
    status(ctx)
}

/// App start: fetch models in the background (no-op when present or turned off).
pub fn start(ctx: &Arc<Ctx>) {
    let c = ctx.clone();
    tauri::async_runtime::spawn(async move {
        let _ = ensure(&c).await;
    });
}

pub fn set_enabled(ctx: &Ctx, on: bool) -> Result<(), String> {
    ctx.db.kv_set(OFF_KEY, if on { "on" } else { "off" })?;
    ctx.notify("library");
    Ok(())
}

// ---------- Inference ----------

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get().clamp(1, 8) / 2).unwrap_or(2).max(1)
}

fn tokenizer_files(dir: &Path) -> Result<TokenizerFiles, String> {
    let read = |n: &str| std::fs::read(dir.join(n)).map_err(|e| format!("{n}: {e}"));
    Ok(TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    })
}

type Loaded<T> = Mutex<Option<(PathBuf, T)>>;

fn embedder() -> &'static Loaded<TextEmbedding> {
    static E: OnceLock<Loaded<TextEmbedding>> = OnceLock::new();
    E.get_or_init(Default::default)
}

fn reranker() -> &'static Loaded<TextRerank> {
    static R: OnceLock<Loaded<TextRerank>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn load_embedder(dir: &Path) -> Result<TextEmbedding, String> {
    let onnx = std::fs::read(dir.join("model.onnx")).map_err(|e| e.to_string())?;
    let model = UserDefinedEmbeddingModel::new(onnx, tokenizer_files(dir)?)
        .with_pooling(Pooling::Cls)
        .with_quantization(QuantizationMode::Static);
    TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new().with_max_length(512).with_intra_threads(threads()))
        .map_err(|e| format!("load embedder: {e}"))
}

fn load_reranker(dir: &Path) -> Result<TextRerank, String> {
    let model = UserDefinedRerankingModel::new(OnnxSource::File(dir.join("model.onnx")), tokenizer_files(dir)?);
    TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::new().with_max_length(512).with_intra_threads(threads()))
        .map_err(|e| format!("load reranker: {e}"))
}

/// Blocking. L2-normalized 384-dim vectors, one per input.
pub fn embed(data_dir: &Path, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    if !installed(data_dir, &EMBED) {
        return Err("local embedder not downloaded".into());
    }
    let dir = spec_dir(data_dir, &EMBED);
    let mut g = embedder().lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(d, _)| d != &dir) {
        *g = Some((dir.clone(), load_embedder(&dir)?));
    }
    let (_, m) = g.as_mut().expect("loaded");
    m.embed(texts, Some(32)).map_err(|e| format!("embed: {e}"))
}

/// Blocking. `(index, score)` for every document, best first.
pub fn rerank(data_dir: &Path, query: &str, docs: &[String]) -> Result<Vec<(usize, f64)>, String> {
    if docs.is_empty() {
        return Ok(vec![]);
    }
    if !installed(data_dir, &RERANK) {
        return Err("local reranker not downloaded".into());
    }
    let dir = spec_dir(data_dir, &RERANK);
    let mut g = reranker().lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(d, _)| d != &dir) {
        *g = Some((dir.clone(), load_reranker(&dir)?));
    }
    let (_, m) = g.as_mut().expect("loaded");
    let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let out = m.rerank(query, refs, false, Some(16)).map_err(|e| format!("rerank: {e}"))?;
    Ok(out.into_iter().map(|r| (r.index, r.score as f64)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Path as AxPath, routing::get, Router};

    static SMALL: Spec = Spec {
        id: "fixture",
        label: "fixture",
        repo: "org/fixture",
        revision: "rev1",
        version: "local:fixture@rev1",
        files: &[
            // sha256("hello")
            ModelFile { remote: "a.txt", local: "a.txt", sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", size: 5 },
            ModelFile { remote: "dir/b.txt", local: "b.txt", sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824", size: 5 },
        ],
    };

    async fn serve(body_b: &'static str) -> String {
        let app = Router::new().route(
            "/org/fixture/resolve/rev1/{*p}",
            get(move |AxPath(p): AxPath<String>| async move { if p == "a.txt" { "hello" } else { body_b } }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn download_verifies_checksums_and_never_marks_a_tampered_model_ready() {
        let dd = crate::ctx::test_dir("models");
        let bad = serve("hellO").await;
        let err = download_spec_from(&bad, &dd, &SMALL).await.unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert!(!installed(&dd, &SMALL));
        assert!(!spec_dir(&dd, &SMALL).join("b.txt").exists() && !spec_dir(&dd, &SMALL).join("b.txt.part").exists());

        let good = serve("hello").await;
        download_spec_from(&good, &dd, &SMALL).await.unwrap();
        assert!(installed(&dd, &SMALL));
        assert_eq!(get_progress(&dd, SMALL.id).done, 10);
        assert!(get_progress(&dd, SMALL.id).error.is_none());
        // A file truncated on disk is not ready.
        std::fs::write(spec_dir(&dd, &SMALL).join("a.txt"), "hel").unwrap();
        assert!(!installed(&dd, &SMALL));
    }

    #[test]
    fn missing_models_fail_closed_and_status_reports_them() {
        let ctx = Ctx::test();
        crate::library::init(&ctx).unwrap();
        assert!(embed(&ctx.data_dir, &["x".into()]).is_err());
        assert!(rerank(&ctx.data_dir, "q", &["x".into()]).is_err());
        let s = status(&ctx);
        assert_eq!(s["embed"]["state"], "missing");
        assert_eq!(s["rerank"]["state"], "missing");
        assert_eq!(s["embed"]["bytes_total"], EMBED.bytes());
        set_enabled(&ctx, false).unwrap();
        assert!(!ready(&ctx, &EMBED));
        assert_eq!(status(&ctx)["enabled"], false);
    }

    /// Real weights from Hugging Face (about 105 MB). `cargo test -- --ignored local_models`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn real_local_models_recall_paraphrases_and_rerank() {
        let ctx = Ctx::test();
        crate::library::init(&ctx).unwrap();
        let s = ensure(&ctx).await;
        assert_eq!(s["embed"]["state"], "ready", "{s}");
        assert_eq!(s["rerank"]["state"], "ready", "{s}");

        let dd = ctx.data_dir.clone();
        let docs: Vec<String> = [
            "Bundle the model weights inside the app so it works with no network.",
            "Rotate the OAuth refresh token every 30 days.",
            "Never force push to main.",
        ]
        .map(String::from)
        .to_vec();
        let (v, q) = tokio::task::spawn_blocking({
            let (dd, docs) = (dd.clone(), docs.clone());
            move || (embed(&dd, &docs).unwrap(), embed(&dd, &["Can I use it in airplane mode?".into()]).unwrap())
        })
        .await
        .unwrap();
        assert_eq!(v[0].len(), 384);
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let sims: Vec<f32> = v.iter().map(|d| cos(d, &q[0])).collect();
        assert!(sims[0] > sims[1] && sims[0] > sims[2], "airplane mode should match offline weights: {sims:?}");

        let r = tokio::task::spawn_blocking(move || rerank(&dd, "how do I keep credentials fresh?", &docs).unwrap())
            .await
            .unwrap();
        assert_eq!(r[0].0, 1, "reranker should put token rotation first: {r:?}");

        // End to end through Memory: resolution picks the local models, vectors are pinned to the
        // local version, a zero-overlap paraphrase is recalled, and repo scope is still a hard gate.
        let cfg = crate::memory_ledger::config_get(&ctx);
        assert_eq!(cfg["active_embed_model"], EMBED.version, "{cfg}");
        assert_eq!(cfg["active_rerank_model"], RERANK.version, "{cfg}");
        let save = |scope: &str, text: &str| {
            crate::library::save_structured(&ctx, crate::library::SaveOpts {
                content: text,
                category: "fact",
                scope,
                source: "user",
                trust_tier: "user_direct",
                fact_key: None,
                supersedes_id: None,
                source_event_id: None,
                provenance: None,
            })
            .unwrap()
        };
        let want = save("repo:ra", "Bundle the model weights inside the app so it works with no network.");
        save("repo:ra", "Rotate the OAuth refresh token every 30 days.");
        save("repo:rb", "Airplane mode is unsupported in the sibling repo.");
        let synced = crate::library::sync_vectors_async(&ctx).await.unwrap();
        assert_eq!(synced["embed_model"], EMBED.version);
        let versions = ctx.db.query("SELECT DISTINCT model_version AS v FROM memory_vectors", []).unwrap();
        assert_eq!(versions, vec![json!({ "v": EMBED.version })]);
        let hits = crate::library::agent_memories_async(&ctx, "Can I use it in airplane mode?", "ra", 5).await.unwrap();
        let rows = hits.as_array().cloned().unwrap_or_default();
        assert_eq!(rows.first().map(|r| r["id"].clone()), Some(want["id"].clone()), "{hits}");
        assert!(rows.iter().all(|r| r["scope"] == "repo:ra"), "scope gate: {hits}");
    }
}

