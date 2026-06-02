//! APEX / KB indexed context retrieval (Phase 6 PR-B, REV-420, #550).
//!
//! Why: the review LLM produces higher-quality verdicts when it can see the
//! product specification that the changed code is supposed to implement.  Rather
//! than a bespoke APEX adapter, Option A (spec REV-420) reuses the existing
//! trusty-search index — APEX docs are indexed alongside code in the same index
//! and distinguished by corpus-relative path prefixes (e.g. `apex/`, `specs/`).
//! This module adds no new network dependency: it reuses `SearchClient`.
//!
//! What: exposes `ApexContextResult` (a single APEX snippet) and
//! `fetch_apex_context`, which queries the configured `apex_index`, applies
//! optional path-prefix filtering, and returns up to `MAX_APEX_RESULTS`
//! snippets.  All errors are fail-open: a search failure produces an empty
//! result and a `warn!` log; it never blocks the review.
//!
//! Test: unit tests in this module cover: empty index ⇒ no-op, search error
//! ⇒ fail-open, path-prefix filter, MAX_APEX_RESULTS cap, snippet truncation,
//! empty/whitespace query ⇒ no-op.

use tracing::warn;

use crate::{
    config::constants::{MAX_APEX_QUERY_CHARS, MAX_APEX_RESULTS, MAX_APEX_SNIPPET_CHARS},
    integrations::{context::truncate_on_char_boundary, search_client::SearchClient},
};

// ─── Result type ─────────────────────────────────────────────────────────────

/// A single APEX/KB context item retrieved from trusty-search.
///
/// Why: normalises the `SearchResult` wire type into the minimal shape needed
/// by the prompt builder, isolating the prompt module from the search wire
/// format.
/// What: `file` is the corpus-relative path, `snippet` is a (possibly
/// truncated) excerpt, `score` is the relevance score, and `start_line` is the
/// optional 1-based line number.
/// Test: `apex_context_result_round_trip` in this module.
#[derive(Debug, Clone, PartialEq)]
pub struct ApexContextResult {
    /// Corpus-relative file path (e.g. `apex/auth-spec.md`).
    pub file: String,
    /// Spec/doc excerpt, truncated to `MAX_APEX_SNIPPET_CHARS` chars.
    pub snippet: String,
    /// Combined BM25+vector relevance score from trusty-search.
    pub score: f32,
    /// Optional 1-based starting line number in the file.
    pub start_line: Option<u32>,
}

// ─── Core function ────────────────────────────────────────────────────────────

