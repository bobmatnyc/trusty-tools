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

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::warm_boot::{
    canonicalize_best_effort, derive_warm_boot_stages, SalvageGrant, WarmBootInputs,
};
use crate::service::SearchAppState;

/// The relocation candidate set for one warm boot, computed once (#4846).
///
/// Why: `try_locate_moved_root` used to run
/// `scan_roots_for_colocated_indexes` itself, so every dead entry paid a fresh
/// depth-5 walk of every tracked root. The walk's inputs (`roots.toml`, the
/// on-disk `.trusty-search/` layout) and its result are identical for all of
/// them within a single boot — measured at 9.5–10.5 s per call over the
/// reporting machine's 248 roots, recomputed 55 times. Hoisting it to a
/// once-per-boot value turns `O(dead_entries × roots)` into `O(roots)`.
/// What: the surviving candidate roots, already filtered for a populated
/// `index.redb` and for roots claimed by a live entry.
/// Test: `dead_entries_do_not_consume_the_live_index_budget`.
pub(crate) struct RelocationCandidates {
    /// Unclaimed roots holding a non-empty colocated `index.redb`.
    pub(crate) roots: Vec<std::path::PathBuf>,
}

/// Whether a missing-root entry may still be salvaged on this boot (#4846).
///
/// Why: the pre-fix code had no way to express "do not walk the filesystem for
/// this entry" — the walk was unconditional inside `try_locate_moved_root`, so
/// a caller could not opt out and, more to the point, could not accidentally be
/// spared. Deliberately there is NO variant meaning "compute it yourself": the
/// only way to get candidates is to have paid for the shared scan once, so no
/// future edit can reintroduce the per-entry walk without adding a variant a
/// reviewer has to confront.
/// What: [`Self::Ready`] carries the shared set; [`Self::Unavailable`] means
/// salvage is disabled or its budget is spent, and a missing root is then
/// skipped after its triage stat and nothing more.
/// Test: `disabled_salvage_budget_costs_a_dead_entry_nothing_but_a_stat`.
#[derive(Clone)]
pub(crate) enum RelocationScan {
    /// Candidates collected once for this boot; reused by every entry.
    Ready(std::sync::Arc<RelocationCandidates>),
    /// No salvage this boot — skip missing roots without touching the disk.
    Unavailable,
}

/// Collect the boot's relocation candidates, paying the tracked-root walk once.
///
/// Why: this is the expensive operation #4846 is about, so it takes a
/// [`SalvageGrant`] — a token only `SalvageBudget::try_grant` can mint. The
/// budget therefore gates the walk at the type level rather than by a
/// convention the caller has to remember.
/// What: loads `roots.toml`, walks each tracked root for `.trusty-search/`
/// directories, and keeps those with a non-empty `index.redb` that no live
/// entry already claims. Reads only — nothing is written, moved, or removed.
///
/// On the `claimed` filter: it excludes roots owned by entries whose own
/// `root_path` still exists. Every entry that reaches relocation has a MISSING
/// root, so no such entry can appear in `claimed` and the per-entry
/// `e.id != entry.id` self-exclusion the old code performed was a no-op for
/// exactly this population — which is what makes one shared set correct rather
/// than an approximation.
/// #767: a candidate becomes a registered, watched, PERSISTED root without any
/// operator action, so it is an index-creation door and gets the same gate as
/// `POST /indexes`. The filter lives HERE rather than in `try_locate_moved_root`
/// because that function persists via `upsert_index_registry_entry` before its
/// caller sees the result — a check downstream of it would run after the write.
/// A denied or unapproved directory is dropped from the candidate set, so it can
/// never be selected, and the ambiguity arithmetic (`0`/`1`/`n` below) counts
/// only roots the daemon is allowed to index.
/// Test: `dead_entries_do_not_consume_the_live_index_budget`,
/// `relocation_candidates_drop_unapproved_roots`,
/// `relocation_candidates_drop_denylisted_roots`.
pub(crate) fn collect_relocation_candidates(
    all_entries: &[PersistedIndex],
    _grant: &SalvageGrant,
    allowlist_paths: &crate::allowlist::AllowlistPaths,
) -> RelocationCandidates {
    use crate::service::colocated_storage::COLOCATED_DIR_NAME;
    use crate::service::fs_discovery::{scan_roots_for_colocated_indexes, DEFAULT_SCAN_DEPTH};
    use crate::service::roots_registry::load_roots;

    let claimed: std::collections::HashSet<std::path::PathBuf> = all_entries
        .iter()
        .filter(|e| e.root_path.exists())
        .map(|e| e.root_path.clone())
        .collect();

    let tracked_roots: Vec<std::path::PathBuf> = match load_roots() {
        Ok(r) => r.into_iter().map(|r| r.path).collect(),
        Err(_) => return RelocationCandidates { roots: Vec::new() },
    };
    if tracked_roots.is_empty() {
        return RelocationCandidates { roots: Vec::new() };
    }

    let discovered = scan_roots_for_colocated_indexes(&tracked_roots, DEFAULT_SCAN_DEPTH);

    // A candidate must:
    //   1. Have a populated index.redb (not just an empty .trusty-search/ dir).
    //   2. Not be already claimed by another entry.
    let roots: Vec<std::path::PathBuf> = discovered
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
        .filter(|root| relocation_candidate_is_approved(root, allowlist_paths))
        .collect();

    RelocationCandidates { roots }
}

