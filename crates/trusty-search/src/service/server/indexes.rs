//! Index lifecycle handlers: list, create.
//!
//! Why: Groups the core index registry endpoints (`GET /indexes`, `POST /indexes`)
//! with their request/response types. The `PATCH /indexes/:id` relocate handler
//! lives in `indexes_relocate` to keep this file under the 500-line cap.
//! What: `list_indexes_handler`, `create_index_handler`, and the types they use
//! (`ListIndexesParams`, `IndexListResponse`, `IndexDetailEntry`).
//! Test: `create_index_rejects_relative_root_path` and related tests.
use axum::{
    extract::{Query, State},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};

use super::helpers::{embedder_error_response, embedder_initializing_response, validate_root_path};
use super::router::{CreateIndexRequest, IndexDetailEntry, IndexListResponse};
use super::state::{DaemonEvent, SearchAppState};
use super::status::index_disk_and_mtime;

pub(super) use super::indexes_relocate::relocate_index_handler;

/// Query parameters accepted by `GET /indexes`.
///
/// Why: the `?format=tree` variant returns hierarchy metadata (parent/child
/// relationships derived from `root_path` prefix containment) without breaking
/// the default flat-string response that existing callers depend on.
/// What: `format = "tree"` → object-array response; any other value (or
/// absent) → the existing `{ "indexes": ["id1", "id2"] }` flat response.
/// Test: `list_indexes_tree_format_shape`, `list_indexes_flat_default_unchanged`,
/// and `list_indexes_details_includes_size_bytes`.
#[derive(Deserialize, Default)]
pub(super) struct ListIndexesParams {
    #[serde(default)]
    pub(super) format: Option<String>,
    /// Issue #312: when `true`, return `[{id, size_bytes}]` objects instead of
    /// bare strings so callers can display per-index disk usage.
    #[serde(default)]
    pub(super) details: bool,
}

/// `GET /indexes[?format=tree][?details=true]` — list registered indexes.
///
/// Why: the default flat format is byte-compatible with today's response so
/// existing callers (CLI, MCP, integrators) see no breaking change.  The
/// optional `?format=tree` variant exposes the index hierarchy derived from
/// `root_path` prefix containment (#404 MVP).  The optional `?details=true`
/// variant returns `[{id, size_bytes}]` objects so callers can show per-index
/// disk usage without a separate status round-trip (#312).
/// What: without query params, returns `{ "indexes": ["id1", …] }`.
/// With `?format=tree`, returns object array with hierarchy fields.
/// With `?details=true`, returns `{ "indexes": [{"id": …, "size_bytes": …}] }`.
/// Test: `list_indexes_flat_default_unchanged`, `list_indexes_tree_format_shape`,
/// `list_indexes_details_includes_size_bytes`.
pub(super) async fn list_indexes_handler(
    State(state): State<Arc<SearchAppState>>,
    Query(params): Query<ListIndexesParams>,
) -> Response {
    let want_tree = params
        .format
        .as_deref()
        .map(|f| f == "tree")
        .unwrap_or(false);

    if want_tree {
        let handles = state.registry.list_handles();
        let entries = crate::core::search::hierarchy::build_tree_entries(&state.registry, &handles);
        Json(serde_json::json!({ "indexes": entries })).into_response()
    } else if params.details {
        // Issue #312: return per-index disk usage alongside each id.
        // Issue #661: also include root_path so callers can derive the index
        // from the current project directory without N status round-trips.
        let entries: Vec<IndexDetailEntry> = state
            .registry
            .list_handles()
            .into_iter()
            .map(|handle| {
                let (size_bytes, _) = index_disk_and_mtime(&handle.id.0);
                let root_path = handle.root_path.to_str().map(|s| s.to_string());
                IndexDetailEntry {
                    id: handle.id.0.clone(),
                    root_path,
                    size_bytes,
                }
            })
            .collect();
        Json(serde_json::json!({ "indexes": entries })).into_response()
    } else {
        Json(IndexListResponse {
            indexes: state.registry.list().into_iter().map(|id| id.0).collect(),
        })
        .into_response()
    }
}

