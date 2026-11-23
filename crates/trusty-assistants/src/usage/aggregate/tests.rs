//! Aggregation correctness for the Costs surface (#4098).
//!
//! Why: Every figure the Costs tab shows is this fold's output, so the tests
//! that matter are the ones that pin the honesty rules — a missing log is not
//! `$0.00`, an unreadable line is counted rather than swallowed, and a
//! multi-model log is priced per row rather than at one blended rate.
//! What: Fixture logs written to a tempdir, folded, and asserted field by
//! field.
//! Test: this file.

use super::*;

/// Write `lines` as the project's usage log and return the tempdir.
fn log_with(lines: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join(".trusty-agents").join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::write(state.join("usage.jsonl"), lines.join("\n")).expect("write log");
    dir
}

/// One JSONL row in the exact shape `append_usage` writes.
fn row(ts: &str, agent: &str, model: &str, input: u32, output: u32) -> String {
    format!(
        r#"{{"ts":"{ts}","agent":"{agent}","model":"{model}","runner":"openrouter","input_tokens":{input},"output_tokens":{output},"duration_ms":1000,"task_prefix":"t"}}"#
    )
}

/// Why: The aggregator and `append_usage` must read/write the same file, or
/// the Costs tab reports an empty log next to a populated one.
/// What: The resolved path matches the writer's documented location.
/// Test: this test.
#[test]
fn usage_log_path_matches_writer() {
    let p = usage_log_path(Path::new("/proj"));
    assert!(p.ends_with(".trusty-agents/state/usage.jsonl"), "got {p:?}");
}

