//! Disposable HTTP runner for chunking/BM25/KG experiments; no model is loaded.
//!
//! Invocation: set TRUSTY_DATA_DIR, TRUSTY_SEARCH_TEST_CORPUS_ROOT,
//! TRUSTY_SEARCH_TEST_URL=http://127.0.0.1:17881, and
//! TRUSTY_SEARCH_EXPERIMENT_SOURCE_REVISION to the frozen commit, then run
//! `cargo run -p trusty-search --example context_experiment_daemon`.
//! The data/corpus directories require .trusty-search-test-daemon and
//! .trusty-search-test-corpus markers respectively; the corpus must lack .git.
//! Treatment env: TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS=0|64|128 and
//! TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW=100|64. Use fresh directories per cell.
//! POST /experiment/cards accepts {index_id,results:[CodeChunk,...]} and returns
//! {cards:[...]}; it transforms saved real search results without re-ranking.

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use trusty_search::allowlist::{AllowlistConfig, AllowlistEntry, AllowlistPaths};
use trusty_search::core::experiment::{
    experiment_config, navigation_card, parse_experiment_config, ExperimentConfig,
};
use trusty_search::core::{CodeChunk, Embedder, IndexRegistry};
use trusty_search::service::server::{build_router, SearchAppState};

#[cfg(test)]
#[path = "support/context_experiment_daemon_tests.rs"]
mod tests;

struct RejectingEmbedder {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl Embedder for RejectingEmbedder {
    async fn embed(&self, _: &str) -> Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("embedding forbidden in chunking/BM25/KG experiment")
    }
    async fn embed_batch(&self, _: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("batch embedding forbidden in chunking/BM25/KG experiment")
    }
    fn dimension(&self) -> usize {
        32
    }
}

#[derive(Clone)]
struct RunnerState {
    config: ExperimentConfig,
    calls: Arc<AtomicU64>,
    source_revision: String,
    data_dir: PathBuf,
    corpus_root: PathBuf,
}

#[derive(Serialize)]
struct Evidence {
    config: ExperimentConfig,
    embedding_calls: u64,
    source_revision: String,
    data_dir: PathBuf,
    corpus_root: PathBuf,
}

async fn evidence(State(state): State<RunnerState>) -> Json<Evidence> {
    Json(Evidence {
        config: state.config,
        embedding_calls: state.calls.load(Ordering::SeqCst),
        source_revision: state.source_revision,
        data_dir: state.data_dir,
        corpus_root: state.corpus_root,
    })
}

#[derive(Deserialize)]
struct CardRequest {
    index_id: String,
    results: Vec<CodeChunk>,
}

async fn cards(Json(request): Json<CardRequest>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"cards": request.results.iter()
        .map(|chunk| navigation_card(&request.index_id, chunk)).collect::<Vec<_>>() }))
}

fn marked_root(variable: &str, marker: &str) -> Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os(variable).with_context(|| format!("{variable} required"))?);
    ensure!(path.is_absolute(), "{variable} must be absolute");
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve {variable}"))?;
    ensure!(
        path.is_dir() && path.join(marker).is_file(),
        "{variable} requires {marker}"
    );
    ensure!(
        !path.join(".git").exists(),
        "{variable} must not be a git checkout"
    );
    Ok(path)
}

// The runner exposes only measurement/indexing endpoints, not production lifecycle routes.
async fn fixture_guard(State(state): State<RunnerState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let allowed = request.method() == axum::http::Method::GET
        && (path == "/health"
            || path == "/indexes"
            || path.starts_with("/indexes/")
            || path == "/experiment/evidence")
        || request.method() == axum::http::Method::POST
            && (path == "/indexes"
                || path == "/experiment/cards"
                || path == "/indexes/experiment/reindex"
                || path == "/indexes/experiment/search");
    if !allowed {
        return (StatusCode::FORBIDDEN, "route outside experiment scope").into_response();
    }
    let creating = path == "/indexes";
    let searching = path == "/indexes/experiment/search";
    let reindexing = path == "/indexes/experiment/reindex";
    if (!creating && !searching && !reindexing) || request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 65536).await {
        Ok(bytes) => bytes,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };
    if creating || reindexing {
        let fields: &[&str] = if creating {
            &["id", "root_path", "skip_vector", "skip_kg"]
        } else {
            &["force", "root_path"]
        };
        if !value
            .as_object()
            .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
        {
            return (
                StatusCode::BAD_REQUEST,
                "index mutation contains fields outside experiment scope",
            )
                .into_response();
        }
    }
    if reindexing {
        if !value["root_path"].is_null()
            && value["root_path"]
                .as_str()
                .and_then(|root| PathBuf::from(root).canonicalize().ok())
                .as_ref()
                != Some(&state.corpus_root)
        {
            return (
                StatusCode::BAD_REQUEST,
                "reindex root must match copied corpus",
            )
                .into_response();
        }
        return next
            .run(Request::from_parts(parts, axum::body::Body::from(bytes)))
            .await;
    }
    if searching {
        if !matches!(value["stage"].as_str(), Some("lexical" | "graph")) {
            return (
                StatusCode::BAD_REQUEST,
                "experiment search requires lexical or graph stage",
            )
                .into_response();
        }
        return next
            .run(Request::from_parts(parts, axum::body::Body::from(bytes)))
            .await;
    }
    let root = value["root_path"]
        .as_str()
        .and_then(|root| PathBuf::from(root).canonicalize().ok());
    if value["id"] != "experiment"
        || root.as_ref() != Some(&state.corpus_root)
        || value["skip_vector"] != true
        || value["skip_kg"] != false
    {
        return (
            StatusCode::BAD_REQUEST,
            "requires experiment id, exact marked corpus, skip_vector=true, skip_kg=false",
        )
            .into_response();
    }
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

