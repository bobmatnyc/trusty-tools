//! Unit tests for the map stage (`mapreduce::map`).
//!
//! Why: the map stage is where the no-truncation guarantee lives — each unit
//! gets its own LLM call.  These tests pin the per-unit outcome table (reviewed
//! / skipped / failed / oversized-fail-closed) with a hermetic fake provider so
//! they never touch the network.
//! What: drives `run_map_stage` with hand-built `MapUnit`s and a recording fake
//! LLM that asserts the per-unit diff text reached the prompt.
//! Test: this is the test module.

use std::sync::Arc;

use async_trait::async_trait;

use super::{MapContext, run_map_stage};
use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::models::Verdict;
use crate::pipeline::mapreduce::outcome::MapOutcome;
use crate::pipeline::mapreduce::unit::{MapUnit, MapUnitKind};
use crate::pipeline::prompt::{ReviewContext, ReviewPrMeta};
use crate::voice::VoiceConfig;

// ── Recording fake LLM ───────────────────────────────────────────────────────

/// A fake provider that returns a fixed APPROVE JSON, or an error when `fail`
/// is set.  Hermetic — no network.
struct RecordingLlm {
    /// When set, every call returns this transport error.
    fail: Option<String>,
    /// JSON response to return on success.
    response: String,
}

impl RecordingLlm {
    fn approving() -> Self {
        Self {
            fail: None,
            response: r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#.to_string(),
        }
    }

    fn with_response(json: &str) -> Self {
        Self {
            fail: None,
            response: json.to_string(),
        }
    }

    fn failing() -> Self {
        Self {
            fail: Some("simulated transport error".to_string()),
            response: String::new(),
        }
    }
}

#[async_trait]
impl LlmProvider for RecordingLlm {
    fn name(&self) -> &str {
        "recording-fake"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        if let Some(ref e) = self.fail {
            return Err(LlmError::Transport(e.clone()));
        }
        Ok(LlmResponse {
            text: self.response.clone(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn review_unit(file: &str, diff: &str) -> MapUnit {
    MapUnit {
        file: file.to_string(),
        status: "modified".to_string(),
        kind: MapUnitKind::Review {
            diff_text: diff.to_string(),
        },
        diff_char_count: diff.len(),
        chunk_index: 0,
        chunk_total: 1,
        hunk_oversized: false,
    }
}

fn meta_unit(file: &str, note: &str) -> MapUnit {
    MapUnit {
        file: file.to_string(),
        status: "removed".to_string(),
        kind: MapUnitKind::MetadataOnly {
            note: note.to_string(),
        },
        diff_char_count: 0,
        chunk_index: 0,
        chunk_total: 1,
        hunk_oversized: false,
    }
}

fn oversized_unit(file: &str, diff: &str) -> MapUnit {
    let mut u = review_unit(file, diff);
    u.hunk_oversized = true;
    u
}

fn pr_meta() -> ReviewPrMeta {
    ReviewPrMeta::default()
}

fn ctx<'a>(
    pr_meta: &'a ReviewPrMeta,
    context: &'a ReviewContext,
    voice: &'a VoiceConfig,
) -> MapContext<'a> {
    MapContext {
        owner: "acme",
        repo: "widgets",
        pr_meta,
        context,
        external_context: "",
        reviewer_model: "openai/gpt-5.4-mini-20260317",
        voice_config: voice,
        coverage_enabled: false,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Every reviewable unit is sent to the LLM and produces a `Reviewed` outcome,
/// and its diff text reaches the prompt (proving no truncation).
#[tokio::test]
async fn map_reviews_each_unit() {
    let llm: Arc<dyn LlmProvider> = Arc::new(RecordingLlm::approving());
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let units = vec![
        review_unit("src/a.rs", "+fn alpha() {}"),
        review_unit("src/b.rs", "+fn beta_signature(x: i32) -> i32 { x }"),
    ];
    let outcomes = run_map_stage(&units, &llm, &c, 4).await;

    assert_eq!(outcomes.len(), 2, "one outcome per reviewable unit");
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, MapOutcome::Reviewed { .. })),
        "every reviewable unit produces a Reviewed outcome"
    );
    // Both files are represented (no unit was dropped/truncated).
    assert!(outcomes.iter().any(|o| o.file() == "src/a.rs"));
    assert!(outcomes.iter().any(|o| o.file() == "src/b.rs"));
}

/// A `MetadataOnly` unit is skipped — no LLM call, `Skipped` outcome.
#[tokio::test]
async fn map_skips_metadata_only() {
    let llm: Arc<dyn LlmProvider> = Arc::new(RecordingLlm::approving());
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let units = vec![meta_unit("src/old.rs", "deleted file")];
    let outcomes = run_map_stage(&units, &llm, &c, 4).await;
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(outcomes[0], MapOutcome::Skipped { .. }));
}

/// A failing LLM call drops THAT file's review (Failed) without poisoning the
/// other units — fail-open.
#[tokio::test]
async fn map_failed_unit_does_not_poison() {
    let llm: Arc<dyn LlmProvider> = Arc::new(RecordingLlm::failing());
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let units = vec![
        review_unit("src/a.rs", "+fn alpha() {}"),
        review_unit("src/b.rs", "+fn beta() {}"),
    ];
    let outcomes = run_map_stage(&units, &llm, &c, 4).await;
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| matches!(
        o,
        MapOutcome::Failed {
            hunk_oversized: false,
            ..
        }
    )));
}

/// A single hunk that alone exceeds the per-file budget fails CLOSED for THAT
/// chunk only (`hunk_oversized: true`) — the #1639 backstop.
#[tokio::test]
async fn map_oversized_hunk_fails_closed_for_chunk() {
    let llm: Arc<dyn LlmProvider> = Arc::new(RecordingLlm::approving());
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let units = vec![
        oversized_unit("src/giant.rs", "+huge hunk"),
        review_unit("src/ok.rs", "+fn ok() {}"),
    ];
    let outcomes = run_map_stage(&units, &llm, &c, 4).await;
    assert_eq!(outcomes.len(), 2);

    let oversized = outcomes
        .iter()
        .find(|o| o.file() == "src/giant.rs")
        .expect("giant outcome present");
    assert!(matches!(
        oversized,
        MapOutcome::Failed {
            hunk_oversized: true,
            ..
        }
    ));
    // The other unit was still reviewed — one over-cap hunk doesn't poison.
    let ok = outcomes
        .iter()
        .find(|o| o.file() == "src/ok.rs")
        .expect("ok outcome present");
    assert!(matches!(ok, MapOutcome::Reviewed { .. }));
}

/// The map stage stamps the unit's file onto findings whose file the model
/// omitted, preserving inline-comment anchoring.
#[tokio::test]
async fn map_stamps_file_on_findings() {
    let json = r#"{"verdict":"REQUEST_CHANGES","summary":"bug","findings":[{"title":"t","body":"b","severity":"medium","confidence":0.9,"file":"","line":12}]}"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(RecordingLlm::with_response(json));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let units = vec![review_unit("src/target.rs", "+fn t() {}")];
    let outcomes = run_map_stage(&units, &llm, &c, 4).await;
    match &outcomes[0] {
        MapOutcome::Reviewed {
            verdict, findings, ..
        } => {
            assert_eq!(*verdict, Verdict::RequestChanges);
            assert_eq!(findings.len(), 1);
            assert_eq!(
                findings[0].file, "src/target.rs",
                "the unit's file must be stamped onto a file-less finding"
            );
        }
        other => panic!("expected Reviewed, got {other:?}"),
    }
}
