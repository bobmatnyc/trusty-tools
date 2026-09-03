//! The §1 "Analysis methodology" row, generated from what actually ran (#6675).
//!
//! Why: the row was fixed template text asserting that trusty-analyze and
//! trusty-search were both used, rendered identically whether either one
//! contributed. The 2026-09-02 dogfood run fell back to scan (the repository was
//! not indexed) and every traced finding came back `IndexAbsent` — zero verified
//! symbol anchors — and the report still told a reader who stopped at §1 that
//! AST/complexity analysis and search-grounded tracing had happened. Only §9
//! disclosed the degradation.
//! What: [`analysis_methodology`] reads the provenance the run already recorded
//! — `RepositoryReport::metrics`/`analyze_gap` for the analyze lane, and the
//! investigation's own trace counts for the search lane — and states each lane's
//! actual contribution. A lane that contributed nothing says so in the row
//! itself, so no unqualified tool-use claim can be rendered for a run that made
//! none.
//! Test: `methodology_tests.rs`, and `tests/report_methodology_6675.rs` for the
//! rendered CAST row.

use super::model::ReportModel;

/// The one thing every run does regardless of which daemon answered: it reads
/// the checked-out sources itself.
const BASE: &str = "Repository inspection over the checked-out sources";

/// The §1 methodology sentence for this run.
///
/// Why: the single place the row's text is produced, so the claim and the
/// recorded provenance cannot drift apart — a lane is named as used only when
/// this run's own record says it contributed.
/// What: `BASE`, then one clause per lane. The analyze clause counts the
/// repositories carrying trusty-analyze metrics with no recorded gap; the search
/// clause counts anchored traces against traced candidates across the
/// investigation. Clauses are joined with `; `.
/// Test: `methodology_tests::{both_lanes_absent_claim_neither,
/// a_full_run_names_both_lanes, a_partial_analyze_lane_states_the_fraction}`.
pub fn analysis_methodology(model: &ReportModel) -> String {
    let total = model.repositories.len();
    // #6675: a repository whose analyze fetch recorded a gap did not contribute,
    // whatever else is on it — that gap is the fallback this row used to hide.
    let analyzed = model
        .repositories
        .iter()
        .filter(|r| r.analyze_gap.is_none() && r.metrics.is_some())
        .count();
    let (candidates, anchored) = trace_counts(model);

    let analyze = match analyzed {
        0 => format!(
            "trusty-analyze contributed no data to this run ({total} application(s) assessed \
             without it)"
        ),
        n if n == total => {
            format!("trusty-analyze structural metrics for all {total} application(s)")
        }
        n => format!("trusty-analyze structural metrics for {n} of {total} application(s)"),
    };
    let search = match (candidates, anchored) {
        (0, _) => "trusty-search symbol tracing did not run".to_string(),
        (c, 0) => {
            format!("trusty-search resolved no symbol anchor for any of the {c} traced finding(s)")
        }
        (c, a) => format!("trusty-search anchored {a} of {c} traced finding(s)"),
    };
    format!("{BASE}; {analyze}; {search}")
}

/// This run's traced-candidate and anchored-trace totals across every repository.
///
/// Why: the search lane's contribution is exactly what its traces anchored — a
/// run where every lookup returned `IndexAbsent` traced candidates and anchored
/// none, and the row must be able to say that.
/// What: `(candidates, anchored)`, both `0` when the trace pass never ran.
/// Test: `methodology_tests::both_lanes_absent_claim_neither`.
fn trace_counts(model: &ReportModel) -> (usize, usize) {
    let Some(inv) = &model.investigation else {
        return (0, 0);
    };
    inv.repos
        .iter()
        .filter_map(|r| r.traces.as_ref())
        .fold((0, 0), |(c, a), t| (c + t.candidates, a + t.assembled))
}

#[cfg(test)]
#[path = "methodology_tests.rs"]
mod tests;
