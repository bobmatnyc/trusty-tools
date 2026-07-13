//! Resilient warm-boot index collection for the trusty-search daemon.
//!
//! Why (issues #718 / #723): blocking fs scans and redb opens on a TCC-denied
//! external volume hang uninterruptibly under macOS launchd. #718 bounded each
//! per-root scan and per-index restore with `spawn_blocking` + timeout. #723
//! closes the remaining gap: probes each distinct volume ONCE on a bare OS
//! thread before any redb opens so a single blocked volume costs at most ONE
//! leaked thread (not one-per-index).
//!
//! Submodules:
//!   1. `mod.rs` (this file): public API and timeout env-var readers.
//!   2. `scan.rs`: per-root blocking fs walk.
//!   3. `restore.rs`: per-index timeout wrapper.
//!   4. `probe.rs` (#723): per-volume accessibility probe.
//!   5. `warm_boot_tests.rs` (#860): dedup tests (inline test block extracted here).
//!
//! Test: `warmboot_index_timeout_parses_env_var`,
//!       `colocated_scan_partial_failure_still_returns_accessible`,
//!       `colocated_scan_deduplicates_against_known_ids`,
//!       `colocated_scan_deduplicates_by_root_path_against_basename_legacy_id`.

pub(super) mod probe;
pub mod restore;
pub(crate) mod scan;
pub mod stages;

#[allow(deprecated)]
pub use probe::leaked_probe_thread_count; // deprecated alias for probe_thread_failures (#822)
pub use probe::probe_thread_failures;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;
pub use restore::restore_one_index_bounded;
pub use stages::{derive_warm_boot_stages, WarmBootInputs};

/// Attempt to canonicalize `path` (resolving symlinks), returning the canonical
/// form on success or the original path on failure.
///
/// Why (issues #541 / #860 / #864): both Phase 1 (`restore_indexes` in
/// `commands/start.rs`) and Phase 2 (`collect_colocated_entries` here) must
/// canonicalize root paths via the SAME function so the fallback behaviour (raw
/// path on error, `debug` log) is identical on both sides of the
/// `known_root_paths.contains(...)` equality check.  Placing the implementation
/// here (in the lib target) lets the binary-only `commands/start.rs` re-export
/// it without circular dependencies.  #864 was the finding that Phase 2 used an
/// inline `canonicalize().unwrap_or_else` while Phase 1 used this helper — the
/// semantics are the same when both succeed, but if Phase 1 had already stored
/// the raw path as a fallback and Phase 2 returned a different variant, the
/// `contains` check would miss and a ghost duplicate would slip through.
/// What: calls `std::fs::canonicalize`; on `Err` logs at `debug` level and
/// returns the original path unchanged so warm-boot is never blocked.
/// Test: `warm_boot_canonicalize_best_effort_*` unit tests in `commands/start.rs`;
///       `colocated_scan_dedup_uses_consistent_canonicalization` in
///       `service/warm_boot/warm_boot_tests.rs` (#864).
pub fn canonicalize_best_effort(path: &std::path::Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(e) => {
            tracing::debug!(
                "warm-boot: could not canonicalize root_path {}: {} (using stored path)",
                path.display(),
                e,
            );
            path.to_path_buf()
        }
    }
}

/// Per-root and per-index timeout for warm-boot restore operations.
///
/// Why: a TCC-denied or network-backed root on macOS can hang a `read_dir`,
/// `canonicalize`, or `CorpusStore::open` call for tens of seconds to minutes.
/// We impose a ceiling so that N stalled roots or indexes cost at most N × T
/// seconds, and the user gets actionable log output instead of a silent hang.
///
/// What: duration applied via `tokio::time::timeout` around each root's
/// `spawn_blocking` scan AND around each per-index `restore_one_index` task.
/// Override via `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS` (any positive integer).
///
/// Test: `warmboot_index_timeout_parses_env_var` in this module.
pub const ROOT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Read the per-index warm-boot timeout from `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS`.
///
/// Why (issue #718 Part 3): provides a single configurable knob for the per-index
/// restore deadline (colocated directory scan AND per-index redb open). Operators
/// on machines with very slow or intermittently accessible storage can raise the
/// value; operators who want faster daemon startup on problematic volumes can lower
/// it.
/// What: parses `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS` as a `u64` of seconds.
/// Falls back to `ROOT_SCAN_TIMEOUT` (10 s) on parse failure or if the variable
/// is unset. A value of `0` is treated as the default (0-second timeouts are not
/// useful in practice).
/// Test: `warmboot_index_timeout_parses_env_var` in this module.
pub fn warmboot_index_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(ROOT_SCAN_TIMEOUT.as_secs());
    Duration::from_secs(secs)
}