pub(super) async fn create_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Json(mut req): Json<CreateIndexRequest>,
) -> Response {
    let id = IndexId::new(req.id.clone());
    // Issue #63: validate root_path is absolute and points at an existing
    // directory before registering. Previously the handler accepted any
    // `PathBuf` the client supplied, so a relative path (e.g. `claude-mpm`)
    // was silently resolved against the daemon's startup CWD by every
    // downstream walker / file reader — producing an index whose root
    // pointed at the wrong project on disk. Rejecting non-absolute or
    // non-directory paths up front gives the caller a clear error and
    // prevents the bleed described in #64.
    //
    // Issue (indexed-paths-mismatch): the validator also returns the
    // *canonical* (symlink-resolved) form. We replace `req.root_path` with
    // it so every downstream consumer — the persistence layer, the indexer's
    // root reference, `include_paths` joins, git-head probing, and the
    // registry handle — stores a single canonical identity for the project.
    // Without this, registering via `/Users/foo` when that's a symlink to
    // `/Volumes/Kemono/...` stored the symlink path and search queries from
    // the canonical mount returned zero hits.
    // Issue #829: validate_root_path is now async (uses tokio::fs::canonicalize
    // and tokio::fs::metadata to avoid blocking the executor thread).
    let canonical_root = match validate_root_path(&req.root_path).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    req.root_path = canonical_root;
    if state.registry.get(&id).is_some() {
        return Json(serde_json::json!({
            "id": req.id,
            "created": false,
            "reason": "already exists",
        }))
        .into_response();
    }
    // Why (issue: 10s readiness timeout): the embedder may still be loading
    // when the daemon accepts its first request. Reject hybrid-index creation
    // with `503 Service Unavailable` so the caller (`trusty-search index`)
    // retries instead of producing a BM25-only index that will quietly miss
    // the vector lane forever.
    let Some(embedder) = state.current_embedder().await else {
        // Issue #121: distinguish "still warming up" from "init failed
        // permanently". When the background task has recorded an error,
        // surface it in the 503 so callers stop polling and operators see
        // a useful message in logs / dashboards.
        if let Some(err) = state.current_embedder_error() {
            return embedder_error_response(&err);
        }
        return embedder_initializing_response();
    };
    // Bug A fix: when an embedder is attached to the shared state, wire the
    // newly created indexer with both an `Embedder` and a `VectorStore` so
    // the HNSW lane actually contributes results. Previously every index
    // was BM25-only because `with_components` was never called, which is
    // why the benchmark observed `match_reason: "bm25"` for 100% of hits.
    //
    // Issue #85: if a previously-saved HNSW snapshot + chunks file exist for
    // this id, restore them so the daemon warm-boots without re-indexing.
    //
    // Fix #483/#485: use `build_indexer_from_entry` with `colocated: true`
    // instead of `build_indexer_with_persisted_state` (which hard-codes
    // `colocated: false`).  The entry-aware builder routes the corpus store
    // to `<root>/.trusty-search/index.redb` via `corpus_redb_path_for_entry`,
    // and crucially `colocated_redb_path` → `colocated_storage_dir` calls
    // `create_dir_all` — so the `.trusty-search/` directory exists on-disk
    // BEFORE the first reindex.  Every write-path probe
    // (`has_colocated_storage` in persist.rs / reindex.rs) then sees the dir
    // and routes HNSW + corpus writes to the colocated path too.  Without this
    // fix the writer used the app-data path while the loader used the colocated
    // path (because `indexes.toml` recorded `colocated = true`), producing 0
    // chunks and no corpus store after the first restart.  A missing corpus
    // store also causes `write_schema_version` to return
    // "cannot write schema_version: no durable corpus" (#485).
    let init_entry = crate::service::persistence::PersistedIndex {
        id: req.id.clone(),
        root_path: req.root_path.clone(),
        colocated: true,
        ..Default::default()
    };
    // Issue #954: propagate HNSW alloc failure (OOM) as a 500 response
    // rather than a panic so the daemon continues serving other requests.
    let mut indexer =
        match crate::service::persistence_loader::build_indexer_from_entry(&init_entry, &embedder)
            .await
        {
            Ok(idx) => idx,
            Err(e) => {
                tracing::error!(
                    "create_index: HNSW allocator failed for '{}': {e} (closes #954)",
                    req.id
                );
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({
                        "error": format!("HNSW allocation failed (OOM): {e}")
                    })),
                )
                    .into_response();
            }
        };

    // Resolve repo-config filters (issue: trusty-search.yaml wiring). The
    // CLI sends `paths:` as relative strings; resolve them against `root_path`
    // here so the registry handle carries absolute subtrees ready for the
    // reindex walker. `domain_terms` is attached to the indexer so its
    // `classify_with_domain` lookup runs on every search without needing to
    // reach back into the handle.
    let include_paths: Vec<std::path::PathBuf> = req
        .include_paths
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.trim().is_empty() && p.trim() != ".")
        .map(|p| req.root_path.join(p.trim()))
        .collect();
    let exclude_globs: Vec<String> = req.exclude_globs.clone().unwrap_or_default();
    let extensions: Vec<String> = req
        .extensions
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect();
    let domain_terms: Vec<String> = req.domain_terms.clone().unwrap_or_default();
    let path_filter: Vec<String> = req
        .path_filter
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect();
    indexer.set_domain_terms(domain_terms.clone());

    // Persist the registration so a daemon restart can re-register
    // automatically. Best-effort: a write failure is logged but doesn't fail
    // the request — the in-memory registry still has the index.
    // Issue #118: default `include_docs` is now `true` (was `false` through
    // v0.8.2). `mode=text` searches were silently empty because docs were
    // never indexed; the per-mode `is_allowed_for_mode` filter keeps
    // `mode=code` results clean even with docs in the index.
    let include_docs: bool = req.include_docs.unwrap_or(true);
    // Issue #100: honour `.gitignore` by default. `None` on the wire ⇒ `true`
    // so existing callers (CLI, MCP, integrators) get the fix automatically
    // without having to pass a new field.
    let respect_gitignore: bool = req.respect_gitignore.unwrap_or(true);
    // Issue #109, Phase 1: staged-pipeline opt-out. `None` on the wire ⇒
    // `false` (full pipeline) so existing callers see no behaviour change.
    let lexical_only: bool = req.lexical_only.unwrap_or(false);
    // Issue #313: KG-skip flag. `None` on the wire ⇒ `false` (KG built as
    // normal). TRUSTY_NO_KG=1 provides a machine-wide default that operators
    // can set without modifying per-index config.
    let skip_kg: bool = req.skip_kg.unwrap_or_else(|| {
        let v = std::env::var("TRUSTY_NO_KG").unwrap_or_default();
        matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
    });
    // Issue #923: deferred-embedding mode. Default `true` — the fast-pass /
    // background-embed path. Callers opt out by passing `defer_embed: false`
    // to force synchronous full indexing. Has no effect on `lexical_only` indexes.
    let defer_embed: bool = req.defer_embed.unwrap_or(true);
    // Issue #403: new indexes use colocated storage (`<root>/.trusty-search/`).
    // Register the root in `roots.toml` so the startup scanner can find it on
    // the next daemon boot, and ensure `.trusty-search/` is git-ignored.
    let colocated = true;
    if let Err(e) = crate::service::roots_registry::upsert_root(req.root_path.clone()) {
        tracing::warn!("could not register root in roots.toml for {}: {e}", req.id);
    }
    if let Err(e) = crate::service::colocated_storage::ensure_gitignored(&req.root_path) {
        tracing::warn!(
            "could not add .trusty-search/ to .gitignore for {}: {e}",
            req.id
        );
    }
    if let Err(e) = crate::service::persistence::upsert_index_registry_entry(
        crate::service::persistence::PersistedIndex {
            id: req.id.clone(),
            root_path: req.root_path.clone(),
            include_paths: req.include_paths.clone().unwrap_or_default(),
            exclude_globs: exclude_globs.clone(),
            extensions: extensions.clone(),
            domain_terms: domain_terms.clone(),
            path_filter: path_filter.clone(),
            include_docs,
            respect_gitignore,
            lexical_only,
            skip_kg,
            defer_embed,
            colocated,
            // Issue #993: new indexes have no query/index history yet.
            last_queried_unix: None,
            last_indexed_unix: None,
        },
    ) {
        tracing::warn!("could not persist index registry for {}: {e}", req.id);
    }

    // Issue #75: capture the current git HEAD SHA at registration; the search
    // response uses it to flag stale results when the working tree advances.
    let indexed_head_sha = crate::core::git::head_sha(&req.root_path);
    // Issue #109, Phase 1: pre-mark semantic + graph as `Skipped` for
    // lexical-only indexes so the search handler never tries the HNSW lane.
    // Issue #313: pre-mark graph as `Skipped` for skip_kg indexes.
    let stages = if lexical_only {
        crate::core::registry::IndexStages {
            lexical: crate::core::registry::StageState::pending(),
            semantic: crate::core::registry::StageState::skipped(),
            graph: crate::core::registry::StageState::skipped(),
        }
    } else if skip_kg {
        crate::core::registry::IndexStages {
            lexical: crate::core::registry::StageState::pending(),
            semantic: crate::core::registry::StageState::pending(),
            graph: crate::core::registry::StageState::skipped(),
        }
    } else {
        crate::core::registry::IndexStages::default()
    };
    let handle = IndexHandle {
        id: id.clone(),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: req.root_path,
        include_paths,
        exclude_globs,
        extensions,
        domain_terms,
        include_docs,
        respect_gitignore,
        path_filter,
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(indexed_head_sha)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only,
        skip_kg,
        defer_embed,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    };
    state.registry.register(handle);
    // Issue #41 Phase 1: refresh the index-count gauge so /metrics reflects
    // the registry size without a separate poll.
    crate::service::metrics::set_index_count(state.registry.list().len());
    // Push event so connected dashboards refresh their index list without a
    // page reload (mirrors the trusty-memory `palace_created` pattern).
    state.emit(DaemonEvent::IndexRegistered { id: req.id.clone() });
    Json(serde_json::json!({ "id": req.id, "created": true })).into_response()
}
