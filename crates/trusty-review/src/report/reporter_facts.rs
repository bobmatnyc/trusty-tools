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
//! complexity distribution), re-aggregated across every repository. Density
//! rows take the same source precedence `reporter_fill::fill_profile` uses —
//! `AnalyzeMetrics` when a `--analyze` run supplied it, else the built-in
//! `RepoScan` every model build produces (#6029). Complexity has no scan
//! equivalent, and author count / work estimate / trajectory come from tga's
//! authorship artifact (#5453, completed by
//! `reporter_authorship::fill_authorship_facts`); when any of those inputs is
//! genuinely absent, its row states the named reason rather than dropping out
//! of the table. tga has no effort-estimation metric today (verified:
//! `story_points` fields are documented placeholders, always zero) — the work
//! estimate is a named gap in every run rather than an invented figure.
//! Test: `reporter_facts_tests.rs`.

use super::fill::Scope;
use super::metrics::LanguageLoc;
use super::model::{ReportModel, RepositoryReport};
use super::provenance::{Provenance, tag};

// #6029: a genuinely absent input states WHY it is absent, in its own row.
// Before this, every such row rendered the honesty marker, the omit-empty pass
// dropped all of them, and an empty table collapsed the whole Key Facts block
// to `_No data available — see Gaps & Caveats._` — even in a run whose scan
// had measured 1.5M LoC that never reached the block at all.

/// The complexity row's text when no `--analyze` metrics reached this run.
const GAP_COMPLEXITY: &str =
    "Not computed — this run carried no trusty-analyze metrics; re-run with `--analyze`";
/// The author-count and trajectory rows' text when no authorship artifact loaded.
const GAP_AUTHORSHIP: &str = "Not computed — this run carried no tga authorship artifact";
/// The work-estimate row's text; no upstream effort metric exists yet.
const GAP_WORK_ESTIMATE: &str = "Not computed — no upstream effort-estimation metric exists";

/// Fill the Key Facts scalars from data already on `model`.
///
/// Why: single entry point `reporter::build_scope` calls; keeps the
/// aggregation logic out of `reporter.rs` (SLOC pressure — see its module
/// doc) and testable in isolation.
/// What: sums LoC/file counts across every repository and ranks languages by
/// aggregate LoC, reading each repository's `AnalyzeMetrics` when present and
/// falling back to its `RepoScan` otherwise (#6029); then re-projects the
/// merged complexity distribution as a compact percentage summary. A density
/// scalar is set only when the underlying data is non-empty, so an all-remote
/// manifest states no figure rather than a fabricated zero — but it then
/// states the named reason instead, never a blank row.
/// Test: `reporter_facts_tests::{fills_aggregate_facts,
/// scan_only_model_fills_density_facts, empty_model_is_all_gaps}`.
pub fn fill_key_facts(root: &mut Scope, model: &ReportModel) {
    let total_loc: u64 = model.repositories.iter().map(repo_loc).sum();
    if total_loc > 0 {
        root.set(
            "facts_total_loc",
            tag(total_loc.to_string(), Provenance::Measured),
        );
    }

    let total_files: u64 = model.repositories.iter().map(repo_files).sum();
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

    match complexity_profile(model) {
        Some(summary) => root.set(
            "facts_complexity_summary",
            tag(summary, Provenance::Measured),
        ),
        // #6029: the complexity row names its missing input instead of
        // vanishing — the scan has no complexity equivalent to fall back to.
        None => root.set(
            "facts_complexity_summary",
            tag(GAP_COMPLEXITY, Provenance::NotStated),
        ),
    }

    // #6029: authorship rows state their named gap here; when a repository's
    // artifact did load, `reporter_authorship::fill_authorship_facts` runs
    // next and overwrites both with the real figures.
    root.set(
        "facts_author_count",
        tag(GAP_AUTHORSHIP, Provenance::NotStated),
    );
    root.set(
        "facts_trajectory",
        tag(GAP_AUTHORSHIP, Provenance::NotStated),
    );
    root.set(
        "facts_work_estimate",
        tag(GAP_WORK_ESTIMATE, Provenance::NotStated),
    );
}

/// This repository's total LoC — declared metrics first, else the repo scan.
///
/// Why: `RepoScan` is produced on every model build while `AnalyzeMetrics`
/// arrives only from a `--analyze` run, so reading metrics alone left the
/// whole Key Facts block empty on an ordinary sweep (#6029). The precedence
/// mirrors `reporter_fill::fill_profile`, which already made this choice for
/// the per-application Profile table.
/// What: `0` when neither source carries a positive figure.
/// Test: `reporter_facts_tests::{scan_only_model_fills_density_facts,
/// metrics_win_over_scan_in_key_facts}`.
fn repo_loc(repo: &RepositoryReport) -> u64 {
    repo.metrics
        .as_ref()
        .map(|m| m.loc.total)
        .filter(|n| *n > 0)
        .or_else(|| repo.scan.as_ref().map(|s| s.total_loc).filter(|n| *n > 0))
        .unwrap_or(0)
}

/// This repository's tracked file count, with the same precedence as [`repo_loc`].
fn repo_files(repo: &RepositoryReport) -> u64 {
    repo.metrics
        .as_ref()
        .map(|m| m.counts.files)
        .filter(|n| *n > 0)
        .or_else(|| repo.scan.as_ref().map(|s| s.file_count).filter(|n| *n > 0))
        .unwrap_or(0)
}

/// This repository's per-language LoC breakdown, with the same precedence.
fn repo_languages(repo: &RepositoryReport) -> &[LanguageLoc] {
    match repo
        .metrics
        .as_ref()
        .filter(|m| !m.loc.by_language.is_empty())
    {
        Some(m) => &m.loc.by_language,
        None => repo.scan.as_ref().map_or(&[], |s| &s.by_language),
    }
}

/// The top 5 languages by aggregate LoC across every repository.
fn aggregate_languages(model: &ReportModel) -> Vec<String> {
    let mut totals: Vec<(String, u64)> = Vec::new();
    for lang in model.repositories.iter().flat_map(repo_languages) {
        match totals.iter_mut().find(|(name, _)| *name == lang.language) {
            Some(entry) => entry.1 += lang.loc,
            None => totals.push((lang.language.clone(), lang.loc)),
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