/// Whether a relocation candidate may be adopted as an index root (#767).
///
/// Why: see `collect_relocation_candidates`. A leftover `.trusty-search/` in a
/// personal directory must never become the unique candidate that an
/// approved-but-missing root silently relocates onto.
/// What: the hard denylist first (strict form — nothing here opted in), then the
/// allowlist union. An allowlist that cannot be READ drops the candidate: unlike
/// warm-boot restore, adopting a new root is not something to do on a policy the
/// daemon could not read, and the entry is simply retried on the next boot.
/// Test: `relocation_candidates_drop_unapproved_roots`,
/// `relocation_candidates_drop_denylisted_roots`,
/// `relocation_candidates_keep_approved_roots`.
fn relocation_candidate_is_approved(
    root: &std::path::Path,
    allowlist_paths: &crate::allowlist::AllowlistPaths,
) -> bool {
    if let Some(reason) = crate::allowlist::is_denied(root) {
        tracing::warn!(
            root = %root.display(),
            %reason,
            "warm-boot salvage: candidate refused by the hard denylist (#767)"
        );
        return false;
    }
    match crate::allowlist::sources::resolve_allow_source(root, allowlist_paths) {
        Ok(Some(_)) => true,
        Ok(None) => {
            tracing::warn!(
                root = %root.display(),
                "warm-boot salvage: candidate is not approved for indexing — \
                 not adopting it as a relocated root (#767)"
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                root = %root.display(),
                "warm-boot salvage: allowlist unreadable ({e:#}) — not adopting \
                 this candidate as a relocated root (#767)"
            );
            false
        }
    }
}

