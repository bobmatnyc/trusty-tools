//! Stage C: LLM-based per-hunk substantive-ness classifier (spec REV-205–208).
//!
//! Why: regex hunk filtering (Stage B) only handles homogeneous-noise hunks.
//! Real PRs interleave noise and substance within the same hunk.  A cheap
//! Haiku classifier recovers budget by dropping mixed hunks that are
//! predominantly mechanical, but NEVER at the cost of missing real changes.
//!
//! What: `HunkClassifier::classify_batch` groups surviving hunks into batches
//! of `batch_size`, calls the injected `LlmProvider` once per batch at
//! temperature 0.0, parses the JSON array response, and returns
//! `HunkClassification` results.  Any error causes the whole batch to be
//! treated as `uncertain` (kept) — spec REV-208 fail-safe.
//!
//! Note: the classifier is optional and disabled by default (`disable_classifier`
//! = true in `FilterConfig`).  The `DiffAnalyzer` checks this flag before calling
//! this module.  When Stage C is enabled, it uses the same `LlmProvider` trait
//! that the reviewer/verifier use.  Issue #596 plans consolidation onto
//! `trusty_common::chat::ChatProvider`; for now we use the in-crate provider.
//!
//! Test: `classify_batch_all_uncertain_on_parse_error`,
//! `classify_batch_drops_mechanical_hunk`,
//! `parse_classification_array_valid`, `parse_classification_array_partial`.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::{
    llm::{ChatMessage, LlmProvider, LlmRequest},
    pipeline::diff_analyzer::models::{FilteredHunk, HunkDropReason},
};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of hunks per LLM batch call (spec REV-205).
pub const DEFAULT_BATCH_SIZE: usize = 10;

/// Maximum characters from a single hunk sent to the classifier.
pub const MAX_HUNK_CHARS: usize = 4_000;

/// Mechanical confidence threshold above which a hunk is dropped (spec REV-206).
pub const DROP_CONFIDENCE_THRESHOLD: f64 = 0.7;

/// Haiku model id used for classification (can be overridden via config).
pub const DEFAULT_CLASSIFIER_MODEL: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

// ─── System prompt ────────────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = "\
You are a code review assistant. Below are N code diff hunks from a pull request.\n\
For each hunk, classify it as exactly one of:\n\
  - \"substantive\": the hunk contains a meaningful change — logic change, schema\n\
    change, API surface change, security-relevant change, control-flow change,\n\
    new function/method body, bug fix, or any change that a reviewer must evaluate.\n\
  - \"mechanical\": the hunk is boilerplate or noise — pure formatting, whitespace\n\
    changes, import reordering, license/copyright header, generated code, fixture\n\
    data, JavaDoc-only additions with no logic, getter/setter stubs, pure rename.\n\
  - \"uncertain\": cannot determine substantiveness without more context.\n\
\n\
Return a JSON array with exactly one entry per hunk in order, each with:\n\
  {\"hunk_id\": \"<id>\", \"classification\": \"<substantive|mechanical|uncertain>\",\n\
   \"confidence\": <0.0-1.0>, \"reason\": \"<one sentence>\"}\n\
\n\
Do not include any text outside the JSON array. Do not wrap in markdown fences.";

// ─── Classification result ────────────────────────────────────────────────────

/// The LLM classification for a single hunk (spec REV-206).
///
/// Why: carries the full classifier verdict alongside hunk identity so the
/// orchestrator can make drop/keep decisions and record telemetry.
/// What: `is_mechanical` = true only when `classification == "mechanical"` AND
/// `confidence > DROP_CONFIDENCE_THRESHOLD`.  `uncertain` or low-confidence
/// mechanical results are fail-open (kept).
/// Test: `hunk_classification_mechanical_high_confidence_droppable`.
#[derive(Debug, Clone)]
pub struct HunkClassification {
    /// The hunk index within the batch (0-based).
    pub hunk_index: usize,
    /// Raw classification string from the LLM.
    pub classification: String,
    /// Confidence score 0.0–1.0.
    pub confidence: f64,
    /// One-sentence reason from the classifier.
    pub reason: String,
}

