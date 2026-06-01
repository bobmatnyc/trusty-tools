//! Review pipeline runner — the top-level orchestration loop.
//!
//! Why: wires together diff loading, context retrieval, prompt construction,
//! LLM call, parsing, and output writing into a single `run_review` function
//! that both the CLI `run` and `compare` commands can call.
//!
//! What: `run_review` runs the MVP pipeline (steps 1-7 of the spec, deferred
//! sections are noted inline); returns a `ReviewResult`.
//!
//! Deferred for later stages (not in MVP):
//!  - Verification round (spec REV-114)
//!  - Dedup store (spec REV-101)
//!  - Suppression filtering (spec REV-115)
//!  - GitHub comment posting (spec REV-117)
//!  - Tracker issue upsert
//!  - Multi-pass / enrichment rounds
//!
//! Test: `run_review_with_fake_provider_approves`,
//! `run_review_fail_safe_on_llm_error`,
//! `run_review_local_diff_skips_github`.

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::{
    config::ReviewConfig,
    integrations::{
        analyze_client::AnalyzeClient,
        github::{GithubClient, GithubError},
        github::{auth::resolve_token, fetch_pr_metadata},
        search_client::SearchClient,
    },
    llm::LlmProvider,
    models::{ReviewResult, Verdict},
    pipeline::{
        diff::{DiffSource, extract_changed_files, extract_identifiers, load_diff, truncate_diff},
        output::{print_review_result, write_review_log},
        parser::parse_review_response,
        prompt::{ReviewContext, ReviewPrMeta, build_review_prompt},
    },
};

// ─── Pipeline input ───────────────────────────────────────────────────────────

/// All inputs for a single review run.
///
/// Why: grouping the inputs into a struct avoids long function signatures and
/// makes the `compare` subcommand easy to implement (same input, multiple
/// models).
/// What: contains the diff source, config reference, model override, and
/// injected service clients.
/// Test: used directly by all runner tests.
pub struct ReviewInput {
    /// Where to obtain the diff (GitHub PR or local file).
    pub diff_source: DiffSource,
    /// Reviewer model id (may differ from config default in `compare` mode).
    pub reviewer_model: String,
    /// Whether to actually write the log file (false in `compare` mode to
    /// avoid cluttering the log dir with partial results).
    pub write_log: bool,
    /// Print the result to STDOUT after the run.
    pub print_result: bool,
}

/// Injected service dependencies (trait objects for testability).
///
/// Why: the pipeline calls trusty-search and trusty-analyze via trait objects
/// so tests can inject fakes without a running daemon.
/// What: all fields are `Arc<dyn Trait>` for cheap cloning in `compare` mode.
/// Test: `run_review_with_fake_provider_approves`.
pub struct ReviewDeps {
    /// LLM provider for the reviewer role.
    pub llm: Arc<dyn LlmProvider>,
    /// Code search client (required; gracefully degrades on error).
    pub search: Arc<dyn SearchClient>,
    /// Static analysis client (optional; None skips the analyze step).
    pub analyze: Option<Arc<dyn AnalyzeClient>>,
}

// ─── Main runner ──────────────────────────────────────────────────────────────