/// Why (#4098): the core fold — totals must equal the sum of the breakdowns,
/// and each breakdown must equal the sum of its own rows, or the GUI shows
/// three mutually contradictory numbers.
/// What: Two agents, two models, two days; assert every bucket.
/// Test: this test.
#[test]
fn aggregate_folds_totals_and_breakdowns() {
    let dir = log_with(&[
        &row(
            "2026-07-27T10:00:00Z",
            "assistant",
            "anthropic/claude-sonnet-4-6",
            1_000_000,
            0,
        ),
        &row(
            "2026-07-27T11:00:00Z",
            "ctrl",
            "anthropic/claude-haiku-4",
            1_000_000,
            0,
        ),
        &row(
            "2026-07-28T09:00:00Z",
            "assistant",
            "anthropic/claude-sonnet-4-6",
            1_000_000,
            0,
        ),
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");

    assert_eq!(s.records, 3);
    assert_eq!(s.malformed_lines, 0);
    // Sonnet $3/M x2 + Haiku $0.80/M x1 = $6.80.
    assert!((s.totals.cost_usd - 6.80).abs() < 1e-6, "{s:?}");
    assert_eq!(s.totals.input_tokens, 3_000_000);
    assert_eq!(s.totals.dispatch_count, 3);
    assert_eq!(s.first_ts.as_deref(), Some("2026-07-27T10:00:00Z"));
    assert_eq!(s.last_ts.as_deref(), Some("2026-07-28T09:00:00Z"));

    // Every breakdown must sum back to the grand total.
    for rows in [&s.by_agent, &s.by_model, &s.by_date] {
        let sum: f64 = rows.iter().map(|r| r.cost_usd).sum();
        assert!(
            (sum - s.totals.cost_usd).abs() < 1e-6,
            "breakdown sums to {sum}, total is {}",
            s.totals.cost_usd
        );
        let count: u64 = rows.iter().map(|r| r.dispatch_count).sum();
        assert_eq!(count, s.totals.dispatch_count);
    }
}

/// Why (#4098, COST-05): a log spanning two models must price each row at its
/// own rate. Blending — pricing summed tokens once — was the exact class of bug
/// the retired Haiku table caused.
/// What: Equal token counts on Sonnet and Haiku produce unequal bucket costs.
/// Test: this test.
#[test]
fn aggregate_prices_per_row() {
    let dir = log_with(&[
        &row(
            "2026-07-27T10:00:00Z",
            "a",
            "claude-sonnet-4-6",
            1_000_000,
            0,
        ),
        &row("2026-07-27T10:01:00Z", "a", "claude-haiku-4", 1_000_000, 0),
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    let sonnet = s
        .by_model
        .iter()
        .find(|r| r.key == "claude-sonnet-4-6")
        .expect("sonnet bucket");
    let haiku = s
        .by_model
        .iter()
        .find(|r| r.key == "claude-haiku-4")
        .expect("haiku bucket");
    assert!((sonnet.cost_usd - 3.0).abs() < 1e-6, "{sonnet:?}");
    assert!((haiku.cost_usd - 0.80).abs() < 1e-6, "{haiku:?}");
    assert!(sonnet.cost_usd > haiku.cost_usd);
}

/// Why (#4098): grouping keys are what the legend renders, so agent and model
/// must not be conflated or transposed.
/// What: Two agents on one model group into two agent buckets, one model bucket.
/// Test: this test.
#[test]
fn aggregate_groups_by_agent_and_model() {
    let dir = log_with(&[
        &row("2026-07-27T10:00:00Z", "assistant", "m1", 100, 10),
        &row("2026-07-27T10:01:00Z", "ctrl", "m1", 100, 10),
        &row("2026-07-27T10:02:00Z", "ctrl", "m1", 100, 10),
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(s.by_agent.len(), 2);
    assert_eq!(s.by_model.len(), 1);
    let ctrl = s
        .by_agent
        .iter()
        .find(|r| r.key == "ctrl")
        .expect("ctrl bucket");
    assert_eq!(ctrl.dispatch_count, 2);
    assert_eq!(ctrl.input_tokens, 200);
    assert_eq!(ctrl.output_tokens, 20);
}

/// Why (#4098): a Costs view that renders a confident `$0.00` over a log that
/// does not exist is worse than one that says it has no data. This is the
/// single most important behavior in the module.
/// What: A project with no `.trusty-agents/state` yields `NotRecorded` carrying
/// the path, not an all-zero summary.
/// Test: this test.
#[test]
fn aggregate_reports_not_recorded_for_missing_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = aggregate_usage(dir.path(), None).expect_err("must not succeed");
    match err {
        AggregateError::NotRecorded { path } => {
            assert!(path.ends_with("usage.jsonl"), "got {path:?}");
        }
        other => panic!("expected NotRecorded, got {other:?}"),
    }
    assert!(
        aggregate_usage(dir.path(), None)
            .unwrap_err()
            .to_string()
            .contains("no usage log recorded")
    );
}

/// Why (#4098): an EXISTING but empty log is a different fact from a missing
/// one — the project has a state dir and simply has not dispatched yet. It must
/// aggregate cleanly to zero rows rather than erroring.
/// What: An empty file (and one of only blank lines) yields `records == 0` with
/// no error and no malformed count.
/// Test: this test.
#[test]
fn aggregate_empty_file_is_zero_rows_not_an_error() {
    let dir = log_with(&[]);
    let s = aggregate_usage(dir.path(), None).expect("empty log must aggregate");
    assert_eq!(s.records, 0);
    assert_eq!(s.malformed_lines, 0);
    assert_eq!(s.totals.cost_usd, 0.0);
    assert!(s.by_agent.is_empty() && s.by_model.is_empty() && s.by_date.is_empty());
    assert!(s.first_ts.is_none() && s.last_ts.is_none());

    let blanks = log_with(&["", "   ", ""]);
    let s = aggregate_usage(blanks.path(), None).expect("blank lines must aggregate");
    assert_eq!(s.records, 0);
    assert_eq!(s.malformed_lines, 0);
}

/// Why (#4098): the totals over a partially-unreadable log are INCOMPLETE, and
/// presenting a short total as a whole one is the failure the epic's honesty
/// requirement targets. Malformed lines are counted so the GUI can say so.
/// What: Truncated JSON, non-JSON text, a JSON non-object, and an object
/// missing a required attribution field are each skipped and counted; the valid
/// rows still fold.
/// Test: this test.
#[test]
fn aggregate_reports_malformed_lines() {
    let dir = log_with(&[
        &row("2026-07-27T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        r#"{"ts":"2026-07-27T10:01:00Z","agent":"a","#, // truncated
        "not json at all",
        "[1,2,3]",                                      // valid JSON, wrong shape
        r#"{"agent":"a","model":"m"}"#,                 // no ts — unplaceable
        r#"{"ts":"2026-07-27T10:02:00Z","model":"m"}"#, // no agent — unattributable
        &row("2026-07-27T10:03:00Z", "a", "claude-haiku-4", 1_000_000, 0),
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(s.records, 2, "only the two well-formed rows count");
    assert_eq!(s.malformed_lines, 5, "every unreadable line is reported");
    assert!((s.totals.cost_usd - 1.60).abs() < 1e-6, "{s:?}");
    assert_eq!(
        s.totals.dispatch_count, 2,
        "unreadable rows must not inflate the dispatch count"
    );
}

/// Why (#4098): a row written by an older binary lacks fields a newer one
/// writes. Those rows are real usage and must still count — only the three
/// attribution fields are load-bearing enough to reject a row over.
/// What: A row with just `ts`/`agent`/`model` parses with zeroed counts.
/// Test: this test.
#[test]
fn aggregate_tolerates_rows_missing_optional_fields() {
    let dir = log_with(&[r#"{"ts":"2026-07-27T10:00:00Z","agent":"a","model":"m"}"#]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(s.records, 1);
    assert_eq!(s.malformed_lines, 0);
    assert_eq!(s.totals.input_tokens, 0);
    assert_eq!(s.totals.cost_usd, 0.0);
}

/// Why (#4098): an empty `agent` is MISSING attribution, which the epic
/// requires be explicit. A blank legend entry reads as a rendering bug.
/// What: A row with an empty agent lands in an `(unattributed)` bucket.
/// Test: this test.
#[test]
fn aggregate_labels_missing_attribution_explicitly() {
    let dir = log_with(&[&row("2026-07-27T10:00:00Z", "", "claude-haiku-4", 100, 0)]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(s.by_agent[0].key, "(unattributed)");
}

/// Why (#4098): the date breakdown is the chart's X axis, and it must be UTC so
/// the same log renders identically regardless of the reader's timezone —
/// "rollups are reproducible" in the epic's Done-when.
/// What: Two stamps that fall on different local days but the same UTC day
/// group together.
/// Test: this test.
#[test]
fn aggregate_groups_by_date_in_utc() {
    let dir = log_with(&[
        &row("2026-07-27T23:30:00Z", "a", "m", 100, 0),
        &row("2026-07-27T20:30:00-04:00", "a", "m", 100, 0), // = 2026-07-28T00:30Z
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    let dates: Vec<&str> = s.by_date.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(dates, vec!["2026-07-27", "2026-07-28"], "{s:?}");
}

/// Why (#4098): `?days=N` narrows the report. Anchoring on the NEWEST RECORDED
/// row rather than wall-clock now means a log that stopped last week still
/// renders its final days instead of an honest-but-useless empty window.
/// What: `days=1` over a three-day log keeps only the newest day.
/// Test: this test.
#[test]
fn aggregate_window_filters_by_date() {
    let dir = log_with(&[
        &row("2026-07-26T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        &row("2026-07-27T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        &row("2026-07-28T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
    ]);
    let all = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(all.records, 3);
    assert_eq!(all.window_days, None);

    let one = aggregate_usage(dir.path(), Some(1)).expect("aggregate");
    assert_eq!(one.records, 1);
    assert_eq!(one.window_days, Some(1));
    assert_eq!(one.by_date.len(), 1);
    assert_eq!(one.by_date[0].key, "2026-07-28");

    let two = aggregate_usage(dir.path(), Some(2)).expect("aggregate");
    assert_eq!(two.records, 2, "a 2-day window is inclusive of both ends");
}

/// Why (#4098): see `aggregate_window_filters_by_date` — the anchor choice is
/// the surprising half, so it gets its own assertion.
/// What: A log whose newest row is long past still returns rows for `days=1`.
/// Test: this test.
#[test]
fn aggregate_window_anchors_on_newest_row() {
    let dir = log_with(&[&row("1999-01-01T10:00:00Z", "a", "m", 100, 0)]);
    let s = aggregate_usage(dir.path(), Some(1)).expect("aggregate");
    assert_eq!(s.records, 1, "a stale log must not window down to nothing");
}

/// Why (#4098): the GUI renders breakdowns in payload order, so the order is
/// part of the contract rather than an accident of the fold.
/// What: Agent/model rows descend by cost; date rows ascend by date.
/// Test: this test.
#[test]
fn aggregate_sorts_breakdowns() {
    let dir = log_with(&[
        &row("2026-07-28T10:00:00Z", "cheap", "claude-haiku-4", 1_000, 0),
        &row(
            "2026-07-26T10:00:00Z",
            "pricey",
            "claude-sonnet-4-6",
            1_000_000,
            0,
        ),
        &row(
            "2026-07-27T10:00:00Z",
            "middle",
            "claude-haiku-4",
            500_000,
            0,
        ),
    ]);
    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    let agents: Vec<&str> = s.by_agent.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(agents, vec!["pricey", "middle", "cheap"]);
    let dates: Vec<&str> = s.by_date.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(dates, vec!["2026-07-26", "2026-07-27", "2026-07-28"]);
}

/// Why (#4098): the aggregator must read back exactly what the writer emits —
/// a reader with its own DTO would drift silently. This is the end-to-end
/// proof that they share one shape.
/// What: `append_usage` writes two records; the aggregator folds both.
/// Test: this test.
#[tokio::test]
async fn aggregate_reads_what_append_usage_wrote() {
    let dir = tempfile::tempdir().expect("tempdir");
    let r1 = UsageRecord::new(
        "assistant",
        "claude-sonnet-4-6",
        "openrouter",
        1000,
        200,
        50,
        "a",
    );
    let r2 = UsageRecord::new("ctrl", "claude-haiku-4", "openrouter", 400, 100, 25, "b");
    super::super::append_usage(dir.path(), &r1).await;
    super::super::append_usage(dir.path(), &r2).await;

    let s = aggregate_usage(dir.path(), None).expect("aggregate");
    assert_eq!(s.records, 2);
    assert_eq!(s.malformed_lines, 0);
    assert_eq!(s.totals.input_tokens, 1400);
    assert_eq!(s.totals.output_tokens, 300);
    assert_eq!(s.totals.duration_ms, 75);
    assert_eq!(s.by_agent.len(), 2);
    assert!(s.totals.cost_usd > 0.0 && s.totals.cost_usd.is_finite());
}