/// Probe the volumes backing a list of index entries and return a set of
/// inaccessible volume keys.
///
/// Why (issue #723): called once at the start of each warm-boot phase
/// (legacy and colocated). A single probe per distinct volume is cheaper and
/// safer than one probe per index — it limits leaked OS threads to
/// one-per-volume rather than one-per-index when a volume hangs.
///
/// What: extracts unique volume keys from `entries` via `probe::volume_key`,
/// runs `probe::probe_all_volumes` with `probe::volume_probe_timeout()`, and
/// returns the resulting inaccessible set. Each caller (`start.rs`) should
/// filter out entries whose root path maps to an inaccessible volume key
/// BEFORE calling `restore_one_index_bounded`.
///
/// Test: `probe.rs` unit tests cover volume_key and probe_all_volumes directly.
/// End-to-end covered by the acceptance criteria in issue #723.
pub fn probe_warmboot_volumes(entries: &[PersistedIndex]) -> HashSet<PathBuf> {
    if entries.is_empty() {
        return HashSet::new();
    }
    let paths: Vec<PathBuf> = entries.iter().map(|e| e.root_path.clone()).collect();
    let deadline = probe::volume_probe_timeout();
    probe::probe_all_volumes(&paths, deadline)
}

/// Probe the volumes backing a list of root paths and return a set of
/// inaccessible volume keys.
///
/// Why (issues #736 / #737): the colocated warm-boot phase previously
/// constructed dummy `PersistedIndex { ..Default::default() }` values solely
/// to pass to `probe_warmboot_volumes`. That construction was wasteful and
/// fragile (a root without a filesystem path defaulted `root_path` to empty,
/// making the probe target `/`). This path-based variant lets callers pass
/// the actual root `PathBuf` slices directly without any dummy struct
/// overhead. Both warm-boot phases (legacy and colocated) share the same
/// underlying `probe::probe_all_volumes` implementation.
///
/// What: delegates to `probe::probe_all_volumes` with the supplied `paths`
/// slice and the configured volume-probe timeout.
///
/// Test: both call sites in `start.rs` — existing probe_tests cover the
/// underlying `probe_all_volumes` behaviour.
pub fn probe_warmboot_volumes_from_paths(paths: &[PathBuf]) -> HashSet<PathBuf> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let deadline = probe::volume_probe_timeout();
    probe::probe_all_volumes(paths, deadline)
}

/// Returns `true` if `root_path` is on an inaccessible volume.
///
/// Why (issue #723): factored out of `start.rs::restore_indexes` so both the
/// legacy and colocated restore loops can cheaply test membership without
/// re-computing the volume key on every iteration.
/// What: computes `probe::volume_key(root_path)` and checks membership in
/// `inaccessible_volumes`.
/// Test: covered indirectly by the volume-probe filtering tests.
pub fn is_on_inaccessible_volume(
    root_path: &std::path::Path,
    inaccessible_volumes: &HashSet<PathBuf>,
) -> bool {
    let key = probe::volume_key(root_path);
    inaccessible_volumes.contains(&key)
}

