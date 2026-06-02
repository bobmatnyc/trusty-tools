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

use crate::{
    config::ReviewConfig,
    integrations::{
        context::{
            ConfluenceSource, ContextSource, GithubIssuesSource, JiraSource, ReviewSubject,
            gather_external_context, render_sections,
        },
        github::RunMode,
    },
    pipeline::prompt::ReviewContext,
    pipeline::runner::ReviewDeps,
};

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

/// Gather external enrichment context (JIRA / Confluence / GitHub Issues).
///
/// Why: the runner needs the `## Related <source>` markdown to append to the
/// reviewer prompt, but the source set is best built next to the other context
/// gathering so the runner stays a thin loop.  These sources are best-effort /
/// fail-open enrichment — DISTINCT from the REQUIRED trusty-search/trusty-analyze
/// gate (#590): a source outage logs and contributes nothing, it never blocks
/// or skips the review (#550).
/// What: constructs the enabled context sources from `config.context_sources`
/// (each auto-disabled when its credentials are absent), runs them concurrently
/// and fail-open via the orchestrator, and renders the surviving sections to a
/// markdown block.  Returns an empty string when no source contributes.
/// Test: source construction is covered by each source's `from_config` tests;
/// the orchestrator fail-open + ordering + rendering is covered in
/// `integrations::context::orchestrator` tests.
pub(crate) async fn gather_external_context_md(
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    identifiers: &[String],
    changed_files: &[String],
    pr_title: &str,
    run_mode: RunMode,
) -> String {
    let cs = &config.context_sources;
    let sources: Vec<Box<dyn ContextSource>> = vec![
        Box::new(JiraSource::from_config(&cs.jira)),
        Box::new(ConfluenceSource::from_config(&cs.confluence)),
        Box::new(GithubIssuesSource::from_config(
            &cs.github_issues,
            run_mode,
            config.clone(),
        )),
    ];

    // Skip the whole fan-out if nothing is enabled (no creds, no explicit opt-in).
    if !sources.iter().any(|s| s.is_enabled()) {
        debug!("no external context sources enabled — skipping enrichment");
        return String::new();
    }

    let subject = ReviewSubject {
        owner: owner.to_string(),
        repo: repo.to_string(),
        title: pr_title.to_string(),
        changed_files: changed_files.to_vec(),
        identifiers: identifiers.to_vec(),
    };

    let sections = gather_external_context(&sources, &subject).await;
    render_sections(&sections)
}
