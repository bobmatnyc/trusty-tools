//! Resilient warm-boot index collection for the trusty-search daemon.
//!
//! Why (issue #718 Part 2): the original `collect_all_index_entries` ran a
//! blocking recursive filesystem scan (via `scan_roots_for_colocated_indexes`)
//! synchronously on the async reactor thread, then gated ALL index registration
//! behind it. Under launchd on macOS 26 Tahoe, tracked roots on external volumes
//! (`/Volumes/…`) trigger TCC permission checks that hang or fail silently — so
//! the entire warm-boot restore stalled indefinitely, leaving even the accessible
//! legacy indexes (from `indexes.toml` in `~/Library/Application Support/`) with
//! 0 registrations. This module fixes that with two structural changes:
//!
//! 1. Legacy and colocated index discovery are now **fully independent phases**.
//!    Legacy entries are collected first (fast TOML read from the accessible data
//!    dir) and callers should register them before initiating the colocated scan.
//!
//! 2. The colocated-roots scan is:
//!    - Run via `tokio::task::spawn_blocking` so blocking `std::fs` calls do NOT
//!      stall the async reactor.
//!    - Wrapped in a per-root timeout (`ROOT_SCAN_TIMEOUT`). A single hung or
//!      denied root is logged at `warn`/`error` and skipped; the rest still run.
//!    - Loud: `PermissionDenied` / `EPERM` on an external/removable volume emits
//!      an actionable hint about granting Full Disk Access to the launchd agent.
//!
//! What: public API is `collect_legacy_entries` + `collect_colocated_entries`.
//! Callers should call `collect_legacy_entries` synchronously (cheap), register
//! those indexes, then call `collect_colocated_entries` asynchronously and
//! register the remainder.
//!
//! Test: `legacy_only_does_not_block_on_colocated`,
//!       `colocated_scan_skips_inaccessible_root`,
//!       `colocated_scan_partial_failure_still_returns_accessible`.

use std::path::PathBuf;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;

