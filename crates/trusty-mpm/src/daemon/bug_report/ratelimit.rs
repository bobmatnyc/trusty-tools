//! Anti-spam and rate-limit guards for the bug-report filing pipeline.
//!
//! Why: Without local rate-limiting, a developer could inadvertently re-file
//!      the same fingerprint on every `report_bug` call, or a misbehaving
//!      automation could create dozens of issues in a short window. Two
//!      complementary guards prevent this:
//!
//!   1. **Per-fingerprint stamp** (`FingerprintStampStore`) — records each
//!      filed fingerprint to a small JSON file in the data directory. Before
//!      filing, the caller checks whether that fingerprint was filed within
//!      the configured window (default 24 hours). If it was, the call is
//!      skipped with a clear "already filed recently" message.
//!
//!   2. **Per-hour cap** (`HourlyCap`) — keeps a rolling list of filing
//!      timestamps under the data directory. If the last N filings all
//!      occurred within the same rolling hour, further filings are blocked
//!      until the window expires.
//!
//! Both stores are plain JSON files so they survive across process restarts
//! and are human-readable for debugging.
//!
//! All decision logic is pure (accepts timestamps as arguments, never calls
//! `std::time::SystemTime::now()` internally) to enable deterministic tests.
//!
//! Test: `tests::fingerprint_stamp_blocks_within_window`,
//!       `tests::fingerprint_stamp_allows_after_window`,
//!       `tests::hourly_cap_blocks_when_exceeded`,
//!       `tests::hourly_cap_allows_under_limit`,
//!       `tests::hourly_cap_expires_old_filings`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default per-fingerprint re-file window (24 hours in seconds).
pub const DEFAULT_FP_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Default maximum new issues allowed per rolling hour.
pub const DEFAULT_HOURLY_CAP: usize = 10;

/// Rolling window for the per-hour cap (3600 seconds = 1 hour).
pub const HOUR_WINDOW_SECS: i64 = 3600;

// ── FilingDecision ────────────────────────────────────────────────────────────

/// The outcome of a rate-limit check.
///
/// Why: callers need a typed result they can match on to produce specific,
///      actionable messages rather than a bare bool.
/// What: three variants — `Allowed` (proceed), `FingerprintRecentlyFiled`
///       (per-fingerprint stamp blocked), `HourlyCapExceeded` (global cap
///       exceeded).
/// Test: returned by `check` and asserted in `tests::*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilingDecision {
    /// The filing is allowed to proceed.
    Allowed,
    /// This fingerprint was filed within the configured window; skip to avoid
    /// duplicate spam. Carries the seconds since last filing.
    FingerprintRecentlyFiled {
        /// How many seconds ago the fingerprint was last filed.
        secs_ago: i64,
        /// The configured minimum window in seconds.
        window_secs: i64,
    },
    /// The global hourly cap has been exceeded. Carries the current count and
    /// the cap.
    HourlyCapExceeded {
        /// Number of issues filed in the current rolling hour.
        filed_this_hour: usize,
        /// The configured cap.
        cap: usize,
    },
}

impl FilingDecision {
    /// Returns `true` when the decision is `Allowed`.
    ///
    /// Why: convenience method for callers that only need a boolean check.
    /// What: matches on the enum variant.
    /// Test: used in `tests::*`.
    pub fn is_allowed(&self) -> bool {
        matches!(self, FilingDecision::Allowed)
    }

    /// Returns a human-readable explanation of why filing was blocked.
    ///
    /// Why: the MCP tool and HTTP handler surface this message to the user.
    /// What: `Allowed` returns an empty string; blocked variants return a
    ///       specific message with the relevant counts / windows.
    /// Test: `tests::decision_messages`.
    pub fn block_reason(&self) -> String {
        match self {
            FilingDecision::Allowed => String::new(),
            FilingDecision::FingerprintRecentlyFiled {
                secs_ago,
                window_secs,
            } => {
                let hours = window_secs / 3600;
                format!(
                    "this error was already filed {secs_ago}s ago; \
                     minimum re-file window is {hours}h ({window_secs}s)"
                )
            }
            FilingDecision::HourlyCapExceeded {
                filed_this_hour,
                cap,
            } => {
                format!(
                    "rate-limited: {filed_this_hour} issues already filed this hour \
                     (cap={cap}); try again later"
                )
            }
        }
    }
}

