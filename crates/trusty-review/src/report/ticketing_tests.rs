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

/// Within a major this build reads, an added key is not a failure: that is what
/// every field's `#[serde(default)]` is for, and it keeps a tga that gained a
/// field from breaking a report that does not need it.
#[test]
fn an_artifact_with_unknown_keys_still_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(
        &path,
        r#"{"schema_version": "v0", "commits": 7, "future_key": true}"#,
    )
    .expect("write");

    let summary = load_ticketing(&path).expect("parses");
    assert_eq!(summary.commits, 7);
    assert_eq!(summary.commits_linked, 0);
    assert!(summary.sources.is_empty());
}

/// A newer MINOR of the same major is still readable — the added-field case
/// above, tagged. Refusing it would make every additive tga change a
/// coordinated release.
#[test]
fn an_artifact_with_a_newer_minor_of_a_known_major_still_parses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(
        &path,
        r#"{"schema_version": "v0.3", "commits": 412, "commits_linked": 260}"#,
    )
    .expect("write");

    let summary = load_ticketing(&path).expect("a newer minor of a known major must load");
    assert_eq!(summary.commits, 412);
    assert_eq!(summary.commits_linked, 260);
}

/// #5405: tga and trusty-review are installed independently, so version skew is
/// the normal case. An artifact from a major this build cannot read must fail
/// the same way an unparseable one does. Parsing it leniently would render
/// whatever `#[serde(default)]` produced — `0 of 0 commit(s)` — as a stated
/// fact in a document an acquirer prices a deal from.
#[test]
fn an_artifact_from_an_unknown_schema_major_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    // Well-formed JSON, every key this build knows, renamed counts it does not.
    std::fs::write(
        &path,
        r#"{"schema_version": "v9", "commits": 412, "linked_commit_count": 260}"#,
    )
    .expect("write");

    let err = load_ticketing(&path).expect_err("must refuse a major this build cannot read");
    let rendered = err.to_string();
    assert!(
        rendered.contains("ticketing.json") && rendered.contains("schema_version"),
        "the error must name the artifact and the version mismatch: {rendered}"
    );
    assert!(
        rendered.contains("v9"),
        "the error must quote the version it refused: {rendered}"
    );
}

/// Every tga that writes this artifact writes the tag, so an artifact without
/// one was not produced by a recognised producer. It is refused rather than
/// assumed to be v0 — assuming is how a truncated or hand-edited file renders
/// as zeroed counts.
#[test]
fn an_artifact_with_no_schema_version_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ticketing.json");
    std::fs::write(&path, r#"{"commits": 7, "commits_linked": 4}"#).expect("write");

    let err = load_ticketing(&path).expect_err("an untagged artifact must not degrade to zeros");
    assert!(
        err.to_string().contains("schema_version"),
        "the error must name the missing tag: {err}"
    );
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

/// #6192: a lap-14 grader re-derived the §8 commit total with `git log`, found
/// 2972 against 2909, and had no way to tell a structural gap from an error.
/// Both shapes of the line now state the mechanism.
#[test]
fn coverage_line_footnotes_the_commit_basis() {
    let linked = TicketingSummary {
        schema_version: "v0".into(),
        commits: 2972,
        commits_linked: 1200,
        work_items: 800,
        work_items_linked: 700,
        sources: vec!["github".into()],
    };
    let zero = TicketingSummary {
        commits_linked: 0,
        ..linked.clone()
    };

    for line in [linked.coverage_line(), zero.coverage_line()] {
        assert!(line.contains("Commit-count basis"), "{line}");
        assert!(line.contains("squash"), "{line}");
        assert!(line.contains("git log"), "{line}");
        assert!(
            line.contains("not expected to reconcile"),
            "the footnote must say both figures are correct: {line}"
        );
    }
}
