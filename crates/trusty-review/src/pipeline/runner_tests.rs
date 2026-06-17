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
    /// Output-token count to report (overrides the default 50).  Used by the
    /// truncation test (#1241) to simulate a completion that hit the ceiling.
    output_tokens: Option<u32>,
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
            output_tokens: None,
        }
    }

    /// A response that parses to APPROVE but reports an output-token count at the
    /// ceiling — simulating a truncated completion (#1241).  The runner's
    /// truncation guard must convert this to UNKNOWN BEFORE trusting the parse.
    fn truncated_at_ceiling() -> Self {
        Self {
            response: r#"```json
{"verdict":"APPROVE","summary":"looks fine","findings":[]}
```"#
                .to_string(),
            error: None,
            // 4096 is the non-Gemini reviewer ceiling; 4096 >= ceil(4096*0.95)=3892.
            output_tokens: Some(4096),
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
                output_tokens: None,
            }
    }

    fn errors(msg: impl Into<String>) -> Self {
        Self {
            response: String::new(),
            error: Some(msg.into()),
            output_tokens: None,
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
            output_tokens: self.output_tokens.unwrap_or(50),
            latency_ms: 42,
            cost_usd: 0.000042,
            finish_reason: None,
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
            finish_reason: None,
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
    // Fail-CLOSED (#1241 supersedes REV-130): verdict must be UNKNOWN on LLM error.
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "LLM error must fail CLOSED to UNKNOWN, never silently APPROVE (#1241)"
    );
    assert!(
        result.error.is_some(),
        "error field must be set when LLM fails"
    );
}

#[tokio::test]
async fn run_review_truncated_output_is_unknown() {
    // Fail-CLOSED (#1241): the LLM returns parseable APPROVE JSON but reports an
    // output-token count at the ceiling — i.e. the response was truncated.  The
    // runner's truncation guard must convert this to UNKNOWN BEFORE trusting the
    // (likely incomplete) parse, never posting a silent green APPROVE.
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
    let deps = ready_deps(Arc::new(FakeLlm::truncated_at_ceiling()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "output at the token ceiling must fail CLOSED to UNKNOWN (#1241)"
    );
    let err = result
        .error
        .expect("truncation must set an actionable error");
    assert!(
        err.contains("truncat"),
        "error must explain the truncation: {err}"
    );
}

/// PRIMARY signal (#1357): a length/max_tokens finish_reason flags truncation
/// regardless of the token count.
///
/// Why: the provider's own completion reason is authoritative; an explicit
/// `length` means the model was cut off even if token accounting looks fine.
/// What: asserts `length` / `max_tokens` → truncated even with low output tokens.
/// Test: this test itself.
#[test]
fn is_truncated_finish_reason_length_true() {
    assert!(
        is_truncated(Some("length"), 10, 4096),
        "finish_reason=length is truncated even well under the ceiling"
    );
    assert!(
        is_truncated(Some("max_tokens"), 10, 4096),
        "finish_reason=max_tokens (Bedrock) is truncated"
    );
    // Case-insensitive / padded.
    assert!(is_truncated(Some(" LENGTH "), 10, 4096));
}

/// PRIMARY signal (#1357): a natural-stop finish_reason at a HIGH token ratio is
/// NOT flagged — this is the false-positive the issue targets.
///
/// Why: before #1357 a complete response landing ≥95 % of the ceiling was
/// mis-flagged UNKNOWN.  With finish_reason primary, `stop`/`end_turn` overrides
/// the ratio heuristic entirely.
/// What: asserts `stop` / `end_turn` at 99–100 % of the ceiling → NOT truncated.
/// Test: this test itself.
#[test]
fn is_truncated_finish_reason_stop_at_high_ratio_false() {
    assert!(
        !is_truncated(Some("stop"), 4096, 4096),
        "finish_reason=stop at 100% of ceiling is NOT truncated (#1357 false-positive fix)"
    );
    assert!(
        !is_truncated(Some("end_turn"), 4090, 4096),
        "finish_reason=end_turn (Bedrock natural stop) near the ceiling is NOT truncated"
    );
}

/// FALLBACK heuristic (#1357): when finish_reason is absent, the token-ratio
/// behaves exactly as the pre-#1357 #1241 logic.
///
/// Why: providers that don't surface a reason must still fail closed on a likely
/// cut-off response.
/// What: 4096 ceiling, ceil(4096*0.95)=3892; output >= that → truncated.
/// Test: this test itself.
#[test]
fn is_truncated_ratio_fallback_at_ceiling_true() {
    assert!(
        is_truncated(None, 4096, 4096),
        "no finish_reason, exactly at ceiling → truncated"
    );
    assert!(
        is_truncated(None, 3892, 4096),
        "no finish_reason, at the 95% threshold → truncated"
    );
    // An empty-string finish_reason is treated as "absent" → fall back to ratio.
    assert!(
        is_truncated(Some(""), 4096, 4096),
        "empty reason falls back to ratio"
    );
}

#[test]
fn is_truncated_ratio_fallback_well_under_false() {
    assert!(
        !is_truncated(None, 3891, 4096),
        "no finish_reason, one below the 95% threshold → NOT truncated"
    );
    assert!(
        !is_truncated(None, 50, 4096),
        "no finish_reason, a short response → NOT truncated"
    );
}

#[test]
fn is_truncated_unset_ceiling_false() {
    // max_tokens == 0 means the ceiling is unknown — never false-positive on the
    // fallback path.
    assert!(
        !is_truncated(None, 10_000, 0),
        "unknown ceiling (0) disables the fallback truncation check"
    );
}

// ── Configurable fallback ratio (#1357) ───────────────────────────────────
// These tests mutate a process-global env var, so they must not interleave with
// each other.  A local mutex serialises them; each restores the prior value.

/// Serialises env-mutating ratio tests so they don't race.
static RATIO_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// #1357: a valid `TRUSTY_REVIEW_TRUNCATION_TOKEN_RATIO` override changes the
/// fallback threshold.
///
/// Why: operators must be able to retune the fallback band without a rebuild.
/// What: sets the env ratio to 0.50; asserts a 50 %-of-ceiling response (no
/// finish_reason) is now flagged where the 0.95 default would not flag it.
/// Test: this test itself (serialised via `RATIO_ENV_LOCK`).
#[test]
fn truncation_ratio_env_override_applies() {
    let _guard = RATIO_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(TRUNCATION_TOKEN_RATIO_ENV).ok();
    // SAFETY: single-threaded within the lock; restored before unlock.
    unsafe { std::env::set_var(TRUNCATION_TOKEN_RATIO_ENV, "0.50") };

    assert!(
        (truncation_token_ratio() - 0.50).abs() < f64::EPSILON,
        "env override should set the ratio to 0.50"
    );
    // ceil(4096 * 0.50) = 2048 — a 2048-token response now flags (would not at 0.95).
    assert!(
        is_truncated(None, 2048, 4096),
        "with ratio 0.50, 50% of ceiling is truncated on the fallback path"
    );

    match prev {
        Some(v) => unsafe { std::env::set_var(TRUNCATION_TOKEN_RATIO_ENV, v) },
        None => unsafe { std::env::remove_var(TRUNCATION_TOKEN_RATIO_ENV) },
    }
}

/// #1357: an invalid / out-of-range override falls back to the default ratio.
///
/// Why: a typo (or a nonsensical value like `2.0`) must never silently disable
/// the truncation safety check.
/// What: sets the env ratio to an out-of-range value; asserts the default 0.95
/// is used.
/// Test: this test itself (serialised via `RATIO_ENV_LOCK`).
#[test]
fn truncation_ratio_env_invalid_falls_back() {
    let _guard = RATIO_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(TRUNCATION_TOKEN_RATIO_ENV).ok();
    unsafe { std::env::set_var(TRUNCATION_TOKEN_RATIO_ENV, "2.0") };
    assert!(
        (truncation_token_ratio() - DEFAULT_TRUNCATION_TOKEN_RATIO).abs() < f64::EPSILON,
        "out-of-range override (>1.0) must fall back to the default"
    );

    unsafe { std::env::set_var(TRUNCATION_TOKEN_RATIO_ENV, "not-a-number") };
    assert!(
        (truncation_token_ratio() - DEFAULT_TRUNCATION_TOKEN_RATIO).abs() < f64::EPSILON,
        "unparseable override must fall back to the default"
    );

    match prev {
        Some(v) => unsafe { std::env::set_var(TRUNCATION_TOKEN_RATIO_ENV, v) },
        None => unsafe { std::env::remove_var(TRUNCATION_TOKEN_RATIO_ENV) },
    }
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

/// The verification round, when wired in, CONFIRMS the Medium finding; after #1015
/// a lone confirmed Medium anchors at APPROVE* (path a2), not REQUEST_CHANGES.
///
/// Why: before #1015 path (a) preserved primary_verdict (REQUEST_CHANGES) on any
/// confirmed finding.  After #1015, only confirmed High-effort triggers path (a);
/// confirmed Medium triggers path (a2) which caps the baseline at APPROVE*.
/// The APPROVE* result is correct — the model judged REQUEST_CHANGES holistically
/// but the only confirmed evidence is a single Medium finding, which justifies
/// at most APPROVE*.
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
    // After #1015: confirmed Medium → path (a2) → APPROVE* (not REQUEST_CHANGES).
    assert_eq!(
        result.verdict,
        Verdict::ApproveWithReservations,
        "confirmed Medium → APPROVE* (path a2 — #1015); not REQUEST_CHANGES"
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