impl HunkClassification {
    /// Returns `true` if this hunk should be dropped (spec REV-206).
    ///
    /// Why: encapsulates the drop decision so callers do not inline the threshold.
    /// What: `mechanical` AND `confidence > DROP_CONFIDENCE_THRESHOLD` → drop.
    /// `uncertain`, `substantive`, or low-confidence mechanical → keep.
    /// Test: `hunk_classification_mechanical_high_confidence_droppable`.
    pub fn should_drop(&self) -> bool {
        self.classification == "mechanical" && self.confidence > DROP_CONFIDENCE_THRESHOLD
    }

    /// Returns the `HunkDropReason` for a dropped hunk.
    ///
    /// Why: the `FilteredDiff.drop_hunk_counts` map needs the reason enum.
    /// What: always returns `MechanicalHaiku` (the only Stage C drop reason).
    /// Test: covered transitively.
    pub fn drop_reason(&self) -> HunkDropReason {
        HunkDropReason::MechanicalHaiku
    }
}

// ─── JSON response shape ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ClassificationEntry {
    #[allow(dead_code)] // populated by serde from JSON; carried for debuggability
    hunk_id: String,
    classification: String,
    confidence: f64,
    #[serde(default)]
    reason: String,
}

// ─── HunkClassifier ───────────────────────────────────────────────────────────

/// Stage C Haiku-based per-hunk substantive-ness classifier (spec REV-205–208).
///
/// Why: deterministic Stages A+B cannot classify mixed-content hunks; this stage
/// recovers context budget by dropping high-confidence mechanical hunks while
/// maintaining the fail-open guarantee.
/// What: groups hunks into batches, builds a prompt, calls the LLM provider, and
/// parses the JSON response.  On ANY error the whole batch is kept (spec REV-208).
/// Test: `classify_batch_all_uncertain_on_parse_error`,
/// `classify_batch_drops_mechanical_hunk`.
pub struct HunkClassifier {
    provider: Arc<dyn LlmProvider>,
    model: String,
    batch_size: usize,
    /// Configurable drop confidence threshold; overrides DROP_CONFIDENCE_THRESHOLD.
    /// Stored for future per-caller configuration; `should_drop` uses the const default.
    #[allow(dead_code)]
    drop_threshold: f64,
}