#[tokio::main]
async fn main() -> Result<()> {
    let context = std::env::var("TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS").ok();
    let window = std::env::var("TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW").ok();
    let config = parse_experiment_config(context.as_deref(), window.as_deref())
        .context("invalid experiment treatment")?;
    ensure!(
        *experiment_config() == config,
        "experiment configuration changed during startup"
    );
    let data_dir = marked_root("TRUSTY_DATA_DIR", ".trusty-search-test-daemon")?;
    let corpus_root = marked_root(
        "TRUSTY_SEARCH_TEST_CORPUS_ROOT",
        ".trusty-search-test-corpus",
    )?;
    ensure!(
        !data_dir.starts_with(&corpus_root) && !corpus_root.starts_with(&data_dir),
        "data and corpus directories must be disjoint"
    );
    for entry in walkdir::WalkDir::new(&corpus_root).follow_links(false) {
        let entry = entry.context("inspect copied corpus")?;
        ensure!(
            !entry.file_type().is_symlink(),
            "copied corpus must not contain symlinks: {}",
            entry.path().display()
        );
    }
    ensure!(
        !data_dir.join("indexes.toml").exists(),
        "use a fresh experiment data directory"
    );
    let url =
        std::env::var("TRUSTY_SEARCH_TEST_URL").context("explicit experiment URL required")?;
    let address: std::net::SocketAddr = url
        .strip_prefix("http://")
        .context("experiment URL must use http://")?
        .parse()
        .context("invalid experiment address")?;
    ensure!(
        address.ip() == std::net::Ipv4Addr::LOCALHOST
            && address.port() != 0
            && address.port() != 7878,
        "bind requires 127.0.0.1 and an explicit non-default port"
    );
    let source_revision = std::env::var("TRUSTY_SEARCH_EXPERIMENT_SOURCE_REVISION")
        .context("frozen source revision required")?;
    ensure!(
        source_revision.len() >= 7 && source_revision.chars().all(|c| c.is_ascii_hexdigit()),
        "source revision must be a commit hash"
    );
    let calls = Arc::new(AtomicU64::new(0));
    let runner = RunnerState {
        config,
        calls: calls.clone(),
        source_revision,
        data_dir: data_dir.clone(),
        corpus_root: corpus_root.clone(),
    };
    let allowlist_path = data_dir.join("allowlist.toml");
    AllowlistConfig {
        entries: vec![AllowlistEntry {
            path: corpus_root,
            name: Some("experiment".into()),
            exclude: vec![],
            extensions: vec![],
            skip_kg: false,
        }],
    }
    .save_to(&allowlist_path)?;
    let project_paths = data_dir.join("project-paths.json");
    std::fs::write(&project_paths, "[]").context("write isolated project registry")?;
    let state = SearchAppState::new(IndexRegistry::new())
        .with_registry_path(data_dir.join("indexes.toml"))
        .with_allowlist_paths(AllowlistPaths {
            allowlist: Some(allowlist_path),
            project_paths: Some(project_paths),
        })
        .with_embedder(Arc::new(RejectingEmbedder { calls }));
    let adapter = Router::new()
        .route("/experiment/evidence", get(evidence))
        .route("/experiment/cards", post(cards))
        .with_state(runner.clone());
    let router = build_router(state)
        .merge(adapter)
        .layer(middleware::from_fn_with_state(runner, fixture_guard));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("bind isolated daemon")?;
    std::fs::write(data_dir.join("http_addr"), address.to_string())
        .context("publish isolated address")?;
    eprintln!("experiment daemon listening at {url}; embeddings forbidden");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("signal handler: {error}");
            }
        })
        .await
        .context("serve experiment daemon")
}
