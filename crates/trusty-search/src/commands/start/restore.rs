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

use crate::commands::start_restore::{
    collect_relocation_candidates, restore_one_index, RelocationScan,
};
use crate::service::SearchAppState;

use crate::service::lazy_loader::{select_warmboot_entries, warmboot_max_indexes};
use crate::service::persistence::PersistedIndex;
use crate::service::warm_boot::{
    collect_colocated_entries, collect_legacy_entries, is_on_inaccessible_volume,
    probe_warmboot_volumes, probe_warmboot_volumes_from_paths, restore_one_index_bounded,
    triage_entries, BoundedRestoreOutcome, SalvageBudget,
};

/// Collect colocated index entries for warm-boot, honoring
/// `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER` (issue #3929).
///
/// Why: `restore_indexes` previously called `collect_colocated_entries`
/// unconditionally. `--no-auto-discover`'s documented contract
/// (`commands/start/daemon.rs::handle_start`) is that "the daemon serves only
/// indexes already in `indexes.toml` or registered at runtime" — but the flag
/// only gated the UNRELATED `auto_discover_and_index()` git-repo scan
/// (`daemon.rs` ~line 436), not this one. The colocated-root scan IS a
/// discovery mechanism in exactly the sense the flag promises to disable: it
/// walks every tracked root in `roots.toml` looking for `.trusty-search/`
/// directories and registers whatever it finds under a freshly-derived id —
/// including a SECOND registration, under a different id, for a root that a
/// legacy `indexes.toml` entry already owns. Both registrations resolve to
/// the same `<root>/.trusty-search/index.redb`; redb is single-open, so the
/// second one fails with `DatabaseAlreadyOpen` (issue #3929 — 188/222 indexes
/// on the reporter's production box, despite `--no-auto-discover` being set).
/// What: when `no_auto_discover` is `true`, returns an empty `Vec` without
/// walking a single tracked root — a full no-op for the scan itself.
/// `colocated_inaccessible` (the pre-computed per-volume probe result) is
/// still accepted so the signature stays stable for the caller, which also
/// reuses it for the restore-time TCC skip check on legacy colocated entries
/// regardless of this flag. Otherwise (flag not set) delegates to
/// `collect_colocated_entries` exactly as before the fix.
/// Test: `collect_colocated_for_warmboot_skips_scan_when_no_auto_discover`,
/// `collect_colocated_for_warmboot_scans_when_auto_discover_enabled`.
async fn collect_colocated_for_warmboot(
    no_auto_discover: bool,
    seen_ids: &HashSet<String>,
    seen_root_paths: &HashSet<std::path::PathBuf>,
    colocated_inaccessible: &HashSet<std::path::PathBuf>,
) -> Vec<PersistedIndex> {
    if no_auto_discover {
        tracing::info!(
            "warm-boot: skipping colocated-root discovery scan — \
             --no-auto-discover / TRUSTY_NO_AUTO_DISCOVER is set (issue #3929); \
             serving only indexes already in indexes.toml"
        );
        return Vec::new();
    }
    collect_colocated_entries(seen_ids, seen_root_paths, colocated_inaccessible).await
}

