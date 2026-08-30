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

    /// Same zero-findings APPROVE as `approves()`, but with an explicit
    /// output-token count — used to drive the shallow-clean-review heuristic
    /// (#1877) with a token spend far below what the diff size would plausibly
    /// require.
    fn approves_with_output_tokens(output_tokens: u32) -> Self {
        Self {
            output_tokens: Some(output_tokens),
            ..Self::approves()
        }
    }

    /// A zero-findings APPROVE whose JSON block embeds an explicit self-grade and
    /// an explicit output-token count (#1886).  Used to prove the top-level grade
    /// and the grade embedded in `review_body` agree AFTER a late-stage adjustment
    /// (the #1877 shallow-review cap) lowers the top-level grade below the model's
    /// self-assessed grade.
    fn approves_with_embedded_grade(grade: &str, output_tokens: u32) -> Self {
        Self {
            response: format!(
                "Looks good.\n\n```json\n{{\"verdict\":\"APPROVE\",\"grade\":\"{grade}\",\"summary\":\"LGTM\",\"findings\":[]}}\n```"
            ),
            error: None,
            output_tokens: Some(output_tokens),
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

    /// A structured response whose `findings` string does not decode (#4491).
    /// The verdict token sits last so the tail scan reads APPROVE — pre-fix this
    /// rendered `APPROVE` with `Findings: none` and no error at all.
    fn unparseable_findings() -> Self {
        Self {
            response: r#"{"summary":"looks fine","findings":"[{\"title\": \"truncated","verdict":"APPROVE"}"#
                .to_string(),
            error: None,
            output_tokens: None,
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
{"verdict":"REQUEST_CHANGES","summary":"SQL injection","findings":[{"title":"SQL injection","body":"line 1","severity":"medium","confidence":0.9,"file":"src/a.rs","line":1}]}
```"#
                    .to_string(),
                error: None,
                output_tokens: None,
            }
    }

    /// A BLOCK whose single finding cites `src/hotelPage.ts:207` — a path that
    /// is not in the diff at all (#4042's own nonexistent-path shape).
    ///
    /// Why: this is the fabrication the citation check exists to drop. Used by
    /// `unified_path_emits_no_finding_citing_a_path_outside_the_diff` (#4045).
    fn fabricates_citation_outside_diff() -> Self {
        Self {
            response: r#"```json
{"verdict":"BLOCK","summary":"broken","findings":[{"title":"null deref","body":"the hotel page dereferences a null booking [code: `src/hotelPage.ts:207` — \"const booking = ctx.booking.id\"]","severity":"high","confidence":0.95,"file":"src/hotelPage.ts","line":207,"code_provable":true}]}
```"#
            .to_string(),
            error: None,
            output_tokens: None,
        }
    }

    /// A self-assessed APPROVE carrying a High-severity finding on a file that
    /// IS in the diff (#1902).
    ///
    /// Why: the severity floor escalates the pipeline's verdict well above the
    /// model's own lean, which is exactly the divergence #1902 reports — a
    /// top-level BLOCK beside an embedded APPROVE. Citing a real diff path
    /// keeps the citation check from dropping the finding and relaxing the
    /// verdict back down.
    /// Test: `run_review_outer_and_embedded_verdict_agree_after_severity_floor`.
    fn self_approves_with_blocking_finding() -> Self {
        Self {
            response: r#"Looks fine to me.

```json
{"verdict":"APPROVE","grade":"A","summary":"LGTM","findings":[{"title":"SQL injection","body":"the query is built by string interpolation","severity":"high","confidence":0.95,"file":"src/a.rs","line":1,"code_provable":true}]}
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
            warmboot_summary: None,
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

/// Build a local-diff source as a PROPER unified diff (with a `+++ b/<file>`
/// header) whose path matches a `FakeLlm` fixture's finding `file` (#4042).
///
/// Why: `citation_check::enforce_citation_integrity` now requires every
/// finding's `file` to resolve against the diff actually under review — a
/// bare `"+<line>\n"` fragment with no `diff --git`/`+++ b/` header (what
/// `local_diff_source` alone produces) contains no file path at all, so a
/// fixture finding citing e.g. `"file":"src/a.rs"` would be (correctly)
/// dropped as unresolvable. Tests that exist to exercise VERIFICATION-ROUND /
/// grade-envelope plumbing — not citation checking — need a diff whose file
/// path actually matches the fixture finding so that unrelated machinery
/// keeps working.
///
/// #4999: the hunk is `@@ -1 +1 @@`, so this diff reaches line 1 and nothing
/// further. A fixture finding for this file must cite line 1 — a larger line is
/// now dropped as beyond the file's last diffed line, which silently empties the
/// findings list the test is about.
fn local_diff_source_for_file(
    file: &str,
    added_line: &str,
) -> (DiffSource, tempfile::NamedTempFile) {
    let diff = format!(
        "diff --git a/{file} b/{file}\nindex 0000000..1111111 100644\n--- a/{file}\n+++ b/{file}\n@@ -1 +1 @@\n{added_line}\n"
    );
    local_diff_source(&diff)
}

// ── Helper to build a two-commit git repo for GitRange tests (#2993) ───

/// Build a tiny git repo in a tempdir with two commits, returning the dir and
/// a `GitRange` source spanning them (base..head), so `run_review` can be
/// driven end-to-end from a real `git diff` rather than a hand-written diff.
fn git_range_source() -> (DiffSource, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 git output")
    };

    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write a.rs");
    run(&["add", "a.rs"]);
    run(&["commit", "-q", "-m", "base"]);
    let base = run(&["rev-parse", "HEAD"]).trim().to_string();

    std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").expect("write a.rs");
    run(&["add", "a.rs"]);
    run(&["commit", "-q", "-m", "head"]);
    let head = run(&["rev-parse", "HEAD"]).trim().to_string();

    let source = DiffSource::GitRange {
        repo_root: dir.path().to_path_buf(),
        base,
        head: Some(head),
    };
    (source, dir)
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
    let (source, _tmp) = local_diff_source_for_file(
        "src/a.rs",
        "+fn bad_query(id: &str) { db.exec(format!(\"SELECT * FROM users WHERE id={id}\")) }",
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::request_changes()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].kind, "SQL injection");
    assert_eq!(
        result.findings_count, 1,
        "findings_count must equal findings.len() on the completed path (#1877)"
    );
}

/// #1877: a REAL LLM-error abort still explicitly syncs `findings_count` (to
/// 0, since no findings were ever parsed) rather than leaving it stale.
#[tokio::test]
async fn findings_count_matches_len_on_abort() {
    let (source, _tmp) = local_diff_source("+fn hello() {}\n");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::errors("boom")), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.findings_count, result.findings.len());
    assert_eq!(result.findings_count, 0);
}