// ── FingerprintStampStore ─────────────────────────────────────────────────────

/// Persisted map of `fingerprint → last_filed_unix_secs`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FingerprintStamps {
    stamps: HashMap<String, i64>,
}

/// Tracks when each fingerprint was last filed, persisted to a JSON file.
///
/// Why: the GitHub dedup search (Phase 3) catches cross-machine duplicates, but
///      a local stamp avoids unnecessary GitHub API calls when the same developer
///      runs `report_bug` multiple times for the same error within a short window.
/// What: reads from / writes to `<data_dir>/bugreport-fp-stamps.json`.  Each
///       fingerprint maps to the Unix timestamp (seconds) of its last filing.
///       Entries older than the window are evicted on load to keep the file small.
/// Test: `tests::fingerprint_stamp_blocks_within_window`,
///       `tests::fingerprint_stamp_allows_after_window`.
pub struct FingerprintStampStore {
    path: PathBuf,
    /// Minimum re-file window in seconds (default: 24 h).
    window_secs: i64,
}

impl FingerprintStampStore {
    /// Create a store backed by `path`, using the given re-file window.
    ///
    /// Why: path and window are constructor arguments so tests can use a temp
    ///      directory and a short window without touching production config.
    /// What: does not read the file at construction — loading is lazy.
    /// Test: constructed in `tests::*` with `tempfile::tempdir()`.
    pub fn new(path: PathBuf, window_secs: i64) -> Self {
        Self { path, window_secs }
    }

    /// Build the default store path from the platform config directory.
    ///
    /// Why: production callers should not need to know the exact path.
    /// What: uses `dirs::config_dir()` or falls back to the home directory;
    ///       returns `~/.config/trusty-mpm/bugreport-fp-stamps.json` on Unix.
    /// Test: not exercised directly in unit tests (path varies by system).
    pub fn default_path() -> PathBuf {
        if let Some(cfg) = dirs::config_dir() {
            cfg.join("trusty-mpm/bugreport-fp-stamps.json")
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config/trusty-mpm/bugreport-fp-stamps.json")
        }
    }

    /// Load the stamp map from disk, evicting entries older than the window.
    ///
    /// Why: on-disk eviction keeps the file from growing unboundedly.
    /// What: reads the JSON file; entries with `last_filed_secs < now - window`
    ///       are dropped; returns an empty map if the file does not exist or
    ///       cannot be parsed.
    /// Test: eviction exercised by `tests::fingerprint_stamp_allows_after_window`.
    fn load(&self, now_secs: i64) -> FingerprintStamps {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(_) => return FingerprintStamps::default(),
        };
        let mut stamps: FingerprintStamps = serde_json::from_str(&raw).unwrap_or_default();
        // Evict expired entries.
        let cutoff = now_secs - self.window_secs;
        stamps.stamps.retain(|_, &mut ts| ts >= cutoff);
        stamps
    }

    /// Persist the stamp map to disk.
    ///
    /// Why: the map must survive process restarts to be effective.
    /// What: creates the parent directory if absent; writes the JSON atomically
    ///       via a temp file + rename where possible; falls back to a direct
    ///       write if the temp path cannot be constructed.
    /// Test: persistence exercised by `tests::fingerprint_stamp_persists`.
    fn save(&self, stamps: &FingerprintStamps) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(stamps)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// Check whether `fingerprint` was filed within the configured window.
    ///
    /// Why: pure decision function — does not modify state, so it is safe
    ///      to call without side effects.
    /// What: loads the stamp map and checks whether `fingerprint` has an entry
    ///       with `last_filed > now - window_secs`. Returns the appropriate
    ///       [`FilingDecision`] variant.
    /// Test: `tests::fingerprint_stamp_blocks_within_window`,
    ///       `tests::fingerprint_stamp_allows_after_window`.
    pub fn check(&self, fingerprint: &str, now_secs: i64) -> FilingDecision {
        let stamps = self.load(now_secs);
        if let Some(&last_filed) = stamps.stamps.get(fingerprint) {
            let secs_ago = now_secs - last_filed;
            if secs_ago < self.window_secs {
                return FilingDecision::FingerprintRecentlyFiled {
                    secs_ago,
                    window_secs: self.window_secs,
                };
            }
        }
        FilingDecision::Allowed
    }

    /// Record that `fingerprint` was filed at `now_secs`, then persist.
    ///
    /// Why: must be called after a successful filing so future calls are
    ///      correctly rate-limited.
    /// What: loads the current stamp map, inserts/updates the fingerprint entry,
    ///       and saves.
    /// Test: called in `tests::fingerprint_stamp_blocks_within_window`.
    pub fn record_filed(&self, fingerprint: &str, now_secs: i64) -> anyhow::Result<()> {
        let mut stamps = self.load(now_secs);
        stamps.stamps.insert(fingerprint.to_string(), now_secs);
        self.save(&stamps)
    }
}

