//! Environment-variable helpers for the selective/lazy warm-boot feature (#993).
//!
//! Why: isolates the env-var readers (`warmboot_max_indexes`,
//! `warmboot_max_age`, `cold_reload_timeout`) and the rate-limit constant
//! (`LAST_QUERIED_WRITE_INTERVAL_SECS`) so `store.rs` and `loader.rs` stay
//! focused on their respective data-structure / async-load concerns.
//! What: public readers and constants; no side effects on import.
//! Test: `warmboot_max_indexes_*` and `cold_reload_timeout_*` in `super::tests`;
//! `warmboot_max_age_parses_hours_zero_and_invalid` for the #8275 age gate.

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

/// Env var naming the warm-boot age gate, in hours (#8275).
pub const WARMBOOT_MAX_AGE_HOURS_ENV: &str = "TRUSTY_WARMBOOT_MAX_AGE_HOURS";

/// Age-gate default: an index unused for a day is not eagerly loaded (#8275).
pub const DEFAULT_WARMBOOT_MAX_AGE_HOURS: u64 = 24;

/// Read the warm-boot age gate from `TRUSTY_WARMBOOT_MAX_AGE_HOURS` (#8275).
///
/// Why: warm-boot ranked indexes by recency however old the stamp was, so a
/// boot fully loaded indexes last used two or three days earlier. That was
/// about 4.2 GB of the 8.7 GB footprint measured six minutes after a restart.
/// What: the env value through [`parse_warmboot_max_age`]. `None` means no
/// age limit.
/// Test: `warmboot_max_age_parses_hours_zero_and_invalid`.
pub fn warmboot_max_age() -> Option<Duration> {
    parse_warmboot_max_age(std::env::var(WARMBOOT_MAX_AGE_HOURS_ENV).ok().as_deref())
}

/// Parse a raw `TRUSTY_WARMBOOT_MAX_AGE_HOURS` value (#8275).
///
/// Why: a pure form lets tests cover every spelling without writing the
/// process environment.
/// What: unset → the 24 h default; `0` → `None` (no age limit); `N` → `N`
/// hours. A value that is not a `u64` logs a `warn!` and falls back to the
/// default, so a typo never disables the gate.
/// Test: `warmboot_max_age_parses_hours_zero_and_invalid`.
pub fn parse_warmboot_max_age(raw: Option<&str>) -> Option<Duration> {
    let hours = match raw {
        None => DEFAULT_WARMBOOT_MAX_AGE_HOURS,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(
                    "{WARMBOOT_MAX_AGE_HOURS_ENV}={raw:?} is not a valid u64 ({e}); \
                     falling back to the default of {DEFAULT_WARMBOOT_MAX_AGE_HOURS} h (#8275)"
                );
                DEFAULT_WARMBOOT_MAX_AGE_HOURS
            }
        },
    };
    (hours > 0).then(|| Duration::from_secs(hours.saturating_mul(3600)))
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