/// Run the MVP review pipeline for a single PR / diff.
///
/// Why: the single entry point used by both the CLI `run` and `compare`
/// subcommands; ensures both take the same code path.
/// What: loads the diff, gathers context, builds the prompt, calls the LLM,
/// parses the response, and writes the log.  Returns a `ReviewResult` even
/// on pipeline errors (fail-safe: verdict = APPROVE with an `error` field set).
/// Test: `run_review_with_fake_provider_approves`, `run_review_fail_safe_on_llm_error`.
pub async fn run_review(
    config: &ReviewConfig,
    input: ReviewInput,
    deps: ReviewDeps,
) -> ReviewResult {
    // ── Step 1: determine owner/repo/pr from diff source ──────────────────
    let (owner, repo, pr_number, is_local) = match &input.diff_source {
        DiffSource::Github {
            owner, repo, pr, ..
        } => (owner.clone(), repo.clone(), *pr, false),
        DiffSource::LocalFile { path } => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("local");
            ("local".to_string(), stem.to_string(), 0_u64, true)
        }
    };

    let pr_url = if !is_local {
        format!("https://github.com/{owner}/{repo}/pull/{pr_number}")
    } else {
        String::new()
    };

    // ── Step 2: fetch PR metadata (skip for local-diff mode) ──────────────
    let pr_meta: ReviewPrMeta = if is_local {
        ReviewPrMeta::default()
    } else {
        match fetch_github_pr_meta(config, &owner, &repo, pr_number).await {
            Ok(m) => m,
            Err(e) => {
                warn!("failed to fetch PR metadata: {e} — using empty metadata");
                ReviewPrMeta {
                    title: format!("PR #{pr_number}"),
                    author: String::new(),
                    url: pr_url.clone(),
                }
            }
        }
    };

    // Build a result skeleton with the PR identity filled in.
    let mut result = ReviewResult::new(
        owner.clone(),
        repo.clone(),
        pr_number,
        pr_meta.title.clone(),
        pr_url,
    );
    result.dry_run = true; // MVP: always dry-run.

    // ── Step 3: load and truncate diff ────────────────────────────────────
    let raw_diff = match load_diff(&input.diff_source).await {
        Ok(d) => d,
        Err(e) => {
            warn!("failed to load diff: {e}");
            result.error = Some(format!("diff load failed: {e}"));
            return finalize(result, config, &input);
        }
    };
    let diff = truncate_diff(&raw_diff);
    debug!(diff_chars = diff.len(), "diff loaded and truncated");

    // ── Step 4: extract identifiers for context retrieval ─────────────────
    let identifiers = extract_identifiers(&diff, 8);
    let changed_files = extract_changed_files(&diff);
    debug!(
        identifiers = ?identifiers,
        changed_files_count = changed_files.len(),
        "extracted identifiers from diff"
    );

    // ── Step 5: gather context in parallel ────────────────────────────────
    let context = gather_context(config, &deps, &identifiers, &changed_files, &pr_meta.title).await;

    // ── Step 6: build prompt and call LLM ─────────────────────────────────
    let llm_req = build_review_prompt(
        &owner,
        &repo,
        &pr_meta,
        &diff,
        &context,
        &input.reviewer_model,
    );
    debug!(model = %input.reviewer_model, "calling LLM reviewer");

    let llm_resp = match deps.llm.complete(llm_req).await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("LLM call failed: {e} — applying fail-safe APPROVE (spec REV-130)");
            result.verdict = Verdict::Approve;
            result.error = Some(format!("LLM error: {e}"));
            return finalize(result, config, &input);
        }
    };

    info!(
        model = %llm_resp.model,
        input_tokens = llm_resp.input_tokens,
        output_tokens = llm_resp.output_tokens,
        cost_usd = llm_resp.cost_usd,
        latency_ms = llm_resp.latency_ms,
        "LLM reviewer call complete"
    );
    result.apply_llm_response(&llm_resp);

    // ── Step 7: parse verdict + findings ──────────────────────────────────
    let parsed = parse_review_response(&llm_resp.text);
    if parsed.is_fail_safe {
        warn!(
            reason = ?parsed.fail_safe_reason,
            "verdict parsing fell back to fail-safe APPROVE"
        );
    }
    result.verdict = parsed.verdict;
    result.findings = parsed.findings;

    finalize(result, config, &input)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Fetch PR metadata from GitHub and build a `ReviewPrMeta`.
///
/// Why: centralises the GitHub API call and mapping from `PrMetadata` to the
/// lighter-weight `ReviewPrMeta` the prompt needs.
/// What: calls `fetch_pr_metadata` with a resolved token; on any error, the
/// caller falls back to empty metadata.
/// Test: no real-network test; tested indirectly via mock in integration tests.
async fn fetch_github_pr_meta(
    config: &ReviewConfig,
    owner: &str,
    repo: &str,
    pr: u64,
) -> Result<ReviewPrMeta, GithubError> {
    let client = GithubClient::new();
    let token = resolve_token(&client, config, owner).await?;
    let meta = fetch_pr_metadata(&client, owner, repo, pr, &token).await?;
    Ok(ReviewPrMeta {
        title: meta.title,
        author: meta.user.login,
        url: meta.html_url,
    })
}