/// #1877: a large diff approved with zero findings but an implausibly small
/// output-token spend must be flagged `shallow_clean_review` and have its
/// grade capped at B- rather than defaulting to A+.
#[tokio::test]
async fn run_review_flags_shallow_clean_review_on_large_diff() {
    // Build a diff long enough (~12K chars) to exceed the shallow-review
    // min-diff-len floor after filtering/rendering, but well under
    // MAX_DIFF_CHARS (160K) so it stays on the unified (non-map-reduce) path.
    let mut diff = String::from("diff --git a/src/big.rs b/src/big.rs\n");
    diff.push_str("--- a/src/big.rs\n+++ b/src/big.rs\n");
    diff.push_str("@@ -1,1 +1,300 @@\n");
    let filler_line = "+    let _padding = compute_value(some_argument_here);\n";
    while diff.len() < 12_000 {
        diff.push_str(filler_line);
    }

    let (source, _tmp) = local_diff_source(&diff);
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // Zero-findings APPROVE with only 10 output tokens — implausibly cheap for
    // a diff this size (mirrors the reported `pricerator#637` regression).
    let deps = ready_deps(Arc::new(FakeLlm::approves_with_output_tokens(10)), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.verdict, Verdict::Approve);
    assert_eq!(result.findings.len(), 0);
    assert!(
        result.shallow_clean_review,
        "large diff + near-zero output tokens + zero findings must be flagged shallow (#1877)"
    );
    assert_eq!(
        result.grade.as_deref(),
        Some("B-"),
        "a flagged shallow review must have its grade capped at B- (#1877)"
    );
}

/// #1886: the top-level `grade` and the grade embedded in `review_body` must be
/// EQUAL even when a late-stage adjustment (the #1877 shallow-review cap) lowers
/// the top-level grade below the model's self-assessed grade.
///
/// Why: a divergence here misleads any caller that reads only one of the two
/// fields (e.g. a PM merge gate reading `grade` while the review comment shows the
/// embedded grade) — the exact regression #1886 reported (outer "C+" vs embedded
/// "B+" across PRs #1879/#1883/#1884).
/// What: runs a large-diff zero-findings APPROVE whose JSON self-grades "A+" with
/// an implausibly small token spend, so the shallow cap lowers the top-level grade
/// to "B-"; then re-parses the embedded grade out of the returned `review_body`
/// and asserts it equals the top-level grade (and is no longer the stale "A+").
/// Test: this test itself (no network).
#[tokio::test]
async fn run_review_outer_and_embedded_grade_agree_after_shallow_cap() {
    let mut diff = String::from("diff --git a/src/big.rs b/src/big.rs\n");
    diff.push_str("--- a/src/big.rs\n+++ b/src/big.rs\n");
    diff.push_str("@@ -1,1 +1,300 @@\n");
    let filler_line = "+    let _padding = compute_value(some_argument_here);\n";
    while diff.len() < 12_000 {
        diff.push_str(filler_line);
    }

    let (source, _tmp) = local_diff_source(&diff);
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // Model self-grades A+, but the shallow-review cap lowers the top-level grade.
    let deps = ready_deps(
        Arc::new(FakeLlm::approves_with_embedded_grade("A+", 10)),
        None,
    );

    let result = run_review(&config, input, deps).await;

    assert!(
        result.shallow_clean_review,
        "large diff + near-zero tokens + zero findings must be flagged shallow (#1877)"
    );
    let top_level = result
        .grade
        .clone()
        .expect("a completed APPROVE review must carry a grade");
    assert_eq!(
        top_level, "B-",
        "shallow cap must lower the top-level grade (#1877)"
    );

    // Re-parse the grade embedded in the returned review_body: it must now mirror
    // the authoritative top-level grade, NOT the model's original "A+" (#1886).
    let embedded = crate::pipeline::parser::parse_review_response(&result.review_body)
        .grade
        .expect("review_body must still embed a parseable grade");
    assert_eq!(
        embedded, top_level,
        "embedded review_body grade must equal the top-level grade (#1886)"
    );
    assert_ne!(
        embedded, "A+",
        "the stale model self-grade must have been reconciled away (#1886)"
    );
}

/// REGRESSION (#1902): the top-level `verdict` and the verdict embedded in
/// `review_body` agree after the severity floor moves the top-level one.
///
/// Why: the reported harm — `review_pr` on PR #1901 returned a top-level
/// `BLOCK` while `review_body` said `APPROVE`. An automated merge gate
/// correctly refused on the `BLOCK`, and the human reading the review saw an
/// approval. #1886 fixed this class for `grade` and left `verdict` behind.
/// What: runs a review whose model self-assesses APPROVE while reporting a
/// High-severity finding, so the floor escalates the top-level verdict; then
/// re-parses `review_body` and asserts the embedded verdict equals the
/// top-level one and is no longer the stale APPROVE.
/// Test: this test itself (no network).
#[tokio::test]
async fn run_review_outer_and_embedded_verdict_agree_after_severity_floor() {
    let (source, _tmp) = local_diff_source_for_file(
        "src/a.rs",
        "+fn bad_query(id: &str) { db.exec(format!(\"SELECT * FROM users WHERE id={id}\")) }",
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(FakeLlm::self_approves_with_blocking_finding()),
        None,
    );

    let result = run_review(&config, input, deps).await;

    assert_ne!(
        result.verdict,
        Verdict::Approve,
        "a High-severity finding must move the top-level verdict off the model's APPROVE"
    );
    let embedded = crate::pipeline::parser::parse_review_response(&result.review_body).verdict;
    assert_eq!(
        embedded, result.verdict,
        "the verdict embedded in review_body must equal the authoritative one (#1902)"
    );
    assert_ne!(
        embedded,
        Verdict::Approve,
        "the model's stale pre-floor APPROVE must have been reconciled away (#1902)"
    );
}

/// #1877: the same large diff, but with a token spend at/above the
/// proportional floor, must NOT be flagged — a genuinely thorough pass stays
/// A+.
#[tokio::test]
async fn run_review_large_diff_with_sufficient_tokens_not_flagged_shallow() {
    // ~5K chars → proportional floor is max(5000/200, 50) = 50; the default
    // FakeLlm::approves() output_tokens is 50, which is NOT below the floor.
    let mut diff = String::from("diff --git a/src/big.rs b/src/big.rs\n");
    diff.push_str("--- a/src/big.rs\n+++ b/src/big.rs\n");
    diff.push_str("@@ -1,1 +1,120 @@\n");
    let filler_line = "+    let _padding = compute_value(some_argument_here);\n";
    while diff.len() < 5_000 {
        diff.push_str(filler_line);
    }

    let (source, _tmp) = local_diff_source(&diff);
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        !result.shallow_clean_review,
        "sufficient token spend for the diff size must not be flagged shallow (#1877)"
    );
    assert_eq!(result.grade.as_deref(), Some("A+"));
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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

/// A findings-parse failure reaches the rendered result as an error (#4491).
///
/// Why: the parser failing closed is only half the fix — if the reason never
/// leaves the tracing log, the run still prints `Findings: none` with nothing
/// telling the reader the review did not parse.
/// What: drives a response whose findings payload cannot be decoded, asserts the
/// verdict is UNKNOWN and `result.error` names the parse failure (which
/// `print_review_result` renders as a `Pipeline error:` line).
#[tokio::test]
async fn run_review_findings_parse_failure_sets_error() {
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::unparseable_findings()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "a lost findings payload must fail CLOSED, never render APPROVE (#4491)"
    );
    assert!(result.findings.is_empty());
    let err = result
        .error
        .expect("a findings-parse failure must set an actionable error (#4491)");
    assert!(
        err.contains("findings"),
        "the error must say the findings were not parsed: {err}"
    );
}

