//! Lock-contention honesty tests for `recompute_warm_boot_degraded` (#5633).
//!
//! Why: these live in their own module for the reason
//! `tests_health_degraded.rs` documents in its own header — neither file is
//! classified as a test file by the line-cap gate (their basenames do not end
//! in `_test.rs`/`_tests.rs`), so both are held to the 500-SLOC production cap
//! and adding these cases to `tests_health_degraded.rs` pushed it to 519.
//! Nothing was moved out of that file; this module is new.
//! What: covers the one axis those tests do not — what the recompute writes
//! when it CANNOT read a handle's stages, as opposed to when it reads them and
//! finds a failure, a clean lane, or an in-progress one.
//! Test: these tests.
use super::*;

/// #5633: a stages read the recompute could not perform is not evidence that
/// the stage did not fail.
///
/// Why: `any_stage_failed` folded a contended `try_read` into `false` and then
/// WROTE the result to the sticky `warm_boot_degraded`. The `/health` scan uses
/// the same idiom deliberately — its comment justifies it as "a contended read
/// undercounts this poll only, and the next 2 s poll re-scans" — but this is a
/// one-shot recompute with no re-scan behind it, so the same undercount clears
/// a real degraded signal until the daemon restarts. The contention is not
/// hypothetical: `service/reindex/defer_embed.rs` and
/// `service/reindex/stages.rs` take `stages.write()` while the queue this
/// recompute is triggered by is still moving indexes toward Ready, and tokio's
/// `RwLock` is fair — a QUEUED writer already fails `try_read`, not merely a
/// held one.
/// What: registers an index whose semantic stage genuinely Failed, seeds
/// `warm_boot_degraded = true`, holds that handle's `stages` write lock across
/// the recompute so the scan cannot read it, and asserts the flag is NOT
/// cleared. The sign-flipped error is guarded by
/// `warm_boot_degraded_recomputes_to_false_once_catch_up_drains_cleanly` in
/// `tests_health_degraded.rs`, which still requires an UNCONTENDED clean scan
/// to clear the flag.
/// Test: this IS the test.
#[tokio::test]
async fn recompute_does_not_clear_degraded_when_a_stages_read_is_contended() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageState};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new("contended-5633"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "contended-5633",
            "/tmp/contended-5633",
        ))),
        "/tmp/contended-5633".into(),
    ));
    {
        let mut stages = handle.stages.write().await;
        stages.semantic = StageState::failed("injected embed failure for test".to_string());
    }

    let state = Arc::new(SearchAppState::new(registry));
    {
        let mut summary = state.warmboot_summary.lock().expect("lock");
        // No tcc/timeout/count cause: the Failed stage is the ONLY thing
        // keeping this degraded, so an unreadable scan is the only way the
        // flag can be wrongly cleared.
        summary.warm_boot_degraded = true;
        summary.indexes_skipped_tcc = 0;
        summary.indexes_skipped_timeout = 0;
    }

    // Hold the write lock across the (synchronous) recompute so every
    // `try_read` against this handle fails, exactly as a concurrent
    // deferred-embed stage write would make it fail.
    let _writer = handle.stages.write().await;

    super::health::recompute_warm_boot_degraded(&state);

    let summary = state.warmboot_summary.lock().expect("lock").clone();
    assert!(
        summary.warm_boot_degraded,
        "a contended stages read means the recompute could not establish that \
         nothing failed; clearing the sticky flag on that non-answer discards a \
         real degraded signal until the daemon restarts (#5633)"
    );
}

/// #5633: the inconclusive recompute must not consume the drain edge either.
///
/// Why: `/health` compares the deferred-embed completion epoch against the last
/// one it observed and recomputes on the difference. If an inconclusive scan
/// still commits the epoch, the edge is spent and nothing re-derives the flag
/// until the NEXT full drain — which is what makes "one-shot with no re-scan"
/// true in the first place. Reporting the recompute's own conclusiveness lets
/// the caller leave the edge armed for the next poll.
/// What: asserts the recompute reports `false` (inconclusive) while a handle's
/// stages lock is contended, and `true` when every handle reads cleanly.
/// Test: this IS the test.
#[tokio::test]
async fn recompute_reports_whether_its_scan_was_conclusive() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let registry = IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new("conclusive-5633"),
        Arc::new(RwLock::new(CodeIndexer::new(
            "conclusive-5633",
            "/tmp/conclusive-5633",
        ))),
        "/tmp/conclusive-5633".into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));

    {
        let _writer = handle.stages.write().await;
        assert!(
            !super::health::recompute_warm_boot_degraded(&state),
            "a scan that could not read every handle is not conclusive (#5633)"
        );
    }

    assert!(
        super::health::recompute_warm_boot_degraded(&state),
        "an uncontended scan read every handle and IS conclusive (#5633)"
    );
}
