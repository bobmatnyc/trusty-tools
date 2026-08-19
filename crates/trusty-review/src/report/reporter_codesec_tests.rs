use super::*;
use crate::report::metrics::{
    ComplexityBucket, ComplexityDistribution, CountMetrics, LanguageLoc, LocMetrics, MetricFinding,
    Severity,
};
use crate::report::model::RepositoryReport;

fn repo(metrics: Option<AnalyzeMetrics>) -> RepositoryReport {
    RepositoryReport {
        name: "app".to_string(),
        slug: "app".to_string(),
        source: "/tmp/app".to_string(),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan: None,
        metrics,
        authorship: None,
    }
}

fn model_with(repos: Vec<RepositoryReport>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        instructions: None,
        instructions_source: None,
        report_date: "2026-08-18".to_string(),
        generated_date: "2026-08-18".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: repos,
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
        title: format!("{category} finding"),
        severity,
        category: category.to_string(),
        component: "src/lib.rs".to_string(),
        description: "desc".to_string(),
        remediation: "fix it".to_string(),
    }
}

/// (a): a model with metrics data yields a populated code-quality row.
#[test]
fn code_quality_rows_reproject_metrics() {
    let metrics = AnalyzeMetrics {
        loc: LocMetrics {
            total: 500,
            by_language: vec![LanguageLoc {
                language: "Rust".to_string(),
                loc: 500,
            }],
        },
        counts: CountMetrics {
            files: 10,
            functions: 30,
        },
        complexity: ComplexityDistribution {
            buckets: vec![ComplexityBucket {
                label: "low (1-5)".to_string(),
                count: 30,
            }],
        },
        findings: vec![finding(MAINTAINABILITY_CATEGORY, Severity::Amber)],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_code_quality_rows(&mut root, &model);

    let template = "<!-- BEGIN code_quality_row -->{{cq_app_name}}|{{cq_loc}}|{{cq_tech}}|{{cq_complexity}}|{{cq_maintainability_count}}<!-- END code_quality_row -->";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("app|500"));
    assert!(rendered.contains("Rust"));
    assert!(rendered.contains("low (1-5): 30 (100%)"));
    assert!(rendered.contains("|1"));
}

/// (b): a model with no metrics produces no code-quality rows (honesty, never
/// fabricated) — the block collapses via omit-empty at the reporter layer.
#[test]
fn no_metrics_yields_no_code_quality_rows() {
    let model = model_with(vec![repo(None)]);
    let mut root = Scope::new();
    push_code_quality_rows(&mut root, &model);
    let template = "<!-- BEGIN code_quality_row -->{{cq_app_name}}<!-- END code_quality_row -->";
    let rendered = crate::report::fill::render(template, &root);
    assert_eq!(rendered, "");
}

/// Security rows group by domain and exclude maintainability + GREEN.
#[test]
fn security_rows_group_by_domain_excluding_maintainability() {
    let metrics = AnalyzeMetrics {
        findings: vec![
            finding("clippy", Severity::Red),
            finding("clippy", Severity::Amber),
            finding(MAINTAINABILITY_CATEGORY, Severity::Amber),
            finding("ruff", Severity::Green),
        ],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_security_violation_rows(&mut root, &model);

    let template = "<!-- BEGIN security_violations_table -->{{app_name}}|{{violation_domain}}|{{violation_count}}\n<!-- END security_violations_table -->";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("app|clippy"));
    assert!(rendered.contains("|2 "), "rendered: {rendered}");
    assert!(!rendered.contains("maintainability"));
    assert!(!rendered.contains("ruff"));
}
