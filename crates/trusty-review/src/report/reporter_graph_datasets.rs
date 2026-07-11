//! §7 Graph-Ready Data Appendix dataset fill: repo-derivable datasets (#2366
//! follow-up).
//!
//! Why: live-QA on a real bare `report` run (no `--synthesize`, no external
//! trusty-analyze metrics JSON) found the §7 appendix entirely empty — every
//! dataset table collapsed under omit-empty and the wave-4 Mermaid renderer had
//! nothing to chart.  Root cause: `loc_by_technology`'s data (per-language LoC)
//! is exactly what the built-in scan (#2342.3) already computes, but the
//! dataset had no fill path at all.  Factoring these injectors into their own
//! module (rather than growing `reporter.rs` further) keeps the reporter under
//! the SLOC cap and gives this dataset-population concern one home, mirroring
//! `reporter_fill.rs`'s per-application profile fill.
//! What: [`inject_loc_by_technology_dataset`] and
//! [`inject_complexity_distribution_dataset`] — both `pub(super)` so
//! `reporter.rs::build_scope` calls them while they stay crate-internal. Every
//! other §7 dataset (violations, CVEs, license tiers, remediation economics,
//! …) has no deterministic source anywhere in the model yet and correctly stays
//! empty by design — see the spec's "Dataset population" table for the full
//! per-dataset status.
//! Test: `reporter_tests.rs::{bare_scan_populates_loc_by_technology,
//! declared_metrics_win_for_loc_by_technology,
//! complexity_distribution_fills_from_metrics,
//! complexity_distribution_empty_without_metrics}`; end-to-end via
//! `tests/report_e2e.rs::end_to_end_bare_scan_emits_mermaid_chart`.

use super::fill::Scope;
use super::metrics::LanguageLoc;
use super::model::ReportModel;
use super::provenance::{Provenance, tag};
use super::reporter_fill::format_thousands;

/// Fill the graph-appendix `loc_by_technology` dataset — one row per
/// (application, language) pair, from data the model ALREADY computes.
///
/// Why: this dataset's data (per-language LoC) is exactly what already fills
/// the Profile section's "Technology stack" line (`fill_profile` /
/// `format_language_breakdown`) — this is the SAME data, just exploded into one
/// row per language instead of joined into one summary string, so a bare
/// scan-only run emits a real stacked-bar chart instead of empty scaffolding.
/// What: for each repository, prefers declared metrics (`repo.metrics.loc.
/// by_language`) over the measured scan (`repo.scan.by_language`) — same
/// precedence as `fill_profile` — and pushes one `loc_by_tech_row` child per
/// language, largest first, with `tech_pct` computed against the breakdown's
/// own total (so the column always sums to 100% regardless of source). A
/// repository with no language data from either source contributes nothing
/// (omit-empty applies as usual).
/// Test: `reporter_tests.rs::{bare_scan_populates_loc_by_technology,
/// declared_metrics_win_for_loc_by_technology}`.
pub(super) fn inject_loc_by_technology_dataset(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let source: Option<(&[LanguageLoc], Provenance)> = repo
            .metrics
            .as_ref()
            .map(|m| m.loc.by_language.as_slice())
            .filter(|langs| !langs.is_empty())
            .map(|langs| (langs, Provenance::Declared))
            .or_else(|| {
                repo.scan
                    .as_ref()
                    .map(|s| s.by_language.as_slice())
                    .filter(|langs| !langs.is_empty())
                    .map(|langs| (langs, Provenance::Measured))
            });
        let Some((langs, prov)) = source else {
            continue;
        };
        let total: u64 = langs.iter().map(|l| l.loc).sum();
        if total == 0 {
            continue;
        }
        let mut sorted: Vec<&LanguageLoc> = langs.iter().collect();
        sorted.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.language.cmp(&b.language)));
        for lang in sorted {
            let pct = (lang.loc as f64 / total as f64) * 100.0;
            let mut row = Scope::new();
            row.set("app_name", repo.name.clone());
            row.set("tech_name", lang.language.clone());
            row.set("tech_loc", tag(format_thousands(lang.loc), prov));
            row.set("tech_pct", tag(format!("{pct:.0}%"), prov));
            root.push_block("loc_by_tech_row", row);
        }
    }
}

/// Fill the graph-appendix `complexity_distribution` dataset — one row per
/// (application, complexity bucket).
///
/// Why: unlike LoC-by-language, cyclomatic-complexity buckets are NOT
/// computable by the built-in scan (`RepoScan` has no complexity analysis) —
/// they exist only in an externally supplied trusty-analyze metrics JSON
/// (`AnalyzeMetrics::complexity`). Per the honesty rule, a bare scan-only run
/// correctly leaves this dataset empty (omit-empty collapses the table, no
/// chart) rather than fabricating buckets from nothing; this is a genuine data
/// gap, not a bug (see `docs/trusty-review/spec/report-generation.md`).
/// What: for each repository with a non-empty declared `metrics.complexity.
/// buckets`, pushes one `complexity_bucket_row` child per bucket (declared
/// order preserved) with `complexity_pct` computed against the bucket total.
/// Test: `reporter_tests.rs::{complexity_distribution_fills_from_metrics,
/// complexity_distribution_empty_without_metrics}`.
pub(super) fn inject_complexity_distribution_dataset(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let Some(metrics) = &repo.metrics else {
            continue;
        };
        let buckets = &metrics.complexity.buckets;
        let total: u64 = buckets.iter().map(|b| b.count).sum();
        if total == 0 {
            continue;
        }
        for bucket in buckets {
            let pct = (bucket.count as f64 / total as f64) * 100.0;
            let mut row = Scope::new();
            row.set("app_name", repo.name.clone());
            row.set("complexity_bucket", bucket.label.clone());
            row.set(
                "complexity_count",
                tag(bucket.count.to_string(), Provenance::Declared),
            );
            row.set(
                "complexity_pct",
                tag(format!("{pct:.0}%"), Provenance::Declared),
            );
            root.push_block("complexity_bucket_row", row);
        }
    }
}
