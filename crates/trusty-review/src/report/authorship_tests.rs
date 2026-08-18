use super::{AuthorshipSummary, load_authorship};

const ARTIFACT: &str = r#"{
  "schema_version": "v0",
  "repository": "acme-web",
  "distinct_authors": 3,
  "bus_factor": 1,
  "top_author_share_pct": 82.5,
  "single_author_subsystems": ["src", "scripts"],
  "monthly_trajectory": [
    {"month": "2026-01", "active_authors": 2, "commits": 10},
    {"month": "2026-02", "active_authors": 3, "commits": 15}
  ],
  "caveats": ["no mailmap"]
}"#;

#[test]
fn parses_the_artifact_tga_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("authorship.json");
    std::fs::write(&path, ARTIFACT).expect("write");

    let summary = load_authorship(&path).expect("parses");
    assert_eq!(summary.schema_version, "v0");
    assert_eq!(summary.repository, "acme-web");
    assert_eq!(summary.distinct_authors, 3);
    assert_eq!(summary.bus_factor, 1);
    assert!((summary.top_author_share_pct - 82.5).abs() < f64::EPSILON);
    assert_eq!(summary.single_author_subsystems, vec!["src", "scripts"]);
    assert_eq!(summary.monthly_trajectory.len(), 2);
    assert_eq!(summary.caveats, vec!["no mailmap".to_string()]);
}

#[test]
fn a_malformed_artifact_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("authorship.json");
    std::fs::write(&path, "{not json").expect("write");
    let err = load_authorship(&path).expect_err("must fail");
    assert!(matches!(
        err,
        crate::report::error::ReportError::Authorship { .. }
    ));
}

#[test]
fn an_artifact_from_an_unknown_schema_major_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("authorship.json");
    std::fs::write(&path, r#"{"schema_version": "v99"}"#).expect("write");
    let err = load_authorship(&path).expect_err("must fail");
    assert!(matches!(
        err,
        crate::report::error::ReportError::AuthorshipSchema { .. }
    ));
}

/// An absent `schema_version` is refused, not read as v0 — every tga that
/// writes this artifact writes the tag (mirrors ticketing's, not metrics',
/// behavior — see `load_authorship`'s doc comment).
#[test]
fn an_untagged_artifact_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("authorship.json");
    std::fs::write(&path, r#"{"repository": "acme-web"}"#).expect("write");
    let err = load_authorship(&path).expect_err("must fail");
    assert!(matches!(
        err,
        crate::report::error::ReportError::AuthorshipSchema { .. }
    ));
}

#[test]
fn trajectory_summary_labels_increasing() {
    let s: AuthorshipSummary = serde_json::from_str(ARTIFACT).expect("parse");
    let summary = s.trajectory_summary().expect("some trend");
    assert!(summary.contains("increasing"));
    assert!(summary.contains("2 month"));
}

#[test]
fn trajectory_summary_none_when_empty() {
    let s = AuthorshipSummary::default();
    assert!(s.trajectory_summary().is_none());
}
