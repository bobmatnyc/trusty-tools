//! #12064: the runner names a carryover-window stop, not a copy fault.

use crate::service::index_identity::CarryoverWindowWrite;

/// Why: a run stopped by a window write reported "carryover copy failed",
/// which reads as a corpus fault to an operator.
/// What: the stop error maps to the stop reason; any other error keeps the
/// copy-fault reason. Both are `Failed` in the runner.
/// Test: this function IS the test.
#[test]
fn a_window_write_stop_is_reported_as_a_stop() {
    let stop = anyhow::Error::new(CarryoverWindowWrite {
        index_id: "x".into(),
        writes: 2,
    })
    .context("begin");
    let reason = super::corpus_swap::begin_failure_reason(&stop);
    assert!(
        reason.contains("run stopped") && reason.contains("retried"),
        "{reason}"
    );
    let copy = anyhow::anyhow!("redb copy failed");
    assert_eq!(
        super::corpus_swap::begin_failure_reason(&copy),
        "carryover copy failed — live corpus intact"
    );
}
