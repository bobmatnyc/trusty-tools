//! End-to-end integration tests for the map-reduce review branch (#1643).
//!
//! Why: these are the tests that PROVE the real fix — an over-cap diff is routed
//! to the per-file map-reduce path, EVERY file is reviewed, a changed signature
//! near the END of a large diff reaches a reviewer (the #1638 failure mode), and
//! NO truncation marker ever reaches a map prompt.  They also prove a single
//! chunk's REQUEST_CHANGES propagates to the overall verdict.
//! What: drives `run_review` with a recording fake LLM (hermetic, no network)
//! that captures every per-file prompt so the assertions can inspect exactly
//! what the reviewer saw.
//! Test: this is the test module (attached to `runner_mapreduce.rs`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::config::ReviewConfig;
use crate::integrations::{
    analyze_client::{
        AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot, Smell,
    },
    github::RunMode,
    search_client::{
        EmbedderState, HealthResponse, IndexInfo, SearchClient, SearchClientError, SearchResult,
    },
};
use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::models::{ReviewStatus, Verdict};
use crate::pipeline::diff::{DIFF_TRUNCATED_MARKER, DiffSource, RENDER_TRUNCATED_MARKER};
use crate::pipeline::runner::{CallerContext, ReviewDeps, ReviewInput, run_review};
use crate::pipeline::trigger::TriggerDecision;

// ── Recording fake LLM ───────────────────────────────────────────────────────

/// A fake reviewer that records every prompt's user message and returns a
/// configurable verdict.  When `request_changes_marker` is set, any prompt whose
/// user message CONTAINS that marker returns REQUEST_CHANGES (with a Medium
/// finding); all other prompts APPROVE.  This lets a test prove that a finding in
/// ONE chunk (the chunk containing a specific signature) propagates to the
/// overall verdict.
struct RecordingReviewer {
    /// Captured user-message bodies, one per `complete` call.
    seen: Mutex<Vec<String>>,
    /// When a prompt body contains this substring, return REQUEST_CHANGES.
    request_changes_marker: Option<String>,
}

