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
        // Twice the saved figure — a plausible "before" for a row this test
        // helper builds. Callers that need a specific before/saved ratio for a
        // percent assertion build their own `SavingsRow` instead.
        tokens_before: tokens.max(0) as u64 * 2,
        cost_saved_usd: usd,
        basis: "sources 1000 B - compiled 400 B".to_string(),
        model_source: crate::core::session_model::MODEL_SOURCE_LAUNCH_CONFIG.to_string(),
    }
}

/// Why (#6972): the ledger carries rows written before `model_source` existed,
/// and the fold must keep counting them. A field without `#[serde(default)]`
/// would have made every pre-#6972 line unparseable — silently zeroing the
/// operator's running total on the first render after an upgrade.
/// Test: itself.
#[test]
fn a_row_written_before_the_model_source_field_still_folds() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(
        &dir,
        &[
            r#"{"ts":"2026-09-07T02:41:00Z","session_id":"sess-a","technique":"divert","tokens_saved":5300,"cost_saved_usd":0.0159,"basis":"files 5000 tok - summary 200 tok"}"#,
        ],
    );
    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1, "a pre-#6972 row must still count");
    assert_eq!(total.tokens_saved, 5300);
    let parsed: SavingsRow =
        serde_json::from_str(&std::fs::read_to_string(&ledger).expect("read")).expect("parse");
    assert_eq!(
        parsed.model_source, "",
        "an absent model_source must read back as empty, not fail the parse"
    );
}

/// Why (#7179): the ledger carries rows written before `tokens_before`
/// existed. A field without `#[serde(default)]` would make every such line
/// unparseable — zeroing the operator's fold, not just the percent, on the
/// first render after the upgrade.
/// Test: itself.
#[test]
fn a_row_written_before_tokens_before_existed_still_folds() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ledger_with(
        &dir,
        &[
            r#"{"ts":"2026-09-07T02:41:00Z","session_id":"sess-a","technique":"divert","tokens_saved":5300,"cost_saved_usd":0.0159,"basis":"files 5000 tok - summary 200 tok"}"#,
        ],
    );
    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1, "a pre-#7179 row must still count");
    assert_eq!(total.tokens_saved, 5300);
    assert_eq!(
        total.tokens_before, 0,
        "an absent tokens_before must read back as 0, not fail the parse"
    );
    assert_eq!(
        total.percent_saved(None),
        None,
        "a fold with no tokens_before has no percent to report"
    );
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
            tokens_before: 0,
            cost_saved_usd: 0.0,
            rows: 4,
        }
        .is_zero()
    );
    assert!(
        !SavingsTotal {
            tokens_saved: 1,
            tokens_before: 2,
            cost_saved_usd: 0.0,
            rows: 1,
        }
        .is_zero()
    );
}

/// Why (#7179): the percent is what the `💸` segment renders, so its rounding
/// rule is pinned directly against hand-built totals rather than inferred from
/// the ledger round trip.
/// Test: itself.
#[test]
fn percent_saved_rounds_to_nearest_whole_number() {
    // 1/3 rounds to the nearest whole percent, not truncates.
    assert_eq!(
        SavingsTotal {
            tokens_saved: 1,
            tokens_before: 3,
            cost_saved_usd: 0.01,
            rows: 1,
        }
        .percent_saved(None),
        Some(33)
    );
    assert_eq!(
        SavingsTotal {
            tokens_saved: 5_000,
            tokens_before: 20_000,
            cost_saved_usd: 0.05,
            rows: 1,
        }
        .percent_saved(None),
        Some(25)
    );
}

/// Why (#7179): a zero fold has nothing to divide, and `None` — not `Some(0)`
/// — is what tells the render layer to omit the segment rather than print a
/// false `0%`.
/// Test: itself.
#[test]
fn percent_saved_is_none_on_a_zero_fold() {
    assert_eq!(SavingsTotal::default().percent_saved(None), None);
}

/// Why (#7179): the one output this method may never produce while it accepted
/// at least one row — a naive `round()` with no floor lets a sub-0.5% ratio
/// print `0%`, which reads as "the harness saved nothing" and states a
/// measurement that was never made.
/// Test: itself.
#[test]
fn percent_saved_never_rounds_down_to_zero() {
    assert_eq!(
        SavingsTotal {
            tokens_saved: 1,
            tokens_before: 10_000,
            cost_saved_usd: 0.000_01,
            rows: 1,
        }
        .percent_saved(None),
        Some(1)
    );
}

/// Why (#7179): a ledger with only pre-#7179 rows (no `tokens_before`) folds
/// `tokens_saved > 0` beside `tokens_before == 0` — nothing to divide by, so
/// the segment must omit the percent rather than report a fabricated one.
/// Test: itself.
#[test]
fn percent_saved_clamps_a_legacy_mixed_fold() {
    assert_eq!(
        SavingsTotal {
            tokens_saved: 4_000,
            tokens_before: 0,
            cost_saved_usd: 0.01,
            rows: 1,
        }
        .percent_saved(None),
        None
    );
}