/// PATHOLOGICAL SINGLE-GIANT-HUNK BACKSTOP (#1638 + #1643): a single file whose
/// ONE hunk alone exceeds the per-file budget cannot be reviewed without
/// truncation even by the map-reduce path, so it must still fail CLOSED to
/// UNKNOWN — never APPROVE a diff the reviewer could not fully see.
///
/// Why: #1643 replaced the unconditional fail-closed guard with the per-file
/// map-reduce path, but a single hunk larger than `per_file_chars` is the one
/// case the splitter cannot sub-divide (whole-hunk is the only safe boundary).
/// The map stage fails THAT chunk closed (`hunk_oversized`), and with no other
/// reviewable chunk the reduce + runner backstop returns UNKNOWN.  This proves
/// the #1639 fail-closed guard survives as a backstop for the pathological case.
/// What: builds a single-file diff that is ONE hunk far larger than
/// `MAX_DIFF_CHARS` (so it also exceeds the 120 K per-file budget), with a
/// distinctive changed signature on the LAST line, runs the pipeline with a
/// FakeLlm that would APPROVE, and asserts UNKNOWN + an actionable "could not
/// review" error.
/// Test: this test itself (no network).
#[tokio::test]
async fn run_review_oversized_single_hunk_fails_closed_to_unknown() {
    use crate::config::constants::MAX_DIFF_CHARS;

    // A valid unified-diff header, then a huge body of added lines forming ONE
    // hunk that pushes the rendered diff well past MAX_DIFF_CHARS *and* past the
    // 120 K per-file map budget, then a changed signature on the tail.
    let mut diff = String::from("diff --git a/src/big.rs b/src/big.rs\n");
    diff.push_str("--- a/src/big.rs\n+++ b/src/big.rs\n");
    diff.push_str("@@ -1,1 +1,100000 @@\n");
    // ~2x the cap of added content in a SINGLE hunk so neither render nor the
    // splitter can divide it — the pathological case.
    let filler_line = "+    let _padding = compute_value(some_argument_here);\n";
    while diff.len() < MAX_DIFF_CHARS * 2 {
        diff.push_str(filler_line);
    }
    // The changed signature that the reviewer could only see if the chunk were
    // not over-cap — placed at the very end.
    diff.push_str("+pub fn build(a: i32, b: i32, c: i32, previous: Option<i32>) -> i32 { 0 }\n");

    let (source, _tmp) = local_diff_source(&diff);
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // FakeLlm would APPROVE the chunk if it were ever consulted — proving the
    // backstop fires (the chunk is failed-closed before any partial APPROVE).
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "an over-cap single-giant-hunk diff must fail CLOSED to UNKNOWN, never APPROVE (#1638/#1643 backstop)"
    );
    let err = result
        .error
        .expect("a pathological over-cap diff must set an actionable error");
    assert!(
        err.contains("could not review") || err.contains("assessed no files"),
        "error must explain the review could not complete: {err}"
    );
    assert!(
        !result.posted,
        "a fail-closed review must never be posted live"
    );
    assert!(result.dry_run, "a fail-closed review is dry-run");
}

/// REGRESSION GUARD (#1638): a normal, under-cap diff must STILL be reviewed
/// (the truncation guard must not false-positive on complete diffs).
///
/// Why: the fail-closed guard must only fire when content was actually dropped;
/// a complete diff (the overwhelming common case) must review normally.
/// What: a tiny diff with no truncation marker reviews to APPROVE as before.
/// Test: this test itself.
#[tokio::test]
async fn run_review_under_cap_diff_is_not_flagged_truncated() {
    let (source, _tmp) = local_diff_source(
        "diff --git a/src/s.rs b/src/s.rs\n--- a/src/s.rs\n+++ b/src/s.rs\n\
         @@ -1 +1 @@\n+pub fn build(a: i32, prev: Option<i32>) -> i32 { 0 }\n",
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "a complete under-cap diff must review normally (no false-positive truncation)"
    );
    assert!(
        result.error.is_none(),
        "no error expected for a complete diff: {:?}",
        result.error
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
/// This ALSO doubles as the "hosted CI/webhook mode is unchanged" regression
/// guard for the search-unreachable semantics fix (problem B, scenario c):
/// `InvocationSurface::default()` is `Hosted` (the type used by the webhook bot
/// and CLI GitHub-PR runs) and `require_search` is left unconfigured, so this
/// exercises exactly the "hosted, no explicit override" combination that must
/// keep hard-Skipping.
/// What: a failing search + `Hosted` surface + unconfigured `require_search`
/// must yield `status = Skipped`, `infra_unavailable = true`, an actionable
/// error, and NO LLM-derived APPROVE.
#[tokio::test]
async fn run_review_search_down_skips_when_required() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let config = default_config(); // require_search unconfigured (None)
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::Hosted, // hosted CI/webhook default
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
        "search down + Hosted surface must SKIP, not silently APPROVE"
    );
    assert!(
        result.infra_unavailable,
        "a genuine infra outage must set infra_unavailable so the MCP layer \
         (when this path is reached via MCP) can be loud about it"
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
    config.context.require_search = Some(false); // explicit opt-out
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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

/// #2994 re-review, finding #2: the persisted (degraded) review body must
/// contain the `--source-root`-specific notice text — e.g. the actionable
/// "Run `trusty-search index <dir>`" hint — not just a generic "trusty-search
/// unavailable" banner, matching README.md's documented behaviour that the
/// notice is prepended as a banner.
///
/// Why: this is the actual end-to-end path exercised by `cmd_run`/`cmd_compare`
/// when `--source-root` doesn't match a registered index: `deps.search` is a
/// `NullSearchClient` built from the notice, and `context.require_search` is
/// cleared so the gate degrades instead of skipping. The diff source is a
/// local diff (never GitHub), so `cmd_run` computes `InvocationSurface::Interactive`
/// for it — matched here so the test exercises the composed real path rather
/// than the (irrelevant, since `require_search` is an explicit override here)
/// `Hosted` default.
/// What: drives `run_review` with a real `NullSearchClient` carrying a
/// source-root-shaped notice; asserts the notice's actionable text appears in
/// `result.review_body` (via `degraded_banner`), not merely in `result.error`.
/// Test: this test.
#[tokio::test]
async fn run_review_source_root_fallback_banner_carries_notice_text() {
    use crate::integrations::NullSearchClient;

    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.context.require_search = Some(false); // set by resolve_source_root's DiffOnly branch
    let notice = "--source-root /tmp/proj-2994 has no registered trusty-search index — \
                  proceeding in diff-only mode (no code-context retrieval). Run \
                  `trusty-search index /tmp/proj-2994` to enable full context, or omit \
                  --source-root to use the auto-derived/TRUSTY_SEARCH_INDEX index.";
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::Interactive,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()),
        verifier: None,
        search: Arc::new(NullSearchClient::new(notice)),
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert_eq!(result.status, ReviewStatus::Degraded);
    assert!(
        !result.infra_unavailable,
        "source-root diff-only fallback is a real (degraded) review, not the loud infra-Skip path"
    );
    assert!(
        result
            .review_body
            .contains("Run `trusty-search index /tmp/proj-2994`"),
        "persisted review body must carry the --source-root-specific notice text, not a \
         generic banner: {:?}",
        result.review_body
    );
}

/// SEARCH-UNREACHABLE SEMANTICS FIX (problem B, scenario b): an
/// `InvocationSurface::Interactive` caller (MCP tool call / CLI
/// `--local-diff`/`--base`/`--source-root`) with search down and NO explicit
/// `require_search` override DEGRADES — it does NOT hard-Skip — and the LLM is
/// actually called, producing a real (non-`Unknown`) verdict on the diff.
///
/// Why: this is the "interactive/local surfaces should degrade instead of
/// hard-skip" half of the fix; unlike `run_review_search_down_degraded_when_optout`
/// above (which tests an EXPLICIT opt-out), this exercises the NEW per-surface
/// DEFAULT with no config override at all.
/// What: `Interactive` surface + failing search + unconfigured `require_search`
/// must yield `status = Degraded`, `infra_unavailable = false` (it is not the
/// loud infra-Skip path), a loud non-authoritative banner, AND a real verdict
/// (proving the LLM ran on the diff rather than being skipped).
#[tokio::test]
async fn run_review_interactive_surface_search_down_degrades_by_default() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let config = default_config(); // require_search unconfigured (None)
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::Interactive,
    };
    let deps = ReviewDeps {
        llm: Arc::new(FakeLlm::approves()),
        verifier: None,
        search: Arc::new(FailingSearch), // search down, no override
        analyze: Some(Arc::new(ReadyAnalyze)),
        dedup: None,
    };

    let result = run_review(&config, input, deps).await;
    assert_eq!(
        result.status,
        ReviewStatus::Degraded,
        "Interactive surface + search down + unconfigured must DEGRADE by default, not Skip"
    );
    assert!(
        !result.infra_unavailable,
        "a Degraded outcome is not the loud infra-Skip path"
    );
    assert!(
        result.review_body.contains("NOT AUTHORITATIVE"),
        "degraded body must carry a loud banner: {:?}",
        result.review_body
    );
    assert_ne!(
        result.verdict,
        Verdict::Unknown,
        "the LLM must actually have run and produced a real verdict on the diff"
    );
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
async fn run_review_serve_mode_empty_token_fails_closed_with_actionable_error() {
    // Regression for #1880: serve-mode callers (POST /review, the webhook
    // dispatcher) build `DiffSource::Github` with an empty placeholder token,
    // relying on `run_review` to resolve the real one via `resolve_diff_token`
    // before the diff fetch. With no GitHub App credentials configured, this
    // must fail CLOSED with an actionable error field — never proceed to
    // `load_diff` with an empty bearer token (which previously surfaced as an
    // opaque `401 Bad credentials` from GitHub instead of a clear diagnostic).
    // #6062 moved the stop one step earlier: the same missing credentials fail
    // the PR-metadata fetch first, so the run now halts on the empty head SHA
    // and carries that fetch error's text along — which is what keeps the
    // assertion below meaningful.
    let mut config = default_config();
    config.github_app_id = None;
    config.github_app_private_key = None;
    config.github_installations = Vec::new();

    let input = ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
            pr: 7,
            token: String::new(),
        },
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Serve,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // #5113: the serve path declares `DedupNeed::Required`, so a faithful
    // fixture carries a real store — without one the run now aborts at the
    // claim-gate guard before it ever reaches token resolution.
    let dir = tempfile::tempdir().unwrap();
    let mut deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    deps.dedup = Some(Arc::new(
        crate::store::DedupStore::open(&dir.path().join("dedup.redb")).expect("open"),
    ));

    let result = run_review(&config, input, deps).await;

    assert!(
        result.error.is_some(),
        "empty-token diff fetch without App credentials must set an error"
    );
    let error = result.error.expect("checked above");
    assert!(
        error.contains("GITHUB_APP_ID") || error.contains("GITHUB_APP_PRIVATE_KEY"),
        "error must name the missing config so an operator can fix it: {error}"
    );
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "fail-closed path must never fabricate a verdict"
    );
    assert!(!result.posted, "a failed token resolution must never post");
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
async fn run_review_git_range_is_dry_run_and_not_posted() {
    // Same guarantee as `run_review_local_diff_is_dry_run_and_not_posted`,
    // exercised end-to-end against a real two-commit git repo (#2993): even
    // with the trigger forcing live and posting allowed, a GitRange source
    // must never post.
    let (source, _dir) = git_range_source();
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert!(
        result.dry_run,
        "git-range source must never post — always dry-run"
    );
    assert!(!result.posted, "git-range must not be marked posted");
}

