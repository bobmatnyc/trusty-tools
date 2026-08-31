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
            ConfluenceSource, ConformanceSource, ContextSource, GithubIssuesSource, JiraSource,
            PrHistorySource, ReviewSubject, gather_external_context, render_sections,
        },
        github::RunMode,
    },
    pipeline::prompt::ReviewContext,
    pipeline::runner::ReviewDeps,
};

/// Gather code context from trusty-search and trusty-analyze.
///
/// Why: context retrieval is the most latency-sensitive step; running the
/// search query and the analyze probe in parallel reduces wall-clock time.
/// What: runs the search query (identifier names + PR title) and the analyze
/// probe concurrently; both degrade gracefully on error (empty context).
/// Test: `gather_context_degrades_gracefully_on_search_failure` in runner_tests.rs;
/// `gather_context_makes_no_apex_retrieval` in this module (#4999).
// #4999: APEX retrieval was dropped by owner ruling (0/69 citations at
// ~0.001 relevance); this path issues exactly one search — the code context.
pub(crate) async fn gather_context(
    config: &ReviewConfig,
    deps: &ReviewDeps,
    identifiers: &[String],
    changed_files: &[String],
    pr_title: &str,
    _pr_description: &str,
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
        // Coverage contrib is populated by the runner AFTER context gathering
        // (step 5b), once the diff is available for new-code extraction (#1014).
        coverage_contrib: None,
        // Caller-supplied PR context (#1618) is injected by the runner AFTER
        // context gathering (from `ReviewInput::caller_context`); gather_context
        // has no access to it, so default to None here.
        pr_description: None,
        pr_discussion: None,
        referenced_code: None,
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
/// (each auto-disabled when its credentials are absent), builds a `ReviewSubject`
/// carrying the PR title + body (#599 Fix 3 — the body is scanned for JIRA ticket
/// keys and folded into each source's query) and the PR number (#1359 — the
/// conformance source threads it into the ISR `IntentQuery::Pr`; `0` in local-diff
/// mode), runs the sources concurrently and
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
    pr_number: u64,
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
        // BACK gate (#1359): surfaces the resolved ticket/spec intent so the LLM
        // can flag explicit method contradictions.  Default DISABLED (needs auth).
        Box::new(ConformanceSource::from_config(
            &cs.conformance,
            run_mode,
            config.clone(),
        )),
        // Prior-PR / file change-history source (T10, #1423).  Default DISABLED.
        Box::new(PrHistorySource::from_config(
            &cs.pr_history,
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
        pr_number,
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
                finish_reason: None,
            })
        }
    }

    /// Search client that records every `search()` call so a test can prove
    /// exactly which retrievals `gather_context` issues.
    struct RecordingSearch {
        calls: std::sync::Mutex<Vec<(String, String)>>,
    }
    #[async_trait]
    impl SearchClient for RecordingSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Err(SearchClientError::Unavailable("unused".into()))
        }
        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Err(SearchClientError::Unavailable("unused".into()))
        }
        async fn search(
            &self,
            index_id: &str,
            query: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            self.calls
                .lock()
                .expect("recording mutex not poisoned")
                .push((index_id.to_string(), query.to_string()));
            Ok(Vec::new())
        }
    }

    /// #4999: `gather_context` performs NO APEX retrieval — the only search it
    /// issues is the code-context query against `config.search_index`.
    ///
    /// Why: the owner ruling dropped APEX retrieval (0/69 citations at ~0.001
    /// relevance).  This test is the regression guard: on the pre-fix code a
    /// configured `TRUSTY_SEARCH_APEX_INDEX` drove a SECOND search (the APEX
    /// cross-query built from title+body); after the removal that env var is
    /// ignored, so there must be exactly one search, targeting the code index —
    /// no APEX client is constructed and no APEX call is made.  Setting the env
    /// var is what makes this fail on the parent commit; without it the old code
    /// short-circuited on an empty index and this proved nothing.
    /// What: with `TRUSTY_SEARCH_APEX_INDEX` set to a sentinel, runs
    /// `gather_context` with identifiers + title + body (the inputs that drove
    /// the APEX cross-query) against a call-recording search client; asserts a
    /// single search call whose index is `config.search_index`.
    /// Test: this test; `#[serial_test::serial]` for env isolation, no network.
    #[tokio::test]
    #[serial_test::serial]
    async fn gather_context_makes_no_apex_retrieval() {
        // Would drive a second (APEX) search on the pre-#4999 code path.
        unsafe {
            std::env::set_var("TRUSTY_SEARCH_APEX_INDEX", "apex-sentinel-#4999");
        }
        let search = Arc::new(RecordingSearch {
            calls: std::sync::Mutex::new(Vec::new()),
        });
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlmApprove),
            verifier: None,
            search: search.clone(),
            analyze: Some(Arc::new(NullAnalyze)),
            dedup: None,
        };
        let config = ReviewConfig::load(None);
        let _ctx = gather_context(
            &config,
            &deps,
            &["foo".to_string()],
            &["src/a.rs".to_string()],
            "PR title",
            "PR body",
        )
        .await;
        unsafe {
            std::env::remove_var("TRUSTY_SEARCH_APEX_INDEX");
        }
        let calls = search.calls.lock().expect("recording mutex not poisoned");
        assert_eq!(
            calls.len(),
            1,
            "exactly one search (code context) must run — no APEX retrieval: {calls:?}"
        );
        assert_eq!(
            calls[0].0, config.search_index,
            "the sole search must target the code-context index, not an APEX index"
        );
    }
}
