//! Unit tests for `commands::calibrate`.
//!
//! Why: verifies the fuzzy matcher, metrics computation, corpus loading, and
//! the rust_semantic_fp_rate computation — all without hitting a real LLM or
//! GitHub connection.
//! What: synthetic 3-PR corpus fixture with known ground truth; assertions on
//! recall/precision values and false-positive lists.
//! Test: this is the test file.

use super::*;
use trusty_review::models::{Effort, Finding};

// ─── Shared fixture ───────────────────────────────────────────────────────────

/// Build a synthetic `Finding` for testing.
fn make_finding(file: &str, kind: &str) -> Finding {
    Finding::new(
        file,
        kind,
        format!("test description for {file}"),
        String::new(),
        0.8_f32,
        Effort::Low,
    )
}

/// Build a `HumanFinding` for testing.
fn make_human(file: &str, kind: &str, acted_on: bool) -> HumanFinding {
    HumanFinding {
        file: file.to_string(),
        kind: kind.to_string(),
        acted_on,
    }
}

// ─── Fuzzy matcher tests ──────────────────────────────────────────────────────

#[test]
fn finding_recalled_same_file_and_kind() {
    let tf = make_finding("src/lib.rs", "logic-error");
    let hf = make_human("src/lib.rs", "logic-error", true);
    assert!(is_recalled(&tf, &hf));
}

#[test]
fn finding_recalled_kind_case_insensitive() {
    let tf = make_finding("src/lib.rs", "Logic-Error");
    let hf = make_human("src/lib.rs", "logic-error", true);
    assert!(is_recalled(&tf, &hf));
}

#[test]
fn finding_not_recalled_different_kind() {
    let tf = make_finding("src/lib.rs", "logic-error");
    let hf = make_human("src/lib.rs", "ownership", true);
    assert!(!is_recalled(&tf, &hf));
}

#[test]
fn finding_not_recalled_different_file() {
    let tf = make_finding("src/lib.rs", "logic-error");
    let hf = make_human("src/main.rs", "logic-error", true);
    assert!(!is_recalled(&tf, &hf));
}

// ─── Rust semantic classifier ─────────────────────────────────────────────────

#[test]
fn rust_semantic_classifier_logic_error_rs() {
    let f = make_finding("src/runner.rs", "logic-error");
    assert!(is_rust_semantic(&f));
}

#[test]
fn rust_semantic_classifier_ownership_rs() {
    let f = make_finding("crates/foo/src/bar.rs", "ownership");
    assert!(is_rust_semantic(&f));
}

#[test]
fn rust_semantic_classifier_not_rs_file() {
    let f = make_finding("src/lib.py", "logic-error");
    assert!(!is_rust_semantic(&f));
}

#[test]
fn rust_semantic_classifier_rs_but_wrong_kind() {
    let f = make_finding("src/lib.rs", "security");
    assert!(!is_rust_semantic(&f));
}

// ─── Metrics computation ──────────────────────────────────────────────────────

/// Synthetic 3-PR corpus with known ground truth.
///
/// PR 1: 2 human findings, trusty recalls both → recall=1.0, precision=1.0
/// PR 2: 2 human findings, trusty recalls 1 + 1 FP → recall=0.5, precision=0.5
/// PR 3: 1 human finding (Rust/ownership), trusty emits 0 findings → recall=0.0, precision=1.0
fn three_pr_corpus() -> (Vec<CorpusEntry>, Vec<Vec<Finding>>) {
    let corpus = vec![
        CorpusEntry {
            owner: "acme".to_string(),
            repo: "api".to_string(),
            pr: 101,
            human_findings: vec![
                make_human("src/auth.rs", "security", true),
                make_human("src/db.rs", "logic-error", true),
            ],
        },
        CorpusEntry {
            owner: "acme".to_string(),
            repo: "api".to_string(),
            pr: 102,
            human_findings: vec![
                make_human("src/lib.rs", "ownership", true),
                make_human("src/handler.rs", "security", false),
            ],
        },
        CorpusEntry {
            owner: "acme".to_string(),
            repo: "api".to_string(),
            pr: 103,
            human_findings: vec![make_human("src/runner.rs", "ownership", true)],
        },
    ];

    let trusty_results = vec![
        // PR 101: both human findings recalled, no FPs
        vec![
            make_finding("src/auth.rs", "security"),
            make_finding("src/db.rs", "logic-error"),
        ],
        // PR 102: 1 recalled (ownership), 1 FP (wrong kind on same file), 1 human not recalled
        vec![
            make_finding("src/lib.rs", "ownership"),   // recalled
            make_finding("src/lib.rs", "logic-error"), // FP (no human logic-error there)
        ],
        // PR 103: no trusty findings → recall=0.0, precision=1.0
        vec![],
    ];

    (corpus, trusty_results)
}