#[tokio::test]
async fn run_review_stdin_is_dry_run_and_not_posted() {
    // Same guarantee, for the `Stdin` source (#2993). `run_review` never
    // reads real stdin in this test — `Stdin` loading is exercised via
    // `read_diff_from_reader` unit tests in `diff.rs`; here we only need to
    // confirm the source-selection dry-run/no-post wiring, so an empty stdin
    // (nothing piped into `cargo test`) reading to "" is fine — the pipeline
    // still runs through the fail-safe/gate path and must stay dry-run.
    let mut config = default_config();
    config.dry_run = false; // service-live default
    let input = ReviewInput {
        diff_source: DiffSource::Stdin,
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::ForceLive, // would post if GitHub-sourced
        run_mode: RunMode::Serve,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;
    assert!(
        result.dry_run,
        "stdin source must never post — always dry-run"
    );
    assert!(!result.posted, "stdin must not be marked posted");
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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

/// The verification round, when wired in, CONFIRMS the Medium finding; #1876
/// (superseding #1015) re-escalates a lone confirmed high-confidence Medium
/// back to REQUEST_CHANGES.
///
/// Why: path (a2)'s baseline is still capped at APPROVE* for a confirmed
/// Medium-only finding (#1015 mechanics unchanged) — but `derive_verdict`'s own
/// severity floor, computed independently from the surviving findings, now
/// (#1876) floors a SINGLE confidence > 0.80 Medium to REQUEST_CHANGES on its
/// own merits (see `grade::correctness_floor`). `stricter_of(baseline=APPROVE*,
/// floor=REQUEST_CHANGES)` therefore lands back on REQUEST_CHANGES — matching
/// what the model itself originally said, which is the intended #1876 outcome
/// (a confirmed, well-evidenced finding must not be silently softened).
#[tokio::test]
async fn run_review_verification_confirms_and_preserves_verdict() {
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(FakeLlm::request_changes()),
        Some(Arc::new(FakeVerifier {
            judgment: "CONFIRMED",
        })),
    );

    let result = run_review(&config, input, deps).await;
    // #1876: a confirmed high-confidence Medium re-escalates to REQUEST_CHANGES
    // via derive_verdict's floor, even though the a2 baseline is capped at
    // APPROVE* (supersedes the #1015-era APPROVE* result).
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "confirmed high-confidence Medium → REQUEST_CHANGES (#1876 floor \
         re-escalation; path a2 baseline cap alone is no longer the last word)"
    );
}

