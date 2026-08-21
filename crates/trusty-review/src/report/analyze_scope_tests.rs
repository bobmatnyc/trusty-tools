use super::*;
use crate::report::metrics::{MetricFinding, Severity};

fn metrics_with(components: &[&str]) -> AnalyzeMetrics {
    AnalyzeMetrics {
        findings: components
            .iter()
            .map(|c| MetricFinding {
                title: "Extract method".to_string(),
                severity: Severity::Amber,
                category: "maintainability".to_string(),
                component: (*c).to_string(),
                description: "grade F".to_string(),
                remediation: "split it".to_string(),
            })
            .collect(),
        ..Default::default()
    }
}

/// The live defect: the daemon answered from a checkout at a different path.
#[test]
fn finds_a_component_outside_the_audited_tree() {
    let m = metrics_with(&[
        "/Users/masa/Projects/trusty-tools/crates/a/src/lib.rs",
        "/Users/masa/Projects/trusty-tools/crates/b/src/lib.rs",
    ]);
    let stale = out_of_tree_components(&m, Path::new("/Users/masa/audit/repos/trusty-tools"));
    assert_eq!(stale.len(), 2, "both components are out of tree: {stale:?}");
}

/// A relative component is read against the checkout root, so it is in-tree by
/// construction and must never be flagged.
#[test]
fn relative_components_are_always_in_tree() {
    let m = metrics_with(&["crates/a/src/lib.rs", "crates/b/src/lib.rs:67"]);
    assert!(out_of_tree_components(&m, Path::new("/Users/masa/audit/repo")).is_empty());
}

/// An absolute component under the audited root is a genuine measurement.
#[test]
fn an_in_tree_absolute_component_is_not_flagged() {
    let m = metrics_with(&["/Users/masa/audit/repo/crates/a/src/lib.rs:12"]);
    assert!(out_of_tree_components(&m, Path::new("/Users/masa/audit/repo")).is_empty());
}

/// Duplicates collapse so the gap line names distinct paths.
#[test]
fn duplicate_paths_are_reported_once() {
    let m = metrics_with(&["/elsewhere/a.rs", "/elsewhere/a.rs", "/elsewhere/b.rs"]);
    assert_eq!(
        out_of_tree_components(&m, Path::new("/audit")),
        vec!["/elsewhere/a.rs".to_string(), "/elsewhere/b.rs".to_string()]
    );
}

/// In-tree metrics pass through untouched.
#[test]
fn accept_passes_in_tree_metrics() {
    let m = metrics_with(&["crates/a/src/lib.rs"]);
    let out = accept("app", "repo", Path::new("/audit/repo"), m);
    assert!(out.is_ok(), "in-tree metrics must be admitted");
}

/// Out-of-tree metrics are rejected whole — the complexity distribution came
/// from the same index, so nothing from it is trustworthy.
#[test]
fn accept_rejects_metrics_describing_another_checkout() {
    let m = metrics_with(&["/other/checkout/src/lib.rs"]);
    let out = accept("app", "trusty-tools", Path::new("/audit/repo"), m);
    let gap = out.expect_err("out-of-tree metrics must be rejected");
    assert!(gap.contains("stale"), "gap: {gap}");
}

/// The gap line names the repository, the index, and an example path.
#[test]
fn gap_line_names_the_repository_and_a_path() {
    let line = stale_index_gap(
        "app",
        "trusty-tools",
        &["/other/a.rs".to_string(), "/other/b.rs".to_string()],
    );
    assert!(line.contains("app"), "line: {line}");
    assert!(line.contains("trusty-tools"), "line: {line}");
    assert!(line.contains("/other/a.rs"), "line: {line}");
    assert!(line.contains("not clean"), "line: {line}");
}
