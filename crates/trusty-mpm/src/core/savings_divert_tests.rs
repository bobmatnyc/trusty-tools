//! Tests for the bulk-read diversion savings producer (#6959).
//!
//! Why: the producer's whole contract is "claim only what the diversion
//! genuinely avoided". A permissive implementation — one that ignored the
//! worker's own cost, or priced an unknown model at a guessed rate — would put a
//! fabricated figure on the operator's status bar. The fail-open write branch
//! needs its own test for the opposite reason: a ledger that cannot be written
//! must not turn a diversion that answered into one that failed.
//! What: the row builder is driven with hand-supplied byte counts and an
//! injected price, so every decline branch is asserted with no configured model;
//! the write path is driven against a tempdir.
//! Test: this file.

use super::*;

use crate::core::savings::{fold_session, savings_log_in};

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<(String, f64)> {
    Some(("claude-sonnet-4-6".to_string(), 3.0))
}

/// Why (#6959 acceptance criterion 1): the hand-computed delta is the whole
/// claim. 400,000 file bytes are 100,000 tokens at four bytes each; a 4,000-byte
/// summary is 1,000; the delta is 99,000 tokens, which at Sonnet's $3/Mtok input
/// rate is $0.297, less the worker's own $0.02 — $0.277.
/// Test: itself.
#[test]
fn a_hand_computed_delta_matches_the_row() {
    let row = divert_row("sess-a", 400_000, 4_000, 0.02, sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 99_000);
    assert!(
        (row.cost_saved_usd - 0.277).abs() < 1e-9,
        "cost was {}",
        row.cost_saved_usd
    );
    assert!(
        row.basis.contains("100000") && row.basis.contains("1000") && row.basis.contains("0.02"),
        "the basis must carry file tokens, summary tokens, and the worker cost: {}",
        row.basis
    );
}

/// Why (#6959 acceptance criterion 2): a summary at least as large as the files
/// it summarises saved nothing. Writing a row there would state a saving that
/// did not happen.
/// Test: itself.
#[test]
fn no_row_when_the_summary_is_not_smaller_than_the_files() {
    assert!(
        divert_row("sess-a", 4_000, 8_000, 0.0, sonnet_price).is_none(),
        "a summary larger than the files must write no row"
    );
    assert!(
        divert_row("sess-a", 4_000, 4_000, 0.0, sonnet_price).is_none(),
        "an equal-size summary must write no row"
    );
    // Rounding must not manufacture a delta out of a sub-token difference.
    assert!(
        divert_row("sess-a", 4_003, 4_000, 0.0, sonnet_price).is_none(),
        "a sub-token delta must write no row"
    );
}

/// Why (#6959 acceptance criterion 3): the fold reads `technique` for a
/// per-technique breakdown, so the exact string is part of the contract.
/// Test: itself.
#[test]
fn divert_row_carries_the_named_technique() {
    let row = divert_row("sess-a", 400_000, 4_000, 0.02, sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_DIVERT);
    assert_eq!(row.technique, "divert");
}

/// Why (#6959): a model the shared table does not know must decline the row,
/// never substitute a guessed rate. A fabricated price is worse than no figure —
/// it is a figure the operator would act on.
/// Test: itself.
#[test]
fn no_row_when_the_parent_model_cannot_be_priced() {
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.02, || None).is_none(),
        "an unpriceable parent model must write no row"
    );
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.02, || Some(("m".into(), 0.0))).is_none(),
        "a zero rate must write no row"
    );
}

/// Why (#6959): the worker is not free. A diversion whose worker billed more
/// than the avoided tokens were worth cost money rather than saving it, and the
/// row must not claim the gross figure as if the worker were free. The
/// exactly-cancelling case is the sharp one: the subtraction leaves float
/// residue near `4e-17`, so an implementation testing `> 0.0` writes a row
/// claiming 99,000 tokens saved for no money at all.
/// Test: itself.
#[test]
fn no_row_when_the_worker_cost_exceeds_the_saving() {
    // 400,000 B is 100,000 tokens; less a 4,000 B summary that is $0.297 gross.
    assert!(
        divert_row("sess-a", 400_000, 4_000, 0.297, sonnet_price).is_none(),
        "a worker that cost exactly the delta must write no row"
    );
    assert!(
        divert_row(
            "sess-a",
            400_000,
            4_000,
            0.297 - MIN_COST_SAVED_USD / 2.0,
            sonnet_price
        )
        .is_none(),
        "a sub-micro-dollar saving must write no row"
    );
    assert!(
        divert_row("sess-a", 400_000, 4_000, 5.0, sonnet_price).is_none(),
        "a worker that cost more than the delta must write no row"
    );
}

/// Why (#6959): the row is attributed to a session, and the statusline folds by
/// session id. A row written under no id is unattributable and would inflate
/// nothing while cluttering the ledger.
/// Test: itself.
#[test]
fn no_row_without_a_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    record_divert_with(&ledger, "  ", 400_000, 4_000, 0.02, sonnet_price);
    assert!(
        !ledger.exists(),
        "an empty session id must not create a ledger"
    );
}

/// Why (#6959, deliverable 4): the end of the chain — a recorded diversion has
/// to come back out of the fold under the same session id the statusline reads
/// with, or the segment stays absent no matter what was written.
/// Test: itself.
#[test]
fn a_recorded_diversion_folds_into_the_session_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    record_divert_with(&ledger, "sess-a", 400_000, 4_000, 0.02, sonnet_price);

    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1);
    assert_eq!(total.tokens_saved, 99_000);
    assert!((total.cost_saved_usd - 0.277).abs() < 1e-9);
    // Another session's status bar reads nothing from the same file.
    assert_eq!(fold_session(&ledger, "sess-b").rows, 0);
}

/// Why (#6959, deliverable 3): this is a FAIL-OPEN branch, and fail-open code
/// only stays fail-open if something asserts it. The answer is already on the
/// agent's stdout when this runs, so a ledger that cannot be written must cost
/// the row and nothing else. An implementation that `?`-propagated or
/// `expect()`-ed the write would fail this test by panicking.
/// Test: itself.
#[test]
fn a_ledger_write_failure_does_not_fail_the_diversion() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A regular file where the `usage/` directory must go: `create_dir_all`
    // inside the writer cannot succeed, so the append returns an IO error.
    let blocker = dir.path().join("usage");
    std::fs::write(&blocker, "not a directory").expect("write blocker");
    let ledger = savings_log_in(dir.path());

    record_divert_with(&ledger, "sess-a", 400_000, 4_000, 0.02, sonnet_price);

    assert!(
        !ledger.exists(),
        "the unwritable ledger must stay unwritten, not partially created"
    );
    assert!(
        blocker.is_file(),
        "the blocking file must be left exactly as it was"
    );
}

/// Why: the price the producer uses must come from the shared table, not a
/// fourth copy. Asserting the resolved rate against a direct table lookup pins
/// that without depending on which model this machine is configured for.
/// Test: itself.
#[test]
fn resolve_session_price_agrees_with_the_shared_table() {
    // A machine configured for a model outside the table resolves to `None`,
    // which declines the row — the documented behaviour, not a test failure.
    if let Some((model, rate)) = resolve_session_price() {
        let table = trusty_common::inference::pricing(&model)
            .expect("the resolved model must be one the table knows");
        assert_eq!(rate, table.input, "the rate must be the table's input rate");
        assert!(rate > 0.0, "a priced model must have a positive input rate");
    }
}