/// Drop warm-boot entries whose root the #767 allowlist no longer approves.
///
/// Why: de-allowlisting must actually stop indexing. A root left in
/// `indexes.toml` is re-registered on every boot and its file watcher keeps it
/// current, so without this filter "remove it from the allowlist" would only
/// block NEW registrations while the existing one carried on indefinitely.
/// What: keeps an entry when the allowlist union approves its `root_path`,
/// drops it otherwise with a `warn` naming the root and the remedy.
///
/// Two deliberate asymmetries with the create-time gate:
///
/// - The denylist applied here is
///   [`crate::allowlist::is_denied_allowing_sensitive_path`], not the strict
///   variant. A registered entry ALREADY passed `validate_root_path`, possibly
///   via `allow_sensitive_path: true` — which is how a bake-off root under
///   `/var/folders` legitimately gets indexed. Re-applying the strict form at
///   boot would revoke, on the next restart, an approval the caller explicitly
///   asked for. The credential and home-directory checks are never relaxed.
/// - An allowlist that cannot be READ keeps every entry. That is a different
///   failure from one that denies: un-indexing the whole fleet because a TOML
///   file got corrupted would be a self-inflicted outage.
///
/// The on-disk data is untouched either way; approving the root again restores
/// it on the next boot.
/// Test: `warmboot_drops_unapproved_entries`,
/// `warmboot_keeps_entries_when_allowlist_unreadable`,
/// `warmboot_counts_every_entry_the_allowlist_excluded`.
fn retain_approved_entries(
    entries: Vec<PersistedIndex>,
    paths: &crate::allowlist::AllowlistPaths,
) -> RetainOutcome {
    let mut excluded = 0usize;
    let kept = entries
        .into_iter()
        .filter(|entry| {
            // Canonicalise ONCE and use it for both checks. Every other gate
            // runs on the canonical form (`validate_root_path` canonicalises
            // before either check), so testing the raw stored path here would
            // let a symlinked root answer differently than it does at creation.
            let canonical = crate::service::warm_boot::canonicalize_best_effort(&entry.root_path);
            if let Some(reason) = crate::allowlist::is_denied_allowing_sensitive_path(&canonical) {
                tracing::warn!(
                    id = %entry.id,
                    root = %canonical.display(),
                    %reason,
                    "warm-boot: skipping index — its root is on the hard denylist (#767)"
                );
                excluded += 1;
                return false;
            }
            match crate::allowlist::sources::resolve_allow_source(&canonical, paths) {
                Ok(Some(_)) => true,
                Ok(None) => {
                    tracing::warn!(
                        id = %entry.id,
                        root = %entry.root_path.display(),
                        "warm-boot: skipping index — its root is not approved for indexing. \
                         This is an ALLOWLIST decision, not a permissions problem: approve it \
                         with `trusty-search index add <path>` (#767, #5926)"
                    );
                    excluded += 1;
                    false
                }
                Err(e) => {
                    tracing::warn!(
                        id = %entry.id,
                        root = %entry.root_path.display(),
                        "warm-boot: allowlist unreadable ({e:#}) — keeping this index rather \
                         than un-indexing the fleet over a config-file error (#767)"
                    );
                    true
                }
            }
        })
        .collect();
    RetainOutcome { kept, excluded }
}

/// What [`retain_approved_entries`] kept, and how many entries it dropped.
///
/// Why (#5926): the drop count used to exist only as one `warn` line per entry.
/// Nothing read it, so a boot that excluded 103 of 121 registered indexes
/// surfaced on `/health` as `skipped_tcc: 0` plus a generic "< 80% of prior"
/// error whose remedy text was re-granting Full Disk Access — an explanation the
/// counters themselves contradicted. Returning the count is what lets
/// `WarmBootSummary::indexes_skipped_unapproved` name the real cause.
/// What: the surviving entries plus the number excluded by the denylist or by
/// the allowlist union. An entry kept because the allowlist was UNREADABLE is
/// not counted — nothing was excluded in that case.
/// Test: `warmboot_counts_every_entry_the_allowlist_excluded`.
struct RetainOutcome {
    kept: Vec<PersistedIndex>,
    excluded: usize,
}