/// Resolve the dedup key for `entry` — the identity of its on-disk redb corpus
/// file — WITHOUT any filesystem side effect (issue #2305).
///
/// Why: `corpus_redb_path_for_entry` routes through `colocated_storage_dir`,
/// which calls `create_dir_all` and would materialise a ghost `.trusty-search/`
/// directory for a moved/dead root at collection time (the exact hazard the
/// #484 relocation guard avoids). Deduplication must therefore key on a pure
/// path formula. Only colocated entries can collide: their corpus lives at
/// `<root_path>/.trusty-search/index.redb`, so two entries sharing a
/// `root_path` share one redb file. Legacy (non-colocated) corpora are keyed by
/// unique `index_id` in the global data dir and can never collide, so they are
/// never merged (`None`).
/// What: for a colocated entry returns `Some(canonicalize_best_effort(root_path))`
/// (canonicalization resolves symlink / `/var`↔`/private/var` aliases so two
/// aliased roots map to one key, matching the read-only canonicalization used
/// for the `seen_root_paths` set in `restore_indexes`); returns `None` for a
/// non-colocated entry. `canonicalize_best_effort` is read-only — it never
/// creates directories.
/// Test: `dedup_by_corpus_path_*` in `warm_boot_tests.rs`.
fn corpus_dedup_key(entry: &PersistedIndex) -> Option<PathBuf> {
    if !entry.colocated {
        // Legacy corpora are id-keyed in the global data dir → never collide.
        return None;
    }
    // Same root ⟺ same `<root>/.trusty-search/index.redb`, so keying on the
    // canonical root is equivalent to keying on the redb path and avoids the
    // create_dir_all side effect of resolving the redb path directly.
    Some(canonicalize_best_effort(&entry.root_path))
}

/// Outcome of [`dedup_entries_by_corpus_path_verbose`] (issue #2337).
///
/// Why: [`dedup_entries_by_corpus_path`] discards the dropped entries, which
/// was fine while the only consumer was the in-memory eager/cold split. Issue
/// #2337 needs the dropped entries so `restore_indexes` can prune their
/// `indexes.toml` rows — without this they are re-discovered, re-warned, and
/// re-dropped on every boot forever — and needs to know which survivors
/// absorbed a dropped entry's config so only those rows need rewriting.
/// What: `survivors` is the deduplicated set in original relative order
/// (identical to what [`dedup_entries_by_corpus_path`] returns); `dropped` is
/// every losing entry, pre-merge; `merged_survivor_ids` names the survivor
/// ids that actually absorbed at least one dropped entry's config (a plain
/// pass-through entry with no collision is never listed here).
/// Test: `dedup_verbose_*` in `warm_boot_tests.rs`.
pub struct DedupOutcome {
    pub survivors: Vec<PersistedIndex>,
    pub dropped: Vec<PersistedIndex>,
    pub merged_survivor_ids: HashSet<String>,
}

/// Collapse warm-boot entries that resolve to the SAME on-disk redb corpus file
/// down to a single entry, keeping the most-recently-active one (issue #2305).
///
/// Why: redb is a single-open database. When two colocated registry entries
/// share a `root_path` they resolve to the same
/// `<root_path>/.trusty-search/index.redb`. The sequential warm-boot restore
/// opens the file for the first entry and holds the corpus `Arc` for the
/// daemon's lifetime, so every later entry sharing that file fails its open
/// with `DatabaseAlreadyOpen` — the 50 ms retry in `open_corpus_with_retry`
/// can never clear an *in-process* holder, so the failure recurs on every
/// restart (the #1870 root cause). Two live handles serving byte-identical data
/// from one file is the anomaly, not a feature: trusty-search has no blue/green
/// layout that needs independent handles to one file, so we collapse the
/// duplicates to one healthy entry *before* any open is attempted, guaranteeing
/// zero `DatabaseAlreadyOpen` on warm boot.
/// What: thin wrapper over [`dedup_entries_by_corpus_path_verbose`] that keeps
/// only the survivor set — the shape every existing caller/test expects.
/// Test: `dedup_by_corpus_path_collapses_same_root`,
/// `dedup_by_corpus_path_keeps_distinct_roots`,
/// `dedup_by_corpus_path_keeps_non_colocated`,
/// `dedup_by_corpus_path_keeps_most_recent` in `warm_boot_tests.rs`.
pub fn dedup_entries_by_corpus_path(entries: Vec<PersistedIndex>) -> Vec<PersistedIndex> {
    dedup_entries_by_corpus_path_verbose(entries).survivors
}

