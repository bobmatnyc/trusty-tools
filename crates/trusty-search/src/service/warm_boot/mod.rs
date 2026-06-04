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
//!
//! Test: `warmboot_index_timeout_parses_env_var`,
//!       `colocated_scan_partial_failure_still_returns_accessible`,
//!       `colocated_scan_deduplicates_against_known_ids`.

pub(super) mod probe;
pub mod restore;
mod scan;

pub use probe::leaked_probe_thread_count;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;
pub use restore::restore_one_index_bounded;

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
/// Why (issue #718 Part 2 / issue #723): the previous implementation called the
/// blocking recursive scan directly on the async reactor thread with no timeout.
/// Under launchd on macOS 26 Tahoe, a root on `/Volumes/SSD1` (external volume)
/// can block `canonicalize` or `read_dir` indefinitely due to TCC permission
/// denial. This blocked the entire restore task, preventing even the legacy
/// indexes from registering.
///
/// What: loads `roots.toml`, then — after filtering out roots on volumes already
/// marked inaccessible by `inaccessible_volumes` (issue #723) — for each
/// remaining root:
/// - Spawns a `spawn_blocking` task running `scan_one_root` (the sync fs walk).
/// - Wraps it in `warmboot_index_timeout()`.
/// - On timeout: logs `warn` with the root path and the actionable hint about
///   Full Disk Access for the launchd agent; skips the root.
/// - On scan error: logs `warn` and skips (does not abort other roots).
/// - Deduplicates by index id against `known_ids` (legacy entries already seen).
///
/// Test: `colocated_scan_partial_failure_still_returns_accessible`,
///       `colocated_scan_deduplicates_against_known_ids`.
pub async fn collect_colocated_entries(
    known_ids: &HashSet<String>,
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
mod tests {
    //! Tests for the resilient warm-boot index collection (issues #718 / #723).
    //!
    //! Why: the key invariant is that an inaccessible or hung colocated root
    //! must never prevent the accessible legacy/colocated entries from
    //! registering. We simulate inaccessibility with a nonexistent path (which
    //! returns NotFound immediately — a fast proxy for the TCC hang which
    //! cannot be reproduced in unit tests).
    //! Test: `cargo test -p trusty-search -- warm_boot`.

    use super::*;

    // ── warmboot_index_timeout ────────────────────────────────────────────────

    /// Why: guard that the env var reader parses valid values and falls back.
    /// What: set `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS=42`, assert Duration is
    /// 42s; unset, assert Duration is ROOT_SCAN_TIMEOUT.
    /// Note: `serial` prevents racing with other env-var mutators.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn warmboot_index_timeout_parses_env_var() {
        // Parse a valid value.
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "42") };
        assert_eq!(
            warmboot_index_timeout(),
            Duration::from_secs(42),
            "must parse 42 from env var"
        );
        // Remove and confirm fallback.
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS") };
        assert_eq!(
            warmboot_index_timeout(),
            ROOT_SCAN_TIMEOUT,
            "must fall back to ROOT_SCAN_TIMEOUT when env var is absent"
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
        // No volumes are inaccessible in this test.
        let inaccessible: HashSet<PathBuf> = HashSet::new();
        let results = collect_colocated_entries(&known_ids, &inaccessible).await;

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
        let inaccessible: HashSet<PathBuf> = HashSet::new();

        let results = collect_colocated_entries(&known_ids, &inaccessible).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        assert!(
            results.is_empty(),
            "index already in known_ids must not be returned again; got: {results:?}"
        );
    }

    /// Why (issue #723): roots on inaccessible volumes must be skipped before
    /// any spawn_blocking scan is attempted — the volume probe prevents issuing
    /// any open() calls on a hung volume.
    /// What: register one real root and one root with a mocked inaccessible
    /// volume key. Pass the mocked key in `inaccessible_volumes`; assert only
    /// the real root's index is returned.
    /// Note: `serial` prevents parallel env-var mutation from other tests.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn colocated_scan_skips_inaccessible_volume_roots() {
        use crate::service::fs_discovery::id_from_path;

        let data_tmp = tempfile::tempdir().unwrap();
        let real_root = tempfile::tempdir().unwrap();
        let ts_dir = real_root.path().join(".trusty-search");
        std::fs::create_dir_all(&ts_dir).unwrap();
        let canonical_root = real_root.path().canonicalize().unwrap();
        let real_id = id_from_path(&canonical_root);

        // Register a fake root that looks like it's on /Volumes/BLOCKED.
        // We won't actually create it — the test asserts it is skipped via the
        // inaccessible_volumes filter, not via a scan timeout.
        let fake_blocked = PathBuf::from("/Volumes/BLOCKED/some-project");

        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", data_tmp.path());
        }
        crate::service::roots_registry::upsert_root(real_root.path().to_path_buf()).unwrap();
        crate::service::roots_registry::upsert_root(fake_blocked.clone()).unwrap();

        let known_ids: HashSet<String> = HashSet::new();
        // Simulate: /Volumes/BLOCKED was probed and timed out.
        let mut inaccessible: HashSet<PathBuf> = HashSet::new();
        inaccessible.insert(PathBuf::from("/Volumes/BLOCKED"));

        let results = collect_colocated_entries(&known_ids, &inaccessible).await;

        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }

        // Only the real (non-blocked) root must be found.
        assert_eq!(
            results.len(),
            1,
            "only the accessible root must be returned; got: {results:?}"
        );
        assert_eq!(
            results[0].id, real_id,
            "the returned entry must be the real root, not the blocked one"
        );
    }
}
