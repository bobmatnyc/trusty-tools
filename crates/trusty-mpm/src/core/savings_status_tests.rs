//! Tests for the `tm savings status` fold (#11033).

use super::*;
use chrono::NaiveDate;

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date")
}

fn write(path: &Path, lines: &[&str]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, lines.join("\n")).expect("write");
}

#[allow(clippy::too_many_arguments)] // a row-literal builder
fn opp(
    ts: &str,
    sid: &str,
    event: &str,
    tool_type: &str,
    tool: &str,
    ret: u64,
    would: u64,
    reason: &str,
) -> String {
    format!(
        r#"{{"ts":"{ts}","session_id":"{sid}","event":"{event}","tool_type":"{tool_type}","tool_name":"{tool}","bytes_returned":{ret},"bytes_scanned":{ret},"would_compress_bytes":{would},"skip_reason":"{reason}"}}"#
    )
}

/// Fixture: two opportunity rows (bash, two skip reasons), one tagged compress
/// row, one old-format compress row with no `tokens_before`/`tool_type`, one
/// non-compress row, a marker row and a torn line.
fn fixture(root: &Path) {
    let a = opp(
        "2026-11-19T01:00:00Z",
        "s1",
        "PostToolUse",
        "bash",
        "cargo",
        4_000,
        1_000,
        "compressed",
    );
    let b = opp(
        "2026-11-19T02:00:00Z",
        "s2",
        "PostToolUse",
        "bash",
        "for i",
        800,
        800,
        "chained",
    );
    let c = opp(
        "2026-11-19T03:00:00Z",
        "s2",
        "PostToolUse",
        "bash",
        "git log",
        400,
        400,
        "chained",
    );
    write(
        &opportunity_dir_in(root).join("2026-11-19.jsonl"),
        &[
            &a,
            &b,
            &c,
            r#"{"ts":"t","event":"day_full","day_cap_bytes":8}"#,
            "{torn",
        ],
    );
    write(
        &savings_log_in(root),
        &[
            r#"{"ts":"2026-11-19T01:00:01Z","session_id":"s1","technique":"compress","tokens_saved":750,"tokens_before":1000,"cost_saved_usd":0.0,"basis":"x","model_source":"statusline","tool_type":"bash","tool_name":"cargo"}"#,
            r#"{"ts":"2026-11-19T05:00:00Z","session_id":"s9","technique":"compress","tokens_saved":40,"cost_saved_usd":0.0,"basis":"old"}"#,
            r#"{"ts":"2026-11-19T06:00:00Z","session_id":"s1","technique":"instruction-compression","tokens_saved":99,"tokens_before":500,"cost_saved_usd":0.0,"basis":"x"}"#,
            r#"{"ts":"2026-10-01T00:00:00Z","session_id":"s1","technique":"compress","tokens_saved":5,"tokens_before":9,"cost_saved_usd":0.0,"basis":"out of window","tool_type":"bash"}"#,
        ],
    );
}

#[test]
fn fold_reports_seen_compressed_saved_and_skip_split() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let report = fold_status(dir.path(), GroupBy::Tool, 2, day("2026-11-20"));
    assert_eq!(report.days.len(), 2);
    assert_eq!(report.days[0].day, "2026-11-20");
    assert!(report.days[0].groups.is_empty());
    let d = &report.days[1];
    let bash = &d.groups["bash"];
    assert_eq!(bash.tokens_seen, 1_300, "5200 B / 4");
    assert_eq!(bash.would_save_tokens_upper_bound, 750, "3000 B / 4");
    assert_eq!(bash.compress_rows, 1);
    assert_eq!(bash.tokens_compressed, 1_000);
    assert_eq!(bash.tokens_saved, 750);
    let share = bash.share_compressed_pct.expect("share");
    assert!((share - 76.92).abs() < 0.01, "{share}");
    assert_eq!(bash.skip_reasons["chained"], 2);
    assert_eq!(bash.skip_reasons["compressed"], 1);
    assert_eq!(d.compress_sessions, 2, "s1 and s9");
    assert_eq!(d.opportunity_sessions, 2, "s1 and s2");
}

#[test]
fn old_format_savings_row_folds_as_unattributed() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let report = fold_status(dir.path(), GroupBy::Tool, 2, day("2026-11-20"));
    let old = &report.days[1].groups[UNATTRIBUTED];
    assert_eq!(old.compress_rows, 1);
    assert_eq!(old.tokens_saved, 40);
    assert_eq!(old.tokens_compressed, 0);
    assert_eq!(report.unattributed_rows, 1);
}

#[test]
fn dropped_marker_marks_the_day_undercounted() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    std::fs::write(
        opportunity_dir_in(dir.path()).join("2026-11-19.dropped"),
        "...",
    )
    .expect("w");
    let report = fold_status(dir.path(), GroupBy::Tool, 2, day("2026-11-20"));
    assert!(report.days[1].undercounted);
    assert_eq!(report.days[1].dropped_rows, 3);
    assert!(!report.days[0].undercounted);
    assert!(render_text(&report).contains("UNDERCOUNTED (3 rows dropped)"));
}

#[test]
fn subagent_grouping_uses_event_class() {
    let dir = tempfile::tempdir().expect("tmp");
    let a = opp(
        "2026-11-19T01:00:00Z",
        "s1",
        "SubagentStop",
        "subagent",
        "SubagentStop",
        400,
        400,
        "no_gain",
    );
    let b = opp(
        "2026-11-19T01:00:00Z",
        "s1",
        "PostToolUse",
        "subagent",
        "Agent",
        400,
        400,
        "no_gain",
    );
    let c = opp(
        "2026-11-19T01:00:00Z",
        "s2",
        "PostToolUse",
        "bash",
        "ls",
        400,
        400,
        "no_gain",
    );
    write(
        &opportunity_dir_in(dir.path()).join("2026-11-19.jsonl"),
        &[&a, &b, &c],
    );
    let report = fold_status(dir.path(), GroupBy::Subagent, 1, day("2026-11-19"));
    let groups: Vec<&str> = report.days[0].groups.keys().map(String::as_str).collect();
    assert_eq!(groups, ["Agent PostToolUse", "SubagentStop", "other"]);
}

#[test]
fn render_labels_upper_bound_and_undercount() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let text = render_text(&fold_status(
        dir.path(),
        GroupBy::Tool,
        2,
        day("2026-11-20"),
    ));
    assert!(text.contains("UPPER BOUND"), "{text}");
    assert!(
        text.contains("2 wrote compress rows / 2 in opportunity ledger"),
        "{text}"
    );
    assert!(text.contains("chained=2 compressed=1"), "{text}");
    assert!(text.contains("predate tool_type"), "{text}");
}

#[test]
fn compress_rows_per_day_counts_only_compress_rows() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let series = compress_rows_per_day(dir.path(), day("2026-11-20"), 3).expect("read");
    assert_eq!(
        series,
        vec![
            ("2026-11-18".to_string(), 0),
            ("2026-11-19".to_string(), 2),
            ("2026-11-20".to_string(), 0)
        ]
    );
}
