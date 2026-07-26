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
//! Test: `get_or_load_index_*` — unit tests in `lazy_loader::tests` drive
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

    // Issue #3993 (Gap F): `find_root_path_collision` (issue #2336) originally
    // scanned only LIVE handles, so it was structurally blind to cold/unloaded
    // entries. Second round (adversarial BLOCK): the create/relocate/reindex
    // write-side guards now ALSO consult `state.cold_store` (see
    // `find_root_path_collision`'s updated doc), so a NEW registration can no
    // longer silently steal a pre-existing cold entry's root_path — that gap
    // is closed at the write side, before this function is ever reached for
    // the interloper. What remains here is the live-handle check: if a live
    // sibling already owns this root_path — which, post-fix, can only happen
    // via pre-existing on-disk corruption (indexes.toml entries that already
    // shared a root_path before any guard ever ran, e.g. hand-edited or
    // inherited from a pre-#2336 daemon) rather than a new call stealing it —
    // this cold entry loses and is marked permanently failed. Reuses the
    // existing primitive rather than adding a fourth collision mechanism.
    //
    // Deliberately NOT scanning `state.cold_store` here (passed as `&[]`):
    // unlike the write-side guards, checking sibling COLD entries at this
    // point would risk marking BOTH of two genuinely concurrently-restoring
    // cold entries `Failed` off a stale snapshot — neither has touched disk
    // yet, so neither check would see the other's restore in flight, and a
    // naive cold-vs-cold check could fail the LEGITIMATE winner just as
    // easily as the loser. The `corpus_open_failed` ground-truth check below
    // (mirroring `create_index_handler`/`relocate_index_handler`) is the
    // correct backstop for that race instead: redb's real single-open
    // semantics deterministically decide the actual winner, so only the
    // restore that truly lost the race is ever marked failed.
    let live_handles = state.registry.list_handles();
    if let Some(existing_id) = crate::service::server::helpers::find_root_path_collision(
        &live_handles,
        &[],
        &entry.root_path,
        Some(&id),
    ) {
        tracing::warn!(
            "lazy-load: refusing to restore cold index '{}' at {} — root_path \
             already owned by live index '{}' (issue #3993); marking permanently \
             failed so future queries fail fast instead of retrying the doomed \
             restore",
            entry.id,
            entry.root_path.display(),
            existing_id,
        );
        state.cold_store.mark_failed(&id);
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
    // Issue #3748 slice B PR 1: mirror `restore_one_index` — register the
    // daemon's pool slot so this cold-loaded index's query + catch-up embeds
    // route through Interactive/Background lanes, self-healing across the
    // boot-race window (PR #3784 review finding 1).
    indexer.set_embed_pool_source(Arc::clone(&state.embed_pool));

    // Issue #3993 second round (adversarial BLOCK) ground-truth backstop:
    // `create_index_handler` and `relocate_index_handler` both check
    // `indexer.corpus_open_failed` after building — the best-effort collision
    // guard above is a check-then-act race (same accepted-race shape as
    // #2519's review for those two handlers), so redb's real single-open
    // semantics are the only thing that can deterministically decide a
    // genuine race: e.g. two DIFFERENT cold entries sharing one (corrupted)
    // root_path, both queried at the same instant, both passing the
    // live-handle check above because NEITHER is live yet. Before this fix,
    // `restore_index_on_demand` was the one write path with no such
    // backstop at all — `corpus_open_failed` was read (below, for warm-boot
    // stage derivation) but never gated on, so a losing racer's broken
    // handle would still be registered live. Mirrors the other two handlers'
    // treatment exactly, and reuses the existing #1106 `mark_failed` outcome
    // so a repeat query fails fast instead of retrying a doomed restore.
    if indexer.corpus_open_failed {
        tracing::error!(
            "lazy-load: corpus open failed for cold index '{}' at {} — refusing to \
             register a broken index handle (issue #3993); marking permanently failed",
            entry.id,
            entry.root_path.display(),
        );
        state.cold_store.mark_failed(&id);
        return;
    }

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
    let skip_vector = entry.skip_vector;
    let defer_embed = entry.defer_embed;

    // Issue #1158: read corpus_open_failed before chunk_count so a redb
    // incompatible-format failure surfaces as Failed instead of InProgress.
    let corpus_open_failed = indexer.corpus_open_failed;
    let chunk_count = indexer
        .corpus_store()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or(0);
    // Issue #2922: fold in `hnsw_load_failed` so a truncated/corrupt on-disk
    // snapshot (still detected as "exists" by `has_persisted_hnsw`) doesn't
    // report semantic search as ready when the store actually fell back to
    // fresh/empty. See the matching comment in `commands/start_restore.rs`.
    let hnsw_snapshot_ready = !indexer.hnsw_load_failed
        && crate::service::persistence::hnsw_path_for_entry(&entry)
            .map(|p| crate::service::persistence::has_persisted_hnsw(&p))
            .unwrap_or(false);
    let graph_node_count = indexer.snapshot_symbol_graph().await.node_count();
    let stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
        skip_vector,
        corpus_open_failed,
    });

    tracing::info!(
        "lazy-load: index '{}' restored — chunks={} hnsw_snapshot={} \
         graph_nodes={} lexical_only={} skip_kg={} skip_vector={} corpus_open_failed={}",
        entry.id,
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
        skip_vector,
        corpus_open_failed,
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
        follow_links: entry.follow_links,
        extra_skip_dirs: entry.extra_skip_dirs,
        data_file_max_bytes: crate::service::persistence::resolve_data_file_max_bytes(
            entry.data_file_max_bytes,
        ),
        path_filter: entry.path_filter,
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(indexed_head_sha)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only,
        skip_kg,
        skip_vector,
        defer_embed,
        stages: Arc::new(tokio::sync::RwLock::new(stages)),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    };
    state.registry.register(handle);
}