/// Retrieve APEX/KB context from the configured trusty-search index.
///
/// Why: provides the reviewer with the product specification for the code under
/// review, improving verdict accuracy without adding a new network dependency
/// (REV-420 Option A: same index as code context, distinguished by path prefix).
/// What:
///   1. Returns immediately (empty) when `apex_index` is empty or `cross_query`
///      is blank — APEX is disabled or there is no usable signal.
///   2. Truncates `cross_query` to `MAX_APEX_QUERY_CHARS` (UTF-8 safe) and
///      issues a search against `apex_index` with a slightly inflated `top_k`
///      so that post-filter we still have up to `MAX_APEX_RESULTS` hits.
///   3. Applies `apex_path_prefixes` filtering: when non-empty, retains only
///      results whose `file` starts with one of the prefixes.
///   4. Truncates to `MAX_APEX_RESULTS` and maps to `ApexContextResult`
///      (capping snippets at `MAX_APEX_SNIPPET_CHARS`).
///   5. On any `search()` error: `warn!` and return empty — NEVER propagate
///      (fail-open, matches the analyse degradation pattern in runner_context).
///
/// Test: see unit tests below.
pub async fn fetch_apex_context(
    search: &dyn SearchClient,
    apex_index: &str,
    apex_path_prefixes: &[String],
    cross_query: &str,
) -> Vec<ApexContextResult> {
    // Guard 1: APEX disabled (empty index).
    if apex_index.is_empty() {
        return Vec::new();
    }

    // Guard 2: no usable query signal.
    let query_full = cross_query.trim();
    if query_full.is_empty() {
        return Vec::new();
    }

    // Truncate the query to MAX_APEX_QUERY_CHARS (UTF-8 char-boundary safe).
    let query = truncate_on_char_boundary(query_full, MAX_APEX_QUERY_CHARS);

    // Request a few extra results so path-prefix filtering still yields up to
    // MAX_APEX_RESULTS (in a mixed code+docs index most hits may be code).
    let top_k = (MAX_APEX_RESULTS * 4) as u32;

    let raw_results = match search.search(apex_index, query, Some(top_k)).await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                apex_index,
                "APEX context search failed (fail-open, review continues): {e}"
            );
            return Vec::new();
        }
    };

    // Path-prefix filter (Option A: the same index holds code and APEX docs).
    let filtered = raw_results.into_iter().filter(|r| {
        if apex_path_prefixes.is_empty() {
            true // no filter → treat every hit as APEX
        } else {
            apex_path_prefixes
                .iter()
                .any(|prefix| r.file.starts_with(prefix.as_str()))
        }
    });

    // Cap at MAX_APEX_RESULTS and map to the output type.
    filtered
        .take(MAX_APEX_RESULTS)
        .map(|r| {
            let raw_snippet = r.snippet.unwrap_or_default();
            let snippet =
                truncate_on_char_boundary(&raw_snippet, MAX_APEX_SNIPPET_CHARS).to_string();
            ApexContextResult {
                file: r.file,
                snippet,
                score: r.score,
                start_line: r.start_line,
            }
        })
        .collect()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::search_client::{
        HealthResponse, IndexInfo, SearchClientError, SearchResult,
    };
    use async_trait::async_trait;

    // ── Mock search client ────────────────────────────────────────────────

    /// Mock that records whether `search()` was called and returns a fixed set.
    struct MockSearch {
        results: Vec<SearchResult>,
        error: Option<SearchClientError>,
        called: std::sync::Mutex<bool>,
    }

    impl MockSearch {
        fn with_results(results: Vec<SearchResult>) -> Self {
            Self {
                results,
                error: None,
                called: std::sync::Mutex::new(false),
            }
        }

        fn with_error(err: SearchClientError) -> Self {
            Self {
                results: Vec::new(),
                error: Some(err),
                called: std::sync::Mutex::new(false),
            }
        }

        fn was_called(&self) -> bool {
            *self.called.lock().unwrap()
        }
    }

    #[async_trait]
    impl SearchClient for MockSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Ok(HealthResponse {
                status: "ok".to_string(),
                embedder: true,
            })
        }

        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Ok(vec![])
        }

        async fn search(
            &self,
            _index_id: &str,
            _query: &str,
            _top_k: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            *self.called.lock().unwrap() = true;
            if let Some(ref e) = self.error {
                return Err(SearchClientError::Transport(e.to_string()));
            }
            Ok(self.results.clone())
        }
    }

    fn make_result(file: &str, snippet: &str, score: f32) -> SearchResult {
        SearchResult {
            file: file.to_string(),
            snippet: Some(snippet.to_string()),
            score,
            start_line: Some(1),
            end_line: None,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    /// APEX disabled (empty index) ⇒ returns empty without calling search.
    ///
    /// Why: when `apex_index` is empty the operator has not configured APEX;
    /// no search call should be issued (no side effects, no warnings).
    /// What: passes empty string as `apex_index`; asserts result is empty and
    /// `search()` was not called.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_empty_index_returns_empty_no_search_call() {
        let mock = MockSearch::with_results(vec![make_result("apex/spec.md", "content", 0.9)]);
        let result = fetch_apex_context(&mock, "", &[], "PR title").await;
        assert!(result.is_empty(), "empty index must return empty results");
        assert!(
            !mock.was_called(),
            "search must not be called when apex_index is empty"
        );
    }

    /// Empty cross-query ⇒ returns empty without calling search.
    ///
    /// Why: an empty query would produce irrelevant or random results from the
    /// search daemon; the function must bail early and silently.
    /// What: passes whitespace-only `cross_query`; asserts result is empty and
    /// `search()` was not called.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_empty_query_returns_empty_no_search_call() {
        let mock = MockSearch::with_results(vec![make_result("apex/spec.md", "content", 0.9)]);
        let result = fetch_apex_context(&mock, "my-index", &[], "   \n\t  ").await;
        assert!(result.is_empty(), "blank query must return empty results");
        assert!(
            !mock.was_called(),
            "search must not be called when query is blank"
        );
    }

    /// Search error ⇒ fail-open (returns empty, does not panic, does not propagate).
    ///
    /// Why: REV-420 requires fail-open: an APEX search failure must never block
    /// or skip the review.
    /// What: injects a search client that always returns an error; asserts result
    /// is empty and no panic occurs.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_search_error_fail_open() {
        let mock = MockSearch::with_error(SearchClientError::Transport("refused".to_string()));
        let result = fetch_apex_context(&mock, "my-index", &[], "some PR query").await;
        assert!(
            result.is_empty(),
            "search error must produce empty result (fail-open)"
        );
    }

    /// Results are mapped correctly (file/snippet/score/start_line).
    ///
    /// Why: the prompt builder reads these fields; a mapping bug would silently
    /// drop APEX context from the reviewer input.
    /// What: injects a single known result; asserts all fields round-trip.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_maps_result_fields_correctly() {
        let mock = MockSearch::with_results(vec![SearchResult {
            file: "apex/auth-spec.md".to_string(),
            snippet: Some("The auth flow must verify token expiry.".to_string()),
            score: 0.87,
            start_line: Some(15),
            end_line: None,
        }]);
        let result = fetch_apex_context(&mock, "my-index", &[], "auth token flow").await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file, "apex/auth-spec.md");
        assert_eq!(result[0].snippet, "The auth flow must verify token expiry.");
        assert!((result[0].score - 0.87_f32).abs() < 1e-4);
        assert_eq!(result[0].start_line, Some(15));
    }

    /// Snippet is truncated to MAX_APEX_SNIPPET_CHARS.
    ///
    /// Why: large spec pages must not swamp the reviewer prompt; snippets must be
    /// bounded.
    /// What: injects a result with a snippet longer than MAX_APEX_SNIPPET_CHARS;
    /// asserts the output snippet has exactly MAX_APEX_SNIPPET_CHARS chars.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_truncates_snippet_to_max_chars() {
        let long_snippet = "x".repeat(MAX_APEX_SNIPPET_CHARS + 200);
        let mock = MockSearch::with_results(vec![SearchResult {
            file: "apex/spec.md".to_string(),
            snippet: Some(long_snippet),
            score: 0.5,
            start_line: None,
            end_line: None,
        }]);
        let result = fetch_apex_context(&mock, "idx", &[], "query").await;
        assert_eq!(result.len(), 1);
        let actual_chars = result[0].snippet.chars().count();
        assert_eq!(
            actual_chars, MAX_APEX_SNIPPET_CHARS,
            "snippet must be exactly MAX_APEX_SNIPPET_CHARS chars"
        );
    }

    /// Path-prefix filter retains only APEX hits.
    ///
    /// Why: in a mixed code+docs index the prefix filter is the mechanism that
    /// distinguishes APEX docs from code files; it must work correctly.
    /// What: injects two results — `apex/spec.md` (APEX) and `src/main.rs`
    /// (code); applies prefix `["apex/"]`; asserts only the APEX result survives.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_prefix_filter_retains_only_apex_hits() {
        let mock = MockSearch::with_results(vec![
            make_result("apex/spec.md", "spec content", 0.9),
            make_result("src/main.rs", "fn main() {}", 0.85),
        ]);
        let prefixes = vec!["apex/".to_string()];
        let result = fetch_apex_context(&mock, "idx", &prefixes, "auth flow").await;
        assert_eq!(result.len(), 1, "only apex/ hit must survive prefix filter");
        assert_eq!(result[0].file, "apex/spec.md");
    }

    /// No prefix filter ⇒ all hits from apex_index are treated as APEX.
    ///
    /// Why: when no prefixes are configured, every hit in the `apex_index` is an
    /// APEX doc (the operator manages what is in the index).
    /// What: injects multiple hits with no prefix configuration; asserts up to
    /// MAX_APEX_RESULTS are returned regardless of path.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_no_prefix_all_results_are_apex() {
        let mock = MockSearch::with_results(vec![
            make_result("apex/a.md", "a", 0.9),
            make_result("docs/b.md", "b", 0.8),
        ]);
        let result = fetch_apex_context(&mock, "idx", &[], "query").await;
        assert_eq!(
            result.len(),
            2,
            "no prefix filter → all results returned (up to cap)"
        );
    }

    /// Results are capped at MAX_APEX_RESULTS even when more APEX hits pass the filter.
    ///
    /// Why: prompt size must be bounded; more than MAX_APEX_RESULTS APEX snippets
    /// swamp the code context that drives the verdict.
    /// What: injects MAX_APEX_RESULTS + 2 results all starting with `apex/`;
    /// asserts exactly MAX_APEX_RESULTS are returned.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_caps_at_max_results() {
        let results: Vec<SearchResult> = (0..MAX_APEX_RESULTS + 2)
            .map(|i| {
                make_result(
                    &format!("apex/spec-{i}.md"),
                    "content",
                    0.9 - i as f32 * 0.01,
                )
            })
            .collect();
        let mock = MockSearch::with_results(results);
        let prefixes = vec!["apex/".to_string()];
        let result = fetch_apex_context(&mock, "idx", &prefixes, "query").await;
        assert_eq!(
            result.len(),
            MAX_APEX_RESULTS,
            "results must be capped at MAX_APEX_RESULTS"
        );
    }

    /// Prefix filter with no matching results ⇒ empty.
    ///
    /// Why: when no hits match the configured prefixes the function must return
    /// empty without panicking.
    /// What: injects only code hits; applies `apex/` prefix; asserts empty result.
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_prefix_filter_no_match_returns_empty() {
        let mock = MockSearch::with_results(vec![
            make_result("src/a.rs", "code", 0.8),
            make_result("tests/b.rs", "test", 0.7),
        ]);
        let prefixes = vec!["apex/".to_string()];
        let result = fetch_apex_context(&mock, "idx", &prefixes, "query").await;
        assert!(
            result.is_empty(),
            "no matching prefix ⇒ empty (no code hits treated as APEX)"
        );
    }

    /// Long query is truncated to MAX_APEX_QUERY_CHARS (UTF-8 safe).
    ///
    /// Why: the search daemon benefits from a bounded query; a very long PR body
    /// must be trimmed before being sent.
    /// What: passes a query longer than MAX_APEX_QUERY_CHARS; verifies search is
    /// called (mock does not validate query length, but the test proves no panic
    /// and the truncation logic in production is UTF-8 safe).
    /// Test: this function; no network.
    #[tokio::test]
    async fn apex_context_long_query_does_not_panic() {
        let long_query = "a".repeat(MAX_APEX_QUERY_CHARS + 500);
        let mock = MockSearch::with_results(vec![make_result("apex/spec.md", "content", 0.7)]);
        let result = fetch_apex_context(&mock, "idx", &[], &long_query).await;
        // Should succeed without panic; returns up to MAX_APEX_RESULTS items.
        assert!(result.len() <= MAX_APEX_RESULTS);
        assert!(
            mock.was_called(),
            "search must be called for a non-empty query"
        );
    }
}
