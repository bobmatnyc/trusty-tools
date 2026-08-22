use super::*;
use crate::report::authorship::{AuthorshipSummary, MonthlyActivity};
use crate::report::investigate::{Coverage, Investigation, InvestigationStatus, RepoInvestigation};
use crate::report::model::RepositoryReport;

fn repo() -> RepositoryReport {
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
        report_date: "2026-08-21".to_string(),
        generated_date: "2026-08-21".to_string(),
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

/// The coverage percentage is computed at render time and appears nowhere in
/// the serialized model — but the synthesis prompt quotes it, so the guardrail
/// must know it.
#[test]
fn printed_figures_carry_the_coverage_percentage() {
    let mut model = model_with(vec![repo()]);
    model.investigation = Some(Investigation {
        repos: vec![RepoInvestigation {
            verdicts: None,
            name: "app".to_string(),
            slug: "app".to_string(),
            status: InvestigationStatus::Available,
            coverage: Coverage {
                files_examined: 73,
                total_files: 6664,
                ..Default::default()
            },
            findings: Vec::new(),
            deps: Default::default(),
            traces: None,
        }],
    });

    let printed = printed_figures(&model).join("\n");
    assert!(
        printed.contains("1.1% coverage"),
        "the coverage percentage must be among the printed figures: {}",
        &printed[printed.len().saturating_sub(400)..]
    );
}

/// The trajectory average is likewise derived — the model carries per-month
/// commit counts, never the average the Key Facts row prints.
#[test]
fn printed_figures_carry_the_authorship_trajectory_average() {
    let mut r = repo();
    r.authorship = Some(AuthorshipSummary {
        monthly_trajectory: vec![
            MonthlyActivity {
                month: "2026-07".to_string(),
                commits: 100,
                active_authors: 2,
            },
            MonthlyActivity {
                month: "2026-08".to_string(),
                commits: 201,
                active_authors: 4,
            },
        ],
        ..Default::default()
    });
    let model = model_with(vec![r]);

    let printed = printed_figures(&model).join("\n");
    assert!(
        printed.contains("150.5"),
        "the trajectory average must be among the printed figures"
    );
}
