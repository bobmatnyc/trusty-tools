use super::*;
use crate::report::metrics::{
    AnalyzeMetrics, ComplexityBucket, ComplexityDistribution, CountMetrics, LocMetrics,
};
use crate::report::model::RepositoryReport;
use crate::report::scan::RepoScan;

fn repo(metrics: Option<AnalyzeMetrics>) -> RepositoryReport {
    repo_with(metrics, None)
}

fn repo_with(metrics: Option<AnalyzeMetrics>, scan: Option<RepoScan>) -> RepositoryReport {
    RepositoryReport {
        name: "app".to_string(),
        slug: "app".to_string(),
        source: "/tmp/app".to_string(),
        source_kind: "local_path".to_string(),
        username: None,
        git_ref: None,
        git_info: None,
        local_path: None,
        scan,
        metrics,
        analyze_gap: None,
        authorship: None,
        inspect_priority: Vec::new(),
        crate_topology: None,
    }
}

fn lang(name: &str, loc: u64) -> LanguageLoc {
    LanguageLoc {
        language: name.to_string(),
        loc,
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

/// (b)-style: a model with neither metrics nor scan never fabricates a
/// density figure — the three density rows stay unset, and the rows whose
/// input is genuinely absent name that input instead of blanking (#6029).
#[test]
fn empty_model_is_all_gaps() {
    let model = model_with(vec![repo(None)]);
    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let density = crate::report::fill::render(
        "{{facts_total_loc}}|{{facts_total_files}}|{{facts_languages}}",
        &scope,
    );
    assert_eq!(
        density,
        format!("{h}|{h}|{h}", h = crate::report::fill::HONESTY_MARKER)
    );

    let named = crate::report::fill::render(
        "{{facts_complexity_summary}}|{{facts_author_count}}|{{facts_work_estimate}}|{{facts_trajectory}}",
        &scope,
    );
    assert!(named.contains("--analyze"), "{named}");
    assert_eq!(named.matches("authorship artifact").count(), 2, "{named}");
    assert!(named.contains("effort-estimation"), "{named}");
    assert!(
        !named.contains(crate::report::fill::HONESTY_MARKER),
        "a genuinely absent input must name itself, never blank: {named}"
    );
}

/// #6029 regression: a sweep with no `--analyze` still carries `RepoScan`
/// data on every repository, and the Key Facts block must render that LoC
/// figure. Before the fix `fill_key_facts` read `metrics` alone, every row
/// fell to the honesty marker, and the omit-empty pass collapsed the whole
/// block to `_No data available — see Gaps & Caveats._` while the scan held
/// the number.
#[test]
fn scan_only_model_fills_density_facts() {
    let scan = RepoScan {
        total_loc: 1_500_000,
        file_count: 8_432,
        by_language: vec![lang("Rust", 1_400_000), lang("TypeScript", 100_000)],
        frameworks: vec![],
    };
    let model = model_with(vec![repo_with(None, Some(scan))]);

    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render(
        "{{facts_total_loc}}|{{facts_total_files}}|{{facts_languages}}",
        &scope,
    );

    assert!(rendered.contains("1500000"), "{rendered}");
    assert!(rendered.contains("8432"), "{rendered}");
    assert!(rendered.contains("Rust"), "{rendered}");
    assert!(
        !rendered.contains(crate::report::fill::HONESTY_MARKER),
        "scan data must reach every density row: {rendered}"
    );
}

/// A `--analyze` figure wins over the scan's for the same repository, the
/// precedence `reporter_fill::fill_profile` already established.
#[test]
fn metrics_win_over_scan_in_key_facts() {
    let metrics = AnalyzeMetrics {
        loc: LocMetrics {
            total: 900,
            by_language: vec![lang("Rust", 900)],
        },
        counts: CountMetrics {
            files: 9,
            functions: 90,
        },
        ..Default::default()
    };
    let scan = RepoScan {
        total_loc: 111,
        file_count: 1,
        by_language: vec![lang("Shell", 111)],
        frameworks: vec![],
    };
    let model = model_with(vec![repo_with(Some(metrics), Some(scan))]);

    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render(
        "{{facts_total_loc}}|{{facts_total_files}}|{{facts_languages}}",
        &scope,
    );
    assert!(rendered.contains("900"), "{rendered}");
    assert!(!rendered.contains("111"), "{rendered}");
    assert!(!rendered.contains("Shell"), "{rendered}");
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

/// #6080: Key Facts must state the same remedy the gap bullet states.
///
/// Why: this row said "re-run with `--analyze`" for every missing-complexity
/// case, including a run where the lane HAD run and its index was rejected as
/// stale. §9 said to re-index under a distinct id, and a reader following Key
/// Facts would re-run the same command and get the same rejection.
/// What: the row renders the repository's own `analyze_gap` remedy.
#[test]
fn complexity_gap_carries_the_lane_remedy() {
    let mut r = repo(None);
    r.analyze_gap = Some(crate::report::analyze_scope::STALE_INDEX_REMEDY.to_string());
    let model = model_with(vec![r]);
    let mut scope = Scope::new();
    fill_key_facts(&mut scope, &model);
    let rendered = crate::report::fill::render("{{facts_complexity_summary}}", &scope);
    assert!(
        rendered.contains(crate::report::analyze_scope::STALE_INDEX_REMEDY),
        "rendered: {rendered}"
    );
    assert!(!rendered.contains("re-run with"), "rendered: {rendered}");
}
