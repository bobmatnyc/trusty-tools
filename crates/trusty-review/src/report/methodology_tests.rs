//! Tests for the per-run §1 methodology line (#6675).

use super::*;
use crate::report::investigate::{
    Investigation, InvestigationStatus, RepoInvestigation, TraceLimits, TraceSet,
};
use crate::report::metrics::AnalyzeMetrics;
use crate::report::model::{ReportModel, RepositoryReport};

/// One repository, optionally carrying analyze metrics and an analyze gap.
fn repo(name: &str, metrics: bool, gap: Option<&str>) -> RepositoryReport {
    RepositoryReport {
        name: name.to_string(),
        slug: name.to_lowercase(),
        source: format!("/tmp/{name}"),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics: metrics.then(AnalyzeMetrics::default),
        analyze_gap: gap.map(str::to_string),
        authorship: None,
        inspect_priority: Vec::new(),
        crate_topology: None,
    }
}

fn model_with(repos: Vec<RepositoryReport>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd-cast".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
        instructions: None,
        instructions_source: None,
        report_date: "2026-09-02".to_string(),
        generated_date: "2026-09-02".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: repos,
        gaps: vec![],
        findings: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

/// An investigation whose trace pass ran over `candidates` findings and anchored
/// `assembled` of them.
fn investigation(candidates: usize, assembled: usize) -> Investigation {
    Investigation {
        repos: vec![RepoInvestigation {
            slug: "estate".to_string(),
            name: "Estate".to_string(),
            status: InvestigationStatus::Available,
            findings: Vec::new(),
            deps: Default::default(),
            coverage: Default::default(),
            traces: Some(TraceSet {
                index_id: None,
                traces: Vec::new(),
                candidates,
                assembled,
                no_trace: candidates - assembled,
                limits: TraceLimits::default(),
            }),
            verdicts: None,
            exposure: Vec::new(),
        }],
    }
}

/// The live defect (#6675): `--analyze` fell back to scan and every trace
/// lookup returned `IndexAbsent`, and §1 still asserted both tools were used.
///
/// Fails before the fix: the row was fixed template text with no per-run input
/// at all, so there was nothing for this function to return.
#[test]
fn both_lanes_absent_claim_neither() {
    let mut model = model_with(vec![repo("Estate", false, Some("re-run with --analyze"))]);
    model.investigation = Some(investigation(10, 0));
    let line = analysis_methodology(&model);
    assert!(
        line.contains("trusty-analyze contributed no data to this run"),
        "the analyze lane must state its own absence: {line}"
    );
    assert!(
        line.contains(
            "trusty-search resolved no symbol anchor for any of the 10 traced finding(s)"
        ),
        "the search lane must state its own absence: {line}"
    );
}

/// A run where both lanes did contribute names both, with its own figures.
#[test]
fn a_full_run_names_both_lanes() {
    let mut model = model_with(vec![repo("Estate", true, None)]);
    model.investigation = Some(investigation(10, 7));
    let line = analysis_methodology(&model);
    assert!(
        line.contains("trusty-analyze structural metrics for all 1 application(s)"),
        "{line}"
    );
    assert!(
        line.contains("trusty-search anchored 7 of 10 traced finding(s)"),
        "{line}"
    );
}

/// A partly-populated analyze lane states the fraction rather than either
/// extreme, and a run with no trace pass says the pass did not run.
#[test]
fn a_partial_analyze_lane_states_the_fraction() {
    let model = model_with(vec![
        repo("One", true, None),
        repo("Two", false, Some("not indexed")),
    ]);
    let line = analysis_methodology(&model);
    assert!(
        line.contains("trusty-analyze structural metrics for 1 of 2 application(s)"),
        "{line}"
    );
    assert!(
        line.contains("trusty-search symbol tracing did not run"),
        "{line}"
    );
}
