//! Tests for the per-session grouping and the average across sessions (#7074).
//!
//! Why: the owner ruled (2026-09-09, option A) that the `💸` segment shows the
//! mean of each session's own percentage beside the current session's figure.
//! An implementation that pooled every row into one ratio renders a plausible
//! number that is not the mean, so each test here fixes known per-session
//! percentages and asserts the arithmetic.
//! What: temp-directory ledgers built line by line, and a caller-supplied
//! actual-tokens lookup so no test touches the statusline state store.
//! Test: this file.

use super::*;

/// Write `lines` verbatim as a ledger and return its path.
fn ledger_with(dir: &tempfile::TempDir, lines: &[&str]) -> std::path::PathBuf {
    let ledger = savings_log_in(dir.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, format!("{}\n", lines.join("\n"))).expect("write");
    ledger
}

/// One ledger line with an exact saved/before pair, so a percent assertion is
/// arithmetic rather than a guess about a helper's ratio.
fn pct_row(session: &str, tokens_saved: i64, tokens_before: u64) -> String {
    serde_json::to_string(&SavingsRow {
        ts: now_ts(),
        session_id: session.to_string(),
        technique: TECHNIQUE_DIVERT.to_string(),
        tokens_saved,
        tokens_before,
        cost_saved_usd: 0.01,
        basis: "fixture".to_string(),
        model_source: crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG.to_string(),
    })
    .expect("serialize")
}

/// Why (#7074): the average is the mean of each session's own percentage, so
/// the fold must produce one total per session. A grouping bug that folded
/// every row into one bucket would still render a plausible percent.
/// Test: itself.
#[test]
fn fold_sessions_groups_by_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(
        &dir,
        &[
            &pct_row("sess-a", 100, 1_000),
            &pct_row("sess-a", 100, 1_000),
            &pct_row("sess-b", 500, 1_000),
        ],
    );

    let by_session = fold_sessions(&ledger);
    assert_eq!(by_session.len(), 2, "two sessions, two totals");
    assert_eq!(by_session["sess-a"].tokens_saved, 200);
    assert_eq!(by_session["sess-a"].tokens_before, 2_000);
    assert_eq!(by_session["sess-a"].rows, 2);
    assert_eq!(by_session["sess-b"].tokens_saved, 500);
}

/// Why: an absent ledger is the ordinary pre-producer state; it must yield an
/// empty map, never a map of zero-valued sessions the average would then treat
/// as real measurements.
/// Test: itself.
#[test]
fn fold_sessions_of_a_missing_ledger_is_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(fold_sessions(&dir.path().join("nope.jsonl")).is_empty());
}

/// Why (#7074, acceptance criterion a): three sessions with known percentages
/// must average to their arithmetic mean. An implementation that pooled every
/// row into one ratio would report 29 % here, not 30 %.
/// Test: itself.
#[test]
fn average_percent_is_the_mean_of_each_sessions_percent() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(
        &dir,
        &[
            &pct_row("sess-a", 100, 1_000),    // 10 %
            &pct_row("sess-b", 3_000, 10_000), // 30 %
            &pct_row("sess-c", 500, 1_000),    // 50 %
        ],
    );

    let by_session = fold_sessions(&ledger);
    let per_session: Vec<u32> = by_session
        .values()
        .filter_map(|t| t.percent_saved(None))
        .collect();
    assert_eq!(per_session, vec![10, 30, 50], "the three known percentages");
    assert_eq!(
        average_percent_saved(&by_session, |_| None),
        Some(30),
        "the arithmetic mean of 10, 30 and 50"
    );
}

/// Why (#7074, acceptance criterion a): with one session the average IS that
/// session's figure — an implementation that divided by a session count taken
/// from somewhere else would drift here first.
/// Test: itself.
#[test]
fn average_of_one_session_is_that_sessions_percent() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(&dir, &[&pct_row("sess-only", 250, 1_000)]);
    let by_session = fold_sessions(&ledger);
    assert_eq!(by_session["sess-only"].percent_saved(None), Some(25));
    assert_eq!(average_percent_saved(&by_session, |_| None), Some(25));
}

/// Why: a session's own actual-token count is the primary denominator (#7179),
/// and the average must honour it per session rather than applying one
/// session's reading to every session.
/// Test: itself.
#[test]
fn average_uses_each_sessions_own_actual_token_reading() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(
        &dir,
        &[
            &pct_row("sess-a", 40_000, 999_999),
            &pct_row("sess-b", 10_000, 999_999),
        ],
    );
    let by_session = fold_sessions(&ledger);
    // sess-a: 40k / (160k + 40k) = 20 %; sess-b: 10k / (90k + 10k) = 10 %.
    let actual = |session: &str| match session {
        "sess-a" => Some(160_000),
        "sess-b" => Some(90_000),
        _ => None,
    };
    assert_eq!(average_percent_saved(&by_session, actual), Some(15));
}

/// Why (#7074, acceptance criterion d): a corrupt line must not silently become
/// a 0 % session dragging the mean down — it is skipped, and the average is the
/// mean of the valid rows only. A wholly unreadable ledger yields `None`, never
/// `Some(0)`.
/// Test: itself.
#[test]
fn average_skips_corrupt_rows_and_never_reports_a_false_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let negative = r#"{"ts":"x","session_id":"sess-corrupt","technique":"divert","tokens_saved":-5,"tokens_before":1000,"cost_saved_usd":0.01,"basis":"b","model_source":""}"#;
    let ledger = ledger_with(
        &dir,
        &[
            &pct_row("sess-a", 100, 1_000), // 10 %
            "{not json at all",
            negative,
            &pct_row("sess-b", 300, 1_000), // 30 %
        ],
    );

    let by_session = fold_sessions(&ledger);
    assert_eq!(
        by_session.len(),
        2,
        "the corrupt line and the negative row contribute no session"
    );
    assert_eq!(
        average_percent_saved(&by_session, |_| None),
        Some(20),
        "the mean of the valid rows only"
    );

    // A ledger that cannot be read at all renders nothing, not 0 %.
    let unreadable = fold_sessions(&dir.path().join("missing.jsonl"));
    assert_eq!(average_percent_saved(&unreadable, |_| None), None);
}

/// Why: a session whose rows leave no percent denominator on either path must
/// contribute nothing rather than a fabricated `0`, or one legacy session would
/// halve a real average.
/// Test: itself.
#[test]
fn average_of_sessions_with_no_denominator_is_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(&dir, &[&pct_row("sess-legacy", 4_000, 0)]);
    let by_session = fold_sessions(&ledger);
    assert_eq!(by_session["sess-legacy"].percent_saved(None), None);
    assert_eq!(average_percent_saved(&by_session, |_| None), None);
}
