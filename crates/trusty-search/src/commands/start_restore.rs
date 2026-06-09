//! Index-restore helpers extracted from `start.rs` to stay within the 500-line
//! file budget (issue #610 / line-cap CI gate).
//!
//! Why: `start.rs` crossed its allowlist budget after the OOM error-propagation
//! edits landed. The two pure restore helpers — `try_locate_moved_root` and
//! `restore_one_index` — are the natural extraction targets: they are the leaf
//! functions in the restore call chain, they have no dependencies on `start.rs`-
//! private state, and they are already `pub(crate)` so integration tests can
//! reach them directly.
//! What: this module re-exports nothing from `start.rs`; `start.rs` imports
//! `restore_one_index` and `try_locate_moved_root` from here.
//! Test: unit tests for `try_locate_moved_root` live in `start.rs`'s `mod tests`
//! (where the warm-boot fixtures are) and reference this module via
//! `crate::commands::start_restore::*`.

use std::sync::Arc;

use crate::commands::start::{derive_warm_boot_stages, WarmBootInputs};
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::warm_boot::canonicalize_best_effort;
use crate::service::SearchAppState;

/// Attempt to locate a moved project root for a colocated index (issue #484).
///
/// Why: when a project is moved (e.g. `mv projA projA-moved`), the daemon
/// restarts with a stale `root_path` in `indexes.toml`. Without relocation
/// detection, `build_indexer_from_entry` calls `colocated_storage_dir` which
/// calls `create_dir_all` on the non-existent old path, silently producing an
/// empty ghost directory and a 0-chunk index. This function intercepts that
/// case before any disk mutation.
///
/// What: scans all tracked roots for `.trusty-search/` directories containing
/// a populated `index.redb`. Filters out roots already claimed by another live
/// entry in `indexes.toml`. Returns the new root path ONLY when exactly one
/// candidate exists (ambiguous = skip, zero = skip). If a unique candidate is
/// found, updates `indexes.toml` atomically so subsequent restarts are instant.
///
/// Test: `restore_moved_colocated_index_relinks_unique_candidate`,
/// `restore_missing_root_with_no_candidate_skips`,
/// `restore_missing_root_with_ambiguous_candidates_skips` in `start.rs` tests.
pub(crate) fn try_locate_moved_root(
    entry: &PersistedIndex,
    all_entries: &[PersistedIndex],
) -> Option<std::path::PathBuf> {
    use crate::service::colocated_storage::COLOCATED_DIR_NAME;
    use crate::service::fs_discovery::{scan_roots_for_colocated_indexes, DEFAULT_SCAN_DEPTH};
    use crate::service::roots_registry::load_roots;

    // Only attempt relocation for colocated indexes with a missing root.
    if !entry.colocated || entry.root_path.exists() {
        return None;
    }

    // Collect root paths that are already claimed by other live entries so
    // we don't accidentally steal their `.trusty-search/` directory.
    let claimed: std::collections::HashSet<std::path::PathBuf> = all_entries
        .iter()
        .filter(|e| e.id != entry.id && e.root_path.exists())
        .map(|e| e.root_path.clone())
        .collect();

    // Scan tracked roots for colocated index directories.
    let tracked_roots: Vec<std::path::PathBuf> = match load_roots() {
        Ok(r) => r.into_iter().map(|r| r.path).collect(),
        Err(_) => return None,
    };
    if tracked_roots.is_empty() {
        return None;
    }

    let discovered = scan_roots_for_colocated_indexes(&tracked_roots, DEFAULT_SCAN_DEPTH);

    // A candidate must:
    //   1. Have a populated index.redb (not just an empty .trusty-search/ dir).
    //   2. Not be already claimed by another entry.
    let candidates: Vec<std::path::PathBuf> = discovered
        .into_iter()
        .filter(|c| {
            if claimed.contains(&c.root_path) {
                return false;
            }
            let redb = c.root_path.join(COLOCATED_DIR_NAME).join("index.redb");
            // Require a non-empty redb file so we don't relink to a ghost dir.
            std::fs::metadata(&redb)
                .map(|m| m.is_file() && m.len() > 0)
                .unwrap_or(false)
        })
        .map(|c| c.root_path)
        .collect();

    match candidates.len() {
        1 => {
            let raw_root = candidates.into_iter().next().expect("len==1");
            // Issue #541: canonicalize the new root so the persisted path
            // matches the absolute chunk paths already in the index's redb.
            let new_root = canonicalize_best_effort(&raw_root);
            tracing::info!(
                "warm-boot: index '{}' root_path moved: {} → {} (auto-relink, issue #484)",
                entry.id,
                entry.root_path.display(),
                new_root.display(),
            );
            // Persist the new root_path so subsequent restarts skip the scan.
            let updated = PersistedIndex {
                root_path: new_root.clone(),
                ..entry.clone()
            };
            if let Err(e) = crate::service::persistence::upsert_index_registry_entry(updated) {
                tracing::warn!(
                    "warm-boot: could not persist relocated root_path for '{}': {e}",
                    entry.id
                );
            }
            Some(new_root)
        }
        0 => {
            tracing::warn!(
                "warm-boot: skipping index '{}' — root_path {} no longer exists and no \
                 unique candidate found in tracked roots",
                entry.id,
                entry.root_path.display(),
            );
            None
        }
        n => {
            tracing::warn!(
                "warm-boot: skipping index '{}' — root_path {} no longer exists and {} \
                 ambiguous candidates found (manual `trusty-search index <path>` required)",
                entry.id,
                entry.root_path.display(),
                n,
            );
            None
        }
    }
}

