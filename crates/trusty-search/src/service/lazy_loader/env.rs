//! Environment-variable helpers for the selective/lazy warm-boot feature (#993).
//!
//! Why: isolates the two env-var readers (`warmboot_max_indexes`,
//! `cold_reload_timeout`) and the rate-limit constant
//! (`LAST_QUERIED_WRITE_INTERVAL_SECS`) so `store.rs` and `loader.rs` stay
//! focused on their respective data-structure / async-load concerns.
//! What: three public items; no side effects on import.
//! Test: `warmboot_max_indexes_*` and `cold_reload_timeout_*` in `super::tests`.

use std::time::Duration;

/// Minimum number of seconds that must elapse before `last_queried_unix` is
/// persisted again for the same index (rate-limiting the write to avoid
/// excessive TOML rewrites on hot indexes).
///
/// Why: if every search query wrote to `indexes.toml`, a busy index would
/// generate constant disk I/O. 60 s is the same cadence as the BM25/chunk
/// idle-eviction ticker, which is already an accepted background write rate.
/// What: compared against `SystemTime::now()` in the search handler.
/// Test: covered indirectly — the guard prevents double-writes within the window.
pub const LAST_QUERIED_WRITE_INTERVAL_SECS: u64 = 60;

/// Read the maximum number of indexes to warm-boot eagerly from the env var
/// `TRUSTY_WARMBOOT_MAX_INDEXES` (issue #993).
///
/// Why: operators with 100+ registered indexes can bound the startup time by
/// capping how many are loaded at boot. Cold indexes are loaded on first query.
/// What: parses the env var as a `usize`. Unset → `None` (warm-boot all,
/// back-compat default); `0` → `Some(0)` (lazy-load everything); `N` →
/// `Some(N)` (warm-boot top-N most-recently-used). A parse failure is logged
/// and treated as `None` (fallback to warm-boot-all).
/// Test: `warmboot_max_indexes_*` in the parent module's `tests` block.
pub fn warmboot_max_indexes() -> Option<usize> {
    let raw = std::env::var("TRUSTY_WARMBOOT_MAX_INDEXES").ok()?;
    match raw.trim().parse::<usize>() {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!(
                "TRUSTY_WARMBOOT_MAX_INDEXES={raw:?} is not a valid usize ({e}); \
                 falling back to warm-boot-all"
            );
            None
        }
    }
}

/// Per-query lazy-load deadline from `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS`.
///
/// Why: loading a cold index from disk can take several seconds (redb open +
/// HNSW snapshot read). We enforce a timeout so a query against a not-yet-loaded
/// index doesn't hang indefinitely — instead it returns a `503 index_loading`
/// response with a `retry_after_secs` field.
/// What: parses `TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS` as a positive `u64`.
/// Falls back to 30 s on parse failure or if the variable is unset.
/// `0` is treated as the default (zero-second timeouts are not useful).
/// Test: `cold_reload_timeout_*` in the parent module's `tests` block.
pub fn cold_reload_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_INDEX_COLD_RELOAD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(30);
    Duration::from_secs(secs)
}
