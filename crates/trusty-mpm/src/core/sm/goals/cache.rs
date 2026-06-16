//! Atomic `goals.json` hot-cache persistence for the SM goal store (§9.4).
//!
//! Why: §9.4 mirrors the active goal set in a daemon state file
//! (`~/.trusty-mpm/sm/goals.json`) so the TUI can render goals without a memory
//! round-trip on every poll. The palace is the source of truth; this file is a
//! DERIVED cache, rebuilt from the palace on startup and re-written on every
//! mutation. The write must be ATOMIC — a crash mid-write must never leave a
//! truncated, unparseable cache — so we write to a sibling temp file and rename it
//! over the target (atomic on the same filesystem), exactly mirroring SM-5's
//! conversation-state store. The storage ROOT is injectable so tests use a
//! `tempdir` instead of the real home directory.
//! What: [`GoalCache`] owns the `<root>/sm/` directory and the `goals.json` path;
//! [`GoalCache::save`] serialises the goal list via write-tmp-then-rename, and
//! [`GoalCache::load`] reconstructs it (a missing file loads as an empty list, so
//! first-touch is not an error). Errors map to the shared [`SmGoalError`].
//! Test: `goals/cache_tests.rs` covers round-trip identity, atomic overwrite, the
//! missing-file → empty path, and a malformed-file error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{SmGoalError, SmGoalResult};
use super::model::Goal;

/// Process-wide monotonic counter that uniquifies temp-file names per `save`.
///
/// Why: two `save` calls in the same nanosecond (coarse clock) would otherwise
/// generate identical temp paths and race; a monotonic counter mixed with the
/// wall-clock nanos guarantees a distinct temp path per call (same rationale as
/// SM-5's conversation store).
/// What: incremented once per `save`; mixed into the temp filename.
/// Test: exercised indirectly by the cache round-trip tests.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Subdirectory under the data root that holds SM state files.
///
/// Why: §9.4 nests `goals.json` under `~/.trusty-mpm/sm/`, alongside the SM
/// conversation state files; isolating the segment keeps the layout consistent.
/// What: the `"sm"` path segment joined under the injected root.
/// Test: `cache_path_is_under_sm_subdir`.
pub const SM_STATE_SUBDIR: &str = "sm";

/// The cache filename under the `sm/` subdirectory.
///
/// Why: a single well-known name (`goals.json`, §9.4) the TUI and the store agree
/// on.
/// What: the basename joined under `<root>/sm/`.
/// Test: `cache_path_is_under_sm_subdir`.
pub const GOALS_CACHE_FILE: &str = "goals.json";

/// Atomic, root-injectable store for the `goals.json` hot cache (§9.4).
///
/// Why: centralises the path layout and the write-tmp-then-rename atomicity in one
/// tested type, and makes the storage root a constructor argument so tests never
/// write to `~/.trusty-mpm`. The daemon (SM-7) builds it from the real data dir;
/// tests build it from a `tempdir`.
/// What: holds `<root>/sm/` and persists the goal list as `goals.json`.
/// Test: `goals/cache_tests.rs`.
#[derive(Debug, Clone)]
pub struct GoalCache {
    /// `<data_root>/sm/` — the directory holding the cache file.
    dir: PathBuf,
}

impl GoalCache {
    /// Build a cache rooted at `data_root` (the SM data directory).
    ///
    /// Why: the daemon passes the real `~/.trusty-mpm` while tests pass a
    /// `tempdir`; the cache derives its `sm/` subdir from whichever root it is
    /// given, so the same code path is exercised in both.
    /// What: joins [`SM_STATE_SUBDIR`] under `data_root`. The directory is created
    /// lazily on the first `save`, so merely constructing performs no I/O.
    /// Test: `cache_path_is_under_sm_subdir`, `save_creates_sm_subdir`.
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            dir: data_root.into().join(SM_STATE_SUBDIR),
        }
    }

    /// The on-disk path of the `goals.json` cache.
    ///
    /// Why: callers/tests need to assert/locate the exact file; it also drives
    /// `save`/`load`.
    /// What: returns `<root>/sm/goals.json`.
    /// Test: `cache_path_is_under_sm_subdir`.
    pub fn path(&self) -> PathBuf {
        self.dir.join(GOALS_CACHE_FILE)
    }

    /// The directory holding the cache (read-only accessor).
    ///
    /// Why: tests assert the layout; diagnostics may want it.
    /// What: returns `<root>/sm/`.
    /// Test: `cache_path_is_under_sm_subdir`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Atomically persist the goal list to `goals.json` (§9.4).
    ///
    /// Why: a daemon crash mid-write must never corrupt the cache; an atomic
    /// rename guarantees readers see either the old or the new content, never a
    /// partial write.
    /// What: ensures the `sm/` directory exists, serialises `goals` to pretty JSON,
    /// writes it to a sibling temp file whose name is unique per call, then renames
    /// the temp file over the target. Failures map to [`SmGoalError`].
    /// Test: `save_then_load_round_trips`, `save_is_atomic_overwrite`,
    /// `save_creates_sm_subdir`.
    pub fn save(&self, goals: &[Goal]) -> SmGoalResult<()> {
        std::fs::create_dir_all(&self.dir).map_err(|source| SmGoalError::Io { source })?;

        let json =
            serde_json::to_vec_pretty(goals).map_err(|source| SmGoalError::Serde { source })?;

        let target = self.path();
        let tmp = self.dir.join(format!(
            "{GOALS_CACHE_FILE}.tmp.{}.{}.{}",
            std::process::id(),
            unique_temp_token(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, &json).map_err(|source| SmGoalError::Io { source })?;
        std::fs::rename(&tmp, &target).map_err(|source| {
            // Best-effort cleanup of the temp file on a failed rename.
            let _ = std::fs::remove_file(&tmp);
            SmGoalError::Io { source }
        })?;
        Ok(())
    }

    /// Load the goal list from `goals.json`, or an empty list if absent (§9.4).
    ///
    /// Why: on startup, before the palace rebuild runs (or as the fallback when the
    /// palace is unavailable), the cache is read; a never-written cache must load
    /// as empty rather than erroring.
    /// What: returns `vec![]` when the file is absent. Otherwise reads and
    /// deserialises it. A present but malformed file is a hard
    /// [`SmGoalError::Serde`] (corruption the operator should see).
    /// Test: `save_then_load_round_trips`, `load_missing_is_empty`,
    /// `load_rejects_malformed_file`.
    pub fn load(&self) -> SmGoalResult<Vec<Goal>> {
        match std::fs::read(self.path()) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| SmGoalError::Serde { source })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(SmGoalError::Io { source }),
        }
    }
}

/// Wall-clock nanoseconds since the Unix epoch, for temp-file uniqueness.
///
/// Why: combined with the pid and the monotonic counter, the nanosecond timestamp
/// makes a temp filename overwhelmingly unlikely to collide (same approach as
/// SM-5's conversation store).
/// What: returns `SystemTime::now()` as nanos since the epoch, falling back to `0`
/// if the clock predates the epoch (the counter still guarantees per-call
/// uniqueness, so `0` is safe).
/// Test: exercised indirectly by the cache round-trip tests.
fn unique_temp_token() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
