//! Unit tests for `pipeline::runner`.
//!
//! Why: split from `runner.rs` to keep that file under the 500-line cap while
//! preserving full coverage of the orchestration loop (fail-safe paths, the
//! post-or-log finalisation, and the dry-run log side effect).
//! What: drives `run_review` with fake LLM / search / analyze deps.
//! Test: this is the test module; each function is a self-contained unit test.

use super::*;
use crate::{
    integrations::{
        analyze_client::{AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell},
        search_client::{
            EmbedderState, HealthResponse, IndexInfo, SearchClientError, SearchResult,
        },
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    models::ReviewStatus,
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
        // Severity is "high" which maps to Effort::High → BLOCK floor.
        // Use "medium" here so the severity floor produces REQUEST_CHANGES,
        // letting this test verify REQUEST_CHANGES-verdict round-trip parsing.
        // The critical/high → BLOCK escalation path is covered in grade.rs tests.
        Self {
                response: r#"There is a bug.

```json
{"verdict":"REQUEST_CHANGES","summary":"SQL injection","findings":[{"title":"SQL injection","body":"line 42","severity":"medium","confidence":0.9,"file":"src/a.rs","line":42}]}
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

// ── Fake verifier provider (Phase 2, #583) ────────────────────────────────
// Returns a fixed CONFIRMED / REFUTED judgment so the runner-level wiring of
// the verification round can be asserted deterministically.

struct FakeVerifier {
    judgment: &'static str,
}

#[async_trait]
impl LlmProvider for FakeVerifier {
    fn name(&self) -> &str {
        "fake-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: format!(r#"{{"judgment":"{}","reason":"test"}}"#, self.judgment),
            model: req.model.clone(),
            input_tokens: 5,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
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
            embedder: EmbedderState::Bool(true),
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

// ── Fake analyze clients ──────────────────────────────────────────────
// `FakeAnalyze` reports NOT ready (the daemon is down); `ReadyAnalyze` reports
// ready with empty enrichment.  The required-context gate (#590) treats a
// not-ready / absent analyze client as "analyze unavailable", so positive tests
// must inject `ReadyAnalyze` for the gate to pass.

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

struct ReadyAnalyze;

#[async_trait]
impl AnalyzeClient for ReadyAnalyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Ok(AnalyzeHealthResponse {
            status: "ok".to_string(),
            search_reachable: true,
        })
    }

    async fn has_analysis(&self, _: &str) -> bool {
        true
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

/// Build deps with healthy search + ready analyze so the required-context gate
/// (#590) passes.  Positive tests use this to exercise the post-gate pipeline.
fn ready_deps(llm: Arc<dyn LlmProvider>, verifier: Option<Arc<dyn LlmProvider>>) -> ReviewDeps {
    ReviewDeps {
        llm,
        verifier,
        search: Arc::new(FakeSearch),
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
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
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        result.error.is_none(),
        "no error expected: {:?}",
        result.error
    );
    assert_eq!(
        result.status,
        ReviewStatus::Completed,
        "both deps healthy → authoritative Completed status"
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
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::request_changes()), None);

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
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::errors("simulated transport error")), None);

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

/// REQUIRED-CONTEXT GATE (#590): when trusty-search is unreachable and required
/// (the default), the review is SKIPPED loudly — NOT a silent APPROVE.
///
/// Why: a review without code context gives false confidence; the old
/// graceful-degrade behaviour (which this test replaces) was actively harmful.
/// What: a failing search + default `require_search=true` must yield
/// `status = Skipped`, an actionable error, and NO LLM-derived APPROVE.
#[tokio::test]
async fn run_review_search_down_skips_when_required() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let config = default_config(); // require_search defaults true
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()), // would APPROVE if ever consulted
        verifier: None,
        search: Arc::new(FailingSearch), // search is down
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.status,
        ReviewStatus::Skipped,
        "search down + required must SKIP, not silently APPROVE"
    );
    assert!(!result.posted, "a skipped review must never be posted live");
    assert!(result.dry_run, "a skipped review is dry-run");
    let err = result.error.expect("skip must set an actionable error");
    assert!(
        err.contains("trusty-search"),
        "error must name the dep: {err}"
    );
    assert!(
        err.contains("start"),
        "error must be actionable (how to fix): {err}"
    );
    assert_ne!(
        result.verdict,
        Verdict::Approve,
        "a skip must not masquerade as APPROVE"
    );
}

/// REQUIRED-CONTEXT GATE (#590): trusty-analyze unreachable + required (default)
/// also SKIPS the review.
#[tokio::test]
async fn run_review_analyze_down_skips_when_required() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let config = default_config(); // require_analyze defaults true
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()),
        verifier: None,
        search: Arc::new(FakeSearch),         // search healthy
        analyze: Some(Arc::new(FakeAnalyze)), // analyze not ready
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.status,
        ReviewStatus::Skipped,
        "analyze down + required must SKIP"
    );
    let err = result.error.expect("skip must set an actionable error");
    assert!(
        err.contains("trusty-analyze"),
        "error must name the dep: {err}"
    );
}

/// OPT-IN DEGRADED MODE (#590): with `require_search=false`, a down search no
/// longer skips — the review proceeds but is tagged DEGRADED / non-authoritative
/// and the rendered body carries a loud warning banner.
#[tokio::test]
async fn run_review_search_down_degraded_when_optout() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.context.require_search = false; // explicit opt-out
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()),
        verifier: None,
        search: Arc::new(FailingSearch), // search down, but opted out
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.status,
        ReviewStatus::Degraded,
        "opted-out + search down must PROCEED but be tagged Degraded"
    );
    assert!(
        !result.status.is_authoritative(),
        "a degraded review must not be authoritative"
    );
    assert!(
        result.review_body.contains("NOT AUTHORITATIVE"),
        "degraded body must carry a loud banner: {:?}",
        result.review_body
    );
    let err = result
        .error
        .expect("degraded run must record a non-authoritative reason");
    assert!(err.contains("degraded"), "reason must say degraded: {err}");
}

/// REGRESSION GUARD (#590): both deps healthy → a normal, authoritative review.
#[tokio::test]
async fn run_review_both_healthy_completes_authoritative() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.status, ReviewStatus::Completed);
    assert!(result.status.is_authoritative());
    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        result.error.is_none(),
        "healthy run sets no error: {:?}",
        result.error
    );
    assert!(
        !result.review_body.contains("NOT AUTHORITATIVE"),
        "authoritative review must not carry the degraded banner"
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
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

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
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()),
        verifier: None,
        search: Arc::new(FakeSearch),
        analyze: None,
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert!(
        result.error.is_some(),
        "missing diff file must set error field"
    );
    // Still a safe outcome — the verdict stays at the default Unknown when
    // the diff fails to load (no LLM call was made).  The error field is
    // set; Unknown signals "could not assess" rather than a clean APPROVE.
}

#[tokio::test]
async fn run_review_local_diff_is_dry_run_and_not_posted() {
    // A local diff can never be posted (no GitHub source); even with the
    // trigger forcing live and posting allowed, the result stays dry-run.
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.dry_run = false; // service-live default
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::ForceLive, // would post if GitHub-sourced
        run_mode: RunMode::Serve,
        allow_posting: true,
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert!(
        result.dry_run,
        "local-diff source must never post — always dry-run"
    );
    assert!(!result.posted, "local-diff must not be marked posted");
}

#[tokio::test]
async fn run_review_writes_dry_run_log_on_log_only_path() {
    // The LogOnly finalisation path writes a JSON log when write_log is set,
    // making dry-run reviews inspectable (deliverable 6).
    let dir = tempfile::tempdir().expect("tempdir");
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.log_dir = dir.path().to_path_buf();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: true,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let _result = run_review(&config, input, deps).await;
    let json_count = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .count();
    assert_eq!(json_count, 1, "a dry-run JSON log must be written");
}

/// The verification round, when wired in, REFUTES the only blocking finding and
/// the runner's final verdict relaxes from REQUEST_CHANGES to APPROVE.
///
/// Why: end-to-end proof that the runner threads the verification round between
/// parse/grade and finalisation and that a refuted finding correctly relaxes the
/// verdict (Phase 2, #583 deliverable 2/3).
#[tokio::test]
async fn run_review_verification_refutes_and_relaxes_verdict() {
    let (source, _tmp) = local_diff_source("+fn bad() {}\n");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(
        Arc::new(FakeLlm::request_changes()), // 1 medium finding → REQUEST_CHANGES
        Some(Arc::new(FakeVerifier {
            judgment: "REFUTED",
        })),
    );

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "refuting the sole finding must relax REQUEST_CHANGES to APPROVE"
    );
    assert_eq!(
        result.findings.len(),
        1,
        "the finding is demoted, not dropped"
    );
}

/// The verification round, when wired in, CONFIRMS the finding and the verdict
/// is preserved.
#[tokio::test]
async fn run_review_verification_confirms_and_preserves_verdict() {
    let (source, _tmp) = local_diff_source("+fn bad() {}\n");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    let deps = ready_deps(
        Arc::new(FakeLlm::request_changes()),
        Some(Arc::new(FakeVerifier {
            judgment: "CONFIRMED",
        })),
    );

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "a confirmed finding must preserve the REQUEST_CHANGES verdict"
    );
}

/// When verification is disabled by config, the verifier is never consulted and
/// the verdict is the un-verified grade.
#[tokio::test]
async fn run_review_verification_disabled_skips_round() {
    let (source, _tmp) = local_diff_source("+fn bad() {}\n");
    let mut config = default_config();
    config.verification.enabled = false; // disable the round
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
    };
    // A REFUTED verifier is wired in but must NOT be consulted when disabled.
    let deps = ready_deps(
        Arc::new(FakeLlm::request_changes()),
        Some(Arc::new(FakeVerifier {
            judgment: "REFUTED",
        })),
    );

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "with verification disabled the verdict must remain REQUEST_CHANGES"
    );
    assert!(
        result.findings[0].verified.is_none(),
        "disabled verification must not mark any finding"
    );
}

/// Live post + dedup-skip end-to-end requires a real PR + GitHub creds, so
/// it is `#[ignore]`d.  The unit-level guarantees it would assert are covered
/// without a network by `store::dedup::tests` (claim/skip/complete) and
/// `pipeline::post::tests` (post-vs-log decision).
#[tokio::test]
#[ignore = "requires a live GitHub PR + credentials"]
async fn run_review_live_post_and_dedup_skip_integration() {
    // Placeholder for a future live integration test against a fixture PR.
}