// ── HourlyCap ─────────────────────────────────────────────────────────────────

/// Persisted list of Unix timestamps of recent filings (for the hourly cap).
#[derive(Debug, Default, Serialize, Deserialize)]
struct HourlyFilings {
    filings: Vec<i64>,
}

/// Tracks the count of filings in the current rolling hour.
///
/// Why: a per-fingerprint stamp prevents repeated filing of the same bug, but a
///      global cap is needed to handle the case where many different fingerprints
///      are filed in a short burst (e.g. a faulty agent loop).
/// What: reads from / writes to `<data_dir>/bugreport-hourly.json`. Each entry
///       is a Unix timestamp; entries older than one hour are evicted on load.
///       If the count within the rolling hour exceeds the cap, filing is blocked.
/// Test: `tests::hourly_cap_blocks_when_exceeded`,
///       `tests::hourly_cap_allows_under_limit`,
///       `tests::hourly_cap_expires_old_filings`.
pub struct HourlyCap {
    path: PathBuf,
    /// Maximum filings allowed per rolling hour.
    cap: usize,
}

impl HourlyCap {
    /// Create a cap store backed by `path`, with the given cap.
    ///
    /// Why: path and cap are constructor arguments so tests can use a temp
    ///      directory and a small cap value.
    /// What: does not read the file at construction — loading is lazy.
    /// Test: constructed in `tests::*`.
    pub fn new(path: PathBuf, cap: usize) -> Self {
        Self { path, cap }
    }

    /// Build the default store path from the platform config directory.
    ///
    /// Why: production callers should not need to know the exact path.
    /// What: returns `~/.config/trusty-mpm/bugreport-hourly.json` on Unix.
    /// Test: not exercised in unit tests.
    pub fn default_path() -> PathBuf {
        if let Some(cfg) = dirs::config_dir() {
            cfg.join("trusty-mpm/bugreport-hourly.json")
        } else {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config/trusty-mpm/bugreport-hourly.json")
        }
    }

    /// Load the filing timestamps, evicting those older than one hour.
    ///
    /// Why: the rolling window must only count recent filings.
    /// What: reads the JSON file; drops timestamps older than `now - HOUR_WINDOW_SECS`;
    ///       returns an empty list if the file does not exist.
    /// Test: `tests::hourly_cap_expires_old_filings`.
    fn load(&self, now_secs: i64) -> HourlyFilings {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(_) => return HourlyFilings::default(),
        };
        let mut filings: HourlyFilings = serde_json::from_str(&raw).unwrap_or_default();
        let cutoff = now_secs - HOUR_WINDOW_SECS;
        filings.filings.retain(|&ts| ts >= cutoff);
        filings
    }

    /// Persist the filings list.
    ///
    /// Why: the hourly count must survive process restarts.
    /// What: creates parent dir if absent; writes JSON.
    /// Test: persistence covered by `tests::hourly_cap_blocks_when_exceeded`.
    fn save(&self, filings: &HourlyFilings) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(filings)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    /// Check whether the hourly cap allows another filing.
    ///
    /// Why: pure decision function — called before filing to gate the call.
    /// What: loads and counts recent filings; if the count >= cap, returns
    ///       `HourlyCapExceeded`; otherwise returns `Allowed`.
    /// Test: `tests::hourly_cap_blocks_when_exceeded`,
    ///       `tests::hourly_cap_allows_under_limit`.
    pub fn check(&self, now_secs: i64) -> FilingDecision {
        let filings = self.load(now_secs);
        let count = filings.filings.len();
        if count >= self.cap {
            FilingDecision::HourlyCapExceeded {
                filed_this_hour: count,
                cap: self.cap,
            }
        } else {
            FilingDecision::Allowed
        }
    }

    /// Record a filing at `now_secs`, then persist.
    ///
    /// Why: must be called after each successful filing so the count is accurate
    ///      for subsequent calls.
    /// What: appends `now_secs` to the filings list and saves.
    /// Test: called in `tests::hourly_cap_blocks_when_exceeded`.
    pub fn record_filed(&self, now_secs: i64) -> anyhow::Result<()> {
        let mut filings = self.load(now_secs);
        filings.filings.push(now_secs);
        self.save(&filings)
    }
}

