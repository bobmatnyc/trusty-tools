//! Key Facts block: deterministic frontloaded codebase facts (#6004).
//!
//! Why: owner ruling 2026-08-18 — "We should frontload facts: density,
//! complexity, number of authors, how much work we estimate and its
//! trajectory by month." The block sits ahead of the executive summary so a
//! reader gets the numbers before the narrative. Every row here is
//! deterministic and never LLM-touched — it is exactly the anchor set the
//! numeric guardrail (`synthesize_guard::allowed_numbers`) already validates
//! narrative prose against, so this block strengthens that design rather
//! than duplicating it.
//! What: [`fill_key_facts`] sets the `facts_*` scalars on the root [`Scope`]
//! from data already loaded onto the model (LoC, file counts, languages,
//! complexity distribution — all from `AnalyzeMetrics`, re-aggregated across
//! every repository). Author count, work-volume estimate, and monthly
//! trajectory are PR B's authorship-artifact fields (#5453) — left unset here
//! so each renders as the honesty marker and surfaces its own line under
//! Gaps & Caveats until that data exists, per the omit-empty row rule
//! (`polish.rs::process_table`). tga has no effort-estimation metric today
//! (verified: `story_points` fields are documented placeholders, always
//! zero) — `facts_work_estimate` stays unset rather than inventing one.
//! Test: `reporter_facts_tests.rs`.

use super::fill::Scope;
use super::model::ReportModel;
use super::provenance::{Provenance, tag};

/// Fill the Key Facts scalars from data already on `model`.
///
/// Why: single entry point `reporter::build_scope` calls; keeps the
/// aggregation logic out of `reporter.rs` (SLOC pressure — see its module
/// doc) and testable in isolation.
/// What: sums LoC/file counts across every repository with metrics, ranks
/// languages by aggregate LoC, and re-projects the merged complexity
/// distribution as a compact percentage summary. Each scalar is set only
/// when the underlying data is non-empty, so an all-remote or metrics-free
/// manifest leaves every row as a named gap rather than a stated zero.
/// Test: `reporter_facts_tests::{fills_aggregate_facts, empty_model_is_all_gaps}`.
pub fn fill_key_facts(root: &mut Scope, model: &ReportModel) {
    let total_loc: u64 = model
        .repositories
        .iter()
        .filter_map(|r| r.metrics.as_ref())
        .map(|m| m.loc.total)
        .sum();
    if total_loc > 0 {
        root.set(
            "facts_total_loc",
            tag(total_loc.to_string(), Provenance::Measured),
        );
    }

    let total_files: u64 = model
        .repositories
        .iter()
        .filter_map(|r| r.metrics.as_ref())
        .map(|m| m.counts.files)
        .sum();
    if total_files > 0 {
        root.set(
            "facts_total_files",
            tag(total_files.to_string(), Provenance::Measured),
        );
    }

    let langs = aggregate_languages(model);
    if !langs.is_empty() {
        root.set(
            "facts_languages",
            tag(langs.join(", "), Provenance::Measured),
        );
    }

    if let Some(summary) = complexity_profile(model) {
        root.set(
            "facts_complexity_summary",
            tag(summary, Provenance::Measured),
        );
    }

    // #5453: facts_author_count / facts_work_estimate / facts_trajectory are
    // intentionally left unset in PR A — see module doc. PR B's authorship
    // artifact completes them.
}

/// The top 5 languages by aggregate LoC across every repository with metrics.
fn aggregate_languages(model: &ReportModel) -> Vec<String> {
    let mut totals: Vec<(String, u64)> = Vec::new();
    for m in model.repositories.iter().filter_map(|r| r.metrics.as_ref()) {
        for lang in &m.loc.by_language {
            match totals.iter_mut().find(|(name, _)| *name == lang.language) {
                Some(entry) => entry.1 += lang.loc,
                None => totals.push((lang.language.clone(), lang.loc)),
            }
        }
    }
    totals.sort_by_key(|(_, loc)| std::cmp::Reverse(*loc));
    totals.into_iter().take(5).map(|(name, _)| name).collect()
}

/// A compact "label: count (pct%)" complexity summary, merged across every
/// repository's `AnalyzeMetrics.complexity` buckets by matching label.
///
/// Why: a single engagement-wide profile is what "frontload facts" asks for;
/// per-application detail already lives in §4's Health-Factor Scores and the
/// Graph Appendix's `complexity_distribution` dataset.
/// What: `None` when no repository contributed any bucket (nothing to show).
fn complexity_profile(model: &ReportModel) -> Option<String> {
    let mut buckets: Vec<(String, u64)> = Vec::new();
    for m in model.repositories.iter().filter_map(|r| r.metrics.as_ref()) {
        for b in &m.complexity.buckets {
            match buckets.iter_mut().find(|(label, _)| *label == b.label) {
                Some(entry) => entry.1 += b.count,
                None => buckets.push((b.label.clone(), b.count)),
            }
        }
    }
    let total: u64 = buckets.iter().map(|(_, c)| c).sum();
    if total == 0 {
        return None;
    }
    let parts: Vec<String> = buckets
        .iter()
        .map(|(label, count)| {
            let pct = (*count as f64 / total as f64) * 100.0;
            format!("{label}: {count} ({pct:.0}%)")
        })
        .collect();
    Some(parts.join("; "))
}

#[cfg(test)]
#[path = "reporter_facts_tests.rs"]
mod tests;
