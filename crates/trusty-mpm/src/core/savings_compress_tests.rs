//! Tests for the tool-output compression savings producer.
//!
//! Why: the producer's contract is "claim only what the compression genuinely
//! avoided". The passthrough case is the one that matters most: `tm compress`
//! returns short output unchanged, and a row written for such a run would carry
//! a `tokens_before` into the fold while saving nothing — dragging the `💸`
//! segment's per-session percentage, and therefore its average, down.
//! What: the row builder is driven with hand-supplied byte counts and an
//! injected price, so every decline branch is asserted with no configured model
//! and no process-wide env mutation; the write path is driven against a tempdir.
//! Test: this file.

use super::*;

use crate::core::savings::{fold_session, savings_log_in};
use crate::core::session_model::MODEL_SOURCE_STATUSLINE;

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<ParentModel> {
    Some(ParentModel {
        id: "claude-sonnet-4-6".to_string(),
        input_per_million: 3.0,
        source: MODEL_SOURCE_STATUSLINE,
    })
}

/// Why: the hand-computed delta is the whole claim. 400,000 bytes of gate output
/// are 100,000 tokens at four bytes each; the 40,000-byte compressed form is
/// 10,000; the delta is 90,000 tokens, which at Sonnet's $3/Mtok input rate is
/// $0.27.
/// Test: itself.
#[test]
fn a_hand_computed_byte_delta_matches_the_row() {
    let row = compress_row("sess-a", 400_000, 40_000, "rtk_binary", sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 90_000);
    assert_eq!(row.tokens_before, 100_000);
    assert!(
        (row.cost_saved_usd - 0.27).abs() < 1e-9,
        "expected $0.27, got {}",
        row.cost_saved_usd
    );
}

/// Why: the fold and any later attribution pass both key on this string, so it
/// is pinned rather than left to whatever the format call happens to emit.
/// Test: itself.
#[test]
fn a_compress_row_carries_the_named_technique() {
    let row = compress_row("sess-a", 400_000, 40_000, "rtk_binary", sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_COMPRESS);
    assert_eq!(row.technique, "compress");
}

/// Why: `rtk_binary` and `native_fallback` compress by different mechanisms and
/// will need splitting apart later. Carrying the label in `basis` is what makes
/// that possible off the ledger line alone.
/// Test: itself.
#[test]
fn the_basis_names_the_compression_path() {
    let row =
        compress_row("sess-a", 400_000, 40_000, "native_fallback", sonnet_price).expect("a row");
    assert!(
        row.basis.contains("native_fallback"),
        "basis must name the compression path, got: {}",
        row.basis
    );
    assert!(
        row.basis.contains("claude-sonnet-4-6"),
        "basis must name the model it priced at, got: {}",
        row.basis
    );
}

/// Why: `tm compress` passes output under its size gate through unchanged. That
/// run avoided nothing, and a zero-saving row would still fold a `tokens_before`
/// into the percentage and pull the average down.
/// Test: itself.
#[test]
fn passthrough_output_writes_no_row() {
    assert!(compress_row("sess-a", 1_159, 1_159, "native_fallback", sonnet_price).is_none());
}

/// Why: compression can expand its input (a short, non-repetitive payload plus
/// a header). Rounding must never turn that into a positive claim.
/// Test: itself.
#[test]
fn an_expanded_output_writes_no_row() {
    assert!(compress_row("sess-a", 4_000, 5_000, "native_fallback", sonnet_price).is_none());
}

/// Why: a row keyed by nothing is a row the statusline can never fold, so the
/// producer declines rather than writing one.
/// Test: itself.
#[test]
fn no_row_without_a_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_compress_with(
        dir.path(),
        "  ",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );
    assert!(!savings_log_in(dir.path()).exists());
}

/// Why: an unpriceable model must decline rather than substitute a guessed rate
/// — the same rule the divert producer holds.
/// Test: itself.
#[test]
fn no_row_when_the_session_model_cannot_be_priced() {
    assert!(compress_row("sess-a", 400_000, 40_000, "rtk_binary", || None).is_none());
}

/// Why: proves the whole write path — one run, one row, folding back under the
/// session it was keyed by.
/// Test: itself.
#[test]
fn a_compress_run_appends_exactly_one_row() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());

    record_compress_with(
        dir.path(),
        "sess-a",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );

    let contents = std::fs::read_to_string(&ledger).expect("read ledger");
    assert_eq!(
        contents.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "one run must write exactly one row, got: {contents}"
    );
    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1);
    assert_eq!(total.tokens_saved, 90_000);
    assert_eq!(total.tokens_before, 100_000);
}

/// Why: the compressed text is already the caller's return value by the time
/// this runs. An unwritable ledger must not turn a compression that worked into
/// one that failed.
/// Test: itself.
#[test]
fn a_ledger_write_failure_does_not_fail_the_compression() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A regular file where the `usage/` directory must go: `create_dir_all`
    // inside the writer cannot succeed, so the append returns an IO error.
    let blocker = dir.path().join("usage");
    std::fs::write(&blocker, "not a directory").expect("write blocker");
    let ledger = savings_log_in(dir.path());

    record_compress_with(
        dir.path(),
        "sess-a",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );

    assert!(
        !ledger.exists(),
        "the unwritable ledger must stay unwritten, not partially created"
    );
    assert!(
        blocker.is_file(),
        "the blocking file must be left exactly as it was"
    );
}
