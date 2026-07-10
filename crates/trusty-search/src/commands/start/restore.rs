//! Warm-boot index restoration logic for `trusty-search start`.
//!
//! Why: before this module, every daemon restart required full re-indexing.
//! The warm-boot path re-registers all indexes recorded in `indexes.toml` and
//! in colocated roots so that queries are served immediately after restart.
//! Issues #85 / #403 / #718 / #723 bound the scan and open operations with
//! timeouts and per-volume probes.
//!
//! What: `restore_indexes` collects legacy and colocated entries, applies
//! selective warm-boot (issue #993) to split into eager/cold slices, restores
//! only the eager slice via `restore_one_index_bounded`, and parks cold
//! entries in `state.cold_store` for lazy on-demand loading.
//!
//! Test: integration test in `tests/integration_tests.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::commands::start_restore::restore_one_index;
use crate::service::SearchAppState;

use crate::service::lazy_loader::{select_warmboot_entries, warmboot_max_indexes};
use crate::service::warm_boot::{
    collect_colocated_entries, collect_legacy_entries, is_on_inaccessible_volume,
    probe_warmboot_volumes, probe_warmboot_volumes_from_paths, restore_one_index_bounded,
};

/// Restore every index recorded in `indexes.toml` and in colocated roots by
/// re-registering it on the in-memory registry.
///
/// Why (issues #85 / #403 / #718 / #723): before this hook every restart
/// required re-indexing. #718 bounded scans and opens with
/// `spawn_blocking` + timeout. #723 adds probe-per-volume: each distinct
/// volume is probed ONCE on a bare OS thread before any redb opens so a
/// TCC-blocked volume costs at most one leaked thread (not one-per-index).
/// What: collects all entries (legacy + colocated), applies selective warm-boot
/// (issue #993) to split into eager/cold slices, then restores only the eager
/// slice via `restore_one_index_bounded`. Cold entries are registered into
/// `state.cold_store` for lazy on-demand loading.
/// Test: integration test in `tests/integration_tests.rs`.
pub(super) async fn restore_indexes(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
) {
    // Issue #993: read TRUSTY_WARMBOOT_MAX_INDEXES once before collecting.
    let max_warmboot = warmboot_max_indexes();
    if let Some(n) = max_warmboot {
        tracing::info!(
            "warm-boot: TRUSTY_WARMBOOT_MAX_INDEXES={n} — will eager-load top-{n} \
             by recency, defer the rest to cold store (issue #993)"
        );
    }

    // ── Collect: legacy + colocated entries ──────────────────────────────────
    // Self-heal (orphan self-heal): drop legacy registrations whose root_path
    // was deleted (e.g. an ephemeral `.worktrees/<uuid>` removed while the
    // daemon was down) before we try to restore or watch them. Without this the
    // dead entries are re-read, re-warned, and re-skipped on every single boot
    // and never leave `indexes.toml`. Colocated entries are left for the
    // relocation scan; unmounted volumes are spared by `is_reapable_orphan`.
    let legacy_entries = crate::service::orphan_reaper::heal_boot_orphans(collect_legacy_entries());
    let mut seen_ids: HashSet<String> = HashSet::new();
    // Issue #860: track canonicalized root_paths from legacy entries so that
    // colocated scan suppresses entries for the same root.
    let mut seen_root_paths: HashSet<std::path::PathBuf> = HashSet::new();
    for e in &legacy_entries {
        seen_ids.insert(e.id.clone());
        seen_root_paths.insert(crate::service::warm_boot::canonicalize_best_effort(
            &e.root_path,
        ));
    }

    if legacy_entries.is_empty() {
        tracing::warn!(
            "warm-boot: no legacy index entries (indexes.toml absent/empty). \
             Under launchd, set TRUSTY_DATA_DIR to an absolute path (issue #718)."
        );
    }

    let colocated_inaccessible = {
        use crate::service::roots_registry::load_roots;
        match load_roots() {
            Ok(roots) => {
                let root_paths: Vec<std::path::PathBuf> =
                    roots.into_iter().map(|r| r.path).collect();
                probe_warmboot_volumes_from_paths(&root_paths)
            }
            Err(_) => std::collections::HashSet::new(),
        }
    };
    let colocated_entries =
        collect_colocated_entries(&seen_ids, &seen_root_paths, &colocated_inaccessible).await;

    // Merge into a single pool then apply selective warm-boot split (issue #993).
    let all_entries: Vec<_> = legacy_entries
        .into_iter()
        .chain(colocated_entries)
        .collect();

    // Issue #2305: collapse entries that resolve to the SAME on-disk redb corpus
    // (colocated indexes sharing a root_path) down to one handle BEFORE the
    // eager/cold split. redb is a single-open database; without this, the
    // sequential restore below opens the file for the first entry, holds the
    // corpus Arc for the daemon's lifetime, and every later entry sharing that
    // file fails its open with DatabaseAlreadyOpen on every restart (the 50 ms
    // retry can never clear an in-process holder). Deduping before the split
    // also prevents a dropped duplicate from being parked in the cold store and
    // re-triggering the double-open on first query.
    let all_entries = crate::service::warm_boot::dedup_entries_by_corpus_path(all_entries);
    let total_discovered = all_entries.len();

    let (eager_entries, cold_entries) = select_warmboot_entries(all_entries, max_warmboot);
    let indexes_lazy = cold_entries.len();

    if indexes_lazy > 0 {
        tracing::info!(
            "warm-boot: parking {indexes_lazy}/{total_discovered} index(es) in cold store \
             (lazy-load on first query, issue #993)"
        );
        state.cold_store.register_cold_entries(cold_entries);
    }

    // ── Restore: eager entries only ──────────────────────────────────────────
    // Re-build seen_ids for the fail-loud check below — it must cover all
    // legacy entries that were originally discovered (including those that went cold).
    let mut seen_legacy_ids: HashSet<String> = HashSet::new();

    // Issue #873: TCC vs timeout skip counters for WarmBootSummary.
    let mut total_skipped_tcc: usize = 0;
    let mut total_skipped_timeout: usize = 0;
    let mut total_ok: usize = 0;

    // Split eager entries by source so we can probe volumes per-batch.
    let legacy_eager: Vec<_> = eager_entries
        .iter()
        .filter(|e| !e.colocated)
        .cloned()
        .collect();
    let colocated_eager: Vec<_> = eager_entries
        .iter()
        .filter(|e| e.colocated)
        .cloned()
        .collect();

    // ── Eager: legacy entries ────────────────────────────────────────────────
    if !legacy_eager.is_empty() {
        let inaccessible_volumes = probe_warmboot_volumes(&legacy_eager);
        if !inaccessible_volumes.is_empty() {
            tracing::warn!(
                "warm-boot: {} volume(s) inaccessible (issue #723): {}",
                inaccessible_volumes.len(),
                inaccessible_volumes
                    .iter()
                    .map(|v| v.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let total_legacy = legacy_eager.len();
        tracing::info!("warm-boot: restoring {total_legacy} legacy index(es) from indexes.toml");
        let (mut legacy_ok, mut legacy_skipped_tcc, mut legacy_skipped_other) =
            (0usize, 0usize, 0usize);
        for entry in legacy_eager {
            seen_legacy_ids.insert(entry.id.clone());
            if is_on_inaccessible_volume(&entry.root_path, &inaccessible_volumes) {
                tracing::warn!(
                    "warm-boot: skipping index '{}' — volume {} inaccessible (issue #723)",
                    entry.id,
                    entry.root_path.display(),
                );
                legacy_skipped_tcc += 1;
                continue;
            }
            let s = state.clone();
            let e = Arc::clone(embedder);
            if restore_one_index_bounded(entry, move |en| async move {
                restore_one_index(&s, &e, en).await;
            })
            .await
            {
                legacy_ok += 1;
            } else {
                legacy_skipped_other += 1;
            }
        }
        total_skipped_tcc += legacy_skipped_tcc;
        total_skipped_timeout += legacy_skipped_other;
        total_ok += legacy_ok;
        tracing::info!(
            "warm-boot: legacy phase complete — {legacy_ok}/{total_legacy} \
             (skipped tcc={legacy_skipped_tcc} timeout={legacy_skipped_other})"
        );
    }

    // ── Eager: colocated entries ─────────────────────────────────────────────
    if !colocated_eager.is_empty() {
        let total_colocated = colocated_eager.len();
        tracing::info!(
            "warm-boot: restoring {total_colocated} colocated index(es) from tracked roots"
        );
        let (mut colocated_ok, mut colocated_skipped_tcc, mut colocated_skipped_other) =
            (0usize, 0usize, 0usize);
        for entry in colocated_eager {
            if is_on_inaccessible_volume(&entry.root_path, &colocated_inaccessible) {
                colocated_skipped_tcc += 1;
                continue;
            }
            let s = state.clone();
            let e = Arc::clone(embedder);
            if restore_one_index_bounded(entry, move |en| async move {
                restore_one_index(&s, &e, en).await;
            })
            .await
            {
                colocated_ok += 1;
            } else {
                colocated_skipped_other += 1;
            }
        }
        total_skipped_tcc += colocated_skipped_tcc;
        total_skipped_timeout += colocated_skipped_other;
        total_ok += colocated_ok;
        tracing::info!(
            "warm-boot: colocated phase complete — {colocated_ok}/{total_colocated} \
             (skipped tcc={colocated_skipped_tcc} timeout={colocated_skipped_other})"
        );
    }

    let total = state.registry.list().len();
    tracing::info!(
        "warm-boot: complete — {total} loaded, {indexes_lazy} cold (lazy), \
         {total_ok} eager successful (legacy + colocated)"
    );

    // Issue #873: update WarmBootSummary, emit FDA warning, persist count.
    use crate::commands::prior_index_count::record_warm_boot_result;
    record_warm_boot_result(
        state,
        total,
        total_skipped_tcc,
        total_skipped_timeout,
        indexes_lazy,
    );

    // Issue #764: fail-loud warm-boot — tally total skipped/failed indexes and
    // store the count on AppState so `/health` can surface it without operators
    // having to tail logs.
    let registered_ids: std::collections::HashSet<String> =
        state.registry.list().into_iter().map(|id| id.0).collect();
    // TODO(#796): covers only non-cold legacy entries; colocated failures can
    // under-report. Cold entries are excluded from this count intentionally —
    // they are deferred, not failed.
    let failed_count: usize = seen_legacy_ids
        .iter()
        .filter(|id| !registered_ids.contains(*id))
        .count();
    if failed_count > 0 {
        state
            .warmboot_failed_indexes
            .store(failed_count, std::sync::atomic::Ordering::Relaxed);
        tracing::error!(
            failed_count,
            registered = total,
            "warm-boot FAIL-LOUD: {failed_count} index(es) from indexes.toml did NOT load on \
             this boot (TCC denial, redb-format mismatch, or corrupt corpus). \
             These indexes are MISSING from /health and search results. \
             Run `trusty-search health` or check /health?warmboot_failed_indexes \
             for the count, then resolve the root cause and restart (issue #764).",
        );
    }
}
