//! Per-root colocated-index filesystem scan helpers.
//!
//! Why (issue #718 Part 2): extracted from `warm_boot.rs` so that `mod.rs`
//! stays under the 500-line cap and `restore.rs` can live alongside without
//! bloating the main file. All blocking filesystem work for the colocated
//! discovery phase lives here.
//! What: `scan_one_root` runs a sync fs walk for one root (called from
//! `spawn_blocking`), `is_likely_external_volume` provides the TCC-hint
//! heuristic, and `ColocatedDiscovery` carries the discovered index coordinates.
//! Test: `scan_one_root_nonexistent_returns_empty`,
//!       `scan_one_root_finds_colocated_index`,
//!       `is_likely_external_volume_detection`.

use std::path::PathBuf;

/// Minimal discovered-colocated-index record returned from `scan_one_root`.
///
/// Why: a thin local type so `scan_one_root` can be called from `spawn_blocking`
/// without crossing any Arc/Sync boundaries that `ColocatedIndexEntry` might not
/// satisfy in future refactors.
/// What: mirrors the fields of `ColocatedIndexEntry` that the caller needs.
/// Test: populated by `scan_one_root`, consumed by `collect_colocated_entries`.
#[derive(Debug)]
pub(super) struct ColocatedDiscovery {
    pub id: String,
    pub root_path: PathBuf,
}

/// Synchronous per-root scan: discover all `.trusty-search/` directories under
/// `root` and return one `ColocatedDiscovery` per find.
///
/// Why: extracted so it can run inside `spawn_blocking` (keeping blocking fs
/// calls off the async reactor) and so each root gets an independent timeout.
/// What: calls `scan_roots_for_colocated_indexes` for the single root; maps
/// I/O errors to a warn-logged empty result (not a panic). A `PermissionDenied`
/// or `EPERM` error is elevated to `error!` with an actionable hint.
/// Test: `scan_one_root_nonexistent_returns_empty`,
///       `scan_one_root_finds_colocated_index`.
pub(super) fn scan_one_root(root: &std::path::Path) -> Vec<ColocatedDiscovery> {
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

/// Heuristic: returns `true` when `path` is likely on an external or removable
/// volume where macOS TCC may deny launchd access.
///
/// Why: provides a better log message distinguishing TCC-denied external volumes
/// from merely slow NFS/SMB mounts. External volumes on macOS are conventionally
/// mounted under `/Volumes/`; this is not authoritative but is correct for the
/// common case (USB drives, Thunderbolt SSDs, network shares mounted as volumes).
/// Used by both warm-boot (TCC-hint log messages) and shutdown flush
/// (issue #874: skip external-volume indexes to avoid stalling graceful shutdown).
/// What: checks whether the canonical form of `path` starts with `/Volumes/`.
/// Falls back gracefully if canonicalization fails.
/// Test: `is_likely_external_volume_detection` in this module.
pub(crate) fn is_likely_external_volume(path: &std::path::Path) -> bool {
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
    use super::*;

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
}