/// Verbose variant of [`dedup_entries_by_corpus_path`] (issue #2337): also
/// returns the dropped entries and preserves each dropped entry's per-index
/// WHITELIST search configuration by merging it into the surviving entry.
///
/// Why: survivor selection previously discarded the loser's per-entry search
/// config even though the redb data is shared and those filters can
/// genuinely differ between the two registrations. Silently narrowing the
/// survivor's scope to whichever entry happened to win recency is a
/// regression for the loser's callers — but only for the WHITELIST fields
/// (`include_paths`, `extensions`, `domain_terms`, `path_filter`), where
/// union-merging can only broaden coverage. `exclude_globs` is a BLOCKLIST:
/// union-merging two blocklists narrows the survivor instead (issue #2519
/// review), which is the exact hazard this function exists to avoid — so it
/// is deliberately excluded from the merge, same treatment as the scalar
/// flags.
/// What: groups entries by [`corpus_dedup_key`] (colocated only; non-colocated
/// entries are always kept and can never collide). Within a colliding group,
/// picks the winner using the same recency-then-id tie-break as before
/// (highest `warmboot_sort_key`; ties break by lower `id`), then for every
/// losing entry: unions its WHITELIST list-type config fields into the winner
/// (`merge_dropped_config_into_survivor`) and logs its full config — including
/// `exclude_globs` and the scalar flags `include_docs`/`respect_gitignore`/
/// `lexical_only`/`skip_kg`, none of which are auto-merged (see that
/// function's doc) — at `warn` for operator reconciliation. Survivors keep
/// their original input order.
/// Test: `dedup_verbose_reports_dropped_entries`,
/// `dedup_verbose_merges_list_config_into_survivor`,
/// `dedup_verbose_does_not_merge_scalar_flags`,
/// `dedup_verbose_does_not_merge_exclude_globs`,
/// `dedup_verbose_no_collision_yields_empty_dropped_and_merged_sets` in
/// `warm_boot_tests.rs`.
pub fn dedup_entries_by_corpus_path_verbose(entries: Vec<PersistedIndex>) -> DedupOutcome {
    use crate::service::persistence::warmboot_sort_key;
    use std::collections::HashMap;

    // First pass: group entry indices by corpus-path key. Entries with no key
    // (non-colocated) are always retained and can never be a collision member.
    let mut groups: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(key) = corpus_dedup_key(entry) {
            groups.entry(key).or_default().push(i);
        }
    }

    let mut opt_entries: Vec<Option<PersistedIndex>> = entries.into_iter().map(Some).collect();
    let mut dropped: Vec<PersistedIndex> = Vec::new();
    let mut merged_survivor_ids: HashSet<String> = HashSet::new();

    for (key, idxs) in &groups {
        if idxs.len() < 2 {
            continue; // no collision in this group — nothing to drop or merge.
        }

        // Resolve the winner via the same recency-then-id tie-break rule the
        // original single-pass implementation used.
        let mut winner_idx = idxs[0];
        for &i in &idxs[1..] {
            let cur = opt_entries[i].as_ref().expect("group index not yet taken");
            let old = opt_entries[winner_idx]
                .as_ref()
                .expect("group index not yet taken");
            let cur_key = warmboot_sort_key(cur);
            let old_key = warmboot_sort_key(old);
            let cur_wins = cur_key > old_key || (cur_key == old_key && cur.id < old.id);
            if cur_wins {
                winner_idx = i;
            }
        }
        let survivor_id = opt_entries[winner_idx]
            .as_ref()
            .expect("winner not yet taken")
            .id
            .clone();

        for &i in idxs {
            if i == winner_idx {
                continue;
            }
            let loser = opt_entries[i].take().expect("loser not yet taken");
            tracing::warn!(
                "warm-boot: indexes '{}' and '{}' both resolve to the same redb corpus under \
                 {} — collapsing to '{}' to avoid an in-process double-open \
                 (DatabaseAlreadyOpen, issue #2305). List-type search config from '{}' is \
                 merged into the survivor (issue #2337); scalar flags — include_docs={} \
                 respect_gitignore={} lexical_only={} skip_kg={} — are logged here for \
                 operator reconciliation and are NOT auto-merged. Dropped list fields: \
                 include_paths={:?} exclude_globs={:?} extensions={:?} domain_terms={:?} \
                 path_filter={:?}. Re-register at a distinct root_path if the two were \
                 meant to be separate indexes.",
                loser.id,
                survivor_id,
                key.display(),
                survivor_id,
                loser.id,
                loser.include_docs,
                loser.respect_gitignore,
                loser.lexical_only,
                loser.skip_kg,
                loser.include_paths,
                loser.exclude_globs,
                loser.extensions,
                loser.domain_terms,
                loser.path_filter,
            );
            let survivor = opt_entries[winner_idx]
                .as_mut()
                .expect("winner not yet taken");
            merge_dropped_config_into_survivor(survivor, &loser);
            merged_survivor_ids.insert(survivor_id.clone());
            dropped.push(loser);
        }
    }

    // Survivors keep their original relative order — iterating opt_entries in
    // index order and skipping the taken (dropped) slots achieves this for
    // free, no separate reordering pass needed.
    let survivors: Vec<PersistedIndex> = opt_entries.into_iter().flatten().collect();

    DedupOutcome {
        survivors,
        dropped,
        merged_survivor_ids,
    }
}

