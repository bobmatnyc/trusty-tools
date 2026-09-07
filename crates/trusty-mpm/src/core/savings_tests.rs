//! Tests for the per-session token-savings ledger (#6958).
//!
//! Why: the fold is the only thing standing between a producer bug and a wrong
//! number on an operator's status bar, so each of its three skip rules —
//! unparseable line, non-positive tokens, non-positive or non-finite cost — gets
//! a test that a permissive implementation fails.
//! What: temp-directory ledgers built line by line, so a malformed row is
//! literally a malformed line rather than a struct that could not exist.
//! Test: this file.

use super::*;

/// Write `lines` verbatim as a ledger and return its path.
fn ledger_with(dir: &tempfile::TempDir, lines: &[&str]) -> std::path::PathBuf {
    let ledger = savings_log_in(dir.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, format!("{}\n", lines.join("\n"))).expect("write");
    ledger
}

fn row(session: &str, tokens: i64, usd: f64) -> SavingsRow {
    SavingsRow {
        ts: now_ts(),
        session_id: session.to_string(),
        technique: TECHNIQUE_INSTRUCTION_COMPRESSION.to_string(),
        tokens_saved: tokens,
        cost_saved_usd: usd,
        basis: "sources 1000 B - compiled 400 B".to_string(),
    }
}

/// Why: the ledger sits beside where #6873's `usage.redb` will land, and a
/// producer writing to a different directory from the reader is the failure
/// this pins.
/// Test: itself.
#[test]
fn savings_log_in_nests_under_usage() {
    let path = savings_log_in(std::path::Path::new("/tmp/root"));
    assert_eq!(path, std::path::Path::new("/tmp/root/usage/savings.jsonl"));
}

/// Why: producers run in the library, where only the home-relative framework
/// root is in scope; this pins that they land under it and not somewhere else.
/// Test: itself.
#[test]
fn default_savings_log_is_under_the_framework_root() {
    let path = default_savings_log();
    assert!(
        path.ends_with("usage/savings.jsonl"),
        "default ledger must be <root>/usage/savings.jsonl: {}",
        path.display()
    );
    assert!(
        path.to_string_lossy().contains(".trusty-mpm"),
        "default ledger must sit under the framework root: {}",
        path.display()
    );
}

/// Why: the writer and the reader are the whole public surface; a round trip
/// proves the on-disk shape they agree on is the one that was serialised.
/// Test: itself.
#[test]
fn append_then_fold_round_trips() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    append_row(&ledger, &row("sess-a", 5_000, 0.015)).expect("append");
    append_row(&ledger, &row("sess-a", 1_000, 0.003)).expect("append");

    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 2);
    assert_eq!(total.tokens_saved, 6_000);
    assert!((total.cost_saved_usd - 0.018).abs() < 1e-9);
}

/// Why: the `usage/` directory does not exist on a fresh install, and a
/// producer that failed on that would never write a first row.
/// Test: itself.
#[test]
fn append_creates_the_usage_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    assert!(!ledger.parent().expect("parent").exists());
    append_row(&ledger, &row("sess-a", 10, 0.01)).expect("append");
    assert!(ledger.exists(), "the ledger file must have been created");
}

/// Why (#6958): the ordinary state before any producer runs. It must fold to
/// zero rather than error, because the consumer is a status bar.
/// Test: itself.
#[test]
fn fold_of_a_missing_ledger_is_zero() {
    let dir = tempfile::tempdir().expect("temp dir");
    let total = fold_session(&savings_log_in(dir.path()), "sess-a");
    assert_eq!(total, SavingsTotal::default());
    assert!(total.is_zero());
}

/// Why: `is_zero` is what the statusline gates the whole segment on, so its
/// two branches — no rows, and rows summing to nothing — are pinned directly.
/// Test: itself.
#[test]
fn zero_fold_is_zero() {
    assert!(SavingsTotal::default().is_zero());
    assert!(
        SavingsTotal {
            tokens_saved: 0,
            cost_saved_usd: 0.0,
            rows: 4,
        }
        .is_zero()
    );
    assert!(
        !SavingsTotal {
            tokens_saved: 1,
            cost_saved_usd: 0.0,
            rows: 1,
        }
        .is_zero()
    );
}

