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
mod scan;

pub use probe::leaked_probe_thread_count;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;
pub use restore::restore_one_index_bounded;

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