#[test]
fn compute_metrics_three_pr_corpus() {
    let (corpus, trusty_results) = three_pr_corpus();
    let report = compute_metrics(&corpus, &trusty_results);

    // PR 101: recall=2/2=1.0, precision=2/2=1.0
    assert!(
        (report.per_pr[0].recall - 1.0).abs() < 1e-9,
        "PR 101 recall expected 1.0, got {}",
        report.per_pr[0].recall
    );
    assert!(
        (report.per_pr[0].precision - 1.0).abs() < 1e-9,
        "PR 101 precision expected 1.0, got {}",
        report.per_pr[0].precision
    );
    assert!(
        report.per_pr[0].false_positives.is_empty(),
        "PR 101 should have no FPs"
    );

    // PR 102: recall=1/2=0.5, precision=1/2=0.5; 1 FP
    assert!(
        (report.per_pr[1].recall - 0.5).abs() < 1e-9,
        "PR 102 recall expected 0.5, got {}",
        report.per_pr[1].recall
    );
    assert!(
        (report.per_pr[1].precision - 0.5).abs() < 1e-9,
        "PR 102 precision expected 0.5, got {}",
        report.per_pr[1].precision
    );
    assert_eq!(
        report.per_pr[1].false_positives.len(),
        1,
        "PR 102 should have 1 FP"
    );
    assert_eq!(report.per_pr[1].false_positives[0].file, "src/lib.rs");
    assert_eq!(report.per_pr[1].false_positives[0].kind, "logic-error");

    // PR 103: recall=0/1=0.0, precision=1.0 (no trusty findings)
    assert!(
        (report.per_pr[2].recall - 0.0).abs() < 1e-9,
        "PR 103 recall expected 0.0, got {}",
        report.per_pr[2].recall
    );
    assert!(
        (report.per_pr[2].precision - 1.0).abs() < 1e-9,
        "PR 103 precision expected 1.0, got {}",
        report.per_pr[2].precision
    );

    // Aggregate: total_human=5 (2+2+1), total_recalled=3 (2 from PR101 + 1 from PR102 + 0 from PR103)
    // total_trusty=4 (2+2+0)
    // recall=3/5=0.6, precision=3/4=0.75
    assert!(
        (report.recall - 0.6).abs() < 1e-9,
        "aggregate recall expected 0.6, got {}",
        report.recall
    );
    assert!(
        (report.precision - 0.75).abs() < 1e-9,
        "aggregate precision expected 0.75, got {}",
        report.precision
    );
}

#[test]
fn rust_semantic_fp_rate_computed_correctly() {
    let (corpus, trusty_results) = three_pr_corpus();
    let report = compute_metrics(&corpus, &trusty_results);

    // Rust semantic findings (kind=logic-error|ownership on .rs files):
    // PR 101 trusty: src/db.rs/logic-error → recalled (human has it) → +1 recalled, +1 total
    // PR 102 trusty: src/lib.rs/ownership → recalled; src/lib.rs/logic-error → FP → +1 recalled, +2 total
    // PR 103 trusty: none
    // Total Rust semantic: 3; recalled: 2; FP: 1 (lib.rs/logic-error)
    // rust_semantic_fp_rate = 1 - 2/3 = 1/3 ≈ 0.333 (TRUE FP rate; lower is better)
    assert!(
        (report.rust_semantic_fp_rate - (1.0_f64 / 3.0_f64)).abs() < 1e-9,
        "rust_semantic_fp_rate expected 1/3 (true FP rate), got {}",
        report.rust_semantic_fp_rate
    );
}

#[test]
fn rust_semantic_fp_rate_neutral_when_no_rust_semantic_findings() {
    let corpus = vec![CorpusEntry {
        owner: "x".to_string(),
        repo: "y".to_string(),
        pr: 1,
        human_findings: vec![],
    }];
    // Only a Python-file finding (not rust-semantic)
    let results = vec![vec![make_finding("src/main.py", "logic-error")]];
    let report = compute_metrics(&corpus, &results);
    assert!(
        (report.rust_semantic_fp_rate - 0.0).abs() < 1e-9,
        "expected neutral 0.0 (no FPs) when no Rust semantic findings, got {}",
        report.rust_semantic_fp_rate
    );
}