/// Why (#6958, required acceptance): a producer that crashes mid-write, or a
/// half-flushed line, must cost the fold that one row and nothing else. An
/// implementation that returns zero on the first parse error — or that
/// propagates the error — fails this.
/// Test: itself.
#[test]
fn fold_skips_a_malformed_line_and_keeps_the_valid_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let good = serde_json::to_string(&row("sess-a", 4_000, 0.012)).expect("json");
    let also_good = serde_json::to_string(&row("sess-a", 1_000, 0.003)).expect("json");
    let ledger = ledger_with(
        &dir,
        &[
            &good,
            "{\"ts\": \"2026-09-07T02:41:00Z\", \"session_id\": \"sess-a\", trunc",
            "not json at all",
            &also_good,
        ],
    );

    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 2, "only the two well-formed rows may count");
    assert_eq!(total.tokens_saved, 5_000);
    assert!((total.cost_saved_usd - 0.015).abs() < 1e-9);
}

/// Why (#6958, required acceptance): a producer bug that undercounts its
/// baseline writes a negative delta. A fold that summed it would report a
/// TOTAL LOWER than the legitimate rows alone — or, with an unsigned cast, a
/// wildly high one. Neither may happen: the bad row contributes nothing.
/// Test: itself.
#[test]
fn a_negative_row_cannot_raise_the_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let good = serde_json::to_string(&row("sess-a", 4_000, 0.012)).expect("json");
    let negative_tokens = serde_json::to_string(&row("sess-a", -900, 0.05)).expect("json");
    let negative_cost = serde_json::to_string(&row("sess-a", 900, -0.05)).expect("json");
    let zero_tokens = serde_json::to_string(&row("sess-a", 0, 0.02)).expect("json");
    let ledger = ledger_with(
        &dir,
        &[&good, &negative_tokens, &negative_cost, &zero_tokens],
    );

    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1, "only the legitimate row may count");
    assert_eq!(
        total.tokens_saved, 4_000,
        "the total must equal the positive row alone, not the sum"
    );
    assert!((total.cost_saved_usd - 0.012).abs() < 1e-9);
}

/// Why: a row whose `cost_saved_usd` is not a readable number must contribute
/// nothing, and must not poison the running float sum the statusline compares
/// against `$0.01`.
/// Test: itself.
#[test]
fn fold_skips_a_row_whose_cost_is_not_a_number() {
    let dir = tempfile::tempdir().expect("temp dir");
    let good = serde_json::to_string(&row("sess-a", 4_000, 0.012)).expect("json");
    let ledger = ledger_with(
        &dir,
        &[
            &good,
            r#"{"ts":"t","session_id":"sess-a","technique":"x","tokens_saved":10,"cost_saved_usd":null,"basis":"b"}"#,
        ],
    );
    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1);
    assert!(total.cost_saved_usd.is_finite());
    assert!((total.cost_saved_usd - 0.012).abs() < 1e-9);
}

/// Why (#6958, required acceptance): one machine's ledger holds every session's
/// rows, and the status bar shows one session. A fold that ignored the filter
/// would show every concurrent session's savings on all of them.
/// Test: itself.
#[test]
fn fold_ignores_other_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    append_row(&ledger, &row("sess-a", 4_000, 0.012)).expect("append");
    append_row(&ledger, &row("sess-b", 90_000, 1.35)).expect("append");

    let a = fold_session(&ledger, "sess-a");
    assert_eq!(a.rows, 1);
    assert_eq!(a.tokens_saved, 4_000);

    let b = fold_session(&ledger, "sess-b");
    assert_eq!(b.tokens_saved, 90_000);

    assert!(fold_session(&ledger, "sess-c").is_zero());
}

/// Why: the machine-wide fold must use the identical skip rules, or a future
/// `tm usage` surface and the status bar would disagree about a total.
/// Test: itself.
#[test]
fn fold_all_sums_every_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    append_row(&ledger, &row("sess-a", 4_000, 0.012)).expect("append");
    append_row(&ledger, &row("sess-b", 6_000, 0.018)).expect("append");
    append_row(&ledger, &row("sess-b", -1, 0.5)).expect("append");

    let total = fold_all(&ledger);
    assert_eq!(total.rows, 2);
    assert_eq!(total.tokens_saved, 10_000);
}

/// Why: rows sort chronologically by plain string comparison only if every
/// producer stamps the same format.
/// Test: itself.
#[test]
fn now_ts_is_rfc3339() {
    let ts = now_ts();
    assert!(ts.ends_with('Z'), "must be UTC with a Z suffix: {ts}");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&ts).is_ok(),
        "must parse as RFC 3339: {ts}"
    );
}
