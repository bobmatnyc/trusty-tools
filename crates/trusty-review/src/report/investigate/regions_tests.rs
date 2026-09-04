//! Tests for the region discipline over LLM findings (#6082 lap 4).

use super::*;
use crate::report::metrics::{MetricFinding, Severity};

/// The exact hotspot entry the graded report rendered for `meta_ops.rs`.
fn impl_hotspot() -> MetricFinding {
    MetricFinding {
        title: IMPL_BLOCK_TITLE.to_string(),
        severity: Severity::Amber,
        category: "maintainability".to_string(),
        component: "crates/trusty-search/src/core/corpus/meta_ops.rs".to_string(),
        description: "cyclomatic complexity 89 (grade F); smells: long_impl_block(418 lines), \
                      deep_nesting(depth 7)"
            .to_string(),
        remediation: "Split the impl block (lines 19–437) across focused submodules — the \
                      analyzer reported no function name for this region, so it is a whole-impl \
                      hotspot rather than one long function"
            .to_string(),
    }
}

fn metrics(findings: Vec<MetricFinding>) -> AnalyzeMetrics {
    AnalyzeMetrics {
        findings,
        ..AnalyzeMetrics::default()
    }
}

fn finding(description: &str, line: u64) -> VerifiedFinding {
    VerifiedFinding {
        title: "High-cyclomatic bulk-copy function is a maintenance hotspot".to_string(),
        severity: Severity::Amber,
        dimension: "maintainability".to_string(),
        file: "crates/trusty-search/src/core/corpus/meta_ops.rs".to_string(),
        line: Some(line),
        evidence_quote: "let src_meta = match src_txn.open_table(META_TABLE) {".to_string(),
        description: description.to_string(),
        business_impact: String::new(),
        remediation: String::new(),
        cost_effort: String::new(),
        trace_verdict: String::new(),
        cwe_id: None,
    }
}

// ─── Index construction ──────────────────────────────────────────────────────

/// A whole-impl hotspot yields its file, span and score.
#[test]
fn an_impl_hotspot_is_indexed() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    assert_eq!(
        index.regions,
        vec![Region {
            file: "crates/trusty-search/src/core/corpus/meta_ops.rs".to_string(),
            start: 19,
            end: 437,
            cyclomatic: 89,
        }]
    );
}

/// A hotspot the analyzer DID name a function for is not a region — its score
/// belongs to that function, and nothing here should second-guess it.
#[test]
fn a_named_function_hotspot_is_not_a_region() {
    let m = metrics(vec![MetricFinding {
        title: "Extract method — copy_all_from".to_string(),
        severity: Severity::Amber,
        category: "maintainability".to_string(),
        component: "crates/trusty-search/src/core/corpus/meta_ops.rs".to_string(),
        description: "cyclomatic complexity 89 (grade F)".to_string(),
        remediation: "Extract the body of `copy_all_from` (lines 359-420) into 2-3 smaller \
                      functions"
            .to_string(),
    }]);
    assert!(RegionIndex::from_metrics(Some(&m)).is_empty());
}

/// No `--analyze` run means no index and every check below is inert.
#[test]
fn no_metrics_yields_an_empty_index() {
    assert!(RegionIndex::from_metrics(None).is_empty());
}

// ─── FIX 3: attribution ──────────────────────────────────────────────────────

/// The graded defect: an impl-block score credited to one method inside it.
#[test]
fn an_impl_score_is_rescoped_onto_its_region() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![finding(
        "The copy_all_from function has cyclomatic complexity 89 and manually enumerates each \
         table's copy logic.",
        359,
    )];

    assert_eq!(rescope_impl_claims(&mut findings, &index), 0);
    assert_eq!(findings.len(), 1, "a rescope keeps the finding");
    assert_eq!(
        findings[0].description,
        "the impl block enclosing `copy_all_from` (lines 19–437) has cyclomatic complexity 89 and \
         manually enumerates each table's copy logic."
    );
}

/// A misattribution this crate cannot rewrite is dropped, never shipped.
#[test]
fn an_unrewritable_attribution_is_dropped() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![finding(
        "copy_all_from, at cyclomatic complexity 89, is the worst offender in the store.",
        359,
    )];

    assert_eq!(rescope_impl_claims(&mut findings, &index), 1);
    assert!(findings.is_empty());
}

/// A score the analyze data never attributed to a region covering this line is
/// the finding's own claim and is left exactly as written.
#[test]
fn a_score_outside_every_region_is_left_alone() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let original = "The parse_row function has cyclomatic complexity 12.";
    let mut findings = vec![finding(original, 900)];

    assert_eq!(rescope_impl_claims(&mut findings, &index), 0);
    assert_eq!(findings[0].description, original);
}

// ─── FIX 4: duplicate suppression ────────────────────────────────────────────

/// The LLM copy of a hotspot the deterministic list already carries is dropped.
#[test]
fn a_restated_hotspot_is_suppressed() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![
        finding(
            "The region has cyclomatic complexity 89 and enumerates each table.",
            408,
        ),
        finding(
            "Corrupt file-hash rows are silently skipped with only a warn log.",
            76,
        ),
    ];

    assert_eq!(suppress_duplicates(&mut findings, &index), 1);
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0]
            .description
            .starts_with("Corrupt file-hash rows")
    );
}

/// A finding quoting a different score is about something else and survives.
#[test]
fn a_finding_quoting_a_different_score_survives() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![finding("cyclomatic complexity 12 in this branch.", 408)];

    assert_eq!(suppress_duplicates(&mut findings, &index), 0);
    assert_eq!(findings.len(), 1);
}

/// A finding outside the region's span is not the same measurement.
#[test]
fn a_finding_outside_the_region_survives() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![finding("cyclomatic complexity 89 over here.", 900)];

    assert_eq!(suppress_duplicates(&mut findings, &index), 0);
    assert_eq!(findings.len(), 1);
}

/// The two passes compose: the graded finding is rescoped, then suppressed as
/// the duplicate it also is.
#[test]
fn a_rescoped_finding_is_still_recognised_as_a_duplicate() {
    let m = metrics(vec![impl_hotspot()]);
    let index = RegionIndex::from_metrics(Some(&m));
    let mut findings = vec![finding(
        "The copy_all_from function has cyclomatic complexity 89 and manually enumerates each \
         table's copy logic.",
        408,
    )];

    rescope_impl_claims(&mut findings, &index);
    assert_eq!(suppress_duplicates(&mut findings, &index), 1);
    assert!(findings.is_empty());
}

// ─── Parsing helpers ─────────────────────────────────────────────────────────

/// Both dash spellings the daemon has used are accepted.
#[test]
fn a_line_range_parses_either_dash() {
    assert_eq!(
        line_range("Split the impl block (lines 19–437) x"),
        Some((19, 437))
    );
    assert_eq!(
        line_range("Split the impl block (lines 19-437) x"),
        Some((19, 437))
    );
    assert_eq!(line_range("Split the impl block across submodules"), None);
}
