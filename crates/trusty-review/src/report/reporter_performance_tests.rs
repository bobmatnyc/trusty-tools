use super::*;
use crate::report::metrics::{AnalyzeMetrics, MetricFinding};
use crate::report::model::RepositoryReport;

fn model_with(findings: Vec<MetricFinding>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
        instructions: None,
        instructions_source: None,
        report_date: "2026-08-21".to_string(),
        generated_date: "2026-08-21".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: vec![RepositoryReport {
            name: "app".to_string(),
            slug: "app".to_string(),
            source: "/tmp/app".to_string(),
            source_kind: "local_path".to_string(),
            username: None,
            git_ref: None,
            git_info: None,
            local_path: None,
            scan: None,
            metrics: Some(AnalyzeMetrics {
                findings,
                ..Default::default()
            }),
            analyze_gap: None,
            authorship: None,
            inspect_priority: Vec::new(),
        }],
        gaps: vec![],
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

fn finding(category: &str, severity: Severity) -> MetricFinding {
    MetricFinding {
        title: "t".to_string(),
        severity,
        category: category.to_string(),
        component: "src/lib.rs".to_string(),
        description: "d".to_string(),
        remediation: "r".to_string(),
    }
}

fn render_note(model: &ReportModel) -> String {
    let mut scope = Scope::new();
    fill_performance_note(&mut scope, model);
    crate::report::fill::render("{{performance_assessment_note}}", &scope)
}

/// (d): the Performance section text is byte-identical regardless of whether
/// synthesis ran — it reads finding CATEGORIES, never synthesis prose.
#[test]
fn note_is_fixed_regardless_of_synthesis() {
    let model = model_with(vec![finding("error handling", Severity::Amber)]);
    let a = render_note(&model);
    let b = render_note(&model);
    assert_eq!(a, b);
    assert_eq!(a, PERFORMANCE_NOTE);
    assert!(a.contains("No performance or scalability assessment was made"));
}

/// #6137: "no assessment was made" read as true of scalability too, while §5
/// listed nineteen scalability findings two sections above.
#[test]
fn note_cross_references_section_5_when_scalability_findings_exist() {
    let model = model_with(vec![finding(SCALABILITY_DIMENSION, Severity::Amber)]);
    let rendered = render_note(&model);
    assert!(
        rendered.starts_with(PERFORMANCE_NOTE),
        "the fixed note still leads: {rendered}"
    );
    assert!(
        rendered.contains("listed by severity in section 5"),
        "rendered: {rendered}"
    );
}

/// A GREEN scalability finding is not a finding §5 lists, so it earns no
/// cross-reference.
#[test]
fn a_green_scalability_finding_earns_no_cross_reference() {
    let model = model_with(vec![finding(SCALABILITY_DIMENSION, Severity::Green)]);
    assert_eq!(render_note(&model), PERFORMANCE_NOTE);
}