/// When verification is disabled by config, the verifier is never consulted and
/// the verdict is the un-verified grade.
#[tokio::test]
async fn run_review_verification_disabled_skips_round() {
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
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
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
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

/// REGRESSION GUARD (#1486): when a High-effort finding causes the severity floor
/// to escalate the LLM's APPROVE/B- to BLOCK/F, but verification then REFUTES
/// that finding, the envelope verdict must relax (BLOCK → APPROVE) AND the
/// envelope grade must also relax (F → B-), not stay pinned at F.
///
/// Root cause (before fix): step 7d clamped the FLOOR-ESCALATED grade (F) to the
/// post-verification verdict.  `clamp_grade_to_verdict(F, APPROVE)` is a no-op
/// (F implies BLOCK which is already stricter than APPROVE), so the grade stayed F
/// even though the verdict became APPROVE.  The fix clamps the ORIGINAL LLM grade
/// (B-) to the post-verification verdict instead.
///
/// Why this test matters: any automation gating on the top-level `grade` field
/// (not just `verdict`) would see F/APPROVE — an incoherent state.
/// What: runs the full pipeline with a FakeLlm that emits APPROVE/B- plus a
/// High-effort/0.95-confidence finding, a FakeVerifier that refutes that finding,
/// and asserts that the post-verification envelope has verdict=APPROVE and
/// grade=B-.
#[tokio::test]
async fn envelope_grade_tracks_verdict_after_verification_relaxation_1486() {
    // LLM says: clean APPROVE with grade B-, but there is a high-severity finding
    // that the LLM is nonetheless confident about (confidence 0.95).  The severity
    // floor in derive_verdict_with_grade escalates this to BLOCK/F.  Verification
    // then refutes the high-severity finding → verdict drops back to APPROVE.
    let llm_response = r#"Code looks good overall, minor concern.

```json
{"verdict":"APPROVE","grade":"B-","summary":"Looks solid","findings":[{"title":"Potential XSS","body":"line 1 unescaped","severity":"high","confidence":0.95,"file":"src/render.rs","line":1}]}
```"#;
    let (source, _tmp) = local_diff_source_for_file(
        "src/render.rs",
        "+fn render(s: &str) { println!(\"{s}\"); }",
    );
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "fake-model".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(FakeLlm {
            response: llm_response.to_string(),
            error: None,
            output_tokens: None,
        }),
        Some(Arc::new(FakeVerifier {
            judgment: "REFUTED",
        })),
    );

    let result = run_review(&config, input, deps).await;

    // After verification refutes the High-effort finding, the verdict must relax.
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "#1486: verification refutes the only blocking finding → verdict must be APPROVE (got {:?})",
        result.verdict,
    );

    // The envelope grade must be consistent with APPROVE, not the pre-verification F.
    let grade = result.grade.as_deref().unwrap_or("(none)");
    // B- maps to APPROVE; any APPROVE-band grade (A+ through B-) is correct here.
    // The specific value is B- (the original LLM grade, clamped to APPROVE which
    // accepts any grade, so no clamping occurs → grade stays B-).
    assert_eq!(
        grade, "B-",
        "#1486: envelope grade must be the original LLM grade B- after verification \
         relaxes the verdict to APPROVE (before fix, it was F)"
    );

    // Sanity: the finding is preserved (demoted, not dropped) and is refuted.
    assert_eq!(result.findings.len(), 1, "finding must be preserved");
    assert!(
        matches!(
            result.findings[0].verified,
            Some(crate::models::VerifyOutcome::Refuted)
        ),
        "the High-effort finding must be marked Refuted"
    );
}

/// REGRESSION GUARD (#1486 — stable-escalation path): when a High-effort finding
/// is CONFIRMED by verification, the envelope must stay at BLOCK/F (the floor
/// escalation correctly survives).
///
/// Why: the #1486 fix must not accidentally soften verdicts where the escalating
/// finding WAS confirmed — only the refuted case should relax.
#[tokio::test]
async fn envelope_grade_stays_block_when_high_effort_confirmed_1486() {
    let llm_response = r#"Review with confirmed critical finding.

```json
{"verdict":"APPROVE","grade":"B-","summary":"Mostly OK","findings":[{"title":"Auth bypass","body":"line 1","severity":"high","confidence":0.95,"file":"src/auth.rs","line":1,"code_provable":true}]}
```"#;
    let (source, _tmp) = local_diff_source_for_file("src/auth.rs", "+fn auth(t: &str) {}");
    let config = default_config();
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "fake-model".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(FakeLlm {
            response: llm_response.to_string(),
            error: None,
            output_tokens: None,
        }),
        Some(Arc::new(FakeVerifier {
            judgment: "CONFIRMED",
        })),
    );

    let result = run_review(&config, input, deps).await;

    // High-effort confirmed → BLOCK floor must survive verification.
    assert_eq!(
        result.verdict,
        Verdict::Block,
        "#1486 stable path: confirmed High-effort finding must keep verdict at BLOCK"
    );
    // Grade must be clamped to F (BLOCK's band ceiling).
    let grade = result.grade.as_deref().unwrap_or("(none)");
    assert_eq!(
        grade, "F",
        "#1486 stable path: confirmed BLOCK must clamp B- → F (consistent with verdict)"
    );
}

/// `attach_inline_comments` maps on-diff findings to inline comments and leaves
/// off-diff findings for the summary body (#1414).
///
/// Why: this is the runner-side glue that turns findings + the raw diff into the
/// `inline_comments` set; a regression would either post off-diff anchors (which
/// GitHub rejects, failing the whole review) or never post any inline comment.
/// What: builds a result with one on-diff finding (line 1, present in the hunk)
/// and one off-diff finding (line 999), calls `attach_inline_comments`, asserts
/// exactly one inline comment for the on-diff finding.
/// Test: this test itself (no network).
#[test]
fn attach_inline_comments_maps_on_diff() {
    use crate::models::{Effort, Finding};
    use crate::pipeline::runner_helpers::attach_inline_comments;

    let raw_diff = "\
diff --git a/src/db.rs b/src/db.rs
--- a/src/db.rs
+++ b/src/db.rs
@@ -1,1 +1,2 @@
 fn a() {}
+fn b() {}
";
    let mut result = crate::models::ReviewResult::new("o", "r", 1, "t", "u");
    let mut on_diff = Finding::new("src/db.rs", "bug", "desc", "fix", 0.9, Effort::Medium);
    on_diff.line = Some(2); // the added `fn b()` line on the new side.
    let mut off_diff = Finding::new("src/db.rs", "bug2", "desc2", "fix2", 0.9, Effort::Medium);
    off_diff.line = Some(999);
    result.findings = vec![on_diff, off_diff];

    attach_inline_comments(&mut result, raw_diff);

    assert_eq!(
        result.inline_comments.len(),
        1,
        "only the on-diff finding becomes an inline comment"
    );
    assert_eq!(result.inline_comments[0].path, "src/db.rs");
    assert_eq!(result.inline_comments[0].line, 2);
}

/// A refuted finding cannot reach the PR as an unmarked inline comment (#5312).
///
/// Why: verification runs before `attach_inline_comments` on both pipeline paths
/// (`runner.rs` `maybe_verify` → `attach_inline_comments`, and the same order in
/// `runner_mapreduce.rs`), so the `verified` outcome IS on the finding by the
/// time the inline plan is built — it was simply never read. A reader of the PR
/// saw a claim the verifier had disproved rendered exactly like a surviving one.
/// What: runs the real runner-side glue over two on-diff findings — one refuted,
/// one unjudged — and asserts the refuted one's inline body carries the
/// verification caveat while the unjudged one does not.
/// Test: this test itself (no network).
#[test]
fn attach_inline_comments_marks_refuted_finding() {
    use crate::models::{Effort, Finding, VerifyOutcome};
    use crate::pipeline::runner_helpers::attach_inline_comments;

    let raw_diff = "\
diff --git a/src/db.rs b/src/db.rs
--- a/src/db.rs
+++ b/src/db.rs
@@ -1,1 +1,3 @@
 fn a() {}
+fn b() {}
+fn c() {}
";
    let mut result = crate::models::ReviewResult::new("o", "r", 1, "t", "u");
    let mut refuted = Finding::new("src/db.rs", "bug", "desc", "fix", 0.1, Effort::High);
    refuted.line = Some(2);
    refuted.verified = Some(VerifyOutcome::Refuted);
    let mut surviving = Finding::new("src/db.rs", "bug2", "desc2", "fix2", 0.9, Effort::Medium);
    surviving.line = Some(3);
    result.findings = vec![refuted, surviving];

    attach_inline_comments(&mut result, raw_diff);

    assert_eq!(
        result.inline_comments.len(),
        2,
        "both findings anchor inline"
    );
    assert!(
        result.inline_comments[0].body.contains("REFUTED"),
        "a refuted finding must be marked inline: {}",
        result.inline_comments[0].body
    );
    assert!(
        !result.inline_comments[1].body.contains("Verification:"),
        "a surviving finding must stay unqualified: {}",
        result.inline_comments[1].body
    );
}

