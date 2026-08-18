//! Code Quality & Architecture and Security Posture: deterministic
//! re-projections of already-loaded `AnalyzeMetrics` data (#6004).
//!
//! Why: both new sections restate data the model already carries — nothing
//! here is a new data source. Code Quality re-projects the complexity
//! distribution, LoC/tech-stack, and maintainability (refactor/code-smell)
//! findings already used by §3/§4/§5; Security Posture re-projects the SAME
//! RED/AMBER findings §5 lists, grouped by tool/domain — the promoted, now
//! actually-filled, successor to the old §6.1 `security_violations_table`
//! block (previously template scaffolding with no reporter fill code at
//! all). Splitting this out of `reporter.rs` keeps that file under the
//! 500-SLOC production cap.
//! What: [`push_code_quality_rows`] and [`push_security_violation_rows`],
//! both called from `reporter::build_scope`.
//! Test: `reporter_codesec_tests.rs`.

use super::fill::Scope;
use super::metrics::{AnalyzeMetrics, Severity};
use super::model::ReportModel;
use super::provenance::{Provenance, tag};

/// `MetricFinding.category` value `analyze_adapter::refactor_finding` always
/// writes — refactor/code-smell findings are maintainability by construction,
/// so this is the seam between "code quality" and "security posture" below.
const MAINTAINABILITY_CATEGORY: &str = "maintainability";

/// Push one `code_quality_row` per repository carrying metrics.
///
/// Why: re-projects complexity distribution, tech-stack/LoC, and the
/// maintainability-finding count into a dedicated section (#6004) rather than
/// requiring a reader to cross-reference §3/§4/§5.
/// What: `cq_loc`/`cq_tech`/`cq_complexity` restate per-app profile data;
/// `cq_maintainability_count` counts non-GREEN `maintainability`-category
/// findings — a real zero is a stated fact (tagged, not omitted), never a gap.
/// A repository with no metrics at all is skipped (its row would be 100%
/// honesty markers, which the omit-empty pass would drop anyway).
/// Test: `reporter_codesec_tests::code_quality_rows_reproject_metrics`.
pub fn push_code_quality_rows(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let Some(m) = &repo.metrics else { continue };
        let mut row = Scope::new();
        row.set("cq_app_name", repo.name.clone());
        if m.loc.total > 0 {
            row.set("cq_loc", tag(m.loc.total.to_string(), Provenance::Measured));
        }
        let langs = m.primary_languages(3);
        if !langs.is_empty() {
            row.set("cq_tech", tag(langs.join(", "), Provenance::Measured));
        }
        if let Some(summary) = bucket_summary(m) {
            row.set("cq_complexity", tag(summary, Provenance::Measured));
        }
        let maint = m
            .findings
            .iter()
            .filter(|f| f.severity != Severity::Green && f.category == MAINTAINABILITY_CATEGORY)
            .count();
        row.set(
            "cq_maintainability_count",
            tag(maint.to_string(), Provenance::Measured),
        );
        root.push_block("code_quality_row", row);
    }
}

/// Push one `security_violations_table` row per (application, tool/domain)
/// with at least one RED/AMBER finding whose category is NOT maintainability.
///
/// Why (#6004): promotes §6.1 out of Risk Registers into its own top-level
/// Security Posture section — the block name and column shape are unchanged
/// (`app_name`/`violation_domain`/`violation_count`) so this is a straight
/// re-projection of the RED/AMBER findings §5 already lists, grouped by the
/// producing tool. This is the first reporter code that actually fills that
/// block; before #6004 it was unfilled template scaffolding.
/// What: excludes GREEN (no-green-analysis rule) and `maintainability`
/// (Code Quality's finding class, not a lint/security domain).
/// Test: `reporter_codesec_tests::security_rows_group_by_domain_excluding_maintainability`.
pub fn push_security_violation_rows(root: &mut Scope, model: &ReportModel) {
    for repo in &model.repositories {
        let Some(m) = &repo.metrics else { continue };
        let mut counts: Vec<(String, u64)> = Vec::new();
        for f in &m.findings {
            if f.severity == Severity::Green || f.category == MAINTAINABILITY_CATEGORY {
                continue;
            }
            match counts.iter_mut().find(|(c, _)| *c == f.category) {
                Some(entry) => entry.1 += 1,
                None => counts.push((f.category.clone(), 1)),
            }
        }
        for (domain, count) in counts {
            let mut row = Scope::new();
            row.set("app_name", repo.name.clone());
            row.set("violation_domain", tag(domain, Provenance::Measured));
            row.set(
                "violation_count",
                tag(count.to_string(), Provenance::Measured),
            );
            root.push_block("security_violations_table", row);
        }
    }
}

/// A compact "label: count (pct%)" complexity summary for one repository's
/// `AnalyzeMetrics.complexity` — the per-app counterpart of
/// `reporter_facts::complexity_profile`'s engagement-wide merge.
fn bucket_summary(m: &AnalyzeMetrics) -> Option<String> {
    let total: u64 = m.complexity.buckets.iter().map(|b| b.count).sum();
    if total == 0 {
        return None;
    }
    let parts: Vec<String> = m
        .complexity
        .buckets
        .iter()
        .map(|b| {
            let pct = (b.count as f64 / total as f64) * 100.0;
            format!("{}: {} ({pct:.0}%)", b.label, b.count)
        })
        .collect();
    Some(parts.join("; "))
}

#[cfg(test)]
#[path = "reporter_codesec_tests.rs"]
mod tests;