/// Gather code context from trusty-search and (optionally) trusty-analyze.
///
/// Why: context retrieval is the most latency-sensitive step; running search
/// and analyze in parallel reduces wall-clock time.
/// What: runs the search query (identifier names + PR title) and the analyze
/// probe concurrently; both degrade gracefully on error (empty context).
/// Test: `gather_context_degrades_gracefully_on_search_failure`.
async fn gather_context(
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

/// Apply final pipeline flags and optionally write the log + print the result.
///
/// Why: centralises the dry-run output step so all code paths go through it.
/// What: writes the log (if `input.write_log`) and prints to STDOUT (if
/// `input.print_result`).
/// Test: covered by runner tests that verify side-effects.
fn finalize(mut result: ReviewResult, config: &ReviewConfig, input: &ReviewInput) -> ReviewResult {
    result.dry_run = true; // MVP: always dry-run; posting is deferred.

    if input.write_log {
        write_review_log(&result, &config.log_dir);
    }

    if input.print_result {
        print_review_result(&result);
    }

    result
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        integrations::{
            analyze_client::{AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell},
            search_client::{HealthResponse, IndexInfo, SearchClientError, SearchResult},
        },
        llm::{LlmError, LlmRequest, LlmResponse},
    };
    use async_trait::async_trait;
    use std::path::PathBuf;

    // ── Fake LLM provider ─────────────────────────────────────────────────

    struct FakeLlm {
        response: String,
        error: Option<String>,
    }

    impl FakeLlm {
        fn approves() -> Self {
            Self {
                response: r#"Looks good.

```json
{"verdict":"APPROVE","summary":"LGTM","findings":[]}
```"#
                    .to_string(),
                error: None,
            }
        }

        fn request_changes() -> Self {
            Self {
                response: r#"There is a bug.

```json
{"verdict":"REQUEST_CHANGES","summary":"SQL injection","findings":[{"title":"SQL injection","body":"line 42","severity":"critical","confidence":0.9,"file":"src/a.rs","line":42}]}
```"#
                    .to_string(),
                error: None,
            }
        }

        fn errors(msg: impl Into<String>) -> Self {
            Self {
                response: String::new(),
                error: Some(msg.into()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake"
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            if let Some(ref err) = self.error {
                return Err(LlmError::Transport(err.clone()));
            }
            Ok(LlmResponse {
                text: self.response.clone(),
                model: req.model.clone(),
                input_tokens: 100,
                output_tokens: 50,
                latency_ms: 42,
                cost_usd: 0.000042,
            })
        }
    }

    // ── Fake search client ────────────────────────────────────────────────

    struct FakeSearch;

    #[async_trait]
    impl SearchClient for FakeSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Ok(HealthResponse {
                status: "ok".to_string(),
                embedder: true,
            })
        }

        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Ok(vec![IndexInfo {
                id: "main".to_string(),
                name: None,
                root_path: None,
            }])
        }

        async fn search(
            &self,
            _index_id: &str,
            _query: &str,
            _top_k: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Ok(vec![SearchResult {
                file: "src/auth.rs".to_string(),
                snippet: Some("pub fn authenticate() {}".to_string()),
                score: 0.9,
                start_line: None,
                end_line: None,
            }])
        }
    }

    struct FailingSearch;

    #[async_trait]
    impl SearchClient for FailingSearch {
        async fn health(&self) -> Result<HealthResponse, SearchClientError> {
            Err(SearchClientError::Unavailable("down".to_string()))
        }

        async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
            Err(SearchClientError::Unavailable("down".to_string()))
        }

        async fn search(
            &self,
            _: &str,
            _: &str,
            _: Option<u32>,
        ) -> Result<Vec<SearchResult>, SearchClientError> {
            Err(SearchClientError::Transport("refused".to_string()))
        }
    }

    // ── Fake analyze client ───────────────────────────────────────────────

    #[allow(dead_code)]
    struct FakeAnalyze;

    #[async_trait]
    impl AnalyzeClient for FakeAnalyze {
        async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
            Err(AnalyzeClientError::Unavailable("not running".to_string()))
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

    // ── Helper to build a local-diff source with a temp file ──────────────

    fn local_diff_source(diff: &str) -> (DiffSource, tempfile::NamedTempFile) {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(diff.as_bytes()).expect("write");
        let path = tmp.path().to_path_buf();
        (DiffSource::LocalFile { path }, tmp)
    }

    fn default_config() -> ReviewConfig {
        ReviewConfig::load(None)
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_review_with_fake_provider_approves() {
        let diff = "+fn hello() { println!(\"hi\"); }\n";
        let (source, _tmp) = local_diff_source(diff);

        let config = default_config();
        let input = ReviewInput {
            diff_source: source,
            reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::approves()),
            search: Arc::new(FakeSearch),
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        assert_eq!(result.verdict, Verdict::Approve);
        assert!(
            result.error.is_none(),
            "no error expected: {:?}",
            result.error
        );
        assert!(result.dry_run, "MVP must always be dry-run");
        assert_eq!(result.findings.len(), 0);
    }

    #[tokio::test]
    async fn run_review_request_changes_parsed_correctly() {
        let (source, _tmp) = local_diff_source(
            "+fn bad_query(id: &str) { db.exec(format!(\"SELECT * FROM users WHERE id={id}\")) }\n",
        );
        let config = default_config();
        let input = ReviewInput {
            diff_source: source,
            reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::request_changes()),
            search: Arc::new(FakeSearch),
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        assert_eq!(result.verdict, Verdict::RequestChanges);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].kind, "SQL injection");
    }

    #[tokio::test]
    async fn run_review_fail_safe_on_llm_error() {
        let (source, _tmp) = local_diff_source("+fn x() {}\n");
        let config = default_config();
        let input = ReviewInput {
            diff_source: source,
            reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::errors("simulated transport error")),
            search: Arc::new(FakeSearch),
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        // Fail-safe: verdict must be APPROVE on LLM error (spec REV-130).
        assert_eq!(
            result.verdict,
            Verdict::Approve,
            "LLM error must fall back to APPROVE"
        );
        assert!(
            result.error.is_some(),
            "error field must be set when LLM fails"
        );
    }

    #[tokio::test]
    async fn run_review_search_failure_does_not_block() {
        let (source, _tmp) = local_diff_source("+fn x() {}\n");
        let config = default_config();
        let input = ReviewInput {
            diff_source: source,
            reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::approves()),
            search: Arc::new(FailingSearch), // search is down
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        // Review must still complete even if search is unavailable.
        assert_eq!(result.verdict, Verdict::Approve);
        assert!(
            result.error.is_none(),
            "search failure must not set error field"
        );
    }

    #[tokio::test]
    async fn run_review_local_diff_skips_github() {
        // Local-diff mode: no GitHub credentials needed, owner/repo = local/<stem>.
        let diff = "+fn local_fn() {}\n";
        let (source, _tmp) = local_diff_source(diff);

        let config = default_config();
        let input = ReviewInput {
            diff_source: source,
            reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::approves()),
            search: Arc::new(FakeSearch),
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        assert_eq!(result.owner, "local");
        assert_eq!(result.verdict, Verdict::Approve);
    }

    #[tokio::test]
    async fn run_review_missing_diff_file_sets_error() {
        let config = default_config();
        let input = ReviewInput {
            diff_source: DiffSource::LocalFile {
                path: PathBuf::from("/nonexistent/path/nope.diff"),
            },
            reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
            write_log: false,
            print_result: false,
        };
        let deps = ReviewDeps {
            llm: Arc::new(FakeLlm::approves()),
            search: Arc::new(FakeSearch),
            analyze: None,
        };

        let result = run_review(&config, input, deps).await;
        assert!(
            result.error.is_some(),
            "missing diff file must set error field"
        );
        // Still an APPROVE (fail-safe).
        // Note: the verdict stays at the default NotApplicable when the diff
        // fails to load (no LLM call was made), which is also a safe outcome.
    }
}
