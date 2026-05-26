//! L1 essential drawer cache snapshot persistence.
//!
//! Why: The L1 cache (top-15 drawers by importance) is computed from the full
//! drawer table on every palace open. For palaces with thousands of drawers,
//! recomputing this on cold start costs more than necessary; persisting it as
//! a JSON snapshot lets us hydrate the cache instantly and refresh it lazily.
//! What: `L1Cache` provides atomic save/load of `Vec<Drawer>` to
//! `<data_dir>/l1_cache.json` plus an `is_stale` mtime helper.
//! Test: `l1_cache_roundtrip` and `l1_cache_stale` in this module.

use crate::memory_core::palace::Drawer;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use thiserror::Error;

/// Filename for the L1 cache snapshot.
const L1_CACHE_JSON: &str = "l1_cache.json";

/// Monotonic counter that, combined with the process PID, gives every
/// concurrent `save_l1_cache` invocation its own tmp filename.
///
/// Why: Issue #154 — when two writers in the same process raced on
/// `l1_cache.json.tmp`, the first `rename(tmp -> target)` succeeded and
/// removed the tmp, then the second `rename` failed with `ENOENT` because
/// "its" tmp file no longer existed. The reported error was
/// `"No such file or directory at .../l1_cache.json"` even though it was
/// really the tmp source that vanished. Per-invocation tmp names eliminate
/// the trample entirely — each writer renames its own file.
/// What: `AtomicU64` bumped once per `save_l1_cache` call.
/// Test: covered by `concurrent_save_l1_cache_no_enoent` in this module.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Maximum number of drawers stored in the L1 snapshot (mirrors `retrieval::L1_CAP`).
pub const L1_SNAPSHOT_CAP: usize = 15;

/// Errors raised by L1 cache persistence operations.
#[derive(Debug, Error)]
pub enum L1CacheError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("system time error: {0}")]
    Time(String),
}

impl L1CacheError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
    fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.into(),
            source,
        }
    }
}

type Result<T> = std::result::Result<T, L1CacheError>;

/// Stateless namespace for L1 cache persistence helpers.
///
/// Why: Like `PalaceStore`, every operation is a pure function over a path.
/// What: `save_l1_cache` / `load_l1_cache` / `is_stale`.
/// Test: This module's tests cover roundtrip and staleness.
pub struct L1Cache;

impl L1Cache {
    /// Persist the top-by-importance drawers to `<data_dir>/l1_cache.json`.
    ///
    /// Why: Atomic JSON snapshot lets the next palace open hydrate L1 in O(1)
    /// disk reads instead of re-sorting the full drawer table. Concurrency
    /// hazards (issue #154): two concurrent writers must not stomp on each
    /// other's tmp file, and the parent directory must exist at rename time
    /// even if a prior op transiently removed it (e.g. the dream subprocess).
    /// What: Sorts a clone of `drawers` by importance descending, takes the
    /// first `L1_SNAPSHOT_CAP`, writes JSON to a per-call unique tmp path
    /// (PID + monotonic counter so concurrent writers don't share the tmp),
    /// re-asserts `create_dir_all` immediately before the rename so the
    /// destination directory cannot have vanished between this call's first
    /// `create_dir_all` and its rename, and renames atomically. On rename
    /// failure the stray tmp is best-effort removed so disk doesn't fill
    /// with `.tmp.<pid>.<seq>` orphans.
    /// Test: `l1_cache_roundtrip` saves 20 drawers and verifies only the top
    /// 15 (by importance) come back. `concurrent_save_l1_cache_no_enoent`
    /// stresses 16 parallel writers and asserts none hit ENOENT (covers the
    /// trample race fixed by per-call tmp naming).
    pub fn save_l1_cache(drawers: &[Drawer], data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir).map_err(|e| L1CacheError::io(data_dir, e))?;
        let target = data_dir.join(L1_CACHE_JSON);
        // Per-invocation tmp filename: PID + monotonic counter. Two
        // concurrent writers in the same process get distinct tmp paths, so
        // writer A's `rename(tmp_A -> target)` cannot remove writer B's
        // `tmp_B`. Different processes get different PIDs, so cross-process
        // concurrent writes (rare but possible) are also safe.
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let tmp = data_dir.join(format!("{L1_CACHE_JSON}.tmp.{pid}.{seq}"));

