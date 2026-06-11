//! Prior-boot loaded-index-count persistence and warm-boot summary helpers (issue #873).
//!
//! Why: extracted from `commands/start.rs` to keep that file under the
//! 500-line cap allowlist budget. Persisting the prior count lets the daemon
//! detect a macOS TCC FDA regression (caused by `cargo install` changing the
//! binary cdhash) on the NEXT boot and emit an actionable re-grant hint.
//! What: `prior_index_count_path` (file location), `save_prior_index_count`
//! (write), `load_prior_index_count` (read, public),
//! `record_warm_boot_result` (write WarmBootSummary + emit FDA warning).
//! Test: write-then-read round-trip in `commands/start.rs` tests via the
//! public `load_prior_index_count` function.

/// Path to the file that persists the prior-boot loaded index count (issue #873).
///
/// Why: `cargo install` changes the binary cdhash and silently revokes macOS TCC
/// Full Disk Access, causing the daemon to load only a few indexes instead of
/// the full set. Persisting the prior count lets the daemon detect this regression
/// on the NEXT boot and emit an actionable FDA re-grant hint.
/// What: `<daemon_dir>/prior_index_count.txt` — a single ASCII decimal line.
/// Test: `save_prior_index_count` / `load_prior_index_count` round-trip.
pub(crate) fn prior_index_count_path() -> Option<std::path::PathBuf> {
    crate::service::daemon::daemon_lock_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("prior_index_count.txt")))
}

/// Write the loaded index count to disk for comparison on the next boot.
///
/// Why (issue #873): the prior count is used by `restore_indexes` to detect
/// when `cargo install` has revoked FDA and caused a large fraction of indexes
/// to be skipped.
/// What: writes `count\n` to `prior_index_count.txt` in the daemon data dir.
/// Best-effort: failures are logged at debug level and do not abort warm-boot.
/// Test: `save_prior_index_count` / `load_prior_index_count` write-read roundtrip.
pub(crate) fn save_prior_index_count(count: usize) {
    let Some(path) = prior_index_count_path() else {
        return;
    };
    let content = format!("{count}\n");
    if let Err(e) = std::fs::write(&path, content) {
        tracing::debug!(
            "warm-boot: could not save prior index count to {}: {e}",
            path.display()
        );
    }
}