/// Union `dropped`'s WHITELIST-composed list-type search-config fields into
/// `survivor` in place (issue #2337; narrowed by the #2519 review).
///
/// Why: two entries that collapse to one redb corpus can carry genuinely
/// different per-entry filters (e.g. one registration set
/// `extensions: ["rs"]`, the other `extensions: ["py"]`) even though the
/// underlying indexed data is one file. Dropping the loser's filters entirely
/// would silently narrow the survivor's search scope for `include_paths`,
/// `extensions`, `domain_terms`, and `path_filter` — each of these is a
/// whitelist (an ADDITIVE broadening: union-merging can only ever grow what
/// the survivor covers), so unioning them is safe.
///
/// `exclude_globs` is deliberately EXCLUDED from this union (issue #2519
/// review, MEDIUM): unlike the whitelist fields above, `exclude_globs` is a
/// blocklist — union-merging two blocklists NARROWS the surviving index (it
/// can only ever exclude MORE content), which is exactly the silent-scope-
/// change hazard this function exists to avoid. Auto-merging it would mean
/// the next reindex quietly drops more files than either original
/// registration intended. Like the scalar flags below, the dropped entry's
/// `exclude_globs` is surfaced via the caller's `warn` log (see
/// `dedup_entries_by_corpus_path_verbose`) for operator reconciliation
/// instead of being auto-merged.
///
/// Scalar flags (`include_docs`, `respect_gitignore`, `lexical_only`,
/// `skip_kg`) are likewise left untouched here — there is no safe "union"
/// semantics for a boolean pipeline toggle (e.g. OR-ing `lexical_only` could
/// silently disable the semantic lane for an index that was built with it) —
/// so those are also only surfaced via the `warn` log.
/// What: appends every value from each of `dropped`'s WHITELIST list fields
/// (`include_paths`, `extensions`, `domain_terms`, `path_filter`) that is not
/// already present in `survivor`'s corresponding field, preserving order
/// (survivor's existing values first). `exclude_globs` is untouched —
/// `survivor.exclude_globs` keeps exactly its own pre-merge value.
/// Test: `dedup_verbose_merges_list_config_into_survivor`,
/// `dedup_verbose_does_not_merge_scalar_flags`,
/// `dedup_verbose_does_not_merge_exclude_globs`,
/// `merge_dropped_config_deduplicates_overlapping_values` in
/// `warm_boot_tests.rs`.
fn merge_dropped_config_into_survivor(survivor: &mut PersistedIndex, dropped: &PersistedIndex) {
    merge_unique_strings(&mut survivor.include_paths, &dropped.include_paths);
    merge_unique_strings(&mut survivor.extensions, &dropped.extensions);
    merge_unique_strings(&mut survivor.domain_terms, &dropped.domain_terms);
    merge_unique_strings(&mut survivor.path_filter, &dropped.path_filter);
}

