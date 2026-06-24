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
use crate::models::Verdict;
use crate::pipeline::diff::{DIFF_TRUNCATED_MARKER, DiffSource, RENDER_TRUNCATED_MARKER};
use crate::pipeline::runner::{ReviewDeps, ReviewInput, run_review};
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
            r#"{"verdict":"REQUEST_CHANGES","summary":"bug","findings":[{"title":"bug","body":"the build() signature changed and a caller passes null","severity":"medium","confidence":0.95,"file":"src/big.rs","line":1}]}"#
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

/// A REQUEST_CHANGES in the SINGLE chunk that contains the tail signature must
/// propagate to the overall verdict — proving the reduce stage aggregates
/// per-chunk verdicts (no chunk verdict is lost).
#[tokio::test]
async fn run_review_mapreduce_chunk_request_changes_propagates() {
    let (diff, tail_signature) = oversized_multi_file_diff();
    let (source, _tmp) = local_source(&diff);

    // Only the chunk whose prompt contains the tail signature returns
    // REQUEST_CHANGES; every other (early-file) chunk APPROVEs.
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
}