        let mut sorted: Vec<Drawer> = drawers.to_vec();
        sorted.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted.truncate(L1_SNAPSHOT_CAP);

        let bytes = serde_json::to_vec_pretty(&sorted)
            .map_err(|e| L1CacheError::json(target.clone(), e))?;
        std::fs::write(&tmp, &bytes).map_err(|e| L1CacheError::io(tmp.clone(), e))?;
        // Defensive: re-assert the parent dir exists right before rename.
        // `create_dir_all` is a cheap idempotent syscall when the dir
        // already exists, and it eliminates a TOCTOU window in case some
        // other process or test cleanup removed the directory between the
        // first `create_dir_all` above and this rename.
        if let Err(e) = std::fs::create_dir_all(data_dir) {
            // Best-effort tmp cleanup before bubbling the error.
            let _ = std::fs::remove_file(&tmp);
            return Err(L1CacheError::io(data_dir, e));
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            // Best-effort tmp cleanup so a failed rename doesn't leak
            // `.tmp.<pid>.<seq>` files into the palace directory.
            let _ = std::fs::remove_file(&tmp);
            return Err(L1CacheError::io(target, e));
        }
        Ok(())
    }

    /// Load the L1 cache snapshot, returning an empty vec if missing.
    ///
    /// Why: A first-time palace open should not error just because the L1
    /// snapshot has never been written yet.
    /// What: Reads `<data_dir>/l1_cache.json` if present and deserializes.
    /// Returns `Ok(Vec::new())` on missing.
    /// Test: `l1_cache_roundtrip` covers populated; missing returns empty is
    /// exercised by `load_l1_cache_missing_is_empty`.
    pub fn load_l1_cache(data_dir: &Path) -> Result<Vec<Drawer>> {
        let target = data_dir.join(L1_CACHE_JSON);
        if !target.exists() {
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(&target).map_err(|e| L1CacheError::io(target.clone(), e))?;
        let drawers: Vec<Drawer> =
            serde_json::from_slice(&bytes).map_err(|e| L1CacheError::json(target, e))?;
        Ok(drawers)
    }

    /// Check whether the cached snapshot is older than `max_age_secs`.
    ///
    /// Why: Long-lived daemons may want to lazily refresh the L1 snapshot if
    /// it has gotten too old relative to drawer-table mutations elsewhere.
    /// What: Compares the file's mtime to `SystemTime::now()`. Returns
    /// `Ok(true)` if missing (since "no snapshot" is maximally stale), `Ok(false)`
    /// if the snapshot's age is `<= max_age_secs`.
    /// Test: `l1_cache_stale` writes a snapshot, asserts not stale at 60s and
    /// stale at 0s.
    pub fn is_stale(data_dir: &Path, max_age_secs: u64) -> Result<bool> {
        let target = data_dir.join(L1_CACHE_JSON);
        if !target.exists() {
            return Ok(true);
        }
        let meta = std::fs::metadata(&target).map_err(|e| L1CacheError::io(target.clone(), e))?;
        let modified = meta
            .modified()
            .map_err(|e| L1CacheError::io(target.clone(), e))?;
        let age = SystemTime::now()
            .duration_since(modified)
            .map_err(|e| L1CacheError::Time(e.to_string()))?;
        // Compare in milliseconds so a 0-second tolerance treats any
        // already-on-disk snapshot as stale (its mtime predates `now()` by
        // at least the syscall round-trip).
        let max_age_ms = max_age_secs.saturating_mul(1000);
        Ok(age.as_millis() as u64 > max_age_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn drawer_with_importance(content: &str, importance: f32) -> Drawer {
        let mut d = Drawer::new(Uuid::new_v4(), content);
        d.importance = importance;
        d
    }

    #[test]
    fn l1_cache_roundtrip() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path();

        // Build 20 drawers with importance 0.0, 0.05, 0.10, ... 0.95.
        let drawers: Vec<Drawer> = (0..20)
            .map(|i| drawer_with_importance(&format!("drawer {i}"), i as f32 * 0.05))
            .collect();

        L1Cache::save_l1_cache(&drawers, data_dir).expect("save");
        let loaded = L1Cache::load_l1_cache(data_dir).expect("load");

        // Should be capped at 15, sorted by importance descending.
        assert_eq!(loaded.len(), L1_SNAPSHOT_CAP);
        for window in loaded.windows(2) {
            assert!(
                window[0].importance >= window[1].importance,
                "snapshot must be sorted descending by importance"
            );
        }
        // The top entry should be the 0.95-importance drawer.
        assert!((loaded[0].importance - 0.95).abs() < 1e-6);
    }

    #[test]
    fn load_l1_cache_missing_is_empty() {
        let tmp = tempdir().unwrap();
        let drawers = L1Cache::load_l1_cache(tmp.path()).unwrap();
        assert!(drawers.is_empty());
    }

    #[test]
    fn l1_cache_stale() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path();
        let drawers = vec![drawer_with_importance("only", 0.9)];
        L1Cache::save_l1_cache(&drawers, data_dir).unwrap();

        // Freshly written snapshot must not be stale at 60s tolerance.
        assert!(!L1Cache::is_stale(data_dir, 60).unwrap());
        // With 0s tolerance, anything older than the call itself is stale.
        // Sleep a hair to guarantee mtime < now.
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(L1Cache::is_stale(data_dir, 0).unwrap());
    }

    #[test]
    fn is_stale_when_missing() {
        let tmp = tempdir().unwrap();
        // No snapshot ever written -> always stale.
        assert!(L1Cache::is_stale(tmp.path(), 999_999).unwrap());
    }

    /// Regression test for issue #154: concurrent `save_l1_cache` calls
    /// against the same `data_dir` must not race on a shared tmp filename.
    ///
    /// Why: Before the per-call tmp naming fix, two writers would both write
    /// `l1_cache.json.tmp`, then both `rename(tmp -> target)`; the first
    /// rename consumed the tmp, the second failed with
    /// "No such file or directory" at the destination path. Reproducing this
    /// deterministically is hard, but a 16-thread parallel writer pool
    /// reliably hit it before the fix.
    /// What: Spawns 16 OS threads that each call `save_l1_cache` 10 times
    /// against the same directory and asserts every call returns `Ok`. After
    /// the burst the directory contains exactly one `l1_cache.json` (the
    /// last winner) and no `.tmp.*` orphans.
    /// Test: this test.
    #[test]
    fn concurrent_save_l1_cache_no_enoent() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();

        let mut handles = Vec::with_capacity(16);
        for thread_id in 0..16u32 {
            let dir = data_dir.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..10u32 {
                    let drawers = vec![drawer_with_importance(
                        &format!("t{thread_id}-i{i}"),
                        (thread_id as f32) * 0.01 + (i as f32) * 0.001,
                    )];
                    L1Cache::save_l1_cache(&drawers, &dir).unwrap_or_else(|e| {
                        panic!("save_l1_cache (t={thread_id}, i={i}) failed: {e}")
                    });
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        // Final state assertions: a real snapshot exists and no .tmp files leak.
        assert!(data_dir.join(L1_CACHE_JSON).exists());
        let leaked_tmps: Vec<_> = std::fs::read_dir(&data_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(L1_CACHE_JSON) && n.contains(".tmp."))
            .collect();
        assert!(
            leaked_tmps.is_empty(),
            "leaked tmp files after concurrent saves: {leaked_tmps:?}"
        );
    }
}