/// Append every string in `from` that is not already present in `into`,
/// preserving `into`'s existing order and `from`'s relative order for the
/// appended tail.
fn merge_unique_strings(into: &mut Vec<String>, from: &[String]) {
    for item in from {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

/// Collect index entries from the durable `indexes.toml` registry only.
///
/// Why (issue #718 Part 2): legacy entries live in `~/Library/Application
/// Support/trusty-search/` which launchd can always read. Separating this from
/// the colocated-roots scan means the N accessible indexes register
/// immediately, without waiting for any potentially-hung external-volume walk.
/// What: reads `indexes.toml` via `load_index_registry`; logs the resolved data
/// dir path so operators can confirm the correct dir is used. Returns an empty
/// vec when the file is absent (first-run case) and logs `error` on read failure.
/// Test: unit tests in this module; the returned entries feed directly into
/// `restore_one_index_bounded` in `start.rs`.
pub fn collect_legacy_entries() -> Vec<PersistedIndex> {
    use crate::service::persistence::{data_dir, indexes_toml_path, load_index_registry};

    // Issue #718: log the resolved data dir — primary diagnostic for 0-index boots.
    match data_dir() {
        Ok(ref d) => tracing::info!("warm-boot: data directory: {}", d.display()),
        Err(ref e) => tracing::error!(
            "warm-boot: FATAL — cannot resolve data directory; \
             set TRUSTY_DATA_DIR in the launchd plist (issue #718). Error: {e}"
        ),
    }

    let path_hint = indexes_toml_path()
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<path unresolvable>".to_string());

    match load_index_registry() {
        Ok(entries) if entries.is_empty() => {
            tracing::debug!("warm-boot: indexes.toml at {path_hint} — empty (first run)");
            Vec::new()
        }
        Ok(entries) => {
            tracing::info!(
                "warm-boot: loaded {} legacy index(es) from {path_hint}",
                entries.len()
            );
            entries
        }
        Err(e) => {
            tracing::error!(
                "warm-boot: FAILED reading indexes.toml at {path_hint}: {e}. \
                 Indexes MISSING on this boot. \
                 Set TRUSTY_DATA_DIR in the launchd/systemd unit (issue #718)."
            );
            Vec::new()
        }
    }
}

/// Collect colocated index entries by scanning every tracked root in `roots.toml`.
///
/// Why (issue #718 Part 2 / #723 / #860): the previous implementation called the
/// blocking recursive scan directly on the async reactor thread with no timeout.
/// Under launchd on macOS 26 Tahoe, a root on `/Volumes/SSD1` (external volume)
/// can block `canonicalize` or `read_dir` indefinitely due to TCC permission
/// denial. This blocked the entire restore task, preventing even the legacy
/// indexes from registering. Issue #860: legacy entries from `indexes.toml` carry
/// basename-derived IDs (e.g. `trusty-tools`) while `scan_one_root` produces
/// full-path-sanitized IDs (e.g. `Users_mac_workspace_trusty-tools`). The
/// pre-existing ID-only dedup never fired for the same root, producing a duplicate
/// "ghost" colocated entry on every restart. Adding `known_root_paths` closes
/// this gap with equality-only path dedup (strict equality — NOT sub-path; a root
/// that is a parent of a known root is still a distinct index scope).
///
/// What: loads `roots.toml`, then — after filtering out roots on volumes already
/// marked inaccessible by `inaccessible_volumes` (issue #723) — for each
/// remaining root:
/// - Spawns a `spawn_blocking` task running `scan_one_root` (the sync fs walk).
/// - Wraps it in `warmboot_index_timeout()`.
/// - On timeout: logs `warn` with the root path and the actionable hint about
///   Full Disk Access for the launchd agent; skips the root.
/// - On scan error: logs `warn` and skips (does not abort other roots).
/// - Deduplicates by index id against `known_ids` (ID-level dedup, legacy entries).
/// - ALSO deduplicates by canonicalized `root_path` against `known_root_paths`
///   (root-path-level dedup, closes the basename-vs-full-path ID mismatch; #860).
///   Only equality is checked — a colocated root whose path is a strict parent
///   or child of a known root is NOT suppressed (correct for `IndexHierarchy`).
///   Canonicalization uses `crate::commands::start::canonicalize_best_effort` —
///   the SAME helper that `restore_indexes` (Phase 1) uses to build
///   `seen_root_paths`. Both sides must call the same function so their fallback
///   behaviour (raw path on error) is identical and the `contains` check stays
///   consistent (#864 medium finding).
///
/// Test: `colocated_scan_partial_failure_still_returns_accessible`,
///       `colocated_scan_deduplicates_against_known_ids`,
///       `colocated_scan_deduplicates_by_root_path_against_basename_legacy_id` (#860),
///       `colocated_scan_dedup_uses_consistent_canonicalization` (#864).
pub async fn collect_colocated_entries(
    known_ids: &HashSet<String>,
    known_root_paths: &HashSet<PathBuf>,
    inaccessible_volumes: &HashSet<PathBuf>,
) -> Vec<PersistedIndex> {
    use crate::service::roots_registry::load_roots;

    let tracked_roots: Vec<PathBuf> = match load_roots() {
        Ok(r) => r.into_iter().map(|r| r.path).collect(),
        Err(e) => {
            tracing::error!(
                "warm-boot: FAILED reading roots.toml: {e}. \
                 Colocated indexes not discovered on this boot (issue #718)."
            );
            return Vec::new();
        }
    };

    if tracked_roots.is_empty() {
        return Vec::new();
    }

    // Issue #723: skip roots on volumes that already failed the pre-flight probe.
    // This prevents issuing any open() calls on a hung volume for the scan phase.
    let (accessible_roots, pre_skipped): (Vec<PathBuf>, Vec<PathBuf>) = tracked_roots
        .into_iter()
        .partition(|r| !is_on_inaccessible_volume(r, inaccessible_volumes));

    if !pre_skipped.is_empty() {
        tracing::warn!(
            "warm-boot: skipping {} colocated root(s) on inaccessible volumes (issue #723): {}",
            pre_skipped.len(),
            pre_skipped
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if accessible_roots.is_empty() {
        return Vec::new();
    }

    tracing::info!(
        "warm-boot: scanning {} tracked root(s) for colocated indexes",
        accessible_roots.len()
    );

    let timeout = warmboot_index_timeout();
    let mut results: Vec<PersistedIndex> = Vec::new();
    let mut seen_ids = known_ids.clone();

    for root in accessible_roots {
        let root_for_log = root.clone();
        let root_for_task = root.clone();

        // Run the blocking fs walk off the async reactor.
        let scan_future = tokio::task::spawn_blocking(move || scan::scan_one_root(&root_for_task));

        match tokio::time::timeout(timeout, scan_future).await {
            Ok(Ok(entries)) => {
                for colocated in entries {
                    // ID-level dedup: catches re-discovery of same-ID legacy entries.
                    if seen_ids.contains(&colocated.id) {
                        tracing::debug!(
                            "dual-discovery: colocated index '{}' at {} skipped \
                             (id already in registry)",
                            colocated.id,
                            colocated.root_path.display()
                        );
                        continue;
                    }
                    // Root-path-level dedup (issue #860): catches the basename-vs-full-path
                    // ID mismatch where the same root_path is registered under a different
                    // ID scheme. Use `canonicalize_best_effort` (same helper as Phase 1 in
                    // `restore_indexes`) so both sides normalize identically — symlinks,
                    // /private/var↔/var aliases, and relocation renames all resolve the
                    // same way on both sides of the `contains` check.
                    let canonical_colocated = canonicalize_best_effort(&colocated.root_path);
                    if known_root_paths.contains(&canonical_colocated) {
                        tracing::debug!(
                            "dual-discovery: colocated index '{}' at {} skipped \
                             (root_path already owned by a legacy entry, issue #860)",
                            colocated.id,
                            colocated.root_path.display()
                        );
                        continue;
                    }
                    seen_ids.insert(colocated.id.clone());
                    results.push(PersistedIndex {
                        id: colocated.id,
                        root_path: colocated.root_path,
                        colocated: true,
                        ..Default::default()
                    });
                }
            }
            Ok(Err(join_err)) => {
                // spawn_blocking task panicked — should be very rare.
                tracing::warn!(
                    "warm-boot: colocated scan task panicked for root {}: {join_err}",
                    root_for_log.display()
                );
            }
            Err(_elapsed) => {
                // Timeout: likely a TCC-denied or network-backed external volume.
                let is_external = scan::is_likely_external_volume(&root_for_log);
                if is_external {
                    tracing::warn!(
                        "warm-boot: colocated scan TIMED OUT for external-volume root {} \
                         (>{:.0}s, likely TCC/permission denial under launchd). \
                         HINT: grant Full Disk Access to the launchd agent in \
                         System Settings → Privacy & Security → Full Disk Access, \
                         or move the index off the external volume. \
                         Skipping this root — other roots still restored. (issue #718)",
                        root_for_log.display(),
                        timeout.as_secs_f32(),
                    );
                } else {
                    tracing::warn!(
                        "warm-boot: colocated scan TIMED OUT for root {} \
                         (>{:.0}s). The root may be on a network or slow filesystem. \
                         Skipping this root — other roots still restored. (issue #718)",
                        root_for_log.display(),
                        timeout.as_secs_f32(),
                    );
                }
            }
        }
    }

    results
}

#[cfg(test)]
#[path = "warm_boot_tests.rs"]
mod tests;
