use super::*;
use crate::report::authorship::{AuthorshipSummary, MonthlyActivity};
use crate::report::model::RepositoryReport;

fn repo(authorship: Option<AuthorshipSummary>) -> RepositoryReport {
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
        metrics: None,
        analyze_gap: None,
        authorship,
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
        findings: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        ticketing: None,
    }
}

fn summary() -> AuthorshipSummary {
    AuthorshipSummary {
        schema_version: "v0".to_string(),
        repository: "app".to_string(),
        distinct_authors: 3,
        bus_factor: 1,
        top_author_share_pct: 80.0,
        single_author_subsystems: vec!["src".to_string()],
        monthly_trajectory: vec![
            MonthlyActivity {
                month: "2026-01".to_string(),
                active_authors: 1,
                commits: 5,
            },
            MonthlyActivity {
                month: "2026-02".to_string(),
                active_authors: 2,
                commits: 10,
            },
        ],
        unresolved_authors: 0,
        caveats: vec!["no vendored-path exclusion".to_string()],
    }
}

/// A model with a loaded artifact yields a populated row, never blank.
#[test]
fn populates_rows_from_loaded_artifacts() {
    let model = model_with(vec![repo(Some(summary()))]);
    let mut root = Scope::new();
    push_authorship_rows(&mut root, &model);

    let template = "<!-- BEGIN authorship_row -->{{au_app_name}}|{{au_distinct_authors}}|{{au_bus_factor}}|{{au_top_author_share}}|{{au_single_author_subsystems}}<!-- END authorship_row -->{{au_caveats}}";
    let rendered = crate::report::fill::render(template, &root);
    assert!(rendered.contains("app|3"));
    assert!(rendered.contains("|1 "), "rendered: {rendered}");
    assert!(rendered.contains("80%"));
    assert!(rendered.contains("src"));
    assert!(rendered.contains("no vendored-path exclusion"));
}

/// A repository with no loaded artifact contributes no row — fail-open, the
/// gap already lives in `model.gaps` from the load step.
#[test]
fn no_artifact_yields_no_row() {
    let model = model_with(vec![repo(None)]);
    let mut root = Scope::new();
    push_authorship_rows(&mut root, &model);
    let template = "<!-- BEGIN authorship_row -->{{au_app_name}}<!-- END authorship_row -->";
    let rendered = crate::report::fill::render(template, &root);
    assert_eq!(rendered, "");
}

/// Key Facts author-count and trajectory complete once authorship data
/// exists.
#[test]
fn completes_key_facts_author_rows() {
    let model = model_with(vec![repo(Some(summary()))]);
    let mut root = Scope::new();
    fill_authorship_facts(&mut root, &model);
    let rendered =
        crate::report::fill::render("{{facts_author_count}}|{{facts_trajectory}}", &root);
    assert!(rendered.contains('3'));
    assert!(rendered.contains("increasing") || rendered.contains("month"));
}

/// No repository has authorship data — Key Facts rows stay unset (honesty
/// marker, folded to a gap by the caller's omit-empty pass).
#[test]
fn no_authorship_data_leaves_facts_unset() {
    let model = model_with(vec![repo(None)]);
    let mut root = Scope::new();
    fill_authorship_facts(&mut root, &model);
    let rendered =
        crate::report::fill::render("{{facts_author_count}}|{{facts_trajectory}}", &root);
    assert_eq!(
        rendered,
        format!("{h}|{h}", h = crate::report::fill::HONESTY_MARKER)
    );
}