/// `build_author_rationale` returns None when neither input is present (#1618).
///
/// Why: with no caller context the verifier prompt must be unchanged.
#[test]
fn build_author_rationale_none_when_empty() {
    use crate::pipeline::runner_helpers::build_author_rationale;
    assert!(build_author_rationale(None, None).is_none());
    // Whitespace-only inputs are treated as absent.
    assert!(build_author_rationale(Some("  "), Some("\n\t")).is_none());
}

/// `build_author_rationale` folds description + discussion under headings (#1618).
///
/// Why: the verifier needs the author's words under clear labels; the combined
/// block is what `maybe_verify` passes through.
#[test]
fn build_author_rationale_combines_present_fields() {
    use crate::pipeline::runner_helpers::build_author_rationale;
    let out = build_author_rationale(Some("Adds retry guard."), Some("Checked the source."))
        .expect("present fields yield Some");
    assert!(out.contains("## PR Description"));
    assert!(out.contains("Adds retry guard."));
    assert!(out.contains("## PR Discussion / Author Rationale"));
    assert!(out.contains("Checked the source."));
}

/// `build_author_rationale` includes only the present field (#1618).
///
/// Why: a caller may supply just the discussion (or just the description); the
/// block must contain only what was given, with no empty heading for the other.
#[test]
fn build_author_rationale_single_field_only() {
    use crate::pipeline::runner_helpers::build_author_rationale;
    let only_disc = build_author_rationale(None, Some("Author checked the data source."))
        .expect("discussion present yields Some");
    assert!(only_disc.contains("## PR Discussion / Author Rationale"));
    assert!(only_disc.contains("Author checked the data source."));
    assert!(
        !only_disc.contains("## PR Description"),
        "absent description must not render a heading"
    );
}

// ─── #5064: the dedup claim gate fails closed ────────────────────────────────

/// Why: this is the arm that made #5064 dangerous. The store now refuses to
/// start without a claim gate, but the *operation* path used to log
/// `"dedup claim failed (proceeding without dedup)"` and review anyway — so a
/// stuck holder produced an ungated live comment, and another on redelivery.
/// A stuck holder is not hypothetical: a rolling upgrade where a pre-fix
/// `serve --stdio` is still running produces exactly one.
/// What: `Contended` must map to `Abort`, never `Proceed`.
/// Test: this test.
#[test]
fn classify_claim_contended_aborts() {
    let gate = classify_claim(Err(DedupError::Contended {
        path: "/tmp/dedup.redb".to_string(),
        waited_ms: 2000,
    }));
    match gate {
        ClaimGate::Abort(reason) => assert!(
            reason.contains("locked"),
            "the abort reason must name the contention: {reason}"
        ),
        ClaimGate::Proceed => {
            panic!("a contended claim must NOT proceed — that posts an ungated comment (#5064)")
        }
        ClaimGate::DuplicateSkip => panic!("a contended claim is not a duplicate"),
        ClaimGate::InProgressElsewhere => panic!("a contended claim is not a live claim"),
    }
}

/// Why: every `DedupError` means the same thing operationally — the gate did
/// not engage — so the fail-closed rule cannot be special-cased to one variant.
/// What: `Open`, `Transaction`, and `Serde` all abort too.
/// Test: this test.
#[test]
fn classify_claim_open_error_aborts() {
    for err in [
        DedupError::Open("no such file".to_string()),
        DedupError::Transaction("commit failed".to_string()),
        DedupError::Serde("bad json".to_string()),
    ] {
        assert!(
            matches!(classify_claim(Err(err)), ClaimGate::Abort(_)),
            "every store error must abort — the gate did not engage"
        );
    }
}

/// Why: the fail-closed rule must not break the happy path.
/// What: `Claimed` proceeds.
/// Test: this test.
#[test]
fn classify_claim_claimed_proceeds() {
    assert!(matches!(
        classify_claim(Ok(ClaimOutcome::Claimed)),
        ClaimGate::Proceed
    ));
}

/// Why: a completed review for this SHA is the case dedup exists to catch.
/// What: `Skipped` is a duplicate, distinct from an abort.
/// Test: this test.
#[test]
fn classify_claim_skipped_is_duplicate() {
    assert!(matches!(
        classify_claim(Ok(ClaimOutcome::Skipped)),
        ClaimGate::DuplicateSkip
    ));
}

/// REGRESSION (#5126): a claim another holder owns must not classify as a
/// completed duplicate.
///
/// Why: `DuplicateSkip` is the arm that sets `Verdict::Approve` and returns. A
/// stranded `InProgress` record — what a process that died between `claim` and
/// `complete` leaves behind — reached that arm too, so for the whole
/// `DEDUP_STALE_SECS` window every re-run of that PR reported approval for a
/// review that never ran. This drives the outcome through a real store rather
/// than a hand-built enum value, so it exercises the store and the classifier
/// together and compiles against the pre-fix source as well.
/// What: one store writes an in-progress claim; a second store on the same file
/// claims the same key; the resulting gate must not be `DuplicateSkip`.
/// Test: this test. Fails pre-fix at the first assertion — the second claim
/// returned `Skipped`, which classified as `DuplicateSkip`.
#[tokio::test]
async fn stranded_in_progress_claim_is_not_a_duplicate_skip() {
    use crate::store::DedupStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");

    // A holder claims the slot and then dies without completing or releasing.
    let stranded = DedupStore::open(&path).expect("open");
    stranded
        .claim_blocking("acme", "backend", 42, "sha-stranded")
        .expect("claim");
    drop(stranded);

    let store = Arc::new(DedupStore::open(&path).expect("open"));
    let gate = classify_claim(store.claim("acme", "backend", 42, "sha-stranded").await);

    assert!(
        !matches!(gate, ClaimGate::DuplicateSkip),
        "a stranded in-progress claim is not a completed review — classifying it as one \
         is what made the runner report Verdict::Approve for an unrun review (#5126)"
    );
    assert!(
        matches!(gate, ClaimGate::InProgressElsewhere),
        "the runner must be told the slot is held, so it can report 'not reviewed'"
    );
}

/// Why: fixing the stranded-claim arm must not disarm the case dedup exists
/// for — a genuinely completed review must still short-circuit.
/// What: a store completes a claim; a second store's claim for the same key
/// still classifies as `DuplicateSkip`.
/// Test: this test.
#[tokio::test]
async fn completed_claim_still_classifies_as_duplicate_skip() {
    use crate::store::DedupStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");

    let owner = DedupStore::open(&path).expect("open");
    owner
        .claim_blocking("acme", "backend", 42, "sha-done")
        .expect("claim");
    owner
        .complete_blocking("acme", "backend", 42, "sha-done")
        .expect("complete");
    drop(owner);

    let store = Arc::new(DedupStore::open(&path).expect("open"));
    let gate = classify_claim(store.claim("acme", "backend", 42, "sha-done").await);

    assert!(
        matches!(gate, ClaimGate::DuplicateSkip),
        "a completed review must still suppress the re-run"
    );
}

