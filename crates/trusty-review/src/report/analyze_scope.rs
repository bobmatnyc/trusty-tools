//! Does the analyze data describe the tree this report audits? (#6137)
//!
//! Why: `analyze_adapter::derive_index_id` addresses a trusty-analyze index by
//! the checkout directory's BASENAME, so two checkouts of the same repository
//! at different paths — `/Users/x/Projects/trusty-tools` and
//! `.../repos/local/trusty-tools` — resolve to the same index id. The daemon
//! answers with whichever one it indexed. That is how one due-diligence run
//! stamped twenty complexity findings ⁽ᵐ⁾ measured whose component paths all
//! pointed at a different checkout on a different branch: the report presented
//! another tree's code as a measurement of the audited one. An out-of-tree path
//! is stale-index evidence, not measurement, and there is no way to tell from
//! the data alone which commit it describes.
//! What: [`out_of_tree_components`] reports the ABSOLUTE component paths in a
//! fetched [`AnalyzeMetrics`] that do not live under the audited checkout.
//! Relative paths are in-tree by construction (they are read against the
//! checkout root) and never counted. [`stale_index_gap`] words the Gaps &
//! Caveats line for a rejection.
//! Test: `analyze_scope_tests.rs`.

use std::path::Path;

use super::metrics::AnalyzeMetrics;

/// The absolute component paths in `metrics` that fall outside `root`.
///
/// Why/What: see the module doc. Comparison is a plain path-prefix test on the
/// two paths as given — neither is canonicalised, because canonicalising would
/// resolve a symlinked checkout onto its target and call a genuinely different
/// tree in-tree. Returned in first-seen order, de-duplicated, so the gap line
/// can name a few examples deterministically.
/// Test: `analyze_scope_tests::{finds_a_component_outside_the_audited_tree,
/// relative_components_are_always_in_tree,
/// an_in_tree_absolute_component_is_not_flagged}`.
pub fn out_of_tree_components(metrics: &AnalyzeMetrics, root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for finding in &metrics.findings {
        // A component can carry a `:line` suffix; the path is what precedes it.
        let path = finding
            .component
            .split_once(':')
            .map_or(finding.component.as_str(), |(p, _)| p);
        if path.is_empty() || !Path::new(path).is_absolute() || Path::new(path).starts_with(root) {
            continue;
        }
        if !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

/// Admit fetched metrics for the audited checkout, or reject them as stale.
///
/// Why: the single decision point — `enrich_with_analyze_gaps` should read as
/// "accept or name the gap", not carry the path arithmetic. Keeping it here
/// also keeps `analyze_adapter.rs` under the 500-SLOC production cap.
/// What: `Ok(metrics)` when no component path falls outside `root`; otherwise
/// `Err(`[`stale_index_gap`]`)`, so the caller drops the metrics entirely —
/// complexity distribution included, since it came from the same index. Both
/// outcomes print one operator line to stderr.
/// Test: `analyze_scope_tests::{accept_passes_in_tree_metrics,
/// accept_rejects_metrics_describing_another_checkout}`.
pub fn accept(
    repo: &str,
    index_id: &str,
    root: &Path,
    metrics: AnalyzeMetrics,
) -> Result<AnalyzeMetrics, String> {
    let stale = out_of_tree_components(&metrics, root);
    if stale.is_empty() {
        eprintln!(
            "[trusty-review report] --analyze: populated metrics for '{repo}' from index '{index_id}'"
        );
        return Ok(metrics);
    }
    eprintln!(
        "[trusty-review report] --analyze: rejected metrics for '{repo}' — index '{index_id}' \
         describes a checkout outside {} ({} path(s))",
        root.display(),
        stale.len()
    );
    Err(stale_index_gap(repo, index_id, &stale))
}

/// How many example paths the gap line names before it stops.
const EXAMPLE_PATHS: usize = 2;

/// What an operator does about a stale index — stated once, read everywhere.
///
/// Why (#6080): Key Facts wrote its own remedy for a missing complexity
/// profile ("re-run with `--analyze`") while §9 stated the real one for this
/// case, and the two contradicted each other on the same page. Every surface
/// that reports the gap now renders this string.
pub const STALE_INDEX_REMEDY: &str = "Re-index the audited checkout under a distinct index id.";

/// What an operator does when no analyze data reached a repository at all.
///
/// Why: the counterpart of [`STALE_INDEX_REMEDY`], homed beside it so the two
/// analyze-lane remedies stay one lookup rather than two (#6080).
pub const NO_ANALYZE_DATA_REMEDY: &str =
    "Build a trusty-analyze index for this checkout and re-run with `--analyze`.";

/// The Gaps & Caveats line for a repository whose analyze data described a
/// different checkout.
///
/// Why: the rejection must be visible as a NAMED gap. A section that silently
/// loses its complexity data reads as "nothing to report" — the exact failure
/// #5239 exists to prevent — and here the alternative is worse than silence,
/// because the data it replaced was wrong and marked measured.
/// What: names the repository, the count, and up to [`EXAMPLE_PATHS`] example
/// paths, then states the operator's remedy.
/// Test: `analyze_scope_tests::gap_line_names_the_repository_and_a_path`.
pub fn stale_index_gap(repo: &str, index_id: &str, paths: &[String]) -> String {
    let examples: Vec<&str> = paths
        .iter()
        .take(EXAMPLE_PATHS)
        .map(String::as_str)
        .collect();
    format!(
        "trusty-analyze data rejected as stale for {repo} — the index '{index_id}' describes a \
         different checkout ({} of its component paths fall outside the audited tree, e.g. {}). \
         Complexity distribution and analyze-derived findings are therefore not assessed for \
         this application, not clean. {STALE_INDEX_REMEDY}",
        paths.len(),
        examples.join(", ")
    )
}

#[cfg(test)]
#[path = "analyze_scope_tests.rs"]
mod tests;
