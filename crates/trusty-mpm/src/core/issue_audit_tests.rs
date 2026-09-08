//! Unit tests for the pure `tm issue audit` evaluation (#7097).
//!
//! Every case runs against fixture JSON shaped exactly like a live
//! `gh issue view --json` payload, so no test needs a network, a repository, or
//! an authenticated `gh`.

use super::*;
use crate::core::trusty_tools_config::{TrustyToolsConfig, resolve_ticketing};

/// The built-in standard: milestone and project both required.
fn standard() -> ResolvedTicketing {
    resolve_ticketing(&TrustyToolsConfig::default()).expect("the defaults resolve")
}

/// A real `gh issue view 7093 --json <AUDIT_JSON_FIELDS>` payload, captured
/// live on 2026-09-08. A compliant issue: milestone, project, component label.
const COMPLIANT: &str = r#"{
  "number": 7093,
  "milestone": {"title": "Backlog · mpm/core"},
  "projectItems": [{"title": "trusty-mpm"}],
  "labels": [{"name": "enhancement"}, {"name": "trusty-mpm"}],
  "comments": [],
  "state": "OPEN",
  "createdAt": "2026-09-08T00:30:20Z",
  "parent": null,
  "blockedBy": {"nodes": [], "totalCount": 0},
  "subIssues": {"nodes": [], "totalCount": 0}
}"#;

fn parse(json: &str) -> IssueFacts {
    serde_json::from_str(json).expect("the fixture parses")
}

fn row<'a>(audit: &'a IssueAudit, requirement: &str) -> &'a AuditRow {
    audit
        .rows
        .iter()
        .find(|r| r.requirement == requirement)
        .unwrap_or_else(|| panic!("the audit carries a `{requirement}` row"))
}

#[test]
fn facts_parse_a_live_view_payload() {
    let facts = parse(COMPLIANT);
    assert_eq!(facts.number, 7093);
    assert_eq!(
        facts.milestone.as_ref().map(|m| m.title.as_str()),
        Some("Backlog · mpm/core")
    );
    assert_eq!(facts.project_items.len(), 1);
    assert_eq!(facts.created_at, "2026-09-08T00:30:20Z");
}

#[test]
fn facts_parse_a_list_payload() {
    // #7097: `gh issue list --json` returns an ARRAY of the same objects, so
    // one shape must serve both fetches or a batch run would audit less.
    let facts: Vec<IssueFacts> =
        serde_json::from_str(&format!("[{COMPLIANT}]")).expect("the list fixture parses");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].number, 7093);
}

#[test]
fn json_fields_cover_every_audited_requirement() {
    // A field dropped from the list is a requirement that silently stops being
    // checked, so the list is asserted rather than trusted.
    for field in [
        "number",
        "milestone",
        "projectItems",
        "labels",
        "comments",
        "state",
        "createdAt",
        "parent",
        "blockedBy",
        "subIssues",
    ] {
        assert!(
            AUDIT_JSON_FIELDS.split(',').any(|f| f == field),
            "AUDIT_JSON_FIELDS must request `{field}`: {AUDIT_JSON_FIELDS}"
        );
    }
}

#[test]
fn a_compliant_issue_passes_every_requirement() {
    let audit = audit_issue(&parse(COMPLIANT), &standard());
    assert!(!audit.failed(), "{:?}", audit.rows);
    assert_eq!(row(&audit, "project").verdict, Verdict::Pass);
    assert_eq!(row(&audit, "milestone").verdict, Verdict::Pass);
    assert_eq!(row(&audit, "component label").verdict, Verdict::Pass);
    assert_eq!(row(&audit, "component label").detail, "trusty-mpm");
}