/// Register one index entry into the in-memory registry, restoring HNSW + corpus.
///
/// Why: extracted so the loop in `restore_indexes` remains readable and so
/// colocated-index integration tests can drive this path directly.
/// What: checks `root_path.exists()` before building — when the path is missing
/// for a colocated index, attempts relocation via `try_locate_moved_root` (issue
/// #484); for non-colocated or unresolvable entries, logs WARN and skips.
/// Skips entries already in the in-memory registry (idempotent), builds the
/// indexer via `build_indexer_from_entry`, and registers the resulting
/// `IndexHandle`.
/// Issue #541: after the existence guard, re-canonicalizes the stored root_path
/// to match the absolute paths the indexer stored in chunk records. If
/// canonicalization yields a different path, persists the canonical form back to
/// indexes.toml so subsequent restarts are stable.
/// Issue #718 Part 3: this function is `pub(crate)` so `warm_boot::restore` can
/// call it from inside a bounded `tokio::spawn` task. Callers in `restore_indexes`
/// should use `restore_one_index_bounded` instead of calling this directly.
/// Issue #954: HNSW alloc failures (OOM) are propagated as a skip rather than
/// a panic so the daemon can still serve the remaining indexes.
/// Test: covered by the warm-boot integration tests and the
/// `restore_moved_colocated_index_*` unit tests in `start.rs`.
pub(crate) async fn restore_one_index(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    mut entry: PersistedIndex,
) {
    let id = IndexId::new(entry.id.clone());
    if state.registry.get(&id).is_some() {
        // A live create_index handler beat us to it — skip.
        return;
    }

    // Issue #484: guard against missing root_path before any disk mutation.
    // `build_indexer_from_entry` → `corpus_redb_path_for_entry` → `colocated_storage_dir`
    // calls `create_dir_all` on the (now-dead) path, silently creating an empty
    // ghost dir and loading 0 chunks. Block that here.
    if !entry.root_path.exists() {
        // For colocated indexes: attempt relocation scan.
        if entry.colocated {
            // Collect all current registry entries so the scan can exclude
            // already-claimed roots.  Best-effort: an empty vec is safe —
            // it just disables the claimed-root filter.
            let all_entries =
                crate::service::persistence::load_index_registry().unwrap_or_default();
            match try_locate_moved_root(&entry, &all_entries) {
                Some(new_root) => {
                    entry.root_path = new_root;
                }
                None => {
                    // Warn already emitted by try_locate_moved_root.
                    return;
                }
            }
        } else {
            tracing::warn!(
                "warm-boot: skipping index '{}' — root_path {} no longer exists \
                 (run `trusty-search prune-orphans` to clean up or \
                 `trusty-search index <path>` to re-register at the new location)",
                entry.id,
                entry.root_path.display(),
            );
            return;
        }
    }

    // Issue #541: re-canonicalize the stored root_path so handle.root_path
    // matches the absolute paths the indexer stored in chunk records. Symlink
    // aliases, volume-mount renames, and macOS /private/var ↔ /var aliases all
    // cause `file_is_within_root` to drop valid search results if the handle
    // holds the non-canonical form. Canonicalization is best-effort: if it
    // fails (e.g. path disappeared between the exists() check and now) we fall
    // back to the stored path rather than aborting the whole warm-boot.
    let canonical_root = canonicalize_best_effort(&entry.root_path);
    if canonical_root != entry.root_path {
        tracing::info!(
            "warm-boot: index '{}' root_path canonicalized: {} → {} (issue #541, persisting)",
            entry.id,
            entry.root_path.display(),
            canonical_root.display(),
        );
        entry.root_path = canonical_root;
        // Persist so subsequent restarts see the canonical path immediately,
        // avoiding repeated canonicalization and keeping indexes.toml accurate.
        let updated = PersistedIndex {
            root_path: entry.root_path.clone(),
            ..entry.clone()
        };
        if let Err(e) = crate::service::persistence::upsert_index_registry_entry(updated) {
            tracing::warn!(
                "warm-boot: could not persist canonicalized root_path for '{}': {e}",
                entry.id,
            );
        }
    }

    // Issue #954: propagate HNSW alloc failure (OOM) as a skip rather than
    // a panic so the daemon can still serve the remaining indexes.
    let mut indexer = match build_indexer_from_entry(&entry, embedder).await {
        Ok(idx) => idx,
        Err(e) => {
            tracing::error!(
                "warm-boot: skipping index '{}' — HNSW allocator failed: {e} \
                 (closes #954; daemon will restart on next boot via systemd Restart=on-failure)",
                entry.id
            );
            return;
        }
    };
    // Restore per-index filters and domain vocabulary from indexes.toml.
    // Resolve `include_paths` to absolute under `root_path` so the reindex
    // walker can prune without per-call path arithmetic. `.` and empty
    // entries collapse to "walk the whole root".
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
    // Issue #75: capture the current git HEAD SHA at registration so the
    // search response can flag staleness when the working tree advances
    // past the indexed commit. Best-effort: `None` outside a git repo.
    let indexed_head_sha = crate::core::git::head_sha(&entry.root_path);
    let lexical_only = entry.lexical_only;
    // Issue #313: read skip_kg from the persisted entry. When true, the
    // graph stage is forced to Skipped at warm-boot regardless of on-disk
    // state (config intent wins over stale on-disk graph data).
    let skip_kg = entry.skip_kg;
    // Issue #923: read defer_embed from the persisted entry. Default `true`.
    let defer_embed = entry.defer_embed;
    // Issue #135: inspect the on-disk artifacts that
    // `build_indexer_from_entry` just restored and derive the staged-pipeline
    // state from them. Before this, every warm-booted index landed with
    // `stages = Pending` and `search_capabilities` computed from that —
    // so the search handler silently disabled the vector + KG lanes on every
    // existing index until the user ran a force reindex.
    //
    // The inspection is cheap: `chunk_count` is one redb metadata read,
    // `hnsw.usearch` is a `path.exists()` filesystem call (the dim /
    // deserialise check already happened inside the loader), and the
    // symbol-graph node count is an `Arc::clone` + in-memory read.
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
        "warm-boot: index '{}' restored (colocated={}) — chunks={} hnsw_snapshot={} \
         graph_nodes={} lexical_only={} skip_kg={} → \
         stages(lexical={:?}, semantic={:?}, graph={:?})",
        entry.id,
        entry.colocated,
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
        stages.lexical.status,
        stages.semantic.status,
        stages.graph.status,
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
