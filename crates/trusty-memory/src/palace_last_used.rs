//! Per-palace "last used" stamp, persisted beside the palace's own data (#6424).
//!
//! Why: the trusty-console memory roster shows a Last Used column and sorts by
//! it, and nothing in trusty-memory answered that question durably. The only
//! recency signal was `idle_evict`'s in-process `last_accessed` counter, which
//! exists to decide what to evict and resets to nothing on every daemon
//! restart — a column built on it would read "never" for every palace after a
//! bounce.
//!
//! What: one small file, `<palace data_dir>/last_used`, holding unix epoch
//! seconds as ASCII decimal. It is deliberately NOT a field on `palace.json`:
//! that file is the palace's identity record, rewritten wholesale by
//! `palace_update`, and folding a per-recall stamp into it would make every
//! recall a read-modify-write of the metadata a rename can lose against a
//! concurrent update.
//!
//! Write granularity — the chosen trade (#6424): a durable write per recall is
//! not worth a column measured in days, so [`stamp`] writes at most once per
//! [`MIN_WRITE_INTERVAL_SECS`] per palace, gated by an in-memory cache on
//! `AppState`. The first use after the daemon starts always writes, because the
//! cache is empty; subsequent uses inside the window are dropped. The cost of
//! that is bounded and stated plainly: the recorded stamp can lag real use by
//! up to `MIN_WRITE_INTERVAL_SECS`, and a daemon killed inside the window loses
//! that much resolution. There is no shutdown flush, because a column rendered
//! as a date cannot tell a minute from a minute.
//!
//! Test: `palace_last_used::tests` covers the round-trip, the absent and
//! garbage reads, and the throttle window.

use std::path::Path;

/// Basename of the stamp file inside a palace's data directory.
pub const STAMP_FILE: &str = "last_used";

/// Minimum seconds between two persisted stamps for the same palace.
///
/// Matches `trusty-search`'s `LAST_QUERIED_WRITE_INTERVAL_SECS`, which throttles
/// the same kind of write for the same reason — one cadence across both daemons
/// keeps the console's two tabs comparable.
pub const MIN_WRITE_INTERVAL_SECS: u64 = 60;

/// In-memory record of when each palace's stamp was last persisted.
///
/// Keyed by palace id. Lives on `AppState` so the throttle survives across tool
/// calls without a global.
pub type StampCache = std::sync::Arc<dashmap::DashMap<String, u64>>;

/// Read a palace's persisted last-used stamp.
///
/// Why: the console report and `palace_info` both answer "when was this last
/// used" off disk, without opening the palace — `console_metrics` is forbidden
/// from opening one at all (#1924).
/// What: parses `<data_dir>/last_used` as unix epoch seconds. Returns `None`
/// when the file is absent (a palace never used since this feature shipped),
/// unreadable, or does not parse. Every one of those is honestly "no stamp",
/// which the console renders as "never" and sorts last — never as the epoch.
/// Test: `stamp_round_trips`, `read_missing_is_none`, `read_garbage_is_none`.
pub fn read(data_dir: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(data_dir.join(STAMP_FILE)).ok()?;
    raw.trim().parse::<u64>().ok()
}

/// Write a palace's last-used stamp, replacing any previous one atomically.
///
/// Why: a torn stamp would parse as a wrong date rather than no date, and the
/// palace directory is shared with the redb files a concurrent reader may hold.
/// What: writes `<data_dir>/last_used.tmp` and renames it over the target, the
/// same create-then-rename `PalaceStore::save_palace` uses. Errors propagate;
/// callers log and continue, because a missing stamp must never fail a recall.
/// Test: `stamp_round_trips`.
pub fn write(data_dir: &Path, now_unix: u64) -> std::io::Result<()> {
    let target = data_dir.join(STAMP_FILE);
    let tmp = data_dir.join(format!("{STAMP_FILE}.tmp"));
    std::fs::write(&tmp, now_unix.to_string())?;
    std::fs::rename(&tmp, &target)
}

/// Record a palace as used now, unless it was already recorded recently.
///
/// Why: this is the throttle the module header describes. Putting the decision
/// here rather than at each call site is what stops one tool handler from
/// drifting into a write-per-call.
/// What: consults `cache` for the last stamp written in this process. Writes
/// and returns `true` when the palace has no cached stamp, or its cached stamp
/// is at least [`MIN_WRITE_INTERVAL_SECS`] old; returns `false` otherwise
/// without touching the disk. The cache is updated BEFORE the write so two
/// concurrent recalls in the same window cannot both write. A failed write is
/// logged at debug and swallowed — a stamp is a display convenience, never a
/// reason to fail the operation that earned it.
/// Test: `stamp_throttles_within_the_window`, `stamp_writes_again_after_the_window`.
pub fn stamp(cache: &StampCache, data_dir: &Path, palace_id: &str, now_unix: u64) -> bool {
    let fresh = cache
        .get(palace_id)
        .map(|prev| now_unix.saturating_sub(*prev) < MIN_WRITE_INTERVAL_SECS)
        .unwrap_or(false);
    if fresh {
        return false;
    }
    cache.insert(palace_id.to_string(), now_unix);
    if let Err(e) = write(data_dir, now_unix) {
        tracing::debug!(palace = %palace_id, "last_used stamp write failed: {e}");
    }
    true
}

/// Seconds since the unix epoch, saturating at 0 on a clock before 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> StampCache {
        std::sync::Arc::new(dashmap::DashMap::new())
    }

    #[test]
    fn stamp_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 1_800_000_000).unwrap();
        assert_eq!(read(dir.path()), Some(1_800_000_000));
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read(dir.path()),
            None,
            "a palace with no stamp is 'never used', not 'used at the epoch'"
        );
    }

    #[test]
    fn read_garbage_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STAMP_FILE), "not-a-timestamp").unwrap();
        assert_eq!(read(dir.path()), None);
    }

    /// The stamp survives the process that wrote it (#6424).
    ///
    /// The whole point of the file: `idle_evict`'s in-memory counter could
    /// already answer "recently used" for a live daemon. Reading the value back
    /// through a FRESH cache, with nothing carried over, is what proves the
    /// answer came off disk.
    #[test]
    fn stamp_survives_a_fresh_cache() {
        let dir = tempfile::tempdir().unwrap();
        assert!(stamp(&cache(), dir.path(), "p", 1_850_000_000));
        assert_eq!(read(dir.path()), Some(1_850_000_000));
    }

    #[test]
    fn stamp_throttles_within_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let c = cache();
        assert!(
            stamp(&c, dir.path(), "p", 1_000_000),
            "first use always writes"
        );
        assert!(
            !stamp(&c, dir.path(), "p", 1_000_000 + MIN_WRITE_INTERVAL_SECS - 1),
            "a second use inside the window must not write"
        );
        assert_eq!(
            read(dir.path()),
            Some(1_000_000),
            "the throttled call must leave the earlier stamp untouched"
        );
    }

    #[test]
    fn stamp_writes_again_after_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let c = cache();
        let later = 1_000_000 + MIN_WRITE_INTERVAL_SECS;
        assert!(stamp(&c, dir.path(), "p", 1_000_000));
        assert!(stamp(&c, dir.path(), "p", later));
        assert_eq!(read(dir.path()), Some(later));
    }

    #[test]
    fn stamp_throttles_per_palace_not_globally() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let c = cache();
        assert!(stamp(&c, a.path(), "a", 1_000_000));
        assert!(
            stamp(&c, b.path(), "b", 1_000_000),
            "one palace's write must not throttle another's"
        );
        assert_eq!(read(b.path()), Some(1_000_000));
    }
}
