//! Tests for the ticketing artifact loader and its rendered line (#5405).

use super::{TicketingSummary, load_ticketing};

/// The exact bytes `tga::report::ticketing::TicketingSummary::to_json` writes.
/// Its mirror lives in
/// `trusty-git-analytics/src/report/ticketing_tests.rs::round_trips_through_the_review_schema`;
/// the two crates have no Cargo edge, so this literal is the contract.
const ARTIFACT: &str = r#"{
  "schema_version": "v0",
  "commits": 412,
  "commits_linked": 260,
  "work_items": 180,
  "work_items_linked": 155,
  "sources": [
    "jira",
    "linear"
  ]
}"#;

#[test]
fn parses_the_artifact_tga_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(&path, ARTIFACT).expect("write");

    let summary = load_ticketing(&path).expect("parses");
    assert_eq!(summary.schema_version, "v0");
    assert_eq!(summary.commits, 412);
    assert_eq!(summary.commits_linked, 260);
    assert_eq!(summary.work_items, 180);
    assert_eq!(summary.work_items_linked, 155);
    assert_eq!(summary.sources, vec!["jira".to_string(), "linear".into()]);
}

/// Every field defaults, so an artifact from a different tga release still
/// parses instead of failing the whole report over a key this build has not
/// heard of.
#[test]
fn an_artifact_with_unknown_or_missing_keys_still_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(&path, r#"{"commits": 7, "future_key": true}"#).expect("write");

    let summary = load_ticketing(&path).expect("parses");
    assert_eq!(summary.commits, 7);
    assert_eq!(summary.commits_linked, 0);
    assert!(summary.sources.is_empty());
}

/// A declared artifact that will not parse is a producer bug, so it is a named
/// error rather than a quietly empty section — the failure mode #5405 is about.
#[test]
fn a_malformed_artifact_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(&path, "not json at all").expect("write");

    let err = load_ticketing(&path).expect_err("must not degrade to a default");
    let rendered = err.to_string();
    assert!(
        rendered.contains("ticketing") && rendered.contains("ticketing.json"),
        "the error must name what failed and where: {rendered}"
    );
}

#[test]
fn coverage_line_states_counts_and_sources() {
    let summary = TicketingSummary {
        schema_version: "v0".into(),
        commits: 412,
        commits_linked: 260,
        work_items: 180,
        work_items_linked: 155,
        sources: vec!["jira".into(), "linear".into()],
    };

    let line = summary.coverage_line();
    assert!(line.contains("260 of 412 commit(s)"), "{line}");
    assert!(line.contains("155 of 180 synced board item(s)"), "{line}");
    assert!(line.contains("jira, linear"), "{line}");
    // The owner deferred the linkage-quality signal to the board-selection
    // axis; nothing here may present a ratio as a grade.
    for forbidden in ["%", "grade", "score", "GOOD", "POOR"] {
        assert!(
            !line.contains(forbidden),
            "the line must state counts, not a calibrated judgement: {line}"
        );
    }
}

/// A run that linked nothing states that in full. It is a finding about the
/// codebase — commits do not cite tracked work — and must never be renderable
/// as the same blank the missing-artifact case produces.
#[test]
fn coverage_line_states_a_zero_run() {
    let summary = TicketingSummary {
        schema_version: "v0".into(),
        commits: 300,
        commits_linked: 0,
        work_items: 12,
        work_items_linked: 0,
        sources: Vec::new(),
    };

    let line = summary.coverage_line();
    assert!(
        line.contains("No commit referenced a tracked board item"),
        "{line}"
    );
    assert!(line.contains("300 commit(s)"), "{line}");
    assert!(line.contains("12 synced board item(s)"), "{line}");
}
