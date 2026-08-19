use super::*;
use crate::report::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, CountMetrics, LanguageLoc, LocMetrics,
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

/// (a)-style: facts fill from data already present on the model.
#[test]
fn fills_aggregate_facts() {
    let metrics = AnalyzeMetrics {
        loc: LocMetrics {
            total: 1000,
            by_language: vec![
                LanguageLoc {
                    language: "Rust".to_string(),
                    loc: 800,
                },
                LanguageLoc {
                    language: "Shell".to_string(),
                    loc: 200,
                },
            ],
        },
        counts: CountMetrics {
            files: 42,
            functions: 100,
        },
        complexity: ComplexityDistribution {
            buckets: vec![
                ComplexityBucket {
                    label: "low (1-5)".to_string(),
                    count: 80,
                },
                ComplexityBucket {
                    label: "high (>20)".to_string(),
                    count: 20,
                },
            ],
        },
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(metrics))]);

    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render(
        "{{facts_total_loc}}|{{facts_total_files}}|{{facts_languages}}|{{facts_complexity_summary}}",
        &scope,
    );

    assert!(rendered.contains("1000"));
    assert!(rendered.contains("42"));
    assert!(rendered.contains("Rust"));
    assert!(rendered.contains("low (1-5): 80 (80%)"));
    assert!(rendered.contains("high (>20): 20 (20%)"));
    // Author/work/trajectory rows are PR B's — never fabricated here.
    assert!(!rendered.contains("facts_author_count"));
}

/// (b)-style: a model with no metrics produces honesty markers, never
/// fabricated values — every `facts_*` scalar stays unset.
#[test]
fn empty_model_is_all_gaps() {
    let model = model_with(vec![repo(None)]);
    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render(
        "{{facts_total_loc}}|{{facts_total_files}}|{{facts_languages}}|{{facts_complexity_summary}}",
        &scope,
    );
    assert_eq!(
        rendered,
        format!("{h}|{h}|{h}|{h}", h = crate::report::fill::HONESTY_MARKER)
    );
}

/// Complexity buckets with the same label across repositories merge counts
/// rather than producing duplicate rows.
#[test]
fn complexity_buckets_merge_across_repositories() {
    let m1 = AnalyzeMetrics {
        complexity: ComplexityDistribution {
            buckets: vec![ComplexityBucket {
                label: "low (1-5)".to_string(),
                count: 10,
            }],
        },
        ..Default::default()
    };
    let m2 = AnalyzeMetrics {
        complexity: ComplexityDistribution {
            buckets: vec![ComplexityBucket {
                label: "low (1-5)".to_string(),
                count: 10,
            }],
        },
        ..Default::default()
    };
    let model = model_with(vec![repo(Some(m1)), repo(Some(m2))]);
    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render("{{facts_complexity_summary}}", &scope);
    assert_eq!(rendered, "low (1-5): 20 (100%) ⁽ᵐ⁾");
}