// ─── Recall cap test ──────────────────────────────────────────────────────────

/// Verify recall stays ≤ 1.0 when multiple trusty findings match one human finding.
///
/// Why: if we counted trusty-side matches for recall (instead of distinct human
/// findings), recall could exceed 1.0 when two trusty findings both match the
/// same human finding — making the metric meaningless.  This test guards that.
/// What: 1 human finding, 2 trusty findings both matching it → recall must be 1.0
/// (one distinct human finding recalled), not 2.0 (two trusty hits).
/// Test: asserts recall == 1.0 and precision == 1.0 (both trusty matched).
#[test]
fn recall_does_not_exceed_one_when_two_trusty_match_same_human_finding() {
    let corpus = vec![CorpusEntry {
        owner: "acme".to_string(),
        repo: "api".to_string(),
        pr: 200,
        human_findings: vec![make_human("src/lib.rs", "logic-error", true)],
    }];
    // Two trusty findings that BOTH match the single human finding.
    let results = vec![vec![
        make_finding("src/lib.rs", "logic-error"),
        make_finding("src/lib.rs", "logic-error"),
    ]];
    let report = compute_metrics(&corpus, &results);
    // Recall: 1 distinct human finding recalled / 1 total → 1.0 (not 2.0)
    assert!(
        (report.per_pr[0].recall - 1.0).abs() < 1e-9,
        "recall expected 1.0 (not > 1.0), got {}",
        report.per_pr[0].recall
    );
    assert!(
        report.per_pr[0].recall <= 1.0,
        "recall must never exceed 1.0, got {}",
        report.per_pr[0].recall
    );
    // Precision: both trusty findings matched → 2/2 = 1.0
    assert!(
        (report.per_pr[0].precision - 1.0).abs() < 1e-9,
        "precision expected 1.0, got {}",
        report.per_pr[0].precision
    );
    // No false positives
    assert!(
        report.per_pr[0].false_positives.is_empty(),
        "no false positives expected"
    );
}

// ─── Corpus loader ────────────────────────────────────────────────────────────

#[test]
fn load_corpus_roundtrip() {
    let tmpdir = tempfile::tempdir().expect("tmpdir");
    let path = tmpdir.path().join("corpus.jsonl");

    // Write two entries
    let lines = [
        r#"{"owner":"acme","repo":"api","pr":1,"human_findings":[{"file":"src/lib.rs","kind":"logic-error","acted_on":true}]}"#,
        r#"{"owner":"acme","repo":"api","pr":2,"human_findings":[]}"#,
    ];
    std::fs::write(&path, lines.join("\n")).unwrap();

    let entries = load_corpus(&path).expect("load corpus");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].pr, 1);
    assert_eq!(entries[0].human_findings.len(), 1);
    assert_eq!(entries[0].human_findings[0].kind, "logic-error");
    assert!(entries[0].human_findings[0].acted_on);
    assert_eq!(entries[1].pr, 2);
    assert!(entries[1].human_findings.is_empty());
}

#[test]
fn load_corpus_skips_blank_lines() {
    let tmpdir = tempfile::tempdir().expect("tmpdir");
    let path = tmpdir.path().join("corpus.jsonl");

    let content = "\n{\"owner\":\"x\",\"repo\":\"y\",\"pr\":1,\"human_findings\":[]}\n\n";
    std::fs::write(&path, content).unwrap();

    let entries = load_corpus(&path).expect("load corpus");
    assert_eq!(entries.len(), 1);
}

#[test]
fn load_corpus_empty_file_returns_empty_vec() {
    let tmpdir = tempfile::tempdir().expect("tmpdir");
    let path = tmpdir.path().join("corpus.jsonl");
    std::fs::write(&path, "").unwrap();

    let entries = load_corpus(&path).expect("load empty corpus");
    assert!(entries.is_empty());
}

#[test]
fn pr_label_formatted_correctly() {
    let (corpus, trusty_results) = three_pr_corpus();
    let report = compute_metrics(&corpus, &trusty_results);
    assert_eq!(report.per_pr[0].pr, "acme/api#101");
    assert_eq!(report.per_pr[1].pr, "acme/api#102");
    assert_eq!(report.per_pr[2].pr, "acme/api#103");
}