/// Restore every index recorded in `indexes.toml` and in colocated roots by
/// re-registering it on the in-memory registry.
///
/// Why (issues #85 / #403 / #718 / #723): before this hook every restart
/// required re-indexing. #718 bounded scans and opens with
/// `spawn_blocking` + timeout. #723 adds probe-per-volume: each distinct
/// volume is probed ONCE on a bare OS thread before any redb opens so a
/// TCC-blocked volume costs at most one leaked thread (not one-per-index).
/// Issue #3929: `no_auto_discover` must gate the colocated-root discovery
/// scan too — see `collect_colocated_for_warmboot`.
/// What: collects all entries (legacy + colocated), applies selective warm-boot
/// (issue #993) to split into eager/cold slices, then restores only the eager
/// slice via `restore_one_index_bounded`. Cold entries are registered into
/// `state.cold_store` for lazy on-demand loading.
/// Test: integration test in `tests/integration_tests.rs`;
///       `collect_colocated_for_warmboot_*` in this module (issue #3929).
pub(super) async fn restore_indexes(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    no_auto_discover: bool,
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
    let colocated_entries = collect_colocated_for_warmboot(
        no_auto_discover,
        &seen_ids,
        &seen_root_paths,
        &colocated_inaccessible,
    )
    .await;

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
    //
    // Issue #2337 follow-up: the verbose variant also reports which entries
    // were dropped and which survivors absorbed a dropped entry's config, so
    // `prune_and_persist_dedup_outcome` can self-heal `indexes.toml` — pruning
    // the losing rows (otherwise re-discovered/re-warned/re-dropped forever)
    // and persisting the merged survivor config so it survives future boots.
    // #767: an entry in `indexes.toml` is a record of a PAST approval, not a
    // standing one. Without this filter, pruning a root from `allowlist.toml`
    // would not stop it being indexed — warm-boot would re-register it and its
    // file watcher would keep it current, so the acceptance criterion "removing
    // it stops indexing" would hold for `POST /indexes` and fail on restart.
    let retained = retain_approved_entries(all_entries, &state.allowlist_paths);
    let indexes_skipped_unapproved = retained.excluded;
    let all_entries = retained.kept;

    let dedup_outcome =
        crate::service::warm_boot::dedup_entries_by_corpus_path_verbose(all_entries);
    prune_and_persist_dedup_outcome(&dedup_outcome);
    let all_entries = dedup_outcome.survivors;
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
    // #4087 review follow-up: panicked restores are NOT timeouts and are never
    // parked — tallied separately so the timeout counter stops absorbing them.
    let mut total_panicked: usize = 0;
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

    // #4846: settle every eager entry's root with ONE stat before any restore
    // runs. An entry whose root_path is gone used to enter the same loop under
    // the same per-index deadline as a live one and drag a full tracked-root
    // relocation walk (measured 9.5–10.5 s over 248 roots) behind it, per
    // entry — which is how 55 dead registrations on the reporting machine
    // starved a live 70k-chunk index for the better part of an hour. Triage
    // returns two SEPARATE vectors, so the loops below iterate only live
    // entries and have no way to reach a dead one; "live first" is a property
    // of the types, not of loop ordering.
    let legacy_triaged = triage_entries(legacy_eager);
    let colocated_triaged = triage_entries(colocated_eager);
    // The fail-loud diff at the end of this function keys off every legacy id
    // that was DISCOVERED, so record the missing ones here — they never reach
    // the legacy loop that used to do it.
    for e in &legacy_triaged.missing {
        seen_legacy_ids.insert(e.id.clone());
    }
    let missing_roots: Vec<PersistedIndex> = legacy_triaged
        .missing
        .into_iter()
        .chain(colocated_triaged.missing)
        .collect();
    let legacy_eager = legacy_triaged.present;
    let colocated_eager = colocated_triaged.present;
    if !missing_roots.is_empty() {
        tracing::info!(
            "warm-boot: {} eager entr(ies) have a root_path that no longer exists — \
             deferred to the budgeted salvage phase so they cannot consume the live \
             indexes' restore budget (issue #4846)",
            missing_roots.len(),
        );
    }

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
        // #4087 review follow-up: `timed_out` and `panicked` are counted
        // SEPARATELY — they used to share one bucket, which is how a panicked
        // restore came to be parked (and reported) as a recoverable timeout.
        let (mut legacy_ok, mut legacy_skipped_tcc, mut legacy_timed_out, mut legacy_panicked) =
            (0usize, 0usize, 0usize, 0usize);
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
            // #4846: `Unavailable` is correct here, not a limitation — this
            // entry's root existed at triage, so `restore_one_index`'s
            // relocation branch is unreachable for it. Should the root vanish
            // between the stat and the restore, skipping is the right answer
            // and it costs nothing.
            match restore_eager_entry(state, embedder, entry, RelocationScan::Unavailable).await {
                BoundedRestoreOutcome::Completed => legacy_ok += 1,
                BoundedRestoreOutcome::TimedOut => legacy_timed_out += 1,
                BoundedRestoreOutcome::Panicked => legacy_panicked += 1,
            }
        }
        total_skipped_tcc += legacy_skipped_tcc;
        total_skipped_timeout += legacy_timed_out;
        total_panicked += legacy_panicked;
        total_ok += legacy_ok;
        tracing::info!(
            "warm-boot: legacy phase complete — {legacy_ok}/{total_legacy} \
             (skipped tcc={legacy_skipped_tcc} timeout={legacy_timed_out} \
             panicked={legacy_panicked})"
        );
    }

    // ── Eager: colocated entries ─────────────────────────────────────────────
    if !colocated_eager.is_empty() {
        let total_colocated = colocated_eager.len();
        tracing::info!(
            "warm-boot: restoring {total_colocated} colocated index(es) from tracked roots"
        );
        let (mut colocated_ok, mut colocated_skipped_tcc, mut colo_timed_out, mut colo_panicked) =
            (0usize, 0usize, 0usize, 0usize);
        for entry in colocated_eager {
            if is_on_inaccessible_volume(&entry.root_path, &colocated_inaccessible) {
                colocated_skipped_tcc += 1;
                continue;
            }
            // #4846: see the legacy loop — a present root never consults the
            // relocation set.
            match restore_eager_entry(state, embedder, entry, RelocationScan::Unavailable).await {
                BoundedRestoreOutcome::Completed => colocated_ok += 1,
                BoundedRestoreOutcome::TimedOut => colo_timed_out += 1,
                BoundedRestoreOutcome::Panicked => colo_panicked += 1,
            }
        }
        total_skipped_tcc += colocated_skipped_tcc;
        total_skipped_timeout += colo_timed_out;
        total_panicked += colo_panicked;
        total_ok += colocated_ok;
        tracing::info!(
            "warm-boot: colocated phase complete — {colocated_ok}/{total_colocated} \
             (skipped tcc={colocated_skipped_tcc} timeout={colo_timed_out} \
             panicked={colo_panicked})"
        );
    }

    // ── Salvage: entries whose root_path was missing at triage (#4846) ───────
    // Runs strictly AFTER both live phases and under its own global ceiling, so
    // however long it takes or however early it gives up, no live index has
    // already lost anything to it.
    if !missing_roots.is_empty() {
        let salvage = salvage_missing_root_entries(state, embedder, missing_roots).await;
        total_ok += salvage.ok;
        total_skipped_timeout += salvage.timed_out;
        total_panicked += salvage.panicked;
    }

    let total = state.registry.list().len();
    // #4087: read the LIVE cold-store size rather than the pre-loop split
    // count, so indexes parked by `ColdIndexStore::park_timed_out` are reported as
    // lazy/recoverable instead of vanishing from every counter (the old
    // `indexes_lazy: 0` alongside `indexes_skipped_timeout: 11` that made the
    // drop invisible on `/health`).
    let indexes_lazy = state.cold_store.len();
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
        indexes_skipped_unapproved,
    );

    // #4087 review follow-up: a panicked restore is real breakage. It is never
    // parked, so it does not appear in `indexes_lazy`; log it at ERROR here so
    // it is loud on its own terms rather than relying on the legacy-only
    // id-diff below (which under-reports colocated entries — TODO #796).
    if total_panicked > 0 {
        tracing::error!(
            panicked = total_panicked,
            "warm-boot FAIL-LOUD: {total_panicked} index(es) PANICKED during restore. These \
             are BROKEN, not slow: they were deliberately NOT parked for lazy retry and are \
             absent from search until the fault is fixed and the daemon restarts (#4087)."
        );
    }

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