/// Read the prior-boot loaded index count from disk (issue #873).
///
/// Why: called at daemon startup to load the prior count before warm-boot
/// so `restore_indexes` can detect a large drop (FDA regression).
/// What: reads `prior_index_count.txt`; returns `0` when absent or unparseable.
/// Test: write then read roundtrip in the `tests` submodule of `commands/start.rs`.
pub(crate) fn load_prior_index_count() -> usize {
    let Some(path) = prior_index_count_path() else {
        return 0;
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

/// Write the WarmBootSummary onto `state`, emit a loud FDA re-grant warning
/// when the loaded count dropped below 80% of the prior count, and persist
/// the new count for the next boot (issues #873, #993).
///
/// Why: factored out of `restore_indexes` in `commands/start.rs` to keep that
/// file under the line-cap allowlist budget. Gathers the three counter-update
/// concerns (summary write, FDA warning, count save) into one callable.
/// What: writes `WarmBootSummary` to `state.warmboot_summary`, emits
/// `tracing::error!` with FDA hint when `total < prior * 80%`, persists
/// `total` via `save_prior_index_count`, and updates `state.prior_index_count`.
/// `indexes_lazy` is stored in the summary and reflects the count at warm-boot
/// completion; the health handler re-reads from `cold_store.len()` live so the
/// value decreases as lazy loads occur.
/// Test: covered indirectly by the warm-boot integration tests in `start.rs`.
pub(crate) fn record_warm_boot_result(
    state: &crate::service::SearchAppState,
    total: usize,
    total_skipped_tcc: usize,
    total_skipped_timeout: usize,
    indexes_lazy: usize,
) {
    let prior_count = state
        .prior_index_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let degraded_by_tcc = total_skipped_tcc > 0;
    // Issue #1091: scan timeouts must also set warm_boot_degraded so monitors
    // can distinguish "timed-out index (0 chunks)" from "healthy empty index".
    let degraded_by_timeout = total_skipped_timeout > 0;
    // Single source of truth for the 80%-of-prior threshold (issue #873 review nit).
    let degraded_by_count = prior_count > 0 && total < prior_count * 4 / 5;
    let warm_boot_degraded = degraded_by_tcc || degraded_by_timeout || degraded_by_count;

    if let Ok(mut summary) = state.warmboot_summary.lock() {
        *summary = crate::service::server::WarmBootSummary {
            indexes_loaded: total,
            indexes_skipped_tcc: total_skipped_tcc,
            indexes_skipped_timeout: total_skipped_timeout,
            warm_boot_degraded,
            // Issue #993: stored at boot-completion; health handler re-reads
            // from cold_store.len() live so it decreases as lazy loads happen.
            indexes_lazy,
        };
    }

    // Issue #1091: emit error! (not just warn!) when scan timeouts occurred so
    // this is visible without trace-level logging. The individual per-index
    // warn! logs in `restore_one_index_bounded` identify which indexes timed out;
    // this error! surfaces the aggregate on /health and in log aggregators.
    if degraded_by_timeout {
        tracing::error!(
            loaded = total,
            skipped_timeout = total_skipped_timeout,
            "warm-boot DEGRADED: {total_skipped_timeout} index(es) TIMED OUT during restore \
             and were NOT loaded. These indexes are missing from search results and /health. \
             The skipped indexes may have been on a slow or temporarily inaccessible filesystem. \
             Increase TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS (default 10s) if the filesystem is \
             legitimately slow, or investigate the per-index warn! logs above for root causes. \
             (issue #1091)"
        );
    }

    if degraded_by_count {
        tracing::error!(
            loaded = total,
            prior = prior_count,
            skipped_tcc = total_skipped_tcc,
            "warm-boot DEGRADED: only {total}/{prior_count} indexes loaded (< 80% of prior). \
             If you just ran `cargo install trusty-search`, macOS TCC likely revoked \
             Full Disk Access because the new binary has a different cdhash. \
             ACTION REQUIRED: re-grant Full Disk Access in \
             System Settings → Privacy & Security → Full Disk Access → \
             remove and re-add ~/.cargo/bin/trusty-search. \
             This is NOT data loss — all on-disk indexes are intact. (issue #873)"
        );
    }

    if total > 0 {
        save_prior_index_count(total);
        state
            .prior_index_count
            .store(total, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for WarmBootSummary degraded-flag logic (issues #873, #1091).
    //!
    //! Why: `record_warm_boot_result` is the single source of truth for the
    //! `warm_boot_degraded` flag. These tests pin the flag semantics so
    //! refactors cannot accidentally drop the timeout-degrades-flag behaviour
    //! added for issue #1091.
    //! Test: run with `cargo test -p trusty-search -- prior_index_count::tests`.

    use super::*;
    use crate::core::registry::IndexRegistry;
    use crate::service::SearchAppState;

    fn make_state() -> SearchAppState {
        SearchAppState::new(IndexRegistry::new())
    }

    /// Why (issue #1091): `warm_boot_degraded` must be `true` when at least one
    /// index timed out during restore, even when no TCC denials occurred and
    /// `indexes_loaded` is above 80% of prior. Previously the flag was only set
    /// for TCC skips and count drops, so a pure-timeout scenario produced
    /// `warm_boot_degraded: false` — indistinguishable from a clean boot.
    /// What: call `record_warm_boot_result` with 1 timeout and 0 TCC skips;
    /// assert `warm_boot_degraded = true` and `indexes_skipped_timeout = 1`.
    /// Test: this test.
    #[test]
    fn warmboot_summary_timeout_sets_degraded_flag() {
        let state = make_state();
        // 5 loaded, 1 timed out, 0 TCC denials, prior count = 0 (first run).
        record_warm_boot_result(&state, 5, 0, 1, 0);
        let summary = state.warmboot_summary.lock().unwrap().clone();
        assert_eq!(summary.indexes_loaded, 5, "loaded count must be recorded");
        assert_eq!(
            summary.indexes_skipped_timeout, 1,
            "timeout count must be recorded"
        );
        assert_eq!(summary.indexes_skipped_tcc, 0, "tcc count must be 0");
        assert!(
            summary.warm_boot_degraded,
            "warm_boot_degraded must be true when at least one index timed out (issue #1091)"
        );
    }

    /// Why (regression for issue #873): `warm_boot_degraded` must be `true`
    /// when `indexes_skipped_tcc > 0`, even if no timeouts or count drops occurred.
    /// What: call with 1 TCC skip, 0 timeouts, prior = 0; assert degraded = true.
    /// Test: this test.
    #[test]
    fn warmboot_summary_tcc_skip_sets_degraded_flag() {
        let state = make_state();
        record_warm_boot_result(&state, 5, 1, 0, 0);
        let summary = state.warmboot_summary.lock().unwrap().clone();
        assert!(
            summary.warm_boot_degraded,
            "warm_boot_degraded must be true when TCC skips occurred"
        );
    }

    /// Why: when all indexes load cleanly (0 TCC, 0 timeout, count above 80% of
    /// prior), `warm_boot_degraded` must be `false` — no false alarms.
    /// What: call with 5 loaded, 0 skipped, prior = 5; assert degraded = false.
    /// Test: this test.
    #[test]
    fn warmboot_summary_clean_boot_not_degraded() {
        let state = make_state();
        state
            .prior_index_count
            .store(5, std::sync::atomic::Ordering::Relaxed);
        record_warm_boot_result(&state, 5, 0, 0, 0);
        let summary = state.warmboot_summary.lock().unwrap().clone();
        assert!(
            !summary.warm_boot_degraded,
            "warm_boot_degraded must be false on a clean boot with no skips"
        );
    }

    /// Why (regression for issue #873): count-drop (loaded < 80% of prior) must
    /// trigger `warm_boot_degraded` even with zero TCC skips and zero timeouts.
    /// What: set prior = 10, load only 7 (70%), call with 0 skips; assert degraded.
    /// Test: this test.
    #[test]
    fn warmboot_summary_count_drop_sets_degraded_flag() {
        let state = make_state();
        state
            .prior_index_count
            .store(10, std::sync::atomic::Ordering::Relaxed);
        record_warm_boot_result(&state, 7, 0, 0, 0);
        let summary = state.warmboot_summary.lock().unwrap().clone();
        assert!(
            summary.warm_boot_degraded,
            "warm_boot_degraded must be true when loaded < 80% of prior count"
        );
    }
}