/// Attempt to locate a moved project root for a colocated index (issue #484).
///
/// Why: when a project is moved (e.g. `mv projA projA-moved`), the daemon
/// restarts with a stale `root_path` in `indexes.toml`. Without relocation
/// detection, `build_indexer_from_entry` calls `colocated_storage_dir` which
/// calls `create_dir_all` on the non-existent old path, silently producing an
/// empty ghost directory and a 0-chunk index. This function intercepts that
/// case before any disk mutation.
///
/// What: reads the boot's shared [`RelocationCandidates`] (#4846 — it no
/// longer runs the scan itself) and returns the new root path ONLY when exactly
/// one candidate exists (ambiguous = defer to the reaper, zero = skip). If a
/// unique candidate is found, updates `indexes.toml` atomically so subsequent
/// restarts are instant. The decision rules are unchanged; only where the
/// candidates come from changed.
///
/// Test: `restore_moved_colocated_index_relinks_unique_candidate`,
/// `restore_missing_root_with_no_candidate_skips`,
/// `restore_missing_root_with_ambiguous_candidates_skips` in `start.rs` tests.
pub(crate) fn try_locate_moved_root(
    entry: &PersistedIndex,
    candidates: &RelocationCandidates,
) -> Option<std::path::PathBuf> {
    // Only attempt relocation for colocated indexes with a missing root.
    if !entry.colocated || entry.root_path.exists() {
        return None;
    }

    let candidates: Vec<std::path::PathBuf> = candidates.roots.clone();

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
            // #4391: `PersistedIndex` is `#[non_exhaustive]`, so this crate (the
            // binary, a separate crate from the library) assigns rather than
            // using struct-update syntax. Same value, same one field overridden.
            let mut updated = entry.clone();
            updated.root_path = new_root.clone();
            if let Err(e) = crate::service::persistence::upsert_index_registry_entry(updated) {
                tracing::warn!(
                    "warm-boot: could not persist relocated root_path for '{}': {e}",
                    entry.id
                );
            }
            // #4095: the entry resolved, so stop any ambiguity grace clock that
            // an earlier boot started — otherwise a transient ambiguity would
            // eventually reap a registration that has been healthy since.
            crate::service::orphan_reaper::clear_ambiguous_root_stamp(entry);
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
            // #4095: hand the deferral to the reaper instead of logging a WARN
            // that reaches no diagnostic surface and returning. The reaper
            // stamps a start time, escalates the log to ERROR so the debris is
            // actually visible, and — past an explicit grace period — removes
            // the registration (never the on-disk index data).
            crate::service::orphan_reaper::handle_ambiguous_root(entry, n);
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
/// #4846: `relocation` carries the boot's shared candidate set instead of this
/// function triggering a fresh tracked-root walk per entry.
/// Test: covered by the warm-boot integration tests and the
/// `restore_moved_colocated_index_*` unit tests in `start.rs`.
pub(crate) async fn restore_one_index(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    mut entry: PersistedIndex,
    relocation: RelocationScan,
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
        // For colocated indexes: consult the boot's shared relocation set.
        if entry.colocated {
            // #4846: this used to re-read `indexes.toml` from disk AND re-walk
            // every tracked root, per entry. Both are now done once per boot and
            // handed in; `Unavailable` means the salvage budget is spent or
            // disabled, so the entry is skipped without any filesystem walk. The
            // registration and its on-disk corpus are left untouched either way.
            let RelocationScan::Ready(candidates) = &relocation else {
                tracing::warn!(
                    "warm-boot: skipping index '{}' — root_path {} no longer exists and the \
                     warm-boot salvage budget is spent or disabled, so no relocation scan was \
                     run for it (issue #4846). The registration and its on-disk index data are \
                     untouched; it is retried on the next boot, or fix it now with \
                     `trusty-search index <path>`.",
                    entry.id,
                    entry.root_path.display(),
                );
                return;
            };
            match try_locate_moved_root(&entry, candidates) {
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
        // `entry.root_path` was set to `canonical_root` just above, so the old
        // struct-update form was a verbose `entry.clone()`.
        let updated = entry.clone();
        if let Err(e) = crate::service::persistence::upsert_index_registry_entry(updated) {
            tracing::warn!(
                "warm-boot: could not persist canonicalized root_path for '{}': {e}",
                entry.id,
            );
        }
    }

    // DOC-37 (issue #2611): backfill the canonical repo identity for indexes
    // registered before identity tracking existed. The warm-boot pass is the
    // natural reconcile point — `root_path` is present and canonical here, so a
    // one-time derive + persist upgrades legacy `indexes.toml` entries in place.
    // Best-effort: a root with no derivable identity (no remote, no commits)
    // stays `None` and keeps working as a flat index.
    if entry.repo_identity.is_none() {
        if let Some(identity) = trusty_common::repo_identity::RepoIdentity::derive(&entry.root_path)
            .map(|r| r.canonical())
        {
            entry.repo_identity = Some(identity.clone());
            // `entry.repo_identity` was set to `identity` just above.
            let updated = entry.clone();
            if let Err(e) = crate::service::persistence::upsert_index_registry_entry(updated) {
                tracing::warn!(
                    "warm-boot: could not persist backfilled repo_identity for '{}': {e}",
                    entry.id,
                );
            }
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
    // Issue #3748 slice B PR 1: wire the priority-lane pool so this index's
    // query + catch-up embeds route through Interactive/Background lanes
    // instead of the raw embedder. Registers the daemon's OWN pool slot
    // (not a one-time snapshot) so a boot-race window (this index built
    // before `install_embed_pool` completes) self-heals on the first embed
    // call instead of staying poolless forever (PR #3784 review finding 1).
    indexer.set_embed_pool_source(Arc::clone(&state.embed_pool));
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
    // Issue #75: the handle carries the HEAD SHA its corpus was built against so
    // the search response can flag staleness when the working tree advances past
    // the indexed commit.
    // #4391: read it from `indexes.toml`, not from live git. Re-deriving it here
    // made `reconcile_git_path` compare current HEAD against current HEAD, so no
    // git-backed index could ever be found stale at boot.
    let indexed_head_sha = crate::service::boot_markers::resolve_indexed_head_sha(&entry);
    let lexical_only = entry.lexical_only;
    // Issue #313: read skip_kg from the persisted entry. When true, the
    // graph stage is forced to Skipped at warm-boot regardless of on-disk
    // state (config intent wins over stale on-disk graph data).
    let skip_kg = entry.skip_kg;
    // Issue #2984 Phase 1: read skip_vector from the persisted entry —
    // mirrors skip_kg but forces the semantic (not graph) stage to Skipped.
    let skip_vector = entry.skip_vector;
    // Issue #923: read defer_embed from the persisted entry. Default `true`.
    let defer_embed = entry.defer_embed;
    // #4390: read the marker before `entry` is consumed by the handle below.
    let deferred_embed_pending = entry.deferred_embed_pending;
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
    //
    // Issue #1158: also read `corpus_open_failed` so the classifier emits
    // `StageStatus::Failed` instead of the silent `InProgress` when the
    // corpus file existed but could not be opened (incompatible format etc.).
    // #4333: carry the CLASSIFIED failure kind, not a bare bool, so the
    // stage reason distinguishes a transient open timeout from real corruption.
    let corpus_open_failure = indexer.corpus_open_failure;
    let chunk_count = indexer
        .corpus_store()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or(0);
    // Issue #2922: a persisted HNSW file existing on disk does not mean it
    // actually loaded — `hnsw_load_failed` (set by `build_indexer_from_entry`
    // via `build_store_for_entry`) catches the truncated/corrupt case that
    // mere `has_persisted_hnsw` file-existence checking silently missed,
    // which previously let `/health` report `semantic: ready` for an index
    // that had silently fallen back to an empty in-memory store.
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
        corpus_open_failure,
    });
    tracing::info!(
        "warm-boot: index '{}' restored (colocated={}) — chunks={} hnsw_snapshot={} \
         graph_nodes={} lexical_only={} skip_kg={} skip_vector={} corpus_open_failure={:?} → \
         stages(lexical={:?}, semantic={:?}, graph={:?})",
        entry.id,
        entry.colocated,
        chunk_count,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only,
        skip_kg,
        skip_vector,
        corpus_open_failure,
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
    let registered = state.registry.register(handle);
    // #4390: an embed pass interrupted before it committed leaves the corpus
    // silently short its most recent vectors, and warm boot reports `Ready`
    // regardless because an older HNSW snapshot exists on disk. Re-arm it.
    crate::service::boot_markers::rearm_deferred_embed_if_pending(
        &registered,
        deferred_embed_pending,
        chunk_count,
    )
    .await;
    // Issue #1621 (epic #1619 WI-2): activate the filesystem watcher for this
    // warm-booted index so subsequent saves are incrementally indexed within
    // the 500ms debounce window. No-op when the watcher is disabled
    // (`TRUSTY_DISABLE_WATCHER=1`) or already watching this index.
    state.watcher_manager.spawn_for_index(&registered).await;
}

// #4390 / #4391: end-to-end warm-boot marker tests live in a sibling file so
// this module stays under the 500-SLOC production cap.
#[cfg(test)]
#[path = "start_restore_markers_tests.rs"]
mod markers_tests;

// #767: the relocation gate's own tests. Inline (rather than in a sibling file)
// because `relocation_candidate_is_approved` is private to this module.
#[cfg(test)]
mod relocation_gate_tests_767 {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A candidate root that survives the strict denylist — see
    /// `service::server::test_support`'s module doc for why `$HOME`, not
    /// `$TMPDIR`.
    fn home_anchored(prefix: &str) -> tempfile::TempDir {
        let base = dirs::home_dir()
            .expect("HOME must be set to run trusty-search tests")
            .join(".trusty-search-test-roots");
        std::fs::create_dir_all(&base).expect("create test-roots base");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(&base)
            .expect("create home-anchored candidate root")
    }

    fn approving(dir: &Path, roots: &[&Path]) -> crate::allowlist::AllowlistPaths {
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(dir.join("allowlist.toml"))
            .with_project_paths(dir.join("no-projects.json"));
        let cfg = crate::allowlist::AllowlistConfig {
            entries: roots
                .iter()
                .map(|p| crate::allowlist::AllowlistEntry {
                    path: p.to_path_buf(),
                    name: None,
                    exclude: Vec::new(),
                    extensions: Vec::new(),
                    skip_kg: false,
                })
                .collect(),
        };
        cfg.save_to(&paths.allowlist_file())
            .expect("write allowlist");
        paths
    }

    /// An unapproved candidate is dropped, so no approved-but-missing entry can
    /// silently relocate onto it.
    ///
    /// Why (#767 CRITICAL): the salvage phase persists the adopted root via
    /// `upsert_index_registry_entry` and starts a watcher on it, with no
    /// operator action. Before this filter a leftover `.trusty-search/` in a
    /// personal directory was a valid candidate.
    #[test]
    fn relocation_candidates_drop_unapproved_roots() {
        let fx = tempfile::tempdir().expect("tempdir");
        let unapproved = home_anchored("ts-767-unapproved");
        let canonical = unapproved.path().canonicalize().expect("canonicalize");
        let paths = approving(fx.path(), &[]);
        assert!(!relocation_candidate_is_approved(&canonical, &paths));
    }

    /// The hard denylist drops a candidate even when it is allowlisted — the
    /// relocation door does not get a weaker denylist than `POST /indexes`.
    #[test]
    fn relocation_candidates_drop_denylisted_roots() {
        let fx = tempfile::tempdir().expect("tempdir");
        let ssh = dirs::home_dir().expect("home").join(".ssh");
        let paths = approving(fx.path(), &[&ssh]);
        assert!(!relocation_candidate_is_approved(&ssh, &paths));

        // An OS temp dir is denied too — this is the case the warm-boot RESTORE
        // filter deliberately relaxes and this one deliberately does not.
        let tmp = tempfile::tempdir().expect("tempdir");
        let tmp_canonical = tmp.path().canonicalize().expect("canonicalize");
        let paths = approving(fx.path(), &[&tmp_canonical]);
        assert!(!relocation_candidate_is_approved(&tmp_canonical, &paths));
    }

    /// An approved candidate is kept — the gate denies by policy, not by
    /// breaking relocation.
    #[test]
    fn relocation_candidates_keep_approved_roots() {
        let fx = tempfile::tempdir().expect("tempdir");
        let approved = home_anchored("ts-767-approved");
        let canonical = approved.path().canonicalize().expect("canonicalize");
        let paths = approving(fx.path(), &[&canonical]);
        assert!(relocation_candidate_is_approved(&canonical, &paths));
    }

    /// An unreadable allowlist drops the candidate. Adopting a NEW root is not
    /// something to do on a policy the daemon could not read — unlike warm-boot
    /// restore, which keeps what it already had.
    #[test]
    fn relocation_candidates_drop_when_allowlist_unreadable() {
        let fx = tempfile::tempdir().expect("tempdir");
        let root = home_anchored("ts-767-corrupt");
        let canonical = root.path().canonicalize().expect("canonicalize");
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(fx.path().join("allowlist.toml"))
            .with_project_paths(fx.path().join("no-projects.json"));
        std::fs::write(paths.allowlist_file(), "not toml [[[").expect("write");
        assert!(!relocation_candidate_is_approved(&canonical, &paths));
    }

    /// The filter runs inside `collect_relocation_candidates`, BEFORE
    /// `try_locate_moved_root` can persist anything — a check downstream of
    /// that function would run after `upsert_index_registry_entry`.
    #[test]
    fn unapproved_candidate_never_reaches_try_locate_moved_root() {
        let fx = tempfile::tempdir().expect("tempdir");
        let unapproved = home_anchored("ts-767-notreached");
        let canonical = unapproved.path().canonicalize().expect("canonicalize");
        let paths = approving(fx.path(), &[]);

        // Simulate what `collect_relocation_candidates` produces after filtering.
        let roots: Vec<PathBuf> = vec![canonical]
            .into_iter()
            .filter(|r| relocation_candidate_is_approved(r, &paths))
            .collect();
        let candidates = RelocationCandidates { roots };
        assert!(candidates.roots.is_empty());

        let mut entry = PersistedIndex::new("missing", PathBuf::from("/nonexistent-767"));
        entry.colocated = true;
        assert!(
            try_locate_moved_root(&entry, &candidates).is_none(),
            "with an empty candidate set there is nothing to adopt or persist"
        );
    }
}