/// Why (#7179, owner ruling): the session-share denominator is the primary
/// reading now — `actual + saved`, not the ledger's own before-figure — and
/// this pins the exact division against the scenario the issue named: 40 000
/// tokens saved of a session that actually spent 160 000.
/// Test: itself.
#[test]
fn percent_saved_uses_the_session_actual_denominator_when_given() {
    let total = SavingsTotal {
        tokens_saved: 40_000,
        // Deliberately different from the actual-tokens path so the test
        // fails if the fallback formula runs instead.
        tokens_before: 999_999,
        cost_saved_usd: 0.5,
        rows: 3,
    };
    assert_eq!(total.percent_saved(Some(160_000)), Some(20));
}

/// Why (#7179): a brand-new session (or a state file that could not be read)
/// has no compaction tick yet — the percent must still render, falling back
/// to the ledger's own before-total rather than omitting the segment.
/// Test: itself.
#[test]
fn percent_saved_falls_back_to_tokens_before_when_actual_is_unknown() {
    let total = SavingsTotal {
        tokens_saved: 5_000,
        tokens_before: 20_000,
        cost_saved_usd: 0.05,
        rows: 1,
    };
    assert_eq!(total.percent_saved(None), Some(25));
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

/// Why (#7411): the hook re-derives an instruction-compression row when nothing
/// was staged, and guards that on "this session has one already". A guard built
/// on [`fold_session`] would read a session that has only a `divert` row as
/// already covered and never write the row the segment is missing.
/// Test: itself.
#[test]
fn has_row_sees_only_the_named_technique() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    let mut diverted = row("sess-a", 4_000, 0.012);
    diverted.technique = TECHNIQUE_DIVERT.to_string();
    append_row(&ledger, &diverted).expect("append");

    assert!(
        !has_row(&ledger, "sess-a", TECHNIQUE_INSTRUCTION_COMPRESSION),
        "a divert row must not mask a missing instruction-compression row"
    );
    assert!(has_row(&ledger, "sess-a", TECHNIQUE_DIVERT));

    append_row(&ledger, &row("sess-a", 6_000, 0.018)).expect("append");
    assert!(has_row(
        &ledger,
        "sess-a",
        TECHNIQUE_INSTRUCTION_COMPRESSION
    ));
    assert!(
        !has_row(&ledger, "sess-b", TECHNIQUE_INSTRUCTION_COMPRESSION),
        "another session's row must not answer for this one"
    );
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

/// A `Write` that records the size of every call it receives.
///
/// Why: the #7579 defect is invisible in the resulting bytes — one write of
/// `{…}\n` and two writes of `{…}` then `\n` produce an identical file. The
/// only observable is the call boundary, which is what an `O_APPEND`
/// descriptor interleaves on, so the test has to count calls rather than
/// inspect content.
struct CountingSink {
    writes: Vec<usize>,
    bytes: Vec<u8>,
}

impl std::io::Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writes.push(buf.len());
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Why (#7579): the module header promises two racing producers "interleave
/// rows, never bytes within a row", and `writeln!` broke that promise — it
/// hands the formatter's pieces to the sink separately, so the row and its
/// newline reached the `O_APPEND` descriptor as two writes and a producer
/// racing between them landed its own row mid-line. The operator's ledger
/// carries ten such lines.
/// FAILS BEFORE THIS CHANGE: `writeln!(sink, "{line}")` records two writes —
/// measured `[226, 1]` — where this asserts one.
/// Test: itself.
#[test]
fn a_row_and_its_newline_leave_in_one_write() {
    let mut sink = CountingSink {
        writes: Vec::new(),
        bytes: Vec::new(),
    };
    write_row_line(&mut sink, &row("sess-a", 4_000, 0.012)).expect("write");

    assert_eq!(
        sink.writes.len(),
        1,
        "the row and its newline must reach the descriptor together, got {:?}",
        sink.writes
    );
    let written = String::from_utf8(sink.bytes).expect("utf-8");
    assert!(
        written.ends_with('\n'),
        "the single write must carry the terminator: {written:?}"
    );
    assert_eq!(
        written.matches('\n').count(),
        1,
        "exactly one terminator: {written:?}"
    );
}

/// Why (#7658): the live regression. The operator's ledger carried 312
/// byte-identical `instruction-compression` rows for one session because both
/// producers called [`append_row`] without ever reading the file. N attempts at
/// one measurement must leave exactly one row.
/// Test: itself.
#[test]
fn append_row_once_writes_one_row_for_n_attempts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    let measurement = row("sess-dup", 183, 0.000_549);

    let mut appended = 0usize;
    for _ in 0..26 {
        match append_row_once(&ledger, &measurement).expect("append") {
            AppendOnce::Appended => appended += 1,
            AppendOnce::AlreadyPresent => {}
            AppendOnce::LedgerUnreadable => panic!("the ledger is readable in this test"),
        }
    }

    assert_eq!(appended, 1, "only the first attempt may write");
    let text = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(
        text.lines().filter(|line| !line.trim().is_empty()).count(),
        1,
        "26 attempts at one measurement must leave one row, got:\n{text}"
    );
}

/// Why (#7658): a session whose prompt genuinely changes mid-run folds a second,
/// DIFFERENT measurement, and suppressing that would lose real data. The key is
/// `(session_id, technique, basis)`, never the session alone.
/// Test: itself.
#[test]
fn append_row_once_still_records_a_different_basis() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    let first = row("sess-a", 183, 0.000_549);
    let mut second = first.clone();
    second.basis = "sources 30000 B - compiled 20000 B".to_string();

    assert_eq!(
        append_row_once(&ledger, &first).expect("append"),
        AppendOnce::Appended
    );
    assert_eq!(
        append_row_once(&ledger, &second).expect("append"),
        AppendOnce::Appended,
        "a different basis is a different measurement"
    );
    assert_eq!(fold_session(&ledger, "sess-a").rows, 2);
}