/// Run one eager restore under its deadline, parking it cold ONLY if it timed
/// out (#4087, and the review follow-up that scoped this correctly).
///
/// Why: the parking decision previously lived inline in BOTH eager loops, keyed
/// off `!completed` — which is `TimedOut` OR `Panicked`. That parked a panicked
/// (genuinely broken) index into the cold store, where `/health` reports it as
/// lazy and therefore recoverable. #4087 exists to stop broken indexes from
/// masquerading as fine; parking breakage reintroduces exactly that, one layer
/// up. Centralising the decision here means there is one place that can make it,
/// and it is gated on [`BoundedRestoreOutcome::is_parkable`] — a named
/// predicate rather than a negation, because the negation is what shipped the
/// bug.
/// What: runs `restore_one_index_bounded` and hands the entry plus its typed
/// outcome to `ColdIndexStore::park_if_parkable`, which owns the decision — this
/// function no longer has a branch to get wrong. Returns the outcome for the
/// caller's tally.
/// Test: `only_timed_out_is_parkable` (the predicate) and
/// `panicked_restore_is_not_parked_in_cold_store` /
/// `timed_out_entry_is_parked_in_cold_store` (the effect) in
/// `service::server::tests_4087`.
async fn restore_eager_entry(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    entry: PersistedIndex,
    relocation: RelocationScan,
) -> BoundedRestoreOutcome {
    let s = state.clone();
    let e = Arc::clone(embedder);
    // Keep a copy so a TIMED-OUT restore can be parked cold rather than dropped.
    let parked = entry.clone();
    let outcome = restore_one_index_bounded(entry, move |en| async move {
        restore_one_index(&s, &e, en, relocation).await;
    })
    .await;
    state.cold_store.park_if_parkable(parked, outcome);
    outcome
}

