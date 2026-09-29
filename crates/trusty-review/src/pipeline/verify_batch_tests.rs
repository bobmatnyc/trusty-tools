//! Tests for `verify_batch` (#8904): planning, request shape, answer parsing,
//! and per-file diff slicing.
//!
//! Test: this module.

use super::*;

fn finding(file: &str, effort: Effort, confidence: f32) -> Finding {
    Finding::new(file, "logic", "a bug", "fix it", confidence, effort)
}

#[test]
fn plan_batches_chunks_by_impact_and_caps_calls() {
    let findings = vec![
        finding("src/a.rs", Effort::Medium, 0.5),
        finding("src/b.rs", Effort::Medium, 0.7),
        finding("src/c.rs", Effort::Medium, 0.6),
        finding("src/d.rs", Effort::Medium, 0.8),
    ];
    let plan = plan_batches(&findings, &[0, 1, 2, 3], 3, 8);
    assert_eq!(plan.batches, vec![vec![3, 1, 2], vec![0]]);
    assert!(plan.over_cap.is_empty());

    let capped = plan_batches(&findings, &[0, 1, 2, 3], 3, 1);
    assert_eq!(capped.batches, vec![vec![3, 1, 2]]);
    assert_eq!(capped.over_cap, vec![0]);
}

#[test]
fn plan_batches_cuts_the_lowest_impact_findings() {
    let findings = vec![
        finding("src/a.rs", Effort::Low, 0.99),
        finding("src/b.rs", Effort::High, 0.60),
        finding("src/c.rs", Effort::Medium, 0.90),
    ];
    let plan = plan_batches(&findings, &[0, 1, 2], 1, 2);
    assert_eq!(plan.batches, vec![vec![1], vec![2]]);
    assert_eq!(plan.over_cap, vec![0], "the Low finding is cut first");
}

#[test]
fn file_diff_slice_keeps_only_the_named_files() {
    let diff = "diff --git a/src/a.rs b/src/a.rs\n+a\ndiff --git a/src/b.rs b/src/b.rs\n+b\n\
                diff --git a/src/c.rs b/src/c.rs\n+c\n";
    assert_eq!(
        file_diff_slice(diff, ["b/src/b.rs"]).as_deref(),
        Some("diff --git a/src/b.rs b/src/b.rs\n+b\n")
    );
    assert_eq!(
        file_diff_slice(diff, ["src/a.rs", "src/c.rs"]).as_deref(),
        Some("diff --git a/src/a.rs b/src/a.rs\n+a\ndiff --git a/src/c.rs b/src/c.rs\n+c\n")
    );
    assert_eq!(file_diff_slice(diff, ["src/a.rs", "src/zzz.rs"]), None);
}

#[test]
fn batch_request_numbers_every_finding() {
    let a = finding("src/a.rs", Effort::High, 0.9);
    let b = finding("src/b.rs", Effort::Low, 0.5);
    let req = build_batch_request("bedrock/m", "+ diff", &[&a, &b], None, None, None);
    let user = &req.messages[0].content;
    assert!(user.contains("### Finding 1\n- file: `src/a.rs`"), "{user}");
    assert!(user.contains("### Finding 2\n- file: `src/b.rs`"), "{user}");
    assert_eq!(req.model, "m");
    assert_eq!(req.max_tokens, 2 * VERIFY_MAX_TOKENS);
    assert_eq!(
        req.response_schema.as_ref().map(|s| s.name.as_str()),
        Some(VERIFY_BATCH_SCHEMA_NAME)
    );
}

#[test]
fn parse_batch_judgments_fails_closed_per_missing_finding() {
    let text = r#"{"judgments":[
        {"finding":1,"judgment":"confirmed","reason":"r"},
        {"finding":3,"judgment":"REFUTED","reason":"r"},
        {"finding":3,"judgment":"CONFIRMED","reason":"r"},
        {"finding":9,"judgment":"CONFIRMED","reason":"r"}
    ]}"#;
    assert_eq!(
        parse_batch_judgments(text, 3),
        vec![Some("CONFIRMED".to_string()), None, None],
        "2 is missing, 3 is contradicted, 9 is out of range"
    );
    assert_eq!(parse_batch_judgments("not json", 2), vec![None, None]);
}
