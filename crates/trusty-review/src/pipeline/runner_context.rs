//! Context retrieval for the review pipeline.
//!
//! Why: gathering code context from trusty-search and trusty-analyze is a
//! self-contained, latency-sensitive concern; extracting it from `runner.rs`
//! keeps that file under the 500-line cap and lets the retrieval logic be read
//! and tested in isolation.
//! What: exposes `gather_context`, which runs the search query and the analyze
//! probe concurrently and folds both into a `ReviewContext`.  This runs only
//! AFTER the required-context gate (`pipeline::context_gate`, #590) has confirmed
//! the dependencies are reachable (or the operator opted into a degraded run), so
//! a transient per-query error here degrades gracefully to a partial context
//! rather than re-deciding the hard require/skip policy.
//! Test: covered transitively by `runner_tests` (gate + gather paths).

use tracing::{debug, warn};

use crate::{config::ReviewConfig, pipeline::prompt::ReviewContext, pipeline::runner::ReviewDeps};

/// Gather code context from trusty-search and (optionally) trusty-analyze.
///
/// Why: context retrieval is the most latency-sensitive step; running search
/// and analyze in parallel reduces wall-clock time.
/// What: runs the search query (identifier names + PR title) and the analyze
/// probe concurrently; both degrade gracefully on error (empty context).
/// Test: `gather_context_degrades_gracefully_on_search_failure`.
pub(crate) async fn gather_context(
    config: &ReviewConfig,
    deps: &ReviewDeps,
    identifiers: &[String],
    changed_files: &[String],
    pr_title: &str,
) -> ReviewContext {
    // Build a search query from identifiers + changed files.
    let query_parts: Vec<&str> = {
        let mut parts: Vec<&str> = identifiers.iter().map(|s| s.as_str()).collect();
        if !pr_title.is_empty() {
            parts.push(pr_title);
        }
        // Limit to 5 terms to avoid query bloat.
        parts.truncate(5);
        parts
    };
    let query = query_parts.join(" ");

    let search_fut = async {
        if query.is_empty() {
            return Vec::new();
        }
        match deps
            .search
            .search(&config.search_index, &query, Some(8))
            .await
        {
            Ok(results) => {
                debug!(count = results.len(), "search context retrieved");
                results
            }
            Err(e) => {
                warn!("trusty-search unavailable (proceeding with no context): {e}");
                Vec::new()
            }
        }
    };

    let analyze_fut = async {
        let Some(ref analyze) = deps.analyze else {
            return (Vec::new(), Vec::new());
        };
        if !analyze.has_analysis(&config.search_index).await {
            debug!("trusty-analyze not available or has no index — skipping");
            return (Vec::new(), Vec::new());
        }
        // Filter hotspots to changed files only.
        let hotspots = match analyze
            .complexity_hotspots(&config.search_index, Some(10))
            .await
        {
            Ok(h) => h
                .into_iter()
                .filter(|h| changed_files.iter().any(|f| f == &h.file))
                .collect(),
            Err(e) => {
                debug!("complexity_hotspots failed (optional): {e}");
                Vec::new()
            }
        };
        let smells = match analyze.smells(&config.search_index).await {
            Ok(s) => s
                .into_iter()
                .filter(|s| changed_files.iter().any(|f| f == &s.file))
                .collect(),
            Err(e) => {
                debug!("smells failed (optional): {e}");
                Vec::new()
            }
        };
        (hotspots, smells)
    };

    let (search_results, (complexity_hotspots, smells)) = tokio::join!(search_fut, analyze_fut);

    ReviewContext {
        search_results,
        complexity_hotspots,
        smells,
    }
}