/// Outcome tally for the salvage phase (#4846).
///
/// Why: the salvage phase reports into the same warm-boot counters as the live
/// phases, but it is a separate pass with a separate budget, so its results are
/// gathered separately rather than mutating the caller's locals from inside a
/// helper.
/// What: `skipped` counts entries the budget refused — they are neither loaded
/// nor failed, just untouched until the next boot.
#[derive(Default)]
struct SalvageTally {
    ok: usize,
    timed_out: usize,
    panicked: usize,
    skipped: usize,
}

/// Attempt to relink and restore the missing-root cohort under one global
/// budget (#4846).
///
/// Why: this is the whole point of the issue. The pre-fix path gave each dead
/// entry its own 10-second per-index deadline and, inside it, a fresh
/// depth-5 walk of every tracked root — so cost scaled with accumulated
/// registry cruft rather than with real index count, and the walk's identical
/// result was recomputed once per dead entry. Here the walk runs ONCE for the
/// whole cohort, and [`SalvageBudget`] caps the cohort's total wall time
/// instead of multiplying a per-entry allowance by however many dead rows the
/// registry has accreted.
/// What: takes a grant (refusing outright when salvage is disabled or already
/// spent), runs `collect_relocation_candidates` once on the blocking pool,
/// then walks the cohort re-checking the budget before each entry so a slow
/// cohort stops at the ceiling instead of running past it. Entries the budget
/// refuses are left exactly as they are: registered, on-disk data untouched,
/// retried next boot. Nothing here reaps a registration or deletes a corpus —
/// a failed probe is not evidence about the corpus (#4846 operator note).
/// Test: `dead_entries_do_not_consume_the_live_index_budget`,
/// `disabled_salvage_budget_costs_a_dead_entry_nothing_but_a_stat`.
async fn salvage_missing_root_entries(
    state: &SearchAppState,
    embedder: &Arc<dyn crate::core::Embedder>,
    entries: Vec<PersistedIndex>,
) -> SalvageTally {
    let mut tally = SalvageTally::default();
    let budget = SalvageBudget::from_env();
    let Some(grant) = budget.try_grant() else {
        tally.skipped = entries.len();
        tracing::warn!(
            "warm-boot salvage: {} entr(ies) with a missing root_path were NOT probed — \
             the salvage budget is disabled or already spent ({}). Their registrations and \
             on-disk index data are untouched; they are retried on the next boot. \
             (issue #4846)",
            tally.skipped,
            crate::service::warm_boot::SALVAGE_BUDGET_ENV,
        );
        return tally;
    };

    // One walk for the whole cohort, on the blocking pool — it is recursive
    // `read_dir` + `canonicalize`, not async work.
    let all_entries = crate::service::persistence::load_index_registry().unwrap_or_default();
    let started = std::time::Instant::now();
    // #767: adopting a relocated root registers, watches, and PERSISTS it with
    // no operator action, so the candidate set is gated like `POST /indexes`.
    let allowlist_paths = state.allowlist_paths.clone();
    let candidates = match tokio::task::spawn_blocking(move || {
        collect_relocation_candidates(&all_entries, &grant, &allowlist_paths)
    })
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tally.skipped = entries.len();
            tracing::error!(
                "warm-boot salvage: the shared relocation scan panicked ({e}) — {} \
                     missing-root entr(ies) left untouched for the next boot (issue #4846)",
                tally.skipped,
            );
            return tally;
        }
    };
    tracing::info!(
        "warm-boot salvage: tracked-root relocation scan completed in {:?} — ONE walk shared \
         by all {} missing-root entr(ies) instead of one walk each (issue #4846)",
        started.elapsed(),
        entries.len(),
    );

    let scan = RelocationScan::Ready(Arc::new(candidates));
    let total = entries.len();
    for (i, entry) in entries.into_iter().enumerate() {
        if budget.try_grant().is_none() {
            tally.skipped = total - i;
            tracing::warn!(
                "warm-boot salvage: budget exhausted after {i}/{total} missing-root \
                 entr(ies) — the remaining {} are left registered and untouched for the next \
                 boot. No live index paid for this: the salvage phase runs only after both \
                 eager phases have finished. (issue #4846)",
                tally.skipped,
            );
            break;
        }
        match restore_eager_entry(state, embedder, entry, scan.clone()).await {
            BoundedRestoreOutcome::Completed => tally.ok += 1,
            BoundedRestoreOutcome::TimedOut => tally.timed_out += 1,
            BoundedRestoreOutcome::Panicked => tally.panicked += 1,
        }
    }

    tracing::info!(
        "warm-boot salvage: complete — {} relinked, {} timed out, {} panicked, {} skipped \
         (issue #4846)",
        tally.ok,
        tally.timed_out,
        tally.panicked,
        tally.skipped,
    );
    tally
}