/// Why (#7658) — the Fail-Open Check. An unreadable ledger must NOT read as "no
/// row exists": that is exactly the fail-open that turns a read fault into an
/// unbounded append loop. The write is skipped and the caller is told.
/// Test: itself.
#[test]
fn append_row_once_skips_an_unreadable_ledger() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    // A DIRECTORY at the ledger path reads back as an error that is not
    // `NotFound` — the same shape as every other unreadable ledger this guard
    // has to survive, and the one a test can create portably.
    std::fs::create_dir_all(&ledger).expect("occupy the ledger path");

    assert_eq!(
        row_presence(&ledger, "sess-a", TECHNIQUE_INSTRUCTION_COMPRESSION, "b"),
        RowPresence::Unreadable,
        "an unreadable ledger must never report Absent"
    );
    assert_eq!(
        append_row_once(&ledger, &row("sess-a", 183, 0.000_549)).expect("no error"),
        AppendOnce::LedgerUnreadable,
        "the write is skipped when the ledger cannot be read"
    );
}

/// Why (#7658): a ledger that does not exist yet is the ordinary state on a
/// machine no producer has run on — absent, not unreadable, so the first row
/// still lands.
/// Test: itself.
#[test]
fn row_presence_of_a_missing_ledger_is_absent() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    assert_eq!(
        row_presence(&ledger, "sess-a", TECHNIQUE_INSTRUCTION_COMPRESSION, "b"),
        RowPresence::Absent
    );
}

/// Why (#7658): the presence check must see a row the FOLD rejects, or a
/// producer whose row is unfoldable re-appends it on every invocation forever —
/// the same unbounded growth by another route.
/// Test: itself.
#[test]
fn row_presence_finds_a_row_the_fold_rejects() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut unfoldable = row("sess-a", 0, 0.0);
    unfoldable.basis = "sources 10 B - compiled 10 B".to_string();
    let line = serde_json::to_string(&unfoldable).expect("encode");
    let ledger = ledger_with(&dir, &[&line]);

    assert_eq!(
        fold_session(&ledger, "sess-a").rows,
        0,
        "the fold rejects a row with non-positive tokens_saved"
    );
    assert_eq!(
        row_presence(
            &ledger,
            "sess-a",
            TECHNIQUE_INSTRUCTION_COMPRESSION,
            "sources 10 B - compiled 10 B"
        ),
        RowPresence::Present,
        "a rejected row is still on the file and must suppress a re-append"
    );
}

/// Why (#7658): `basis` is what separates a repeat from a new measurement, so a
/// presence check that ignored it would suppress real data.
/// Test: itself.
#[test]
fn row_presence_distinguishes_a_different_basis() {
    let dir = tempfile::tempdir().expect("temp dir");
    let line = serde_json::to_string(&row("sess-a", 183, 0.000_549)).expect("encode");
    let ledger = ledger_with(&dir, &[&line]);

    assert_eq!(
        row_presence(
            &ledger,
            "sess-a",
            TECHNIQUE_INSTRUCTION_COMPRESSION,
            "sources 1000 B - compiled 400 B"
        ),
        RowPresence::Present
    );
    assert_eq!(
        row_presence(
            &ledger,
            "sess-a",
            TECHNIQUE_INSTRUCTION_COMPRESSION,
            "sources 9999 B - compiled 400 B"
        ),
        RowPresence::Absent
    );
}
