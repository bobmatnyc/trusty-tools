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
        analyze_gap: None,
        authorship: None,
        inspect_priority: Vec::new(),
        crate_topology: None,
    }
}

fn model_with(repos: Vec<RepositoryReport>) -> ReportModel {
    ReportModel {
        title: "Test".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: None,
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        inference: None,
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

/// #6137: only the security dimension's RED/AMBER findings count. The table
/// used to group EVERY non-maintainability finding by category, so
/// error-handling and test-coverage findings were reported as security
/// violations.
#[test]
fn security_rows_count_only_the_security_dimension() {
    let metrics = AnalyzeMetrics {
        findings: vec![
            finding(SECURITY_DIMENSION, Severity::Red),
            finding(SECURITY_DIMENSION, Severity::Amber),
            finding("error handling", Severity::Amber),
            finding("test coverage", Severity::Amber),
            finding(MAINTAINABILITY_CATEGORY, Severity::Amber),
            finding(SECURITY_DIMENSION, Severity::Green),
        ],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_security_violation_rows(&mut root, &model);

    let template = "<!-- BEGIN security_violations_table -->{{app_name}}|{{violation_domain}}|{{violation_count}}\n<!-- END security_violations_table -->";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("app|authentication & secrets"));
    assert!(
        rendered.contains("|2 "),
        "only the two non-GREEN security findings count: {rendered}"
    );
    assert!(!rendered.contains("error handling"), "{rendered}");
    assert!(!rendered.contains("test coverage"), "{rendered}");
    assert!(!rendered.contains("maintainability"), "{rendered}");
}

/// #6137: GREEN findings in the security dimension are credited by title. The
/// no-green-analysis rule bans elaboration, not acknowledgement.
#[test]
fn security_section_credits_clean_signals() {
    let mut green = finding(SECURITY_DIMENSION, Severity::Green);
    green.title = "Constant-time bearer token comparison".to_string();
    let metrics = AnalyzeMetrics {
        findings: vec![finding(SECURITY_DIMENSION, Severity::Amber), green],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_security_violation_rows(&mut root, &model);

    let rendered = crate::report::fill::render("{{security_clean_signals}}", &root);
    assert!(
        rendered.contains("Constant-time bearer token comparison (`src/lib.rs`)"),
        "rendered: {rendered}"
    );
}

/// #6080: an uncited GREEN is not a clean signal.
///
/// Why: a report credited five clean signals, none carrying a file, and one of
/// them — "Raw SQL string interpolation via multi-line concatenation for PR
/// upsert" — described a defect. A reader had nothing to check any of them
/// against. The citation is what makes the claim falsifiable.
/// What: a GREEN with an empty `component` is dropped from the list, and a
/// section left with none says so.
#[test]
fn an_uncited_green_is_not_a_clean_signal() {
    let mut green = finding(SECURITY_DIMENSION, Severity::Green);
    green.title = "Raw SQL string interpolation for PR upsert".to_string();
    green.component = String::new();
    let metrics = AnalyzeMetrics {
        findings: vec![finding(SECURITY_DIMENSION, Severity::Amber), green],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_security_violation_rows(&mut root, &model);

    let rendered = crate::report::fill::render("{{security_clean_signals}}", &root);
    assert!(
        !rendered.contains("Raw SQL string interpolation"),
        "rendered: {rendered}"
    );
    assert!(
        rendered.contains("No clean security signals"),
        "rendered: {rendered}"
    );
}

/// #6080: a rejected analyze lane must not print a measured zero.
///
/// Why: maintainability findings come from trusty-analyze alone, but the count
/// was taken from `metrics.findings`, which the investigation pass also writes
/// into. A run whose index was rejected as stale printed
/// `Maintainability findings | 0 ⁽ᵐ⁾` — a measurement nobody made — on the same
/// page whose Gaps & Caveats said the lane was rejected.
/// What: with `analyze_gap` set, the cell states the gap instead of a count.
#[test]
fn maintainability_cell_states_the_analyze_gap() {
    let metrics = AnalyzeMetrics {
        findings: vec![finding(SECURITY_DIMENSION, Severity::Amber)],
        loc: LocMetrics {
            total: 500,
            by_language: vec![LanguageLoc {
                language: "Rust".to_string(),
                loc: 500,
            }],
        },
        ..Default::default()
    };
    let mut r = repo(Some(metrics));
    r.analyze_gap = Some(crate::report::analyze_scope::STALE_INDEX_REMEDY.to_string());
    let model = model_with(vec![r]);
    let mut root = Scope::new();
    push_code_quality_rows(&mut root, &model);

    let template =
        "<!-- BEGIN code_quality_row -->{{cq_maintainability_count}}<!-- END code_quality_row -->";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("Not assessed"), "rendered: {rendered}");
    assert!(!rendered.contains('0'), "no measured zero: {rendered}");
}

/// #6137: no GREEN security finding is stated as an absence of evidence, never
/// left blank or dropped to the honesty marker.
#[test]
fn security_section_states_when_no_clean_signal_exists() {
    let metrics = AnalyzeMetrics {
        findings: vec![finding(SECURITY_DIMENSION, Severity::Amber)],
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);
    let mut root = Scope::new();
    push_security_violation_rows(&mut root, &model);

    let rendered = crate::report::fill::render("{{security_clean_signals}}", &root);
    assert!(
        rendered.contains("No clean security signals were recorded"),
        "rendered: {rendered}"
    );
}

/// #6137: the live defect. `--analyze` leaves `loc`/`counts` empty by design
/// (the scanner owns them), so reading metrics alone printed "not stated in
/// source data" for LoC and tech while §4.1 rendered the scan's figures two
/// sections above.
#[test]
fn code_quality_loc_and_tech_fall_back_to_the_scan() {
    let metrics = AnalyzeMetrics {
        findings: vec![finding(MAINTAINABILITY_CATEGORY, Severity::Amber)],
        ..Default::default()
    };
    let mut r = repo(Some(metrics));
    r.scan = Some(crate::report::scan::RepoScan {
        total_loc: 1_553_771,
        file_count: 6664,
        by_language: vec![
            LanguageLoc {
                language: "Rust".to_string(),
                loc: 1_408_871,
            },
            LanguageLoc {
                language: "HTML".to_string(),
                loc: 33_376,
            },
        ],
        frameworks: Vec::new(),
    });
    let model = model_with(vec![r]);
    let mut root = Scope::new();
    push_code_quality_rows(&mut root, &model);

    let template = "<!-- BEGIN code_quality_row -->{{cq_app_name}}|{{cq_loc}}|{{cq_tech}}<!-- END code_quality_row -->";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("1553771"), "rendered: {rendered}");
    assert!(rendered.contains("Rust, HTML"), "rendered: {rendered}");
    assert!(
        !rendered.contains(crate::report::fill::HONESTY_MARKER),
        "rendered: {rendered}"
    );
}