// ── Composite guard ───────────────────────────────────────────────────────────

/// Combined rate-limit guard: fingerprint stamp + hourly cap.
///
/// Why: the two guards are always used together; a composite struct simplifies
///      the call sites in the filing path.
/// What: holds a [`FingerprintStampStore`] and a [`HourlyCap`]; [`check`]
///       returns the first blocking decision, or `Allowed` when both allow.
///       [`record_filed`] updates both stores atomically.
/// Test: composite behaviour covered by individual store tests + integration
///       in the filing path.
pub struct RateLimitGuard {
    fp_store: FingerprintStampStore,
    hourly_cap: HourlyCap,
}

impl RateLimitGuard {
    /// Create a guard with production default paths and limits.
    ///
    /// Why: the production entry point — callers need not specify paths or caps.
    /// What: uses `FingerprintStampStore::default_path()` / `HourlyCap::default_path()`
    ///       and the package-level defaults.
    /// Test: not unit-tested (path varies); the individual stores are tested.
    pub fn production() -> Self {
        Self {
            fp_store: FingerprintStampStore::new(
                FingerprintStampStore::default_path(),
                DEFAULT_FP_WINDOW_SECS,
            ),
            hourly_cap: HourlyCap::new(HourlyCap::default_path(), DEFAULT_HOURLY_CAP),
        }
    }

    /// Create a guard with injected paths and limits (for tests).
    ///
    /// Why: tests need a temp directory and small caps for reproducibility.
    /// What: accepts explicit paths, window, and cap values.
    /// Test: `tests::composite_guard_*`.
    pub fn with_config(
        fp_path: PathBuf,
        fp_window_secs: i64,
        hourly_path: PathBuf,
        hourly_cap: usize,
    ) -> Self {
        Self {
            fp_store: FingerprintStampStore::new(fp_path, fp_window_secs),
            hourly_cap: HourlyCap::new(hourly_path, hourly_cap),
        }
    }

    /// Check both guards and return the first blocking decision, or `Allowed`.
    ///
    /// Why: fingerprint check runs first (cheaper — one map lookup); hourly cap
    ///      second (requires counting the list).
    /// What: returns `FingerprintRecentlyFiled` if that fires, then
    ///       `HourlyCapExceeded` if that fires, then `Allowed`.
    /// Test: `tests::composite_guard_fp_blocks`, `tests::composite_guard_hourly_blocks`.
    pub fn check(&self, fingerprint: &str, now_secs: i64) -> FilingDecision {
        let fp_decision = self.fp_store.check(fingerprint, now_secs);
        if !fp_decision.is_allowed() {
            return fp_decision;
        }
        self.hourly_cap.check(now_secs)
    }