#[test]
fn a_missing_project_fails() {
    let json = COMPLIANT.replace(r#"[{"title": "trusty-mpm"}]"#, "[]");
    let audit = audit_issue(&parse(&json), &standard());
    assert!(audit.failed());
    assert_eq!(audit.failing_requirements(), vec!["project"]);
    assert!(
        row(&audit, "project").detail.contains("--add-project"),
        "the failure names the fix: {}",
        row(&audit, "project").detail
    );
}

#[test]
fn an_unset_milestone_without_a_comment_fails() {
    let json = COMPLIANT.replace(r#"{"title": "Backlog · mpm/core"}"#, "null");
    let audit = audit_issue(&parse(&json), &standard());
    assert_eq!(row(&audit, "milestone").verdict, Verdict::Fail);
    assert!(
        row(&audit, "milestone").detail.contains("no-milestone:"),
        "{}",
        row(&audit, "milestone").detail
    );
}

#[test]
fn an_unset_milestone_with_a_reason_comment_is_a_skip() {
    // #7097: SKIP, never PASS — a reader must be able to tell a waived
    // milestone from a set one, and the audit must still exit 0.
    let json = COMPLIANT
        .replace(r#"{"title": "Backlog · mpm/core"}"#, "null")
        .replace(
            r#""comments": []"#,
            r#""comments": [{"body": "no-milestone: spike, retired before the next release"}]"#,
        );
    let audit = audit_issue(&parse(&json), &standard());
    let milestone = row(&audit, "milestone");
    assert_eq!(milestone.verdict, Verdict::Skip);
    assert!(
        milestone
            .detail
            .contains("spike, retired before the next release"),
        "the reason is quoted: {}",
        milestone.detail
    );
    assert!(!audit.failed(), "a recorded waiver must not exit 1");
}

#[test]
fn a_skip_does_not_fail_the_audit() {
    let audit = IssueAudit {
        number: 1,
        rows: vec![AuditRow {
            requirement: "milestone",
            verdict: Verdict::Skip,
            detail: String::new(),
        }],
    };
    assert!(!audit.failed());
}

#[test]
fn the_no_milestone_comment_is_matched_case_insensitively() {
    let json = COMPLIANT
        .replace(r#"{"title": "Backlog · mpm/core"}"#, "null")
        .replace(
            r#""comments": []"#,
            r#""comments": [{"body": "No-Milestone: tracked upstream"}]"#,
        );
    let audit = audit_issue(&parse(&json), &standard());
    assert_eq!(row(&audit, "milestone").verdict, Verdict::Skip);
}

#[test]
fn a_no_milestone_note_inside_a_longer_comment_counts() {
    // An agent posting the note as one line of its filing report still
    // satisfies the standard; requiring a comment consisting ONLY of the note
    // would make the escape hatch depend on comment formatting.
    let json = COMPLIANT
        .replace(r#"{"title": "Backlog · mpm/core"}"#, "null")
        .replace(
            r#""comments": []"#,
            r#""comments": [{"body": "Filed per the standard.\nno-milestone: no release owns this yet\n"}]"#,
        );
    let audit = audit_issue(&parse(&json), &standard());
    let milestone = row(&audit, "milestone");
    assert_eq!(milestone.verdict, Verdict::Skip);
    assert!(
        milestone.detail.contains("no release owns this yet"),
        "{}",
        milestone.detail
    );
}

#[test]
fn an_unrequired_milestone_is_informational() {
    // #7097: the audit reports the operator's rules, never invents one. With
    // `milestone_required: false` an unset milestone is INFO, not FAIL.
    // Built from YAML rather than a struct literal so the test exercises the
    // same parse an operator's `config.yaml` goes through.
    let config: TrustyToolsConfig = serde_yaml::from_str(
        "agents:\n  ticketing:\n    milestone_required: false\n    project_required: false\n",
    )
    .expect("the relaxed block parses");
    let relaxed = resolve_ticketing(&config).expect("the relaxed block resolves");

    let json = COMPLIANT
        .replace(r#"{"title": "Backlog · mpm/core"}"#, "null")
        .replace(r#"[{"title": "trusty-mpm"}]"#, "[]");
    let audit = audit_issue(&parse(&json), &relaxed);
    assert_eq!(row(&audit, "milestone").verdict, Verdict::Info);
    assert_eq!(row(&audit, "project").verdict, Verdict::Info);
    assert!(!audit.failed());
}

#[test]
fn a_missing_component_label_fails() {
    // The observed #7103/#7104 shape: `enhancement` alone, no owning crate.
    let json = COMPLIANT.replace(
        r#"[{"name": "enhancement"}, {"name": "trusty-mpm"}]"#,
        r#"[{"name": "enhancement"}]"#,
    );
    let audit = audit_issue(&parse(&json), &standard());
    let component = row(&audit, "component label");
    assert_eq!(component.verdict, Verdict::Fail);
    assert!(
        component.detail.contains("trusty-mpm") && component.detail.contains("enhancement"),
        "the failure names both the accepted set and what was found: {}",
        component.detail
    );
}

#[test]
fn a_workstream_label_never_satisfies_the_component_rule() {
    // #7097: `ws/<session>` is a workstream label. Accepting it here would let
    // a session name stand in for the owning crate.
    let json = COMPLIANT.replace(
        r#"[{"name": "enhancement"}, {"name": "trusty-mpm"}]"#,
        r#"[{"name": "ws/trusty-tools-ec"}]"#,
    );
    let audit = audit_issue(&parse(&json), &standard());
    assert_eq!(row(&audit, "component label").verdict, Verdict::Fail);
}

#[test]
fn relationships_are_never_a_failure() {
    // A standalone issue legitimately has none, so their absence is reported
    // and never gates.
    let audit = audit_issue(&parse(COMPLIANT), &standard());
    let rel = row(&audit, "relationships");
    assert_eq!(rel.verdict, Verdict::Info);
    assert!(rel.detail.contains("no parent"), "{}", rel.detail);
    assert!(rel.detail.contains("blocked-by 0"), "{}", rel.detail);
    assert!(rel.detail.contains("sub-issues 0"), "{}", rel.detail);
}

#[test]
fn relationships_report_what_is_set() {
    let json = COMPLIANT
        .replace(r#""parent": null"#, r#""parent": {"number": 6918}"#)
        .replace(
            r#""blockedBy": {"nodes": [], "totalCount": 0}"#,
            r#""blockedBy": {"nodes": [{"number": 42}], "totalCount": 1}"#,
        )
        .replace(
            r#""subIssues": {"nodes": [], "totalCount": 0}"#,
            r#""subIssues": {"nodes": [], "totalCount": 3}"#,
        );
    let audit = audit_issue(&parse(&json), &standard());
    let rel = row(&audit, "relationships");
    assert_eq!(rel.verdict, Verdict::Info);
    assert!(rel.detail.contains("parent #6918"), "{}", rel.detail);
    assert!(rel.detail.contains("blocked-by 1"), "{}", rel.detail);
    // `totalCount` over `nodes.len()` — a truncated page must not read as 0.
    assert!(rel.detail.contains("sub-issues 3"), "{}", rel.detail);
}

#[test]
fn render_audit_prints_one_line_per_requirement() {
    let text = render_audit(&audit_issue(&parse(COMPLIANT), &standard()));
    assert!(text.starts_with("#7093\n"), "{text}");
    for requirement in ["project", "milestone", "component label", "relationships"] {
        assert!(
            text.lines()
                .any(|l| l.trim_start().starts_with(requirement)),
            "a `{requirement}` line renders: {text}"
        );
    }
    assert_eq!(text.lines().count(), 5, "{text}");
}

#[test]
fn render_summary_tabulates_every_issue() {
    let compliant = audit_issue(&parse(COMPLIANT), &standard());
    let broken = audit_issue(
        &parse(&COMPLIANT.replace(r#"[{"title": "trusty-mpm"}]"#, "[]")),
        &standard(),
    );
    let text = render_summary(&[compliant, broken]);
    assert!(text.contains("issue"), "{text}");
    assert_eq!(text.matches("#7093").count(), 2, "{text}");
    assert!(text.contains("2 issue(s) audited, 1 with a FAIL"), "{text}");
}

#[test]
fn render_summary_counts_only_failures() {
    let audits = vec![audit_issue(&parse(COMPLIANT), &standard())];
    let text = render_summary(&audits);
    assert!(text.contains("1 issue(s) audited, 0 with a FAIL"), "{text}");
}

#[test]
fn the_since_window_keeps_the_boundary_day() {
    let facts = parse(COMPLIANT);
    assert!(created_on_or_after(&facts, "2026-09-08"));
    assert!(created_on_or_after(&facts, "2026-09-01"));
    assert!(!created_on_or_after(&facts, "2026-09-09"));
}

#[test]
fn the_since_window_keeps_an_issue_with_no_timestamp() {
    // Dropping it would silently shrink the audited set, which is the failure
    // mode this whole command exists to catch.
    let facts = IssueFacts::default();
    assert!(created_on_or_after(&facts, "2026-09-08"));
}