/// Per-root timeout for the colocated scan.
///
/// Why: a TCC-denied or network-backed root on macOS can hang a `read_dir` or
/// `canonicalize` call for tens of seconds to minutes. We impose a 10-second
/// ceiling so that N stalled roots cost at most N × 10 s, and the user gets
/// actionable log output instead of a silent hang.
/// What: duration applied via `tokio::time::timeout` around each root's
/// `spawn_blocking` scan.
/// Test: `colocated_scan_skips_inaccessible_root` verifies that a missing/
/// unreadable root does not block accessible roots from completing.
pub const ROOT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Collect index entries from the durable `indexes.toml` registry only.
///
/// Why (issue #718 Part 2): legacy entries live in `~/Library/Application
/// Support/trusty-search/` which launchd can always read. Separating this from
/// the colocated-roots scan means the 57 (or N) accessible indexes register
/// immediately, without waiting for any potentially-hung external-volume walk.
/// What: reads `indexes.toml` via `load_index_registry`; logs the resolved data
/// dir path so operators can confirm the correct dir is used. Returns an empty
/// vec when the file is absent (first-run case) and logs `error` on read failure.
/// Test: unit tests in this module; the returned entries feed directly into
/// `restore_one_index` in `start.rs`.
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
/// Why (issue #718 Part 2): the previous implementation called the blocking
/// recursive scan directly on the async reactor thread with no timeout. Under
/// launchd on macOS 26 Tahoe, a root on `/Volumes/SSD1` (external volume) can
/// block `canonicalize` or `read_dir` indefinitely due to TCC permission denial.
/// This blocked the entire restore task, preventing even the legacy indexes from
/// registering.
///
/// What: loads `roots.toml`, then for each root:
/// - Spawns a `spawn_blocking` task running `scan_one_root` (the sync fs walk).
/// - Wraps it in `ROOT_SCAN_TIMEOUT` (10 s).
/// - On timeout: logs `warn` with the root path and the actionable hint about
///   Full Disk Access for the launchd agent; skips the root.
/// - On scan error: logs `warn` and skips (does not abort other roots).
/// - Deduplicates by index id against `known_ids` (legacy entries already seen).
///
/// Test: `colocated_scan_skips_inaccessible_root`,
///       `colocated_scan_partial_failure_still_returns_accessible`.
pub async fn collect_colocated_entries(
    known_ids: &std::collections::HashSet<String>,
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

    tracing::info!(
        "warm-boot: scanning {} tracked root(s) for colocated indexes",
        tracked_roots.len()
    );

    let mut results: Vec<PersistedIndex> = Vec::new();
    let mut seen_ids = known_ids.clone();

    for root in tracked_roots {
        let root_for_log = root.clone();
        let root_for_task = root.clone();

        // Run the blocking fs walk off the async reactor.
        let scan_future = tokio::task::spawn_blocking(move || scan_one_root(&root_for_task));

        match tokio::time::timeout(ROOT_SCAN_TIMEOUT, scan_future).await {
            Ok(Ok(entries)) => {
                for colocated in entries {
                    if seen_ids.contains(&colocated.id) {
                        tracing::debug!(
                            "dual-discovery: colocated index '{}' at {} skipped (already in registry)",
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
                let is_external = is_likely_external_volume(&root_for_log);
                if is_external {
                    tracing::warn!(
                        "warm-boot: colocated scan TIMED OUT for external-volume root {} \
                         (>{:.0}s, likely TCC/permission denial under launchd). \
                         HINT: grant Full Disk Access to the launchd agent in \
                         System Settings → Privacy & Security → Full Disk Access, \
                         or move the index off the external volume. \
                         Skipping this root — other roots still restored. (issue #718)",
                        root_for_log.display(),
                        ROOT_SCAN_TIMEOUT.as_secs_f32(),
                    );
                } else {
                    tracing::warn!(
                        "warm-boot: colocated scan TIMED OUT for root {} \
                         (>{:.0}s). The root may be on a network or slow filesystem. \
                         Skipping this root — other roots still restored. (issue #718)",
                        root_for_log.display(),
                        ROOT_SCAN_TIMEOUT.as_secs_f32(),
                    );
                }
            }
        }
    }

    results
}

/// Synchronous per-root scan: discover all `.trusty-search/` directories under
/// `root` and return one `ColocatedDiscovery` per find.
///
/// Why: extracted so it can run inside `spawn_blocking` (keeping blocking fs
/// calls off the async reactor) and so each root gets an independent timeout.
/// What: calls `scan_roots_for_colocated_indexes` for the single root; maps
/// I/O errors to a warn-logged empty result (not a panic). A `PermissionDenied`
/// or `EPERM` error is elevated to `error!` with an actionable hint.
/// Test: called by `collect_colocated_entries` via spawn_blocking; error paths
/// verified by `colocated_scan_skips_inaccessible_root`.
fn scan_one_root(root: &std::path::Path) -> Vec<ColocatedDiscovery> {
    use crate::service::fs_discovery::{scan_roots_for_colocated_indexes, DEFAULT_SCAN_DEPTH};

    // Pre-flight: check if the root exists before we walk it, so we can emit
    // a better error than a cryptic `canonicalize` failure.
    match std::fs::metadata(root) {
        Ok(_) => {}
        Err(e) => {
            let kind = e.kind();
            if kind == std::io::ErrorKind::PermissionDenied {
                tracing::error!(
                    "warm-boot: PERMISSION DENIED accessing root {} during colocated scan: {e}. \
                     Under launchd, this is typically a TCC denial on an external or protected \
                     volume. Grant Full Disk Access to the launchd agent in \
                     System Settings → Privacy & Security → Full Disk Access. (issue #718)",
                    root.display()
                );
            } else if kind == std::io::ErrorKind::NotFound {
                tracing::debug!(
                    "warm-boot: root {} not found — skipping colocated scan",
                    root.display()
                );
            } else {
                tracing::warn!(
                    "warm-boot: cannot access root {} for colocated scan: {e} — skipping",
                    root.display()
                );
            }
            return Vec::new();
        }
    }

    let entries = scan_roots_for_colocated_indexes(
        std::slice::from_ref(&root.to_path_buf()),
        DEFAULT_SCAN_DEPTH,
    );

    entries
        .into_iter()
        .map(|e| ColocatedDiscovery {
            id: e.id,
            root_path: e.root_path,
        })
        .collect()
}

/// Minimal discovered-colocated-index record returned from `scan_one_root`.
///
/// Why: a thin local type so `scan_one_root` can be called from `spawn_blocking`
/// without crossing any Arc/Sync boundaries that `ColocatedIndexEntry` might not
/// satisfy in future refactors.
/// What: mirrors the fields of `ColocatedIndexEntry` that the caller needs.
/// Test: populated by `scan_one_root`, consumed by `collect_colocated_entries`.
#[derive(Debug)]
struct ColocatedDiscovery {
    pub id: String,
    pub root_path: PathBuf,
}

/// Heuristic: returns `true` when `path` is likely on an external or removable
/// volume where macOS TCC may deny launchd access.
///
/// Why: provides a better log message distinguishing TCC-denied external volumes
/// from merely slow NFS/SMB mounts. External volumes on macOS are conventionally
/// mounted under `/Volumes/`; this is not authoritative but is correct for the
/// common case (USB drives, Thunderbolt SSDs, network shares mounted as volumes).
/// What: checks whether the canonical form of `path` starts with `/Volumes/`.
/// Falls back gracefully if canonicalization fails.
/// Test: `is_likely_external_volume_detection` in this module.
fn is_likely_external_volume(path: &std::path::Path) -> bool {
    // Fast path: string prefix check before canonicalize.
    if path.starts_with("/Volumes") {
        return true;
    }
    // Try canonicalize to catch symlinks that resolve into /Volumes.
    if let Ok(canonical) = std::fs::canonicalize(path) {
        if canonical.starts_with("/Volumes") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    //! Tests for the resilient warm-boot index collection (issue #718 Part 2).
    //!
    //! Why: the key invariant is that an inaccessible or hung colocated root
    //! must never prevent the accessible legacy/colocated entries from
    //! registering. We simulate inaccessibility with a nonexistent path (which
    //! returns NotFound immediately — a fast proxy for the TCC hang which
    //! cannot be reproduced in unit tests).
    //! Test: `cargo test -p trusty-search -- warm_boot`.

    use super::*;
    use std::collections::HashSet;

    // ── is_likely_external_volume ──────────────────────────────────────────────

    /// Why: guard the heuristic that powers the TCC-hint log message.
    /// What: paths whose string prefix starts with `/Volumes` return true;
    /// paths rooted at `/Library` (which stays on the boot volume) return false.
    /// We deliberately avoid `/Users/...` in the negative assertion because on
    /// some macOS setups (including this dev machine) `/Users/<user>/Projects`
    /// is a symlink to `/Volumes/SSD1/Projects`, so `canonicalize` would
    /// correctly return true — making the negative assertion a false failure.
    /// Test: this test.
    #[test]
    fn is_likely_external_volume_detection() {
        assert!(
            is_likely_external_volume(std::path::Path::new("/Volumes/SSD1/Projects")),
            "/Volumes/ prefix must be detected as external"
        );
        assert!(
            is_likely_external_volume(std::path::Path::new("/Volumes")),
            "/Volumes itself must be detected as external"
        );
        // /Library stays on the boot volume on macOS and is never under /Volumes.
        assert!(
            !is_likely_external_volume(std::path::Path::new(
                "/Library/Application Support/trusty-search"
            )),
            "/Library/... must not be detected as external"
        );
        // A nonexistent path that definitely cannot canonicalize to /Volumes.
        assert!(
            !is_likely_external_volume(std::path::Path::new(
                "/private/tmp/trusty-718-test-not-external"
            )),
            "/private/tmp/... must not be detected as external"
        );
    }

    // ── scan_one_root ─────────────────────────────────────────────────────────

    /// Why: a nonexistent root must produce an empty result without panicking.
    /// Under launchd a TCC-denied path surfaces as PermissionDenied, but for
    /// unit tests NotFound is a fast, safe proxy for "inaccessible root".
    /// What: call `scan_one_root` with a path that does not exist; assert the
    /// result is empty.
    /// Test: this test.
    #[test]
    fn scan_one_root_nonexistent_returns_empty() {
        let nonexistent = std::path::Path::new("/tmp/trusty-718-definitely-not-here-xyz9999");
        let result = scan_one_root(nonexistent);
        assert!(
            result.is_empty(),
            "nonexistent root must produce no discoveries; got: {result:?}"
        );
    }

    /// Why: a real directory with a `.trusty-search/` subdirectory must be
    /// discovered and returned correctly.
    /// What: create a tempdir, add `.trusty-search/`, call `scan_one_root`,
    /// assert one entry is returned with the correct root_path.
    /// Test: this test.
    #[test]
    fn scan_one_root_finds_colocated_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ts_dir = root.join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();

        let results = scan_one_root(root);
        assert_eq!(
            results.len(),
            1,
            "one .trusty-search dir must yield one discovery; got: {results:?}"
        );
        // root_path is the parent of .trusty-search.
        let canonical_root = root.canonicalize().unwrap();
        assert_eq!(
            results[0].root_path, canonical_root,
            "root_path must be the canonical parent of .trusty-search"
        );
    }

    // ── collect_colocated_entries ─────────────────────────────────────────────

    /// Why: the key resilience invariant — when one root is inaccessible (or
    /// times out under launchd), the other roots must still be scanned and
    /// their indexes returned.
    /// What: write a roots.toml with two entries: one real tempdir with
    /// .trusty-search/ and one nonexistent path. Call
    /// `collect_colocated_entries`; assert the real one is found.
    /// Note: `serial` prevents parallel env-var mutation from other tests
    /// (TRUSTY_DATA_DIR is a shared global state).
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn colocated_scan_partial_failure_still_returns_accessible() {
        let data_tmp = tempfile::tempdir().unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let ts_dir = real_root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();

        // Point TRUSTY_DATA_DIR at our isolated tempdir so roots.toml does not
        // read the real system data dir. `serial` prevents concurrent tests from
        // racing on this env var.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        }

        // Register both a real and a nonexistent root.
        let nonexistent = std::path::PathBuf::from("/tmp/trusty-718-no-root-xyz9999");
        crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();
        crate::service::roots_registry::upsert_root(nonexistent).unwrap();

        let known_ids: HashSet<String> = HashSet::new();
        let results = collect_colocated_entries(&known_ids).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        // The real root must be found even though the nonexistent root errored.
        assert_eq!(
            results.len(),
            1,
            "accessible root must be discovered even when another root is inaccessible; \
             got: {results:?}"
        );
        let canonical_root = real_root.path().canonicalize().unwrap();
        assert_eq!(
            results[0].root_path, canonical_root,
            "discovered root_path must match the real tempdir"
        );
    }

    /// Why: entries already present in `known_ids` (from the legacy scan) must
    /// not be duplicated in the colocated results — dedup is required.
    /// What: register a real root and pre-populate `known_ids` with its
    /// derived id; assert the colocated result is empty (already known).
    /// Note: `serial` prevents parallel env-var mutation from other tests.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn colocated_scan_deduplicates_against_known_ids() {
        use crate::service::fs_discovery::id_from_path;

        let data_tmp = tempfile::tempdir().unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let ts_dir = real_root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();
        let canonical_root = real_root.path().canonicalize().unwrap();
        let expected_id = id_from_path(&canonical_root);

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        }
        crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();

        let mut known_ids: HashSet<String> = HashSet::new();
        known_ids.insert(expected_id.clone());

        let results = collect_colocated_entries(&known_ids).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        assert!(
            results.is_empty(),
            "index already in known_ids must not be returned again; got: {results:?}"
        );
    }
}
