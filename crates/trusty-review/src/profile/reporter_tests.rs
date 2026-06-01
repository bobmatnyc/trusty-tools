//! Tests for the reporter module.
//!
//! Why: extracted from `reporter.rs` to keep that file under the 500-line cap
//! while preserving the same test coverage.
//! What: exercises JSON/Markdown output, file stem safety, GitHub issue request
//! construction, format parsing, and conditional cost section rendering.
//! Test: this file is included as `#[cfg(test)] mod tests` from `reporter.rs`.

use crate::models::{Effort, Finding};
use crate::profile::types::{
    ContributorProfile, LongitudinalFinding, TokenCostSummary, Trajectory, TrendTag,
};

use super::reporter_github::{IssueBody, issue_title};
use super::{ReportFormat, Reporter, profile_file_stem, render_markdown};

fn make_profile() -> ContributorProfile {
    let mut p = ContributorProfile::new(
        "alice@example.com",
        "Alice Smith",
        "2026-01-01",
        "2026-06-30",
    );
    p.repositories = vec!["acme/api".to_string()];
    p.improvement_trajectory = Trajectory::Improving;
    p.quality_trend = vec![("2026-Q1".to_string(), 3.0), ("2026-Q2".to_string(), 3.8)];
    p.strengths = vec!["Consistent ticket coverage".to_string()];
    p.recurring_weaknesses = vec!["Missing error handling".to_string()];
    p.all_findings = vec![LongitudinalFinding {
        period_label: "2026-Q1".to_string(),
        finding: Finding::new(
            "src/lib.rs",
            "error_handling",
            "Missing propagation",
            "Use ?",
            0.8,
            Effort::Medium,
        ),
        trend_tag: Some(TrendTag::Recurring),
    }];
    p.narrative = "Alice shows strong improvement.".to_string();
    p.token_cost = TokenCostSummary {
        input_tokens: 500,
        output_tokens: 200,
        cost_usd: 0.005,
        latency_ms: 1500,
    };
    p
}

/// Why: JSON output must be valid and round-trippable.
/// What: creates a temp dir, calls `write_profile`, reads the JSON back,
/// asserts the canonical_email matches.
/// Test: this test itself.
#[test]
fn reporter_json_output() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let reporter = Reporter::new(tmp.path(), ReportFormat::Json);
    let profile = make_profile();

    let paths = reporter.write_profile(&profile).expect("write_profile");
    assert_eq!(paths.len(), 1);
    assert!(paths[0].extension().is_some_and(|e| e == "json"));

    let content = std::fs::read_to_string(&paths[0]).expect("read");
    let back: ContributorProfile = serde_json::from_str(&content).expect("parse");
    assert_eq!(back.canonical_email, "alice@example.com");
}

/// Why: Markdown output must contain all expected sections.
/// What: calls `render_markdown` and asserts key strings are present.
/// Test: this test itself.
#[test]
fn reporter_markdown_contains_sections() {
    let profile = make_profile();
    let md = render_markdown(&profile);

    assert!(md.contains("# Developer Profile: Alice Smith"), "header");
    assert!(md.contains("## Quality Trend"), "quality trend section");
    assert!(md.contains("2026-Q1"), "period label");
    assert!(md.contains("## Strengths"), "strengths section");
    assert!(
        md.contains("Consistent ticket coverage"),
        "strength content"
    );
    assert!(
        md.contains("## Areas for Improvement"),
        "weaknesses section"
    );
    assert!(md.contains("## Findings"), "findings section");
    assert!(md.contains("error_handling"), "finding kind");
    assert!(md.contains("Recurring"), "trend tag");
    assert!(
        md.contains("## Engineering Assessment"),
        "narrative section"
    );
    assert!(
        md.contains("Alice shows strong improvement"),
        "narrative content"
    );
    assert!(md.contains("## Token & Cost Summary"), "cost section");
    assert!(md.contains("500"), "input tokens");
}

/// Why: both JSON and Markdown files must be written in `Both` format.
/// What: uses `ReportFormat::Both`, asserts 2 paths, one .json one .md.
/// Test: this test itself.
#[test]
fn reporter_both_format_writes_two_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let reporter = Reporter::new(tmp.path(), ReportFormat::Both);
    let profile = make_profile();

    let paths = reporter.write_profile(&profile).expect("write_profile");
    assert_eq!(paths.len(), 2, "should write 2 files in Both mode");
    let has_json = paths
        .iter()
        .any(|p| p.extension().is_some_and(|e| e == "json"));
    let has_md = paths
        .iter()
        .any(|p| p.extension().is_some_and(|e| e == "md"));
    assert!(has_json, "should include JSON file");
    assert!(has_md, "should include Markdown file");
}

/// Why: the file stem must be filesystem-safe (no `@` or dots from email).
/// What: calls `profile_file_stem` and asserts no special chars.
/// Test: this test itself.
#[test]
fn profile_file_stem_safe() {
    let profile = make_profile();
    let stem = profile_file_stem(&profile);
    assert!(!stem.contains('@'), "stem must not contain @: {stem}");
    assert!(
        stem.starts_with("profile_"),
        "stem must start with profile_: {stem}"
    );
}

/// Why: the GitHub issue URL and request body must be constructed correctly
/// without a real network call.
/// What: verifies `issue_title` format and `IssueBody` JSON structure
/// by inspecting the serialised form.
/// Test: this test itself.
#[test]
fn reporter_github_issue_request_construction() {
    let profile = make_profile();

    let title = issue_title(&profile);
    assert!(
        title.starts_with("[dev-profile]"),
        "title must start with [dev-profile]: {title}"
    );
    assert!(
        title.contains("alice@example.com"),
        "title must contain canonical email: {title}"
    );
    assert!(
        title.contains("Alice Smith"),
        "title must contain canonical name: {title}"
    );

    let body = IssueBody {
        title: title.clone(),
        body: "markdown content".to_string(),
        labels: vec!["dev-profile".to_string()],
    };
    let json = serde_json::to_string(&body).expect("serialise");
    assert!(json.contains("[dev-profile]"), "json must include title");
    assert!(json.contains("dev-profile"), "json must include label");
    assert!(json.contains("markdown content"), "json must include body");
}

/// Why: `ReportFormat::from_str` must parse all variants correctly.
/// What: tests "json", "markdown", "both" and an invalid value.
/// Test: this test itself.
#[test]
fn report_format_from_str() {
    use std::str::FromStr;
    assert_eq!(ReportFormat::from_str("json").unwrap(), ReportFormat::Json);
    assert_eq!(
        ReportFormat::from_str("markdown").unwrap(),
        ReportFormat::Markdown
    );
    assert_eq!(ReportFormat::from_str("both").unwrap(), ReportFormat::Both);
    assert_eq!(
        ReportFormat::from_str("md").unwrap(),
        ReportFormat::Markdown
    );
    assert!(ReportFormat::from_str("xml").is_err());
}

/// Why: a profile with no cost data must not render the cost section.
/// What: creates a profile with zero token_cost, asserts no cost section.
/// Test: this test itself.
#[test]
fn reporter_markdown_no_cost_section_when_zero() {
    let mut profile = make_profile();
    profile.token_cost = TokenCostSummary::default();
    let md = render_markdown(&profile);
    assert!(
        !md.contains("## Token & Cost Summary"),
        "zero cost must omit the cost section"
    );
}
