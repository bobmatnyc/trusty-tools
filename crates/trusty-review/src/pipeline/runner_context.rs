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
        apex_context::fetch_apex_context,
        context::{
            ConfluenceSource, ContextSource, GithubIssuesSource, JiraSource, ReviewSubject,
            gather_external_context, render_sections,
        },
        github::RunMode,
    },
    pipeline::prompt::ReviewContext,
    pipeline::runner::ReviewDeps,
};

/// Gather code context from trusty-search, trusty-analyze, and (optionally) APEX.
///
/// Why: context retrieval is the most latency-sensitive step; running search,
/// analyze, and APEX in parallel reduces wall-clock time.
/// What: runs the search query (identifier names + PR title), the analyze probe,
/// and the APEX/KB query concurrently; all degrade gracefully on error (empty
/// context).  APEX is disabled when `config.apex_index` is empty.
/// The `pr_description` parameter is used as part of the APEX cross-query
/// (title + description gives the richest product-spec signal).
/// Test: `gather_context_degrades_gracefully_on_search_failure` in runner_tests.rs;
/// `gather_context_apex_failure_is_fail_open` in this module.
pub(crate) async fn gather_context(
    config: &ReviewConfig,
    deps: &ReviewDeps,
    identifiers: &[String],
    changed_files: &[String],
    pr_title: &str,
    pr_description: &str,
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

    // APEX cross-query: PR title + description (best product-spec signal).
    // Fall back to the first few changed-file paths when both are empty.
    let apex_fut = async {
        let cross_query = build_apex_cross_query(pr_title, pr_description, changed_files);
        fetch_apex_context(
            deps.search.as_ref(),
            &config.apex_index,
            &config.apex_path_prefixes,
            &cross_query,
        )
        .await
    };

    let (search_results, (complexity_hotspots, smells), apex_results) =
        tokio::join!(search_fut, analyze_fut, apex_fut);

    ReviewContext {
        search_results,
        complexity_hotspots,
        smells,
        apex_results,
        // Coverage contrib is populated by the runner AFTER context gathering
        // (step 5b), once the diff is available for new-code extraction (#1014).
        coverage_contrib: None,
    }
}