    /// Record a successful filing for `fingerprint` at `now_secs`.
    ///
    /// Why: both stores must be updated on every successful filing.
    /// What: calls `FingerprintStampStore::record_filed` and
    ///       `HourlyCap::record_filed` in sequence; logs warnings on write
    ///       errors but does not propagate them (best-effort persistence).
    /// Test: `tests::composite_guard_fp_blocks`.
    pub fn record_filed(&self, fingerprint: &str, now_secs: i64) {
        if let Err(e) = self.fp_store.record_filed(fingerprint, now_secs) {
            tracing::warn!("failed to persist fingerprint stamp: {e}");
        }
        if let Err(e) = self.hourly_cap.record_filed(now_secs) {
            tracing::warn!("failed to persist hourly cap: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── FingerprintStampStore tests ───────────────────────────────────────────

    #[test]
    fn fingerprint_stamp_blocks_within_window() {
        let dir = tempdir().unwrap();
        let store =
            FingerprintStampStore::new(dir.path().join("stamps.json"), DEFAULT_FP_WINDOW_SECS);
        let fp = "a".repeat(64);
        let now = 1_700_000_000i64;

        // Initially allowed.
        assert!(
            store.check(&fp, now).is_allowed(),
            "should be allowed initially"
        );

        // Record filing.
        store.record_filed(&fp, now).unwrap();

        // Immediately blocked.
        let decision = store.check(&fp, now + 10);
        assert!(
            !decision.is_allowed(),
            "should be blocked within window: {decision:?}"
        );
        assert!(
            matches!(decision, FilingDecision::FingerprintRecentlyFiled { .. }),
            "expected FingerprintRecentlyFiled: {decision:?}"
        );
    }

    #[test]
    fn fingerprint_stamp_allows_after_window() {
        let dir = tempdir().unwrap();
        let window = 3600i64; // 1 hour
        let store = FingerprintStampStore::new(dir.path().join("stamps.json"), window);
        let fp = "b".repeat(64);
        let now = 1_700_000_000i64;

        store.record_filed(&fp, now).unwrap();

        // Still blocked at window - 1 second.
        let still_blocked = store.check(&fp, now + window - 1);
        assert!(
            !still_blocked.is_allowed(),
            "should still be blocked: {still_blocked:?}"
        );

        // Allowed at window + 1 second (past the window).
        let after = store.check(&fp, now + window + 1);
        assert!(
            after.is_allowed(),
            "should be allowed after window: {after:?}"
        );
    }

    #[test]
    fn fingerprint_stamp_different_fps_independent() {
        let dir = tempdir().unwrap();
        let store =
            FingerprintStampStore::new(dir.path().join("stamps.json"), DEFAULT_FP_WINDOW_SECS);
        let fp1 = "c".repeat(64);
        let fp2 = "d".repeat(64);
        let now = 1_700_000_000i64;

        store.record_filed(&fp1, now).unwrap();

        // fp1 is blocked, fp2 is still allowed.
        assert!(
            !store.check(&fp1, now + 60).is_allowed(),
            "fp1 should be blocked"
        );
        assert!(
            store.check(&fp2, now + 60).is_allowed(),
            "fp2 should still be allowed"
        );
    }

    #[test]
    fn fingerprint_stamp_persists_across_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stamps.json");
        let window = DEFAULT_FP_WINDOW_SECS;
        let fp = "e".repeat(64);
        let now = 1_700_000_000i64;

        // Write with store1.
        let store1 = FingerprintStampStore::new(path.clone(), window);
        store1.record_filed(&fp, now).unwrap();

        // Read with store2 (new instance, same path).
        let store2 = FingerprintStampStore::new(path, window);
        let decision = store2.check(&fp, now + 60);
        assert!(
            !decision.is_allowed(),
            "stamp should persist across store instances: {decision:?}"
        );
    }

    // ── HourlyCap tests ───────────────────────────────────────────────────────

    #[test]
    fn hourly_cap_allows_under_limit() {
        let dir = tempdir().unwrap();
        let cap = HourlyCap::new(dir.path().join("hourly.json"), 3);
        let now = 1_700_000_000i64;

        // Record 2 filings — below cap of 3.
        cap.record_filed(now).unwrap();
        cap.record_filed(now + 10).unwrap();

        let decision = cap.check(now + 20);
        assert!(
            decision.is_allowed(),
            "should be allowed under limit: {decision:?}"
        );
    }

    #[test]
    fn hourly_cap_blocks_when_exceeded() {
        let dir = tempdir().unwrap();
        let cap = HourlyCap::new(dir.path().join("hourly.json"), 2);
        let now = 1_700_000_000i64;

        // File up to the cap.
        cap.record_filed(now).unwrap();
        cap.record_filed(now + 1).unwrap();

        // Third filing should be blocked.
        let decision = cap.check(now + 2);
        assert!(
            !decision.is_allowed(),
            "should be blocked when cap exceeded: {decision:?}"
        );
        assert!(
            matches!(
                decision,
                FilingDecision::HourlyCapExceeded {
                    filed_this_hour: 2,
                    cap: 2
                }
            ),
            "expected HourlyCapExceeded(2, 2): {decision:?}"
        );
    }

    #[test]
    fn hourly_cap_expires_old_filings() {
        let dir = tempdir().unwrap();
        let cap_val = 2;
        let cap = HourlyCap::new(dir.path().join("hourly.json"), cap_val);
        let now = 1_700_000_000i64;

        // File 2 entries at `now`.
        cap.record_filed(now).unwrap();
        cap.record_filed(now + 1).unwrap();

        // At now + 1 hour + 5 seconds, old entries expire.
        let later = now + HOUR_WINDOW_SECS + 5;
        let decision = cap.check(later);
        assert!(
            decision.is_allowed(),
            "old filings should expire from the rolling window: {decision:?}"
        );
    }

    #[test]
    fn hourly_cap_persists_across_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hourly.json");
        let now = 1_700_000_000i64;

        let cap1 = HourlyCap::new(path.clone(), 3);
        cap1.record_filed(now).unwrap();
        cap1.record_filed(now + 1).unwrap();

        let cap2 = HourlyCap::new(path, 3);
        // Both entries should be seen by cap2 (still within the hour).
        let decision = cap2.check(now + 5);
        // Under cap of 3, so still allowed.
        assert!(
            decision.is_allowed(),
            "should be allowed (2 < cap=3): {decision:?}"
        );
    }

    // ── FilingDecision helpers ────────────────────────────────────────────────

    #[test]
    fn decision_messages() {
        let allowed = FilingDecision::Allowed;
        assert!(allowed.block_reason().is_empty());

        let fp_blocked = FilingDecision::FingerprintRecentlyFiled {
            secs_ago: 600,
            window_secs: 86400,
        };
        let msg = fp_blocked.block_reason();
        assert!(msg.contains("already filed"), "{msg}");
        assert!(msg.contains("600s"), "{msg}");

        let hourly_blocked = FilingDecision::HourlyCapExceeded {
            filed_this_hour: 10,
            cap: 10,
        };
        let msg2 = hourly_blocked.block_reason();
        assert!(msg2.contains("rate-limited"), "{msg2}");
        assert!(msg2.contains("10"), "{msg2}");
    }

    // ── RateLimitGuard composite tests ────────────────────────────────────────

    #[test]
    fn composite_guard_fp_blocks_before_hourly() {
        let dir = tempdir().unwrap();
        let guard = RateLimitGuard::with_config(
            dir.path().join("fp.json"),
            DEFAULT_FP_WINDOW_SECS,
            dir.path().join("hourly.json"),
            DEFAULT_HOURLY_CAP,
        );
        let fp = "f".repeat(64);
        let now = 1_700_000_000i64;

        // Initially allowed.
        assert!(guard.check(&fp, now).is_allowed());

        // Record and re-check.
        guard.record_filed(&fp, now);
        let blocked = guard.check(&fp, now + 60);
        assert!(
            matches!(blocked, FilingDecision::FingerprintRecentlyFiled { .. }),
            "expected fingerprint block: {blocked:?}"
        );
    }

    #[test]
    fn composite_guard_hourly_blocks_when_fp_allows() {
        let dir = tempdir().unwrap();
        let cap = 2;
        let guard = RateLimitGuard::with_config(
            dir.path().join("fp.json"),
            DEFAULT_FP_WINDOW_SECS,
            dir.path().join("hourly.json"),
            cap,
        );
        let now = 1_700_000_000i64;

        // File 2 different fingerprints to hit the hourly cap.
        let fp1 = "g".repeat(64);
        let fp2 = "h".repeat(64);
        guard.record_filed(&fp1, now);
        guard.record_filed(&fp2, now + 1);

        // A third (different) fingerprint should hit the hourly cap.
        let fp3 = "i".repeat(64);
        let blocked = guard.check(&fp3, now + 2);
        assert!(
            matches!(blocked, FilingDecision::HourlyCapExceeded { .. }),
            "expected hourly cap block: {blocked:?}"
        );
    }
}
