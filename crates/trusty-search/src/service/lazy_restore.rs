//! On-demand (lazy) index restore for the search and other per-index handlers
//! (issue #993).
//!
//! Why: `restore_one_index` lives in `commands/start_restore.rs`, which is
//! only accessible from the binary's module tree. HTTP handlers (in the
//! service layer) need the same restore logic when a query hits a cold index.
//! This module provides `restore_index_on_demand`, which is a service-layer
//! mirror of `restore_one_index` without the colocated-relocation scan
//! (`try_locate_moved_root`). Cold indexes are expected to have a valid
//! `root_path` when they were parked at startup; if the path is gone they
//! fall back gracefully.
//!
//! Test: `restore_index_on_demand_*` — unit tests in `lazy_loader::tests` drive
//! `get_or_load_index` with a mock restore_fn; integration coverage comes from
//! the warm-boot integration tests.

use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::warm_boot::{
    canonicalize_best_effort, derive_warm_boot_stages, WarmBootInputs,
};
use crate::service::SearchAppState;

/// Restore one cold index into the hot registry for use by HTTP handlers.
///
/// Why (issue #993): mirrors `restore_one_index` from `commands/start_restore.rs`
/// but lives in the service layer so it is accessible from library code. Skips
/// the colocated-relocation scan (`try_locate_moved_root`, issue #484) because
/// cold indexes were registered at startup with a valid `root_path`; if the path
/// has since disappeared the restore simply skips the index (consistent with the
/// warm-boot skip behaviour for non-colocated indexes).
/// What: builds the indexer via `build_indexer_from_entry`, derives warm-boot
/// stages from on-disk artifacts, constructs an `IndexHandle`, and registers it
/// in `state.registry`. Idempotent if the index is already registered.
/// Test: exercised via `get_or_load_index_loads_cold_index` (mock restore) and
/// the lazy-warmboot integration test scenario.
pub(crate) async fn restore_index_on_demand(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    mut entry: PersistedIndex,
) {
    let id = IndexId::new(entry.id.clone());
    if state.registry.get(&id).is_some() {
        // Already loaded by a concurrent handler — nothing to do.
        return;
    }

    // Guard against missing root_path. For cold indexes the path was valid when
    // they were parked; if it disappeared we skip gracefully (non-colocated path
    // used here — the relocation scan from issue #484 is a warm-boot-only flow).
    if !entry.root_path.exists() {
        tracing::warn!(
            "lazy-load: skipping index '{}' — root_path {} no longer exists",
            entry.id,
            entry.root_path.display(),
        );
        return;
    }

    // Re-canonicalize root_path (issue #541).
    let canonical_root = canonicalize_best_effort(&entry.root_path);
    if canonical_root != entry.root_path {
        tracing::info!(
            "lazy-load: index '{}' root_path canonicalized: {} → {}",
            entry.id,
            entry.root_path.display(),
            canonical_root.display(),
        );
        entry.root_path = canonical_root;
    }

    let mut indexer = match build_indexer_from_entry(&entry, embedder).await {
        Ok(idx) => idx,
        Err(e) => {
            tracing::error!(
                "lazy-load: index '{}' HNSW allocator failed: {e} — skipping",
                entry.id
            );
            return;
        }
    };

    // Build include_paths / extensions, same logic as start_restore.
    let include_paths: Vec<std::path::PathBuf> = entry
        .include_paths
        .iter()
        .filter(|p| !p.trim().is_empty() && p.trim() != ".")
        .map(|p| entry.root_path.join(p.trim()))
        .collect();
    let extensions: Vec<String> = entry
        .extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect();
    indexer.set_domain_terms(entry.domain_terms.clone());

    let indexed_head_sha = crate::core::git::head_sha(&entry.root_path);
    let lexical_only = entry.lexical_only;
    let skip_kg = entry.skip_kg;
    let defer_embed = entry.defer_embed;

    let chunk_count = indexer
        .corpus_store()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or(0);
    let hnsw_snapshot_ready = crate::service::persistence::hnsw_path_for_entry(&entry)
        .map(|p| crate::service::persistence::has_persisted_hnsw(&p))
        .unwrap_or(false);
    let graph_node_count = indexer.snapshot_symbol_graph().await.node_count();
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
    });

    tracing::info!(
        "lazy-load: index '{}' restored — chunks={} hnsw_snapshot={} \
         graph_nodes={} lexical_only={} skip_kg={}",
        entry.id,
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
    );

    let handle = IndexHandle {
        id: id.clone(),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: entry.root_path,
        include_paths,
        exclude_globs: entry.exclude_globs,
        extensions,
        domain_terms: entry.domain_terms,
        include_docs: entry.include_docs,
        respect_gitignore: entry.respect_gitignore,
        path_filter: entry.path_filter,
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
}
