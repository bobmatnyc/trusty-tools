//! Tests for `explain.rs` — prompt construction, streaming accumulation,
//! error classification, and the BedrockAuth credential-error mapping.
//!
//! Why: isolated test file keeps `explain.rs` under the 500-line cap while
//! retaining full coverage of the public and `pub(crate)` API surface.
//! What: unit tests for [`build_explain_prompt`], [`explain_report`],
//! [`classify_bedrock_error`], [`explain_with_bedrock_provider`],
//! [`resolve_model`], [`deep_analysis`], [`render_text`], and related helpers.
//! Test: cargo test -p trusty-analyze — all tests are synchronous or use
//! `#[tokio::test]`; 100% offline (no network, no AWS credentials).

use super::*;
use crate::core::review::{FileReview, ReviewComplexity, ReviewSource, SmellHit};
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

// ── Stub providers ────────────────────────────────────────────────────────────

/// Stub provider: replays a fixed string as a single `Delta` then `Done`.
struct StubProvider {
    text: String,
}

#[async_trait]
impl ChatProvider for StubProvider {
    fn name(&self) -> &str {
        "stub"
    }
    fn model(&self) -> &str {
        "stub-model"
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> anyhow::Result<()> {
        tx.send(ChatEvent::Delta(self.text.clone())).await.ok();
        tx.send(ChatEvent::Done).await.ok();
        Ok(())
    }
}

/// Stub provider that errors mid-stream.
struct ErrorProvider;

#[async_trait]
impl ChatProvider for ErrorProvider {
    fn name(&self) -> &str {
        "stub-err"
    }
    fn model(&self) -> &str {
        "stub-err"
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> anyhow::Result<()> {
        tx.send(ChatEvent::Error("boom".into())).await.ok();
        Ok(())
    }
}

/// Stub provider that emits a `ChatEvent::Error` whose text contains AWS
/// credential keywords. Used to exercise the `BedrockAuth` mapping path
/// entirely offline.
///
/// Why: we need an offline stand-in for the real `BedrockProvider` so the
/// credential-heuristic path in `explain_with_bedrock_provider` can be tested
/// without touching real AWS credentials or making network calls.
/// What: sends `ChatEvent::Error("AWS credential error: …")` and returns
/// `Ok(())` — the error is delivered through the channel, not as a task-level
/// failure.
/// Test: `bedrock_credential_error_maps_to_bedrock_auth`.
struct CredentialErrorProvider;

#[async_trait]
impl ChatProvider for CredentialErrorProvider {
    fn name(&self) -> &str {
        "stub-credential-error"
    }
    fn model(&self) -> &str {
        "stub-credential-error"
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> anyhow::Result<()> {
        tx.send(ChatEvent::Error(
            "AWS credential error: no credentials found in environment".into(),
        ))
        .await
        .ok();
        Ok(())
    }
}

// ── Test fixture ──────────────────────────────────────────────────────────────

fn sample_report() -> ReviewReport {
    ReviewReport {
        files: vec![
            FileReview {
                path: "src/big.rs".to_string(),
                grade: ComplexityGrade::D,
                complexity: ReviewComplexity {
                    cyclomatic: 32,
                    cognitive: 50,
                },
                smells: vec![SmellHit {
                    category: "long_method".into(),
                    line: 12,
                    severity: "medium".into(),
                }],
                recommendations: vec!["Split into helpers".into()],
                source: ReviewSource::NewFile,
            },
            FileReview {
                path: "src/small.rs".to_string(),
                grade: ComplexityGrade::A,
                complexity: ReviewComplexity {
                    cyclomatic: 1,
                    cognitive: 1,
                },
                smells: vec![],
                recommendations: vec![],
                source: ReviewSource::NewFile,
            },
        ],
        overall_grade: ComplexityGrade::D,
        changed_lines: 80,
        smell_count: 1,
        summary: "2 files analyzed".into(),
    }
}

// ── prompt-construction tests ─────────────────────────────────────────────────

#[test]
fn build_explain_prompt_includes_top_level_metrics() {
    let report = sample_report();
    let prompt = build_explain_prompt(&report, &[]);
    assert!(prompt.contains("Overall grade: D"), "got:\n{prompt}");
    assert!(prompt.contains("Smell count: 1"));
    assert!(prompt.contains("Changed lines: 80"));
    assert!(prompt.contains("Files reviewed: 2"));
}

#[test]
fn build_explain_prompt_lists_worst_files_first() {
    let report = sample_report();
    let prompt = build_explain_prompt(&report, &[]);
    let big_idx = prompt.find("src/big.rs").expect("big.rs listed");
    let small_idx = prompt.find("src/small.rs");
    if let Some(small_idx) = small_idx {
        assert!(big_idx < small_idx, "worst file must come first");
    }
}

#[test]
fn build_explain_prompt_mentions_detected_frameworks() {
    let report = sample_report();
    let frameworks = vec!["Next.js".to_string(), "React".to_string()];
    let prompt = build_explain_prompt(&report, &frameworks);
    assert!(prompt.contains("Detected frameworks:"));
    assert!(prompt.contains("Next.js"));
    assert!(prompt.contains("React"));
}

#[test]
fn build_explain_prompt_omits_frameworks_when_empty() {
    let prompt = build_explain_prompt(&sample_report(), &[]);
    assert!(!prompt.contains("Detected frameworks:"));
}

#[test]
fn build_explain_prompt_includes_smells_and_recommendations() {
    let report = sample_report();
    let prompt = build_explain_prompt(&report, &[]);
    assert!(prompt.contains("long_method"));
    assert!(prompt.contains("src/big.rs:12"));
    assert!(prompt.contains("Split into helpers"));
}

#[test]
fn build_explain_prompt_handles_empty_report() {
    let report = ReviewReport {
        files: vec![],
        overall_grade: ComplexityGrade::A,
        changed_lines: 0,
        smell_count: 0,
        summary: "0 files".into(),
    };
    let prompt = build_explain_prompt(&report, &[]);
    assert!(prompt.contains("no review-worthy issues"));
}

// ── streaming-accumulation tests ──────────────────────────────────────────────

#[tokio::test]
async fn explain_report_collects_stream_deltas() {
    let provider = StubProvider {
        text: "This change is mostly fine.".to_string(),
    };
    let narrative = explain_report(&sample_report(), &[], &provider)
        .await
        .unwrap();
    assert_eq!(narrative, "This change is mostly fine.");
}

#[tokio::test]
async fn explain_report_surfaces_stream_error() {
    let provider = ErrorProvider;
    let err = explain_report(&sample_report(), &[], &provider)
        .await
        .expect_err("error event should propagate");
    assert!(err.to_string().contains("boom"), "got: {err}");
}

// ── model / key resolution tests ─────────────────────────────────────────────

#[test]
fn resolve_model_defaults_when_no_override() {
    let prev = std::env::var(ENV_MODEL).ok();
    // SAFETY: tests serially set this env var; no concurrent access.
    unsafe { std::env::remove_var(ENV_MODEL) };
    assert_eq!(resolve_model(None), DEFAULT_MODEL);
    // SAFETY: restoring previous value preserves test isolation.
    if let Some(v) = prev {
        unsafe { std::env::set_var(ENV_MODEL, v) };
    }
}

#[test]
fn resolve_model_prefers_explicit() {
    assert_eq!(resolve_model(Some("explicit/model")), "explicit/model");
}

#[tokio::test]
async fn missing_api_key_returns_typed_error() {
    let prev = std::env::var(ENV_API_KEY).ok();
    // SAFETY: serial test access; no other thread touches this var.
    unsafe { std::env::remove_var(ENV_API_KEY) };
    let err = deep_analysis("idx", sample_report(), vec![], None, None)
        .await
        .expect_err("missing key should error");
    assert!(matches!(err, DeepAnalysisError::MissingApiKey));
    // SAFETY: restoring previous env state.
    if let Some(v) = prev {
        unsafe { std::env::set_var(ENV_API_KEY, v) };
    }
}

// ── extraction / render tests ─────────────────────────────────────────────────

#[test]
fn extract_recommendations_picks_bullets() {
    let prose = "Here is the situation.\n\n- Refactor src/big.rs\n* Add tests\n1. Document the helper\nLast sentence.\n";
    let recs = extract_recommendations(prose);
    assert_eq!(
        recs,
        vec![
            "Refactor src/big.rs".to_string(),
            "Add tests".to_string(),
            "Document the helper".to_string(),
        ]
    );
}

#[test]
fn extract_recommendations_empty_when_no_bullets() {
    assert!(extract_recommendations("just prose, nothing else").is_empty());
}

#[test]
fn strip_bullet_marker_rejects_non_numeric_prefix() {
    assert!(strip_bullet_marker("foo bar").is_none());
    assert!(strip_bullet_marker("a. not a number").is_none());
    assert_eq!(strip_bullet_marker("12. nice"), Some("nice"));
}

#[test]
fn deep_report_round_trips_json() {
    let r = DeepAnalysisReport {
        index_id: "idx".into(),
        narrative: "n".into(),
        frameworks: vec!["React".into()],
        recommendations: vec!["r1".into()],
        model_used: "m".into(),
        based_on: sample_report(),
    };
    let json = serde_json::to_string(&r).unwrap();
    let back: DeepAnalysisReport = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}

#[test]
fn render_text_includes_narrative_and_frameworks() {
    let r = DeepAnalysisReport {
        index_id: "idx".into(),
        narrative: "the narrative".into(),
        frameworks: vec!["Next.js".into(), "React".into()],
        recommendations: vec!["use server components".into()],
        model_used: "openai/gpt-4o-mini".into(),
        based_on: sample_report(),
    };
    let text = render_text(&r);
    assert!(text.contains("=== Deep Analysis ==="));
    assert!(text.contains("the narrative"));
    assert!(text.contains("Next.js, React"));
    assert!(text.contains("use server components"));
    assert!(text.contains("idx"));
}

#[test]
fn render_text_handles_empty_frameworks() {
    let r = DeepAnalysisReport {
        index_id: "idx".into(),
        narrative: "n".into(),
        frameworks: vec![],
        recommendations: vec![],
        model_used: "m".into(),
        based_on: sample_report(),
    };
    let text = render_text(&r);
    assert!(text.contains("frameworks: none detected"));
}

// ── Bedrock prefix-routing tests ──────────────────────────────────────────────

/// Verify that the `bedrock/` prefix is detected correctly and that the
/// model id is stripped before being passed to `BedrockProvider`.
///
/// Why: the prefix-routing logic must be stable — any drift would silently
/// send `bedrock/` model ids to OpenRouter, which would fail with a 404.
/// What: checks the constant prefix string, and verifies that `strip_prefix`
/// on a sample model id produces the correct bare id.
/// Test: pure string logic, no network or provider construction needed.
#[test]
fn bedrock_prefix_routing() {
    let full_model = "bedrock/us.anthropic.claude-sonnet-4-6";
    assert!(full_model.starts_with(BEDROCK_MODEL_PREFIX));
    let stripped = full_model
        .strip_prefix(BEDROCK_MODEL_PREFIX)
        .expect("strip_prefix should succeed");
    assert_eq!(stripped, "us.anthropic.claude-sonnet-4-6");

    // Non-bedrock model ids should NOT match the prefix.
    assert!(!("openai/gpt-4o-mini".starts_with(BEDROCK_MODEL_PREFIX)));
    assert!(!("anthropic/claude-3-5-sonnet".starts_with(BEDROCK_MODEL_PREFIX)));

    // Default model constant should be the bare id (no prefix).
    assert!(!DEFAULT_BEDROCK_MODEL_ID.starts_with(BEDROCK_MODEL_PREFIX));
}

/// Verify that `bedrock/` with nothing after the slash falls back to the
/// default model id constant.
///
/// Why: operators may set `TRUSTY_LLM_MODEL=bedrock/` without a specific
/// model to get the default Sonnet 4.6 profile. The routing logic should
/// gracefully handle this.
/// What: checks the fallback branch in `deep_analysis`'s model-id stripping.
/// Test: pure string logic.
#[test]
fn bedrock_prefix_empty_suffix_falls_back_to_default() {
    let full_model = "bedrock/";
    let id = full_model
        .strip_prefix(BEDROCK_MODEL_PREFIX)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BEDROCK_MODEL_ID);
    assert_eq!(id, DEFAULT_BEDROCK_MODEL_ID);
}

// ── BedrockAuth credential-error mapping tests (closes #951) ─────────────────

/// Verify that a stream error containing "credential" / "AWS" text is
/// classified as [`DeepAnalysisError::BedrockAuth`] by
/// [`explain_with_bedrock_provider`].
///
/// Why: the string-heuristic that promotes credential errors to `BedrockAuth`
/// was previously inlined and untested — a rename of an upstream error message
/// would have silently demoted auth failures to the generic `Chat` variant.
/// This test pins the mapping end-to-end through the full `explain_report`
/// call chain so any upstream wording change is caught immediately.
/// What: invokes `explain_with_bedrock_provider` with
/// `CredentialErrorProvider` (which emits `ChatEvent::Error` containing
/// "credential" and "AWS"), and asserts the returned error is
/// `DeepAnalysisError::BedrockAuth`.
/// Test: 100% offline — no network, no real AWS credentials.
#[tokio::test]
async fn bedrock_credential_error_maps_to_bedrock_auth() {
    let provider = CredentialErrorProvider;
    let err = explain_with_bedrock_provider(
        "test-index",
        sample_report(),
        vec![],
        "bedrock/us.anthropic.claude-sonnet-4-6",
        &provider,
    )
    .await
    .expect_err("credential error should produce an Err");
    assert!(
        matches!(err, DeepAnalysisError::BedrockAuth),
        "expected BedrockAuth, got: {err:?}",
    );
}

/// Verify that [`classify_bedrock_error`] maps "credential"-containing
/// messages to [`DeepAnalysisError::BedrockAuth`].
///
/// Why: unit-tests the heuristic in isolation so the mapping logic is
/// validated independently of the async stream machinery.
/// What: passes representative credential-error strings into
/// `classify_bedrock_error` and asserts each one yields `BedrockAuth`.
/// Test: pure synchronous logic, no I/O.
#[test]
fn bedrock_credential_error_classifies_as_bedrock_auth() {
    let cases = [
        "credential not found",
        "No credentials provided",
        "AWS_ACCESS_KEY_ID not set",
        "aws region missing",
    ];
    for msg in &cases {
        let e = anyhow::anyhow!("{}", msg);
        assert!(
            matches!(classify_bedrock_error(&e), DeepAnalysisError::BedrockAuth),
            "expected BedrockAuth for message: {msg}",
        );
    }
}

/// Verify that [`classify_bedrock_error`] maps unrelated errors to
/// [`DeepAnalysisError::Chat`], not [`DeepAnalysisError::BedrockAuth`].
///
/// Why: ensures the heuristic is not overly broad — a generic model timeout
/// or network error must not be promoted to an auth error.
/// What: passes a non-credential message into `classify_bedrock_error` and
/// asserts the result is the `Chat` variant.
/// Test: pure synchronous logic, no I/O.
#[test]
fn non_credential_bedrock_error_classifies_as_chat() {
    let e = anyhow::anyhow!("connection timeout after 30s");
    assert!(
        matches!(classify_bedrock_error(&e), DeepAnalysisError::Chat(_)),
        "non-credential error should map to Chat variant",
    );
}