/// Why: the round-1 fix routed the failed-claim path into `abort_dry`, which
/// releases the dedup claim so a retry can re-run. That is right for every
/// other abort — they own the claim — and wrong for this one, which never
/// acquired it. `release` is an unconditional `table.remove(key)`, so a failed
/// claim erased whatever record was on disk, including a `Completed` one
/// another process wrote. The next trigger for that SHA then posts a duplicate
/// comment: exactly the outcome the fail-closed change exists to prevent.
/// What: another process completes a review for a SHA; this review aborts on a
/// failed claim for the same SHA; the completed record must survive.
/// Test: this test. Fails when `abort_dry` releases unconditionally.
#[tokio::test]
async fn failed_claim_abort_does_not_delete_another_processes_record() {
    use crate::store::DedupStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");

    // A different process owns and completes the review for this head SHA.
    let owner_store = DedupStore::open(&path).expect("open");
    owner_store
        .claim_blocking("acme", "backend", 42, "sha-owned")
        .expect("claim");
    owner_store
        .complete_blocking("acme", "backend", 42, "sha-owned")
        .expect("complete");
    drop(owner_store);

    // This review holds no claim — its own `claim()` failed.
    let mut deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    deps.dedup = Some(Arc::new(DedupStore::open(&path).expect("open")));

    let mut result = ReviewResult::new("acme", "backend", 42, "t", "u");
    result.head_sha = "sha-owned".to_string();
    let input = ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: String::new(),
        },
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Serve,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };

    let out = abort_dry(
        result,
        &default_config(),
        &input,
        &deps,
        DedupClaim::NotHeld,
    )
    .await;
    assert!(out.dry_run, "an abort is always dry-run");

    // The other process's completed claim must still suppress a re-review.
    let checker = DedupStore::open(&path).expect("open");
    assert_eq!(
        checker
            .claim_blocking("acme", "backend", 42, "sha-owned")
            .unwrap(),
        ClaimOutcome::Skipped,
        "aborting on a FAILED claim deleted another process's completed record — \
         the next trigger for this SHA would post a duplicate comment (#5064)"
    );
}

/// Why: the owning abort must still release, or a review that dies mid-flight
/// leaves an `InProgress` record that suppresses its own retry.
/// What: a review that acquired the claim and then aborts leaves the SHA
/// re-claimable.
/// Test: this test.
#[tokio::test]
async fn held_claim_abort_still_releases() {
    use crate::store::DedupStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dedup.redb");
    let store = Arc::new(DedupStore::open(&path).expect("open"));
    store
        .claim_blocking("acme", "backend", 42, "sha-held")
        .expect("claim");

    let mut deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    deps.dedup = Some(Arc::clone(&store));

    let mut result = ReviewResult::new("acme", "backend", 42, "t", "u");
    result.head_sha = "sha-held".to_string();
    let input = ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: String::new(),
        },
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Serve,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };

    abort_dry(result, &default_config(), &input, &deps, DedupClaim::Held).await;

    assert_eq!(
        store
            .claim_blocking("acme", "backend", 42, "sha-held")
            .unwrap(),
        ClaimOutcome::Claimed,
        "an owning abort must release so the retry can re-run"
    );
}

// ── Verdict-calibration regressions (#4044, #5309) ────────────────────
//
// Both defects are fail-open: the pipeline reports a wrong verdict/grade/summary
// as authoritative, so a human or agent gating merges on it is blocked on a
// non-defect. Both fixtures are the reported evidence, near-verbatim.

/// A reviewer response whose prose summary names finding #1 as a merge blocker
/// and whose JSON self-reports BLOCK / F on that one finding — the PR #5308
/// shape (#4044).
fn blocks_citing_finding_one() -> FakeLlm {
    FakeLlm {
        response: r#"Finding #1 is a high-effort defect and must be resolved before merge.

```json
{"verdict":"BLOCK","grade":"F","summary":"blocking defect","findings":[{"title":"missing await","body":"the future is dropped","severity":"high","confidence":0.9,"file":"src/a.rs","line":1,"code_provable":true}]}
```"#
            .to_string(),
        error: None,
        output_tokens: None,
    }
}

/// #4044: a finding the verifier REFUTED must not drive the verdict, must not
/// leave the model's own F standing, and must not still be cited as a merge
/// blocker by the prose summary.
///
/// Pre-fix this failed twice: `clamp_grade_to_verdict` left `grade: "F"` beside
/// the relaxed APPROVE, and `review_body` still read "must be resolved before
/// merge" about the refuted finding with nothing qualifying it.
#[tokio::test]
async fn run_review_refuted_finding_does_not_drive_grade_or_summary() {
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(blocks_citing_finding_one()),
        Some(Arc::new(FakeVerifier {
            judgment: "REFUTED",
        })),
    );

    let result = run_review(&default_config(), input, deps).await;

    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "refuting the sole blocking finding must relax BLOCK"
    );
    assert_eq!(
        result.grade.as_deref(),
        Some("B-"),
        "the model's own F rested on the refuted finding — it must not survive \
         the relaxed verdict (#4044), got {:?}",
        result.grade
    );
    assert!(
        result.review_body.contains("Verification notice"),
        "the summary predates verification and must be qualified (#4044):\n{}",
        result.review_body
    );
    assert!(
        result
            .review_body
            .contains("finding #1 — `src/a.rs`: missing await"),
        "the qualifier must name the refuted finding by the index the prose \
         uses (#4044):\n{}",
        result.review_body
    );
    assert_eq!(
        result.findings_count,
        result.findings.len(),
        "the refuted finding stays in the array for transparency (REV-606)"
    );
}

/// A reviewer response carrying #5309's finding near-verbatim: `code_provable`,
/// High effort, and a description that admits the diff cannot settle it.
fn blocks_on_self_admitted_unverifiable() -> FakeLlm {
    FakeLlm {
        response: r#"One blocking issue.

```json
{"verdict":"BLOCK","grade":"F","summary":"un-awaited futures","findings":[{"title":"async functions called without .await","body":"incidents::run, dora::run, pr_metrics::run and report::run are invoked without .await, so half the sweep pipeline would silently no-op. The diff does not show their signatures, so this cannot be confirmed from the diff alone.","severity":"high","confidence":0.72,"file":"src/a.rs","line":1,"code_provable":true}]}
```"#
            .to_string(),
        error: None,
        output_tokens: None,
    }
}

/// #5309: a `code_provable` finding whose own text says it cannot be confirmed
/// from the diff must never come back `verified: "confirmed"`, and must not
/// drive BLOCK.
///
/// The verifier here answers CONFIRMED — which is exactly what the live verifier
/// did on PR #5303. Pre-fix that stamp landed on the finding and BLOCK/F stood;
/// the four functions were in fact synchronous.
#[tokio::test]
async fn run_review_self_admitted_unverifiable_claim_is_not_confirmed() {
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(blocks_on_self_admitted_unverifiable()),
        Some(Arc::new(FakeVerifier {
            judgment: "CONFIRMED",
        })),
    );

    let result = run_review(&default_config(), input, deps).await;

    assert_eq!(
        result.findings.len(),
        1,
        "the finding must still be reported"
    );
    assert!(
        !matches!(
            result.findings[0].verified,
            Some(crate::models::VerifyOutcome::Confirmed)
        ),
        "nothing read the signatures — the claim must not wear a confirmation \
         (#5309), got {:?}",
        result.findings[0].verified
    );
    assert!(
        !result.findings[0].code_provable,
        "a claim the finding says the diff cannot settle is not diff-provable"
    );
    assert_ne!(
        result.verdict,
        Verdict::Block,
        "an unchecked claim must not drive BLOCK (#5309)"
    );
    assert!(
        result.findings[0].description.contains("incidents::run"),
        "the original claim must still reach the author verbatim"
    );
}