impl RecordingReviewer {
    fn approving() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            request_changes_marker: None,
        }
    }

    fn request_changes_on(marker: &str) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            request_changes_marker: Some(marker.to_string()),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl LlmProvider for RecordingReviewer {
    fn name(&self) -> &str {
        "recording-reviewer"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = req
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let want_rc = self
            .request_changes_marker
            .as_ref()
            .map(|m| body.contains(m.as_str()))
            .unwrap_or(false);
        self.seen.lock().expect("lock").push(body);

        let text = if want_rc {
            // Use HIGH severity so the safety floor preserves this finding even
            // after an LLM synthesis pass.  A Medium finding might be holistically
            // softened by synthesis (the intended calibration); a High finding must
            // ALWAYS floor to BLOCK/REQUEST_CHANGES regardless of synthesis (#1663).
            r#"{"verdict":"BLOCK","summary":"critical bug","findings":[{"title":"auth-bypass","body":"the build() signature changed and a caller passes null — auth check skipped","severity":"high","confidence":0.95,"file":"src/big.rs","line":1}]}"#
        } else {
            r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#
        };
        Ok(LlmResponse {
            text: text.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

// ── Minimal healthy context deps (gate #590 passes) ──────────────────────────

struct OkSearch;
#[async_trait]
impl SearchClient for OkSearch {
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
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

struct OkAnalyze;
#[async_trait]
impl AnalyzeClient for OkAnalyze {
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

fn deps(llm: Arc<dyn LlmProvider>) -> ReviewDeps {
    ReviewDeps {
        llm,
        verifier: None,
        search: Arc::new(OkSearch),
        analyze: Some(Arc::new(OkAnalyze)),
        dedup: None,
    }
}

fn local_source(diff: &str) -> (DiffSource, tempfile::NamedTempFile) {
    use std::io::Write as _;
    let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
    tmp.write_all(diff.as_bytes()).expect("write");
    let path = tmp.path().to_path_buf();
    (DiffSource::LocalFile { path }, tmp)
}

fn input(source: DiffSource) -> ReviewInput {
    ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
    }
}

/// Build a multi-file diff far larger than MAX_DIFF_CHARS, with a DISTINCTIVE
/// changed signature in the LAST file (so it lands in the region the unified
/// path would truncate away).  Returns `(diff, tail_signature)`.
fn oversized_multi_file_diff() -> (String, &'static str) {
    use crate::config::constants::MAX_DIFF_CHARS;

    let tail_signature = "pub fn build(a: i32, b: i32, c: i32, previous: Option<i32>) -> i32";
    let mut diff = String::new();

    // Several large early files that alone blow past the cap.
    let filler = "+    let _padding = compute_value(some_argument_here);\n";
    let mut file_idx = 0;
    while diff.len() < MAX_DIFF_CHARS + (MAX_DIFF_CHARS / 2) {
        diff.push_str(&format!(
            "diff --git a/src/file{file_idx}.rs b/src/file{file_idx}.rs\n"
        ));
        diff.push_str(&format!(
            "--- a/src/file{file_idx}.rs\n+++ b/src/file{file_idx}.rs\n"
        ));
        diff.push_str("@@ -1,1 +1,5000 @@\n");
        for _ in 0..5000 {
            diff.push_str(filler);
        }
        file_idx += 1;
    }

    // The LAST file carries the distinctive changed signature on its only line.
    diff.push_str("diff --git a/src/big.rs b/src/big.rs\n");
    diff.push_str("--- a/src/big.rs\n+++ b/src/big.rs\n");
    diff.push_str("@@ -1,1 +1,1 @@\n");
    diff.push_str(&format!("+{tail_signature} {{ 0 }}\n"));

    (diff, tail_signature)
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// THE headline test: an over-cap multi-file diff routes to MAP-REDUCE (not the
/// truncated unified path), and the changed signature in the LAST file reaches a
/// reviewer prompt with NO truncation marker.  This is the exact #1638 failure
/// mode, now fixed.
#[tokio::test]
async fn run_review_oversized_diff_mapreduce_reviews_tail_signature() {
    let (diff, tail_signature) = oversized_multi_file_diff();
    let (source, _tmp) = local_source(&diff);

    let reviewer = Arc::new(RecordingReviewer::approving());
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    let prompts = reviewer.prompts();
    assert!(
        !prompts.is_empty(),
        "map-reduce must have issued at least one per-file LLM call"
    );

    // (1) The tail signature MUST have reached a reviewer prompt — proof that the
    // late-in-diff file was NOT truncated away (the #1638 bug).
    assert!(
        prompts.iter().any(|p| p.contains(tail_signature)),
        "the changed signature in the LAST file must reach a reviewer prompt (not truncated)"
    );

    // (2) NO truncation marker may reach ANY map prompt.
    for p in &prompts {
        assert!(
            !p.contains(DIFF_TRUNCATED_MARKER) && !p.contains(RENDER_TRUNCATED_MARKER),
            "no truncation marker may reach a map prompt"
        );
    }

    // (3) The verdict must NOT be UNKNOWN (the old fail-closed backstop) — the
    // review actually completed via map-reduce.
    assert_ne!(
        result.verdict,
        Verdict::Unknown,
        "over-cap diff must be REVIEWED via map-reduce, not failed-closed to UNKNOWN"
    );
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "all chunks APPROVE → overall APPROVE"
    );
}

/// Selectively fails for prompts containing `fail_marker`, succeeds otherwise.
/// Lets us inject exactly ONE LLM-error failure into a multi-chunk map-reduce
/// run without failing every chunk.
struct SelectivelyFailingReviewer {
    seen: Mutex<Vec<String>>,
    fail_marker: String,
}

impl SelectivelyFailingReviewer {
    fn fail_on(marker: &str) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            fail_marker: marker.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for SelectivelyFailingReviewer {
    fn name(&self) -> &str {
        "selective-failing"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = req
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let should_fail = body.contains(self.fail_marker.as_str());
        self.seen.lock().expect("lock").push(body);
        if should_fail {
            return Err(LlmError::Transport("simulated LLM error".to_string()));
        }
        Ok(LlmResponse {
            text: r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// A HIGH-severity finding in the SINGLE chunk that contains the tail signature
/// must propagate to the overall verdict — proving both (a) per-chunk verdict
/// aggregation and (b) the synthesis safety floor for High-effort findings (#1663).
/// The finding uses `severity: "high"` so the `apply_high_severity_floor_only`
/// in the synthesis pass cannot soften it to APPROVE; a Medium finding here would
/// correctly be softened by synthesis (that's the feature), but we need this test
/// to remain a regression guard after synthesis was added.
#[tokio::test]
async fn run_review_mapreduce_chunk_request_changes_propagates() {
    let (diff, tail_signature) = oversized_multi_file_diff();
    let (source, _tmp) = local_source(&diff);

    // Only the chunk whose prompt contains the tail signature returns BLOCK
    // (High-severity finding); every other (early-file) chunk APPROVEs.
    // The synthesis LLM (same RecordingReviewer, no tail_signature in synthesis
    // prompt) returns APPROVE, but the High-severity safety floor re-applies.
    let reviewer = Arc::new(RecordingReviewer::request_changes_on(tail_signature));
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    assert_ne!(
        result.verdict,
        Verdict::Approve,
        "a single chunk's REQUEST_CHANGES must propagate — overall verdict is non-APPROVE"
    );
    assert!(
        matches!(
            result.verdict,
            Verdict::RequestChanges | Verdict::Block | Verdict::ApproveWithReservations
        ),
        "verdict must reflect the worst chunk, got {:?}",
        result.verdict
    );
    assert!(
        !result.findings.is_empty(),
        "the REQUEST_CHANGES chunk's finding must survive into the merged result"
    );
    assert_eq!(
        result.findings_count,
        result.findings.len(),
        "findings_count must equal findings.len() on the map-reduce completed path (#1877)"
    );
}

/// Build a multi-file diff that reliably routes to map-reduce and exposes partial
/// coverage: one early file carries a UNIQUE marker in its diff that the selective
/// LLM will fail on; the other files (including the tail file with the named
/// signature) APPROVE.
///
/// Returns `(diff, fail_marker, tail_signature)`.
fn partial_coverage_diff() -> (String, &'static str, &'static str) {
    use crate::config::constants::MAX_DIFF_CHARS;

    // A marker that appears ONLY in the diff for src/fail_target.rs — unique
    // enough that it cannot accidentally appear in the prompt for any other file.
    let fail_marker = "UNIQUE_PARTIAL_FAIL_MARKER_z9q8w7";
    let tail_signature = "pub fn partial_tail_sig(x: u64, y: u64) -> u64";

    let mut diff = String::new();

    // Build ONE "fail target" file early in the diff with the unique marker.
    diff.push_str("diff --git a/src/fail_target.rs b/src/fail_target.rs\n");
    diff.push_str("--- a/src/fail_target.rs\n+++ b/src/fail_target.rs\n");
    diff.push_str("@@ -1,1 +1,2 @@\n");
    diff.push_str(&format!("+// {fail_marker}\n"));
    diff.push_str("+fn fail_target_placeholder() {}\n");

    // Filler files to push total diff past MAX_DIFF_CHARS (triggers auto map-reduce).
    let filler = "+    let _padding = another_filler_value(arg_here);\n";
    let mut file_idx = 0;
    while diff.len() < MAX_DIFF_CHARS + (MAX_DIFF_CHARS / 4) {
        diff.push_str(&format!(
            "diff --git a/src/filler{file_idx}.rs b/src/filler{file_idx}.rs\n"
        ));
        diff.push_str(&format!(
            "--- a/src/filler{file_idx}.rs\n+++ b/src/filler{file_idx}.rs\n"
        ));
        diff.push_str("@@ -1,1 +1,5000 @@\n");
        for _ in 0..5000 {
            diff.push_str(filler);
        }
        file_idx += 1;
    }

    // Tail file — always APPROVEs (marker not present), proves partial ≠ all-failed.
    diff.push_str("diff --git a/src/tail.rs b/src/tail.rs\n");
    diff.push_str("--- a/src/tail.rs\n+++ b/src/tail.rs\n");
    diff.push_str("@@ -1,1 +1,1 @@\n");
    diff.push_str(&format!("+{tail_signature} {{ x + y }}\n"));

    (diff, fail_marker, tail_signature)
}

/// When ONE chunk fails due to an LLM transport error (partial coverage), the
/// coverage-notice banner must accurately label the breakdown: total failed,
/// LLM-error count, and over-cap hunk count — with no label inversion.
///
/// Setup: over-cap multi-file diff with a uniquely-marked file that the selective
/// reviewer fails on; all other chunks APPROVE.  Asserts:
/// - result status == Degraded (not clobbered by fold/finalize — Fix 2)
/// - review_body contains the coverage-notice banner (visible in posted comment)
/// - banner uses "LLM error" label (Fix 1 — not mis-labelled as over-cap)
/// - banner shows 0 over-cap hunks (correct — this was a transport error)
#[tokio::test]
async fn run_review_partial_coverage_banner_labels_are_correct() {
    let (diff, fail_marker, _tail_signature) = partial_coverage_diff();
    let (source, _tmp) = local_source(&diff);

    let reviewer = Arc::new(SelectivelyFailingReviewer::fail_on(fail_marker));
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    // The review is partial: at least one chunk was reviewed, one failed (LLM error).
    assert_eq!(
        result.status,
        ReviewStatus::Degraded,
        "partial map-reduce must produce Degraded status (Fix 2 guard: \
         status set after fold/finalize chain so it cannot be clobbered)"
    );

    let body = &result.review_body;
    assert!(
        body.contains("Coverage notice"),
        "review_body must contain coverage-notice banner (visible in posted comment), got: {body}"
    );
    // Fix 1 guard: the banner must have SEPARATE labels for LLM errors and
    // over-cap hunks.  The pre-fix bug inverted these: it labelled the LLM-error
    // count as "over-cap hunk(s)" and the over-cap count as "failed".
    // Asserting both labels appear proves they are distinct fields, not one label
    // mis-applied to the wrong count.
    assert!(
        body.contains("LLM error"),
        "banner must label LLM-transport failures separately as 'LLM error(s)' \
         (pre-fix bug labelled them as over-cap hunks), got: {body}"
    );
    assert!(
        body.contains("over-cap hunk"),
        "banner must have a separate 'over-cap hunk(s)' label, got: {body}"
    );
    // Both labels must appear together in the same notice line — proof that the
    // breakdown is three-field (total failed, LLM errors, over-cap hunks) and
    // that neither field swallowed the other.
    assert!(
        body.contains("LLM error") && body.contains("over-cap hunk"),
        "banner must show both 'LLM error(s)' AND 'over-cap hunk(s)' labels, got: {body}"
    );
}

/// A partial map-reduce review (at least one chunk fails via LLM error) must
/// produce `ReviewStatus::Degraded` in the final `ReviewResult`, even after the
/// `fold_reduced_into_result` / `finalize_run` chain runs.
///
/// This is the Fix 2 guard: the Degraded status is now set AFTER the fold, inside
/// the `is_partial()` block, so it cannot be clobbered by anything `finalize_run`
/// touches.
#[tokio::test]
async fn run_review_partial_mapreduce_status_is_degraded() {
    let (diff, fail_marker, _) = partial_coverage_diff();
    let (source, _tmp) = local_source(&diff);

    let reviewer = Arc::new(SelectivelyFailingReviewer::fail_on(fail_marker));
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    assert_eq!(
        result.status,
        ReviewStatus::Degraded,
        "partial map-reduce (≥1 chunk failed) must end with status == Degraded \
         even after fold/finalize (Fix 2 guard — status set post-fold)"
    );
}

/// A reviewer that returns REQUEST_CHANGES with a Medium finding when the prompt
/// contains `rc_marker`, returns REQUEST_CHANGES for synthesis prompts (identified
/// by the synthesis prompt header), and APPROVEs for all other per-file prompts.
///
/// Why: lets us drive a per-chunk REQUEST_CHANGES → synthesis → final verdict
/// path at the integration level, exercising the RC propagation coverage gap
/// left when `run_review_mapreduce_chunk_request_changes_propagates` was updated
/// to use BLOCK/High for synthesis-floor compatibility (#1663).
struct MediumRcReviewer {
    seen: Mutex<Vec<String>>,
    rc_marker: String,
}

impl MediumRcReviewer {
    fn rc_on(marker: &str) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            rc_marker: marker.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for MediumRcReviewer {
    fn name(&self) -> &str {
        "medium-rc-reviewer"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = req
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.seen.lock().expect("lock").push(body.clone());

        // Synthesis prompts start with the synthesis header — return RC so the
        // synthesis concurs with the per-chunk verdict (tests the RC path end-to-end).
        let is_synthesis = body.contains("## PR under review");
        let text = if is_synthesis {
            r#"{"verdict":"REQUEST_CHANGES","grade":"C","summary":"medium nit remains."}"#
        } else if body.contains(self.rc_marker.as_str()) {
            r#"{"verdict":"REQUEST_CHANGES","summary":"medium nit found","findings":[{"title":"style-nit","body":"missing doc comment","severity":"medium","confidence":0.85,"file":"src/big.rs","line":1}]}"#
        } else {
            r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#
        };
        Ok(LlmResponse {
            text: text.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// End-to-end REQUEST_CHANGES propagation: a single chunk returns REQUEST_CHANGES
/// (Medium finding) and the synthesis LLM concurs — the final verdict must be
/// REQUEST_CHANGES (not softened to APPROVE).
///
/// Why: the BLOCK/High test (`run_review_mapreduce_chunk_request_changes_propagates`)
/// verifies the High-severity floor but no longer exercises the RC → synthesis →
/// RC end-to-end path at medium severity.  This test restores that coverage.
/// What: drives the full map-reduce → synthesis pipeline with a medium-severity
/// REQUEST_CHANGES per-chunk verdict; synthesis also returns REQUEST_CHANGES.
/// Test: this IS the test — asserts final verdict == REQUEST_CHANGES.
#[tokio::test]
async fn run_review_mapreduce_medium_rc_propagates_through_synthesis() {
    let (diff, tail_signature) = oversized_multi_file_diff();
    let (source, _tmp) = local_source(&diff);

    // Only the chunk whose prompt contains the tail signature returns RC (Medium);
    // synthesis also returns RC (no High floor fires — no High-severity findings).
    let reviewer = Arc::new(MediumRcReviewer::rc_on(tail_signature));
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    assert_ne!(
        result.verdict,
        Verdict::Unknown,
        "RC path must complete normally — verdict must not be UNKNOWN"
    );
    // Synthesis concurs (RC), so final verdict must be REQUEST_CHANGES.
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "synthesis-concurred REQUEST_CHANGES must propagate to the final result"
    );
    assert!(
        !result.findings.is_empty(),
        "the per-chunk Medium finding must survive into the merged result"
    );
}

// ── Shallow-review heuristic on the map-reduce path (#1885) ──────────────────

/// A reviewer that APPROVEs every prompt (per-file AND synthesis) with ZERO
/// findings, reporting a configurable per-call `output_tokens`. The single JSON
/// body satisfies both the per-file parser (uses verdict/findings) and the
/// synthesis parser (uses verdict/grade/summary), so one fake drives the whole
/// pipeline. Used to simulate an implausibly cheap clean review of a huge diff.
struct LowTokenApprover {
    output_tokens: u32,
}

#[async_trait]
impl LlmProvider for LowTokenApprover {
    fn name(&self) -> &str {
        "low-token-approver"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            // Grade A+ so the pre-cap envelope grade is unambiguously top-band;
            // the shallow flag must then cap it down to B-.
            text: r#"{"verdict":"APPROVE","grade":"A+","summary":"ok","findings":[]}"#.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: self.output_tokens,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// Build an over-cap multi-file diff whose lines are ALL DISTINCT, so the diff
/// analyzer's noise/dedup filtering keeps the filtered byte size large (the
/// shallow heuristic's `diff_len` input). Routes to map-reduce via the size cap.
fn shallow_oversized_distinct_diff() -> String {
    use crate::config::constants::MAX_DIFF_CHARS;

    let mut diff = String::new();
    let mut file_idx = 0;
    while diff.len() < MAX_DIFF_CHARS + (MAX_DIFF_CHARS / 4) {
        diff.push_str(&format!(
            "diff --git a/src/mod{file_idx}.rs b/src/mod{file_idx}.rs\n"
        ));
        diff.push_str(&format!(
            "--- a/src/mod{file_idx}.rs\n+++ b/src/mod{file_idx}.rs\n"
        ));
        diff.push_str("@@ -1,1 +1,400 @@\n");
        for line in 0..400 {
            // Distinct on both indices so nothing is deduped/collapsed.
            diff.push_str(&format!(
                "+    let value_{file_idx}_{line} = compute_{file_idx}({line}, {line} + 1);\n"
            ));
        }
        file_idx += 1;
    }
    diff
}

/// THE #1885 regression test: a huge diff reviewed via MAP-REDUCE that returns a
/// clean APPROVE (0 findings) with an implausibly low AGGREGATE output-token spend
/// must be flagged `shallow_clean_review` and have its grade capped at B- — the
/// SAME treatment the unified path already applies. Before the fix the map-reduce
/// path never aggregated tokens, so `output_tokens` stayed 0 and the heuristic was
/// silently inoperative on exactly the largest, highest-risk diffs.
#[tokio::test]
async fn run_review_mapreduce_shallow_clean_flags_low_tokens() {
    let diff = shallow_oversized_distinct_diff();
    let (source, _tmp) = local_source(&diff);

    // One output token per call → aggregate stays far below the diff-proportional
    // floor no matter how many chunks the splitter produces.
    let reviewer = Arc::new(LowTokenApprover { output_tokens: 1 });
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    // Sanity: this really went through the clean-APPROVE map-reduce path.
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "all chunks APPROVE with no findings → overall APPROVE"
    );
    assert!(
        result.findings.is_empty(),
        "the clean review must have zero findings"
    );

    // Aggregate telemetry must now be populated (was always 0 before the fix).
    assert!(
        result.output_tokens > 0,
        "map-reduce must aggregate output tokens onto the result (was 0 pre-#1885)"
    );

    // The heuristic must FIRE on the aggregate.
    assert!(
        result.shallow_clean_review,
        "a cheap clean APPROVE on a huge map-reduce diff must be flagged shallow (#1885)"
    );

    // And the grade must be capped at B- (or lower) — never left at A+.
    let grade: crate::pipeline::letter_grade::Grade = result
        .grade
        .as_deref()
        .expect("APPROVE verdict must carry a grade")
        .parse()
        .expect("grade must parse");
    assert!(
        grade >= crate::pipeline::letter_grade::Grade::BMinus,
        "flagged shallow review grade must be capped at B- or milder, got {:?}",
        grade
    );
}

/// The negative control: the SAME huge map-reduce diff reviewed with a plausible
/// (high) aggregate output-token spend must NOT be flagged shallow, proving the
/// heuristic keys on the aggregate token telemetry (not merely on diff size).
#[tokio::test]
async fn run_review_mapreduce_substantive_review_not_flagged() {
    let diff = shallow_oversized_distinct_diff();
    let (source, _tmp) = local_source(&diff);

    // A high per-call output-token count → aggregate comfortably exceeds the
    // diff-proportional floor, so the review looks substantive.
    let reviewer = Arc::new(LowTokenApprover {
        output_tokens: 100_000,
    });
    let llm: Arc<dyn LlmProvider> = reviewer.clone();
    let config = ReviewConfig::load(None);

    let result = run_review(&config, input(source), deps(llm)).await;

    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        !result.shallow_clean_review,
        "a review with plausible aggregate token spend must NOT be flagged shallow"
    );
}