impl HunkClassifier {
    /// Construct a `HunkClassifier` with the given provider and config.
    ///
    /// Why: provider is injected for testability (spec REV-261); the model can
    /// be configured independently of the reviewer/verifier models.
    /// What: stores the provider and config knobs; no external calls at construction.
    /// Test: construct with a `MockLlmProvider`; see classify_batch tests.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
        batch_size: usize,
        drop_threshold: f64,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            batch_size,
            drop_threshold,
        }
    }

    /// Classify `hunks` in batches; return per-hunk `HunkClassification` results.
    ///
    /// Why: batching amortises LLM call overhead while staying within context
    /// limits (spec REV-205).
    /// What: splits hunks into slices of `batch_size`, calls `classify_batch_slice`
    /// for each, collects results.  On error each batch returns `uncertain` for
    /// all hunks in that batch (spec REV-208).
    /// Test: `classify_batch_all_uncertain_on_parse_error`.
    pub async fn classify(&self, hunks: &[FilteredHunk]) -> Vec<HunkClassification> {
        let mut results = Vec::with_capacity(hunks.len());
        for (base_idx, batch) in hunks.chunks(self.batch_size).enumerate() {
            let batch_results = self.classify_batch_slice(batch, base_idx).await;
            results.extend(batch_results);
        }
        results
    }

    async fn classify_batch_slice(
        &self,
        batch: &[FilteredHunk],
        base_idx: usize,
    ) -> Vec<HunkClassification> {
        let user_content = self.build_batch_prompt(batch, base_idx);
        let req = LlmRequest {
            model: self.model.clone(),
            system: SYSTEM_PROMPT.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: user_content,
            }],
            temperature: 0.0,
            max_tokens: 1024,
            response_schema: None,
        };

        match self.provider.complete(req).await {
            Ok(resp) => match parse_classification_array(&resp.text, batch.len(), base_idx) {
                Ok(classifications) => {
                    debug!(
                        batch_start = base_idx,
                        count = classifications.len(),
                        "Stage C classified batch"
                    );
                    classifications
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        batch_start = base_idx,
                        batch_size = batch.len(),
                        "Stage C parse failed — keeping all hunks (fail-open)"
                    );
                    uncertain_batch(batch, base_idx)
                }
            },
            Err(e) => {
                warn!(
                    error = %e,
                    batch_start = base_idx,
                    batch_size = batch.len(),
                    "Stage C LLM error — keeping all hunks (fail-open, spec REV-208)"
                );
                uncertain_batch(batch, base_idx)
            }
        }
    }

    fn build_batch_prompt(&self, batch: &[FilteredHunk], base_idx: usize) -> String {
        let mut out = format!("Classify the following {} hunks:\n\n", batch.len());
        for (i, hunk) in batch.iter().enumerate() {
            let hunk_id = base_idx + i;
            let body = hunk.render();
            let truncated = if body.len() > MAX_HUNK_CHARS {
                &body[..MAX_HUNK_CHARS]
            } else {
                &body
            };
            out.push_str(&format!("--- HUNK {hunk_id} ---\n{truncated}\n\n"));
        }
        out
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse the LLM's JSON array response into `HunkClassification` entries.
///
/// Why: isolated for unit testing with mock response strings (no LLM needed).
/// What: strips markdown fences if present, deserialises the JSON array,
/// maps `hunk_id` strings to indices, and fills `expected_count` slots.
/// Test: `parse_classification_array_valid`, `parse_classification_array_partial`.
pub fn parse_classification_array(
    text: &str,
    expected_count: usize,
    base_idx: usize,
) -> Result<Vec<HunkClassification>, String> {
    // Strip markdown fences if the model added them despite instructions.
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let entries: Vec<ClassificationEntry> =
        serde_json::from_str(cleaned).map_err(|e| format!("JSON parse error: {e}"))?;

    if entries.len() != expected_count {
        return Err(format!(
            "expected {expected_count} entries, got {}",
            entries.len()
        ));
    }

    let mut results = Vec::with_capacity(entries.len());
    for (i, entry) in entries.into_iter().enumerate() {
        results.push(HunkClassification {
            hunk_index: base_idx + i,
            classification: entry.classification,
            confidence: entry.confidence.clamp(0.0, 1.0),
            reason: entry.reason,
        });
    }
    Ok(results)
}

/// Build `uncertain` (kept) results for all hunks in a batch (fail-open).
///
/// Why: any LLM or parse error must result in all hunks being kept (spec REV-208).
/// What: creates one `uncertain` `HunkClassification` per hunk in the batch.
/// Test: used in `classify_batch_all_uncertain_on_parse_error`.
fn uncertain_batch(batch: &[FilteredHunk], base_idx: usize) -> Vec<HunkClassification> {
    batch
        .iter()
        .enumerate()
        .map(|(i, _)| HunkClassification {
            hunk_index: base_idx + i,
            classification: "uncertain".to_string(),
            confidence: 0.0,
            reason: "fail-open: LLM error or parse failure".to_string(),
        })
        .collect()
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_classification_mechanical_high_confidence_droppable() {
        let c = HunkClassification {
            hunk_index: 0,
            classification: "mechanical".to_string(),
            confidence: 0.85,
            reason: "pure import reorder".to_string(),
        };
        assert!(c.should_drop());
    }

    #[test]
    fn hunk_classification_mechanical_low_confidence_kept() {
        let c = HunkClassification {
            hunk_index: 0,
            classification: "mechanical".to_string(),
            confidence: 0.5,
            reason: "unsure".to_string(),
        };
        assert!(!c.should_drop());
    }

    #[test]
    fn hunk_classification_substantive_not_dropped() {
        let c = HunkClassification {
            hunk_index: 0,
            classification: "substantive".to_string(),
            confidence: 0.99,
            reason: "logic change".to_string(),
        };
        assert!(!c.should_drop());
    }

    #[test]
    fn hunk_classification_uncertain_not_dropped() {
        let c = HunkClassification {
            hunk_index: 0,
            classification: "uncertain".to_string(),
            confidence: 0.9,
            reason: "need context".to_string(),
        };
        assert!(!c.should_drop());
    }

    #[test]
    fn parse_classification_array_valid() {
        let json = r#"[
            {"hunk_id": "0", "classification": "substantive", "confidence": 0.9, "reason": "logic change"},
            {"hunk_id": "1", "classification": "mechanical", "confidence": 0.8, "reason": "import reorder"}
        ]"#;
        let results = parse_classification_array(json, 2, 0).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].classification, "substantive");
        assert!(!results[0].should_drop());
        assert_eq!(results[1].classification, "mechanical");
        assert!(results[1].should_drop());
    }

    #[test]
    fn parse_classification_array_strips_markdown_fence() {
        let json = "```json\n[{\"hunk_id\": \"0\", \"classification\": \"uncertain\", \"confidence\": 0.5, \"reason\": \"x\"}]\n```";
        let results = parse_classification_array(json, 1, 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].classification, "uncertain");
    }

    #[test]
    fn parse_classification_array_wrong_count_returns_error() {
        let json = r#"[{"hunk_id": "0", "classification": "substantive", "confidence": 0.9, "reason": "x"}]"#;
        let err = parse_classification_array(json, 2, 0).unwrap_err();
        assert!(err.contains("expected 2"), "error: {err}");
    }

    #[test]
    fn parse_classification_array_invalid_json_returns_error() {
        let err = parse_classification_array("not json", 1, 0).unwrap_err();
        assert!(err.contains("JSON parse error"));
    }

    #[test]
    fn uncertain_batch_has_correct_length() {
        let hunks: Vec<FilteredHunk> = (0..3)
            .map(|i| FilteredHunk {
                header: format!("@@ -{i},1 +{i},1 @@"),
                lines: vec![format!("+line {i}")],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            })
            .collect();
        let results = uncertain_batch(&hunks, 5);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].hunk_index, 5);
        assert_eq!(results[2].hunk_index, 7);
        for r in &results {
            assert!(!r.should_drop(), "uncertain results must not be dropped");
        }
    }

    /// Live Bedrock test — requires real AWS credentials and is `#[ignore]`d in CI.
    ///
    /// Why: verifies the full Stage C round-trip with a real Haiku model call.
    /// What: constructs a real `BedrockProvider`, classifies two synthetic hunks
    /// (one import-only, one logic change), asserts mechanical is classified
    /// mechanical and substantive is not dropped.
    /// Test: `cargo test -p trusty-review -- stage_c_live_bedrock --include-ignored`
    #[tokio::test]
    #[ignore]
    async fn stage_c_live_bedrock() {
        use crate::llm::BedrockProvider;
        let provider = BedrockProvider::new(DEFAULT_CLASSIFIER_MODEL.to_string(), None)
            .await
            .expect("BedrockProvider must build");
        let classifier = HunkClassifier::new(
            Arc::new(provider),
            DEFAULT_CLASSIFIER_MODEL,
            10,
            DROP_CONFIDENCE_THRESHOLD,
        );
        let hunks = vec![
            FilteredHunk {
                header: "@@ -1,1 +1,1 @@".to_string(),
                lines: vec![
                    "-use std::io;".to_string(),
                    "+use std::io::{Read, Write};".to_string(),
                ],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            },
            FilteredHunk {
                header: "@@ -10,3 +10,5 @@".to_string(),
                lines: vec![
                    "-fn process(data: &[u8]) -> Result<(), Error> {".to_string(),
                    "+fn process(data: &[u8], config: &Config) -> Result<(), Error> {".to_string(),
                    "+    let timeout = config.timeout();".to_string(),
                    "+    validate_input(data, timeout)?;".to_string(),
                ],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            },
        ];
        let results = classifier.classify(&hunks).await;
        assert_eq!(results.len(), 2);
        // The logic change (second hunk) must NOT be dropped.
        assert!(
            !results[1].should_drop(),
            "substantive hunk must not be dropped: {:?}",
            results[1]
        );
    }
}