/// Self-heal `indexes.toml` after warm-boot dedup collapses a corpus-path
/// collision (issue #2337, follow-up to #2305).
///
/// Why: without this, a dropped duplicate's `indexes.toml` row survived
/// forever — re-discovered, re-warned, and re-dropped on every single boot
/// (log spam plus a ghost registry row that no API acknowledges, since
/// `GET /indexes` and friends correctly 404 the dropped id). Mirrors the
/// existing `orphan_reaper::heal_boot_orphans` self-heal pattern that already
/// does the same kind of `indexes.toml` cleanup for a different orphan shape.
/// What: no-ops immediately when nothing was dropped (the common case, zero
/// collisions on most boots). Otherwise: removes every dropped entry's row
/// via `remove_index_registry_entry` (idempotent no-op if the id never had a
/// row — e.g. a purely colocated-scan discovery that was never persisted);
/// for every survivor id in `merged_survivor_ids` (i.e. it actually absorbed
/// a dropped entry's config), upserts the merged `PersistedIndex` so the
/// preserved list-type config (issue #2337 part 2) is durable across future
/// restarts, not just this boot's in-memory restore. Both operations are
/// best-effort: a write failure is logged and retried on the next boot,
/// never blocks startup.
/// Test: `dedup_entries_by_corpus_path_verbose` (the pure decision logic) is
/// unit-tested in `warm_boot_tests.rs`; this thin IO wrapper mirrors the
/// already-covered `remove_index_registry_entry` / `upsert_index_registry_entry`
/// round-trip semantics and is exercised end-to-end by the warm-boot
/// integration tests.
fn prune_and_persist_dedup_outcome(outcome: &crate::service::warm_boot::DedupOutcome) {
    if outcome.dropped.is_empty() {
        return;
    }

    for dropped in &outcome.dropped {
        if let Err(e) = crate::service::persistence::remove_index_registry_entry(&dropped.id) {
            tracing::warn!(
                "warm-boot dedup: could not prune indexes.toml row for dropped index '{}': \
                 {e} (issue #2337; will retry next boot)",
                dropped.id
            );
        }
    }

    for survivor in &outcome.survivors {
        if !outcome.merged_survivor_ids.contains(&survivor.id) {
            continue;
        }
        if let Err(e) = crate::service::persistence::upsert_index_registry_entry(survivor.clone()) {
            tracing::warn!(
                "warm-boot dedup: could not persist merged config for survivor '{}': {e} \
                 (issue #2337; the merge still applies for this boot's in-memory restore)",
                survivor.id
            );
        }
    }

    tracing::info!(
        "warm-boot dedup: pruned {} indexes.toml row(s), persisted merged config for {} \
         survivor(s) (issue #2337)",
        outcome.dropped.len(),
        outcome.merged_survivor_ids.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #3929 — the regression): before the fix,
    /// `collect_colocated_for_warmboot` (like the `restore_indexes` call site
    /// it replaced) ran the colocated-root discovery scan unconditionally,
    /// ignoring `--no-auto-discover`. This is the reporter's exact production
    /// repro: a real colocated root is tracked in `roots.toml`; with
    /// `no_auto_discover = true`, warm-boot must NOT discover or return it.
    /// What: register one real colocated root via `roots_registry::upsert_root`,
    /// call `collect_colocated_for_warmboot(true, ..)`, assert the result is
    /// empty.
    /// Note: `serial` prevents parallel env-var mutation from other tests
    /// (`TRUSTY_DATA_DIR` is shared global state), matching the pattern used
    /// throughout `warm_boot_tests.rs`.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn collect_colocated_for_warmboot_skips_scan_when_no_auto_discover() {
        let data_tmp = tempfile::tempdir().unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let ts_dir = real_root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        }
        crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

        let seen_ids: HashSet<String> = HashSet::new();
        let seen_root_paths: HashSet<std::path::PathBuf> = HashSet::new();
        let inaccessible: HashSet<std::path::PathBuf> = HashSet::new();

        let results =
            collect_colocated_for_warmboot(true, &seen_ids, &seen_root_paths, &inaccessible).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        assert!(
            results.is_empty(),
            "--no-auto-discover must suppress the colocated-root discovery scan \
             entirely (issue #3929); got: {results:?}"
        );
    }

    /// Why: the fix must not regress the default (flag NOT set) path — the
    /// colocated scan must still discover tracked roots when
    /// `no_auto_discover` is `false`, exactly as before issue #3929.
    /// What: same setup as the skip test but with `no_auto_discover = false`;
    /// assert the real root IS discovered.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn collect_colocated_for_warmboot_scans_when_auto_discover_enabled() {
        let data_tmp = tempfile::tempdir().unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let ts_dir = real_root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        }
        crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

        let seen_ids: HashSet<String> = HashSet::new();
        let seen_root_paths: HashSet<std::path::PathBuf> = HashSet::new();
        let inaccessible: HashSet<std::path::PathBuf> = HashSet::new();

        let results =
            collect_colocated_for_warmboot(false, &seen_ids, &seen_root_paths, &inaccessible).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        assert_eq!(
            results.len(),
            1,
            "with no_auto_discover=false the colocated scan must still discover \
             tracked roots exactly as before issue #3929; got: {results:?}"
        );
        let canonical_root = real_root.path().canonicalize().unwrap();
        assert_eq!(results[0].root_path, canonical_root);
    }

    /// A warm-boot entry whose root the allowlist no longer approves is
    /// dropped, so de-allowlisting actually stops indexing (#767).
    ///
    /// Why: `indexes.toml` records a PAST approval. Without this filter a
    /// pruned root would be re-registered on every boot and its watcher would
    /// keep it current forever — the gate would hold only for new roots.
    #[test]
    fn warmboot_drops_unapproved_entries() {
        let fx = tempfile::tempdir().expect("tempdir");
        let approved_dir = tempfile::Builder::new()
            .prefix("ts-warmboot-approved")
            .tempdir_in(dirs::home_dir().expect("home"))
            .expect("tempdir");
        let approved = approved_dir.path().canonicalize().expect("canonicalize");
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(fx.path().join("allowlist.toml"))
            .with_project_paths(fx.path().join("projects.json"));
        crate::allowlist::add_to_allowlist(
            crate::allowlist::AllowlistEntry {
                path: approved.clone(),
                name: None,
                exclude: Vec::new(),
                extensions: Vec::new(),
                skip_kg: false,
            },
            Some(&paths.allowlist_file()),
        )
        .expect("seed allowlist");

        // A SIBLING of the approved root, not a descendant: containment
        // deliberately approves anything inside an approved root, so only an
        // unrelated path proves the filter is doing anything.
        let unrelated = tempfile::Builder::new()
            .prefix("ts-warmboot-unrelated")
            .tempdir_in(dirs::home_dir().expect("home"))
            .expect("tempdir");
        let kept = PersistedIndex::new("kept", approved.clone());
        let dropped = PersistedIndex::new(
            "dropped",
            unrelated.path().canonicalize().expect("canonicalize"),
        );
        let out = retain_approved_entries(vec![kept, dropped], &paths);
        assert_eq!(out.kept.len(), 1, "{:?}", out.kept);
        assert_eq!(out.kept[0].id, "kept");
        assert_eq!(out.excluded, 1, "the drop must be counted, not only logged");
    }

    /// Every entry the allowlist excluded is counted, so `/health` can name the
    /// cause instead of leaving a "< 80% of prior" error to imply a TCC denial.
    ///
    /// Why (#5926): the exclusion existed only as one `warn` line per entry.
    /// A boot that dropped 103 of 121 registered indexes reported
    /// `skipped_tcc: 0` and an error whose remedy was re-granting Full Disk
    /// Access — the one explanation its own counters ruled out. Against the
    /// pre-fix code this test does not compile, because the drop count had no
    /// representation to assert on.
    #[test]
    fn warmboot_counts_every_entry_the_allowlist_excluded() {
        let fx = tempfile::tempdir().expect("tempdir");
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(fx.path().join("allowlist.toml"))
            .with_project_paths(fx.path().join("projects.json"));
        // An empty (but readable) allowlist: nothing is approved, so every
        // entry is excluded by the union rather than by the denylist.
        crate::allowlist::AllowlistConfig::default()
            .save_to(&paths.allowlist_file())
            .expect("write empty allowlist");

        let entries: Vec<PersistedIndex> = (0..3)
            .map(|i| {
                let dir = tempfile::Builder::new()
                    .prefix("ts-warmboot-count")
                    .tempdir_in(dirs::home_dir().expect("home"))
                    .expect("tempdir");
                let root = dir.path().canonicalize().expect("canonicalize");
                // Keep the tempdir alive for the length of the call.
                std::mem::forget(dir);
                PersistedIndex::new(format!("e{i}"), root)
            })
            .collect();

        let out = retain_approved_entries(entries, &paths);
        assert!(out.kept.is_empty(), "{:?}", out.kept);
        assert_eq!(
            out.excluded, 3,
            "every excluded entry must be counted so the summary can report the cause"
        );
    }

    /// The #5926 end-to-end shape: an upgrade whose `allowlist.toml` already
    /// exists but is incomplete must not cost the operator their indexes.
    ///
    /// Why: this is the sequence the daemon runs at boot — the grandfather pass,
    /// then `retain_approved_entries` over the same registry. Against the
    /// pre-fix code the pass returns `skipped_existing` without writing
    /// anything, and this assertion fails with 1 of 3 entries surviving, which
    /// is the reported `only 11/37 indexes loaded` in miniature.
    #[test]
    fn a_partial_pre_gate_allowlist_does_not_cost_indexes_on_upgrade() {
        let fx = tempfile::tempdir().expect("tempdir");
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(fx.path().join("allowlist.toml"))
            .with_project_paths(fx.path().join("projects.json"));
        let registry_path = fx.path().join("indexes.toml");

        let roots: Vec<std::path::PathBuf> = (0..3)
            .map(|_| {
                let dir = tempfile::Builder::new()
                    .prefix("ts-upgrade-partial")
                    .tempdir_in(dirs::home_dir().expect("home"))
                    .expect("tempdir");
                let root = dir.path().canonicalize().expect("canonicalize");
                std::mem::forget(dir);
                root
            })
            .collect();
        let entries: Vec<PersistedIndex> = roots
            .iter()
            .enumerate()
            .map(|(i, root)| PersistedIndex::new(format!("e{i}"), root.clone()))
            .collect();
        crate::service::persistence::save_index_registry_at(&registry_path, &entries)
            .expect("write registry");

        // The pre-upgrade file: one of the three roots, hand-added before the
        // gate read this file at all.
        let mut existing = crate::allowlist::AllowlistConfig::default();
        existing.upsert(crate::allowlist::AllowlistEntry {
            path: roots[0].clone(),
            name: None,
            exclude: Vec::new(),
            extensions: Vec::new(),
            skip_kg: false,
        });
        existing
            .save_to(&paths.allowlist_file())
            .expect("write partial allowlist");

        crate::allowlist::grandfather_existing_indexes(&paths, &registry_path)
            .expect("grandfather");

        let out = retain_approved_entries(entries, &paths);
        assert_eq!(
            out.excluded, 0,
            "an upgrade must not silently un-index a registered root: {:?}",
            out.kept
        );
        assert_eq!(out.kept.len(), 3, "{:?}", out.kept);
    }

    /// An unreadable allowlist keeps every entry. Un-indexing the whole fleet
    /// because a config file got corrupted would be a self-inflicted outage —
    /// a policy that cannot be read is a different failure from one that says
    /// "no" (#767).
    #[test]
    fn warmboot_keeps_entries_when_allowlist_unreadable() {
        let fx = tempfile::tempdir().expect("tempdir");
        let paths = crate::allowlist::AllowlistPaths::default()
            .with_allowlist(fx.path().join("allowlist.toml"))
            .with_project_paths(fx.path().join("projects.json"));
        std::fs::write(paths.allowlist_file(), "not toml [[[").expect("write");

        let entry = PersistedIndex::new("kept", std::path::PathBuf::from("/srv/whatever"));
        let out = retain_approved_entries(vec![entry], &paths);
        assert_eq!(
            out.kept.len(),
            1,
            "a corrupt allowlist must not un-index: {:?}",
            out.kept
        );
        assert_eq!(
            out.excluded, 0,
            "nothing was excluded, so nothing must be counted as excluded"
        );
    }
}
