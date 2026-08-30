//! Unit tests for `commands::calibrate`.
//!
//! Why: verifies the fuzzy matcher, metrics computation, corpus loading, and
//! the rust_semantic_fp_rate computation — all without hitting a real LLM or
//! GitHub connection.
//! What: synthetic 3-PR corpus fixture with known ground truth; assertions on
//! recall/precision values and false-positive lists.
//! Test: this is the test file.

use super::*;
use trusty_review::models::{Effort, Finding, Verdict};

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
            reference_verdict: None,
            owner: "acme".to_string(),
            repo: "api".to_string(),
            pr: 101,
            human_findings: vec![
                make_human("src/auth.rs", "security", true),
                make_human("src/db.rs", "logic-error", true),
            ],
        },
        CorpusEntry {
            reference_verdict: None,
            owner: "acme".to_string(),
            repo: "api".to_string(),
            pr: 102,
            human_findings: vec![
                make_human("src/lib.rs", "ownership", true),
                make_human("src/handler.rs", "security", false),
            ],
        },
        CorpusEntry {
            reference_verdict: None,
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
        reference_verdict: None,
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
        reference_verdict: None,
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

// ─── Verdict bars (#1897 acceptance gate, wired for #2974) ───────────────────

/// Build a corpus entry carrying a reference verdict and no human findings.
fn labelled(pr: u64, reference: Verdict) -> CorpusEntry {
    CorpusEntry {
        owner: "acme".to_string(),
        repo: "api".to_string(),
        pr,
        human_findings: Vec::new(),
        reference_verdict: Some(reference),
    }
}

#[test]
fn verdict_bars_pass_when_all_three_clear() {
    // 8 PRs: 4 reference-clean (0 escalated), 4 reference-RC (4 flagged),
    // 8/8 exact agreement.
    let corpus = vec![
        labelled(1, Verdict::Approve),
        labelled(2, Verdict::Approve),
        labelled(3, Verdict::Approve),
        labelled(4, Verdict::Approve),
        labelled(5, Verdict::RequestChanges),
        labelled(6, Verdict::RequestChanges),
        labelled(7, Verdict::RequestChanges),
        labelled(8, Verdict::Block),
    ];
    let daemon = vec![
        Verdict::Approve,
        Verdict::Approve,
        Verdict::Approve,
        Verdict::Approve,
        Verdict::RequestChanges,
        Verdict::RequestChanges,
        Verdict::RequestChanges,
        Verdict::Block,
    ];

    let bars = compute_verdict_bars(&corpus, &daemon).expect("labelled corpus scores bars");
    assert_eq!(bars.scored_prs, 8);
    assert!((bars.verdict_agreement - 1.0).abs() < f64::EPSILON);
    assert!(bars.verdict_agreement_pass);
    assert_eq!(bars.rc_reference_prs, 4);
    assert!((bars.rc_recall - 1.0).abs() < f64::EPSILON);
    assert_eq!(bars.clean_reference_prs, 4);
    assert!(bars.clean_pr_over_flag_rate.abs() < f64::EPSILON);
    assert!(bars.clean_pr_over_flag_pass);
}

/// The 0.6.3 regression shape #1897 named: clean PRs escalated to
/// REQUEST_CHANGES. Bar 3 must fail, and bar 1 must fail with it.
#[test]
fn verdict_bars_fail_on_over_flagging() {
    let corpus = vec![
        labelled(1, Verdict::Approve),
        labelled(2, Verdict::Approve),
        labelled(3, Verdict::Approve),
        labelled(4, Verdict::Approve),
    ];
    let daemon = vec![
        Verdict::RequestChanges,
        Verdict::RequestChanges,
        Verdict::Approve,
        Verdict::Approve,
    ];

    let bars = compute_verdict_bars(&corpus, &daemon).expect("labelled corpus scores bars");
    assert!((bars.clean_pr_over_flag_rate - 0.5).abs() < f64::EPSILON);
    assert!(!bars.clean_pr_over_flag_pass, "50% over-flagging must fail");
    assert!((bars.verdict_agreement - 0.5).abs() < f64::EPSILON);
    assert!(!bars.verdict_agreement_pass);
}

/// RC-recall is reported separately, so a cap that stops over-flagging by
/// approving everything is visible rather than hidden behind bar 3's pass.
#[test]
fn verdict_bars_expose_an_rc_recall_collapse() {
    let corpus = vec![
        labelled(1, Verdict::Approve),
        labelled(2, Verdict::RequestChanges),
        labelled(3, Verdict::RequestChanges),
    ];
    let daemon = vec![Verdict::Approve, Verdict::Approve, Verdict::Approve];

    let bars = compute_verdict_bars(&corpus, &daemon).expect("labelled corpus scores bars");
    assert!(
        bars.clean_pr_over_flag_pass,
        "nothing was over-flagged, so bar 3 passes"
    );
    assert_eq!(bars.rc_reference_prs, 2);
    assert!(
        bars.rc_recall.abs() < f64::EPSILON,
        "recall collapsed to zero and must be visible"
    );
}

#[test]
fn verdict_bars_are_none_without_reference_verdicts() {
    let corpus = vec![CorpusEntry {
        owner: "acme".to_string(),
        repo: "api".to_string(),
        pr: 1,
        human_findings: Vec::new(),
        reference_verdict: None,
    }];
    assert!(
        compute_verdict_bars(&corpus, &[Verdict::Approve]).is_none(),
        "an unlabelled corpus must report `not measured`, never a vacuous pass"
    );
}

/// #5620's lesson applied to the bars: an empty denominator and a PASS must be
/// unreachable together.
#[test]
fn verdict_bars_never_pass_on_an_empty_denominator() {
    // Every reference PR requests changes, so the clean-PR sample is empty.
    let corpus = vec![labelled(1, Verdict::RequestChanges)];
    let bars = compute_verdict_bars(&corpus, &[Verdict::RequestChanges]).expect("scored");
    assert_eq!(bars.clean_reference_prs, 0);
    assert!(
        !bars.clean_pr_over_flag_pass,
        "no clean PRs were measured, so the over-flag bar cannot pass"
    );
}

/// A pipeline failure yields `Verdict::Unknown`, which agrees with no reference
/// verdict — the PR counts against agreement rather than leaving the sample.
#[test]
fn a_failed_pr_counts_against_agreement() {
    let corpus = vec![labelled(1, Verdict::Approve), labelled(2, Verdict::Approve)];
    let bars =
        compute_verdict_bars(&corpus, &[Verdict::Approve, Verdict::Unknown]).expect("scored");
    assert_eq!(bars.scored_prs, 2);
    assert!((bars.verdict_agreement - 0.5).abs() < f64::EPSILON);
    assert!(
        bars.clean_pr_over_flag_rate.abs() < f64::EPSILON,
        "UNKNOWN is not an escalation"
    );
}

/// A pre-#2974 corpus line — no `reference_verdict` key at all — still loads.
#[test]
fn a_corpus_line_without_a_reference_verdict_still_parses() {
    let line = r#"{"owner":"acme","repo":"api","pr":1,"human_findings":[]}"#;
    let entry: CorpusEntry = serde_json::from_str(line).expect("legacy corpus line parses");
    assert!(entry.reference_verdict.is_none());
}

#[test]
fn a_corpus_line_with_a_reference_verdict_parses_it() {
    let line = r#"{"owner":"acme","repo":"api","pr":1,"human_findings":[],"reference_verdict":"REQUEST_CHANGES"}"#;
    let entry: CorpusEntry = serde_json::from_str(line).expect("labelled corpus line parses");
    assert_eq!(entry.reference_verdict, Some(Verdict::RequestChanges));
}