/// Build the APEX cross-query string from PR metadata.
///
/// Why: the APEX search needs the richest available signal for product-spec
/// matching; `title + "\n" + description` mirrors the incumbent's
/// `title + "\n" + description[:500]` query construction.  When both are empty
/// (e.g. local-diff mode with no PR context), the first few changed-file paths
/// provide a weak fallback signal rather than sending a blank query (which
/// `fetch_apex_context` would silently skip anyway).
/// What: returns `"{title}\n{description}".trim()`, or a space-joined list of
/// up to 6 changed-file paths when the title+description pair is blank.
/// Test: `build_apex_cross_query_*` in this module.
fn build_apex_cross_query(
    pr_title: &str,
    pr_description: &str,
    changed_files: &[String],
) -> String {
    let combined = format!("{}\n{}", pr_title.trim(), pr_description.trim());
    let trimmed = combined.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    // Fallback: join up to 6 changed file paths.
    changed_files
        .iter()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
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
/// (each auto-disabled when its credentials are absent), builds a `ReviewSubject`
/// carrying the PR title + body (#599 Fix 3 — the body is scanned for JIRA ticket
/// keys and folded into each source's query), runs the sources concurrently and
/// fail-open via the orchestrator, and renders the surviving sections to a
/// markdown block.  Returns an empty string when no source contributes.
/// Test: source construction is covered by each source's `from_config` tests;
/// the orchestrator fail-open + ordering + rendering is covered in
/// `integrations::context::orchestrator` tests.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn gather_external_context_md(
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    identifiers: &[String],
    changed_files: &[String],
    pr_title: &str,
    pr_body: &str,
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
        body: pr_body.to_string(),
        changed_files: changed_files.to_vec(),
        identifiers: identifiers.to_vec(),
    };

    let sections = gather_external_context(&sources, &subject).await;
    render_sections(&sections)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        integrations::{
            analyze_client::{AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell},
            search_client::{
                HealthResponse, IndexInfo, SearchClient, SearchClientError, SearchResult,
            },
        },
        pipeline::runner::ReviewDeps,
    };
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FailSearch;
    #[async_trait]
    impl SearchClient for FailSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Err(SearchClientError::Unavailable("down".into()))
        }
        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Err(SearchClientError::Unavailable("down".into()))
        }
        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Err(SearchClientError::Transport("refused".into()))
        }
    }

    struct NullAnalyze;
    #[async_trait]
    impl crate::integrations::analyze_client::AnalyzeClient for NullAnalyze {
        async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
            Err(AnalyzeClientError::Unavailable("down".into()))
        }
        async fn has_analysis(&self, _: &str) -> bool {
            false
        }
        async fn complexity_hotspots(
            &self,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
            Ok(vec![])
        }
        async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
            Ok(vec![])
        }
    }

    use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};

    struct FakeLlmApprove;
    #[async_trait]
    impl LlmProvider for FakeLlmApprove {
        fn name(&self) -> &str {
            "fake"
        }
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#.into(),
                model: req.model,
                input_tokens: 1,
                output_tokens: 1,
                latency_ms: 0,
                cost_usd: 0.0,
            })
        }
    }

    fn make_deps() -> ReviewDeps {
        ReviewDeps {
            llm: Arc::new(FakeLlmApprove),
            verifier: None,
            search: Arc::new(FailSearch),
            analyze: Some(Arc::new(NullAnalyze)),
            dedup: None,
        }
    }

    /// gather_context with failing search and APEX index set ⇒ empty apex_results
    /// (fail-open, review proceeds).
    ///
    /// Why: REV-420 requires APEX to be fail-open; a search failure must produce
    /// empty apex_results without blocking gather_context.
    /// What: configures apex_index in config, uses FailSearch; asserts
    /// apex_results is empty and the function returns (no panic).
    /// Test: this test; no network.
    #[tokio::test]
    async fn gather_context_apex_failure_is_fail_open() {
        let mut config = ReviewConfig::load(None);
        config.apex_index = "apex-index".to_string();
        config.apex_path_prefixes = vec!["apex/".to_string()];
        let deps = make_deps();
        let ctx = gather_context(&config, &deps, &[], &[], "PR title", "PR body").await;
        assert!(
            ctx.apex_results.is_empty(),
            "APEX search failure must produce empty results (fail-open)"
        );
    }

    /// build_apex_cross_query uses title+description when both non-empty.
    ///
    /// Why: the richest APEX signal is title + description; the fallback to
    /// changed-file paths must only trigger when both are empty.
    /// What: asserts the combined string is returned when inputs are non-empty.
    /// Test: this test; no network.
    #[test]
    fn build_apex_cross_query_uses_title_and_body() {
        let q = build_apex_cross_query("Fix auth bug", "Closes PROJ-1", &[]);
        assert_eq!(q, "Fix auth bug\nCloses PROJ-1");
    }

    /// build_apex_cross_query falls back to changed files when title+body empty.
    ///
    /// Why: local-diff mode has no PR title/body; changed-file paths provide a
    /// weak but non-blank fallback so the query is not silently skipped.
    /// What: passes empty title/body with three changed files; asserts all three
    /// appear in the result.
    /// Test: this test; no network.
    #[test]
    fn build_apex_cross_query_falls_back_to_changed_files() {
        let files = vec!["src/a.rs".into(), "src/b.rs".into()];
        let q = build_apex_cross_query("", "", &files);
        assert_eq!(q, "src/a.rs src/b.rs");
    }

    /// build_apex_cross_query returns empty when all inputs are blank.
    ///
    /// Why: empty query ⇒ fetch_apex_context short-circuits (no search call);
    /// the cross-query builder must return empty so the guard triggers.
    /// What: all-blank inputs → empty string.
    /// Test: this test; no network.
    #[test]
    fn build_apex_cross_query_empty_when_all_blank() {
        assert_eq!(build_apex_cross_query("", "  ", &[]), "");
    }
}