// ─── #5113: posting requires the claim gate ─────────────────────────────────

/// REGRESSION (#5113): a run that can post but carries no dedup store must
/// abort before doing anything, not review and post unguarded.
///
/// Why: the CLI `run` path set `allow_posting: true` with `dedup: None`, so a
/// re-run against an already-reviewed `(owner, repo, pr, head_sha)` posted a
/// second comment with no error and no warning. Wiring a store into that one
/// call site fixes the instance; failing closed here makes the combination
/// unusable wherever it is expressed.
/// What: a GitHub diff source, `allow_posting: true`, `dry_run: false` (so
/// `decide_action` selects `Post`), and no dedup store. The run must return
/// UNKNOWN with an error naming the missing claim store, and must not post.
/// Test: this test. Fails pre-fix: the run proceeded past this point, so the
/// error named GitHub token resolution instead of the missing claim store.
#[tokio::test]
async fn run_review_posting_without_dedup_store_fails_closed() {
    let mut config = default_config();
    config.dry_run = false; // service-live: the post path is reachable

    let input = ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: "tok".to_string(),
        },
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // `dedup: None` — the exact `build_deps_async` shape #5113 reported.
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    assert!(deps.dedup.is_none(), "the fixture must carry no claim gate");

    let result = run_review(&config, input, deps).await;

    assert!(!result.posted, "an unguarded run must never post");
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "a run that never happened must not report a verdict"
    );
    let error = result.error.expect("the abort must explain itself");
    assert!(
        error.contains("dedup claim store"),
        "the error must name the missing claim gate so an operator can wire it: {error}"
    );
}

// ─── #6062: an empty head SHA is fail-closed on a posting run ───────────────

/// A config with no GitHub App credentials, so serve-mode token resolution —
/// and therefore `fetch_github_pr_meta` — fails without touching the network.
fn config_without_app_creds() -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.github_app_id = None;
    config.github_app_private_key = None;
    config.github_installations = Vec::new();
    config
}

/// REGRESSION (#6062): a posting run whose PR-metadata fetch produced no head
/// SHA aborts before the review runs, rather than proceeding unkeyed.
///
/// Why: `fetch_github_pr_meta` failing falls back to `head_sha = String::new()`
/// and continues. Both the claim in `run_review` and the `complete()` in
/// `finalize_review` gate on `!head_sha.is_empty()`, and `decide_action` never
/// reads the SHA at all — so the run reached `FinalizeAction::Post` and posted
/// live with the dedup claim never taken and never completed. A retry after the
/// same fetch failure posts a duplicate.
/// What: a GitHub source in serve mode with no App credentials (so the metadata
/// fetch fails offline), `allow_posting: true`, `dry_run: false`, and a real
/// dedup store present so the #5113 guard is not the one firing. The run must
/// return UNKNOWN with an error naming the missing head SHA, and must not post.
/// Test: this test. Fails pre-fix: the run continued past the fetch and the
/// error named GitHub token resolution instead of the missing head SHA.
#[tokio::test]
async fn run_review_empty_head_sha_fails_closed_before_posting() {
    use crate::store::DedupStore;

    let dir = tempfile::tempdir().unwrap();
    let mut config = config_without_app_creds();
    config.dry_run = false; // the post path is reachable
    config.log_dir = dir.path().to_path_buf();

    let input = ReviewInput {
        diff_source: DiffSource::Github {
            owner: "acme".to_string(),
            repo: "backend".to_string(),
            pr: 42,
            token: String::new(),
        },
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Serve,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let mut deps = ready_deps(Arc::new(FakeLlm::approves()), None);
    deps.dedup = Some(Arc::new(
        DedupStore::open(&dir.path().join("dedup.redb")).expect("open"),
    ));

    let result = run_review(&config, input, deps).await;

    assert!(
        !result.posted,
        "a run with no dedup key must never post — a retry would duplicate it"
    );
    assert_eq!(
        result.verdict,
        Verdict::Unknown,
        "a run that never happened must not report a verdict"
    );
    let error = result.error.expect("the abort must explain itself");
    assert!(
        error.contains("head SHA"),
        "the error must name the missing dedup key, not a downstream symptom: {error}"
    );
}

/// Why: the #6062 guard must not fire where nothing can be posted — a local
/// diff has no head SHA by construction and reviews normally.
/// What: a local diff source with `allow_posting: true` and `dry_run: false`
/// must review rather than abort on the empty SHA.
/// Test: this test.
#[tokio::test]
async fn run_review_local_source_with_empty_head_sha_is_not_blocked() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.dry_run = false;

    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;

    assert!(
        !result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("head SHA")),
        "a local diff has no head SHA and nowhere to post: {:?}",
        result.error
    );
}

/// Why: the #5113 guard must not fire where nothing can be posted. The CLI runs
/// `--local-diff` and `--base` with the same `allow_posting: true` it uses for a
/// PR, so a guard keyed on that flag alone would break both — and `compare` and
/// `calibrate` build their deps through the same helper.
/// What: a local diff source with `allow_posting: true` and `dry_run: false`,
/// carrying no dedup store, must review normally rather than abort.
/// Test: this test.
#[tokio::test]
async fn run_review_local_source_without_dedup_store_is_not_blocked() {
    let (source, _tmp) = local_diff_source("+fn x() {}\n");
    let mut config = default_config();
    config.dry_run = false; // would select Post if the source were a GitHub PR

    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-nano-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: true,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::approves()), None);

    let result = run_review(&config, input, deps).await;

    assert!(
        !result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("dedup claim store")),
        "a local diff has nowhere to post, so the claim gate must not block it: {:?}",
        result.error
    );
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "the review itself must still run to completion"
    );
}

// ── #4045: the citation check is wired into the UNIFIED emit path ──────

/// Why (#4045, ask A): #2882 fixed the fabricated-citation defect and #4042
/// reported it back on 0.10.1. Both times the invariant — every emitted
/// finding cites a path that exists in the diff — was asserted over the
/// checking HELPER, so `citation_check_tests.rs` stayed green while production
/// emitted fabrications. This asserts it over the review the runner actually
/// RETURNS: dropping the `enforce_citation_integrity` call from `runner.rs`
/// then fails a test rather than a user's PR.
///
/// What: a one-file diff reviewed by a model that reports BLOCK on a finding
/// citing `src/hotelPage.ts:207` — #4042's own nonexistent-path shape, a file
/// absent from the diff entirely. The emitted review must carry no such
/// finding, and the fabricated BLOCK must not survive as the verdict.
/// Test: this test.
#[tokio::test]
async fn unified_path_emits_no_finding_citing_a_path_outside_the_diff() {
    let (source, _tmp) = local_diff_source_for_file("src/real.rs", "+fn real() -> u32 { 7 }");

    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(Arc::new(FakeLlm::fabricates_citation_outside_diff()), None);

    let result = run_review(&default_config(), input, deps).await;

    let leaked: Vec<_> = result
        .findings
        .iter()
        .filter(|f| f.file.contains("hotelPage.ts"))
        .map(|f| (f.file.clone(), f.line))
        .collect();
    assert!(
        leaked.is_empty(),
        "a finding citing a file absent from the diff reached the emitted review — the \
         citation check is not wired into the unified path (#2881 / #4042 / #4045): {leaked:?}"
    );
    assert!(
        result.findings.is_empty(),
        "the fabricated finding was the only one, so nothing may be emitted: {:?}",
        result.findings
    );
}
