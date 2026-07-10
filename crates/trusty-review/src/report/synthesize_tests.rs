//! Tests for report M2 synthesis (#2314).
//!
//! Why: extracted from `synthesize.rs` to keep that file under the 500-line cap.
//! What: covers the numeric guardrail primitives, the fail-closed paths
//! (provider error, malformed JSON, timeout, truncation), happy-path injection,
//! guardrail rejection of a fabricated figure, the structural greens exclusion,
//! and the forced-output JSON schema shape.  No live LLM calls.
//! Test: included as `#[cfg(test)] mod tests` from `synthesize.rs`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::report::metrics::{
    AnalyzeMetrics, CountMetrics, LanguageLoc, LocMetrics, MetricFinding, Severity,
};
use crate::report::model::{ReportModel, RepositoryReport};
use crate::report::synthesize_guard::{allowed_numbers, numbers_in, verify_prose};
use crate::report::synthesize_prompt::{build_synthesis_prompt, synthesis_schema};

use super::{Synthesis, SynthesisStatus, Synthesizer};

// ── Stub providers ──────────────────────────────────────────────────────────

/// Returns a fixed body with an optional `finish_reason`.
struct FixedLlm {
    body: String,
    finish_reason: Option<String>,
}

#[async_trait]
impl LlmProvider for FixedLlm {
    fn name(&self) -> &str {
        "fixed"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.body.clone(),
            model: "stub".to_string(),
            input_tokens: 100,
            output_tokens: 80,
            latency_ms: 5,
            cost_usd: 0.0001,
            finish_reason: self.finish_reason.clone(),
        })
    }
}

struct ErrorLlm;

#[async_trait]
impl LlmProvider for ErrorLlm {
    fn name(&self) -> &str {
        "error"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("connection refused".to_string()))
    }
}

struct SleepLlm;

#[async_trait]
impl LlmProvider for SleepLlm {
    fn name(&self) -> &str {
        "sleep"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        unreachable!("timeout should fire first")
    }
}

// ── Model fixture ───────────────────────────────────────────────────────────

/// Build a one-repo model: 8200 LoC, 120 files, 640 functions, and the given
/// findings.  Numbers 8200/120/640 are the guardrail's allowed figures.
fn fixture_model(findings: Vec<MetricFinding>) -> ReportModel {
    let metrics = AnalyzeMetrics {
        loc: LocMetrics {
            total: 8200,
            by_language: vec![LanguageLoc {
                language: "Rust".to_string(),
                loc: 8200,
            }],
        },
        counts: CountMetrics {
            files: 120,
            functions: 640,
        },
        findings,
        ..Default::default()
    };
    ReportModel {
        title: "Acme Technical DD".to_string(),
        template: "report-technical-dd".to_string(),
        analyst: Some("bobmatnyc".to_string()),
        report_date: "2026-07-10".to_string(),
        generated_date: "2026-07-10".to_string(),
        manifest_path: "manifest.toml".to_string(),
        repositories: vec![RepositoryReport {
            name: "Acme Core".to_string(),
            slug: "acme-core".to_string(),
            source: "local".to_string(),
            source_kind: "local_path".to_string(),
            username: None,
            git_ref: None,
            git_info: None,
            metrics: Some(metrics),
        }],
        synthesis: None,
        benchmark: None,
    }
}

fn red(title: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity: Severity::Red,
        category: "security".to_string(),
        component: "auth".to_string(),
    }
}

fn green(title: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity: Severity::Green,
        category: "maintainability".to_string(),
        component: "core".to_string(),
    }
}

// ── Guardrail primitives ────────────────────────────────────────────────────

/// Why: the guardrail must treat formatting variants of the same magnitude as
/// equal, or it would reject faithful restatements.
/// What: checks comma, `$`, `%`, and trailing-zero-decimal normalisation.
/// Test: this test itself.
#[test]
fn numbers_in_extracts_and_canonicalises() {
    assert_eq!(numbers_in("8,200 LoC"), vec!["8200"]);
    assert_eq!(numbers_in("$1,000 cost"), vec!["1000"]);
    assert_eq!(numbers_in("50% covered"), vec!["50"]);
    assert_eq!(numbers_in("value 8200.0 here"), vec!["8200"]);
    assert_eq!(numbers_in("score 3.50"), vec!["3.5"]);
    assert!(numbers_in("no digits here").is_empty());
}

/// Why: the allowed set is the guardrail's ground truth, derived from the
/// deterministic model.
/// What: asserts the metric figures are present and a fabricated one is not.
/// Test: this test itself.
#[test]
fn allowed_numbers_covers_metrics() {
    let model = fixture_model(vec![]);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    assert!(allowed.contains("8200"), "LoC total must be allowed");
    assert!(allowed.contains("120"), "file count must be allowed");
    assert!(allowed.contains("640"), "function count must be allowed");
    assert!(
        !allowed.contains("9999"),
        "fabricated figure must be absent"
    );
}

/// Why: faithful prose passes; fabricated prose is rejected with the offending
/// token surfaced.
/// What: verifies a clean string and rejects one citing 9999.
/// Test: this test itself.
#[test]
fn verify_prose_passes_and_rejects() {
    let model = fixture_model(vec![]);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    assert!(verify_prose("The 8,200 LoC codebase has 120 files.", &allowed).is_ok());
    assert!(verify_prose("No numbers at all.", &allowed).is_ok());
    assert_eq!(
        verify_prose("Coverage is 9999 percent.", &allowed),
        Err("9999".to_string())
    );
}

// ── Prompt / schema ─────────────────────────────────────────────────────────

/// Why: the no-green-analysis rule must be STRUCTURAL — greens never reach the
/// model, so it cannot elaborate them.
/// What: builds the prompt for a model with a red and a green finding; asserts
/// the digest contains the red title but not the green one.
/// Test: this test itself.
#[test]
fn prompt_excludes_greens() {
    let model = fixture_model(vec![red("SQL injection risk"), green("GREEN_SECRET_TOPIC")]);
    let req = build_synthesis_prompt(&model, "stub/model");
    let digest = &req.messages[0].content;
    assert!(
        digest.contains("SQL injection risk"),
        "RED finding must be present in the digest"
    );
    assert!(
        !digest.contains("GREEN_SECRET_TOPIC"),
        "GREEN finding must NEVER reach the model (structural exclusion)"
    );
}

/// Why: a routing prefix must be stripped so the bare id reaches the provider.
/// What: asserts `bedrock/` and `openrouter/` are stripped from `req.model`.
/// Test: this test itself.
#[test]
fn prompt_strips_prefix() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(&model, "bedrock/us.anthropic.claude-sonnet-4-6");
    assert_eq!(req.model, "us.anthropic.claude-sonnet-4-6");
    let req2 = build_synthesis_prompt(&model, "openrouter/openai/gpt-5.4-mini");
    assert_eq!(req2.model, "openai/gpt-5.4-mini");
}

/// Why: forced structured output requires a well-formed schema.
/// What: asserts the schema name and the three top-level narrative properties.
/// Test: this test itself.
#[test]
fn synthesis_schema_shape() {
    let schema = synthesis_schema();
    assert_eq!(schema.name, "report_synthesis");
    let props = &schema.schema["properties"];
    assert!(props["executive_summary"].is_object());
    assert!(props["top_risks"].is_object());
    assert!(props["findings"].is_object());
    let req = build_synthesis_prompt(&fixture_model(vec![]), "stub/model");
    assert!(
        req.response_schema.is_some(),
        "every synthesis request must force structured output"
    );
}

// ── Synthesize: happy path + fail-closed paths ──────────────────────────────

fn good_response() -> String {
    // Every figure (8200, 120, 640) is present in the fixture model.
    r#"{
      "executive_summary": "The 8,200 LoC Rust codebase spans 120 files and 640 functions with one critical security gap.",
      "top_risks": [
        {"description": "Unauthenticated admin route", "severity": "RED", "cost": "high", "apps": "Acme Core"}
      ],
      "findings": [
        {"app_slug": "acme-core", "title": "SQL injection risk", "severity": "RED",
         "description": "Raw query concatenation in the auth path.",
         "evidence": "Observed in the auth handler.", "component": "auth",
         "business_impact": "Potential data exfiltration.",
         "remediation": "Parameterise queries.", "cost_effort": "moderate"}
      ]
    }"#
    .to_string()
}

/// Why: verified prose must be injected and the status marked available.
/// What: runs synthesize with a clean response; asserts exec/risks/findings and
/// status Available.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_happy_path_injects() {
    let model = fixture_model(vec![red("SQL injection risk")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: good_response(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;

    assert_eq!(result.status, SynthesisStatus::Available);
    assert!(result.executive_summary.is_some());
    assert_eq!(result.top_risks.len(), 1);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].app_slug, "acme-core");
    assert!(result.notes.is_empty(), "clean response records no notes");
}

/// Why: a malformed response must never be partial-trusted.
/// What: returns non-JSON; asserts Unavailable("unparseable response").
/// Test: this test itself.
#[tokio::test]
async fn synthesize_malformed_json_fails_closed() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: "this is not json at all {{{".to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;
    assert_eq!(
        result.status,
        SynthesisStatus::Unavailable("unparseable response".to_string())
    );
    assert!(result.executive_summary.is_none());
    assert!(result.findings.is_empty());
}

/// Why: provider errors fail closed to the deterministic output.
/// What: uses ErrorLlm; asserts Unavailable with a provider-error reason.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_provider_error_fails_closed() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(ErrorLlm);
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;
    match result.status {
        SynthesisStatus::Unavailable(reason) => assert!(reason.contains("provider error")),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// Why: a truncated response is incomplete and must fail closed.
/// What: returns a valid body but `finish_reason = "length"`; asserts
/// Unavailable("truncated response").
/// Test: this test itself.
#[tokio::test]
async fn synthesize_truncation_fails_closed() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: good_response(),
        finish_reason: Some("length".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;
    assert_eq!(
        result.status,
        SynthesisStatus::Unavailable("truncated response".to_string())
    );
}

/// Why: a timeout must fail closed rather than hang the report.
/// What: uses a 30s-sleeping provider with a 10ms timeout; asserts
/// Unavailable("provider timeout").
/// Test: this test itself.
#[tokio::test]
async fn synthesize_timeout_fails_closed() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(SleepLlm);
    let result = Synthesizer::new(llm, "stub/model")
        .with_timeout(Duration::from_millis(10))
        .synthesize(&model)
        .await;
    assert_eq!(
        result.status,
        SynthesisStatus::Unavailable("provider timeout".to_string())
    );
}

/// Why: the numeric guardrail must drop any field citing a figure not in source.
/// What: exec summary cites 9999 (rejected); the finding is clean (kept).
/// Asserts exec is dropped with a `rejected (unverified figure)` note while the
/// finding survives, and the overall status is still Available.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_rejects_unverified_figure() {
    let model = fixture_model(vec![red("SQL injection risk")]);
    let body = r#"{
      "executive_summary": "Coverage sits at 9999 percent across the estate.",
      "top_risks": [],
      "findings": [
        {"app_slug": "acme-core", "title": "SQL injection risk", "severity": "RED",
         "description": "Raw query concatenation.", "evidence": "one path",
         "component": "auth", "business_impact": "data loss",
         "remediation": "parameterise", "cost_effort": "moderate"}
      ]
    }"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: body.to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;

    assert_eq!(result.status, SynthesisStatus::Available);
    assert!(
        result.executive_summary.is_none(),
        "exec summary citing 9999 must be rejected"
    );
    assert_eq!(result.findings.len(), 1, "clean finding must survive");
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("rejected (unverified figure)") && n.contains("9999")),
        "a guardrail rejection note must be recorded: {:?}",
        result.notes
    );
}

/// Why: when nothing survives verification the whole attempt is unavailable.
/// What: every field cites 9999; asserts Unavailable("no verifiable content").
/// Test: this test itself.
#[tokio::test]
async fn synthesize_all_rejected_is_unavailable() {
    let model = fixture_model(vec![]);
    let body = r#"{
      "executive_summary": "Exactly 9999 issues.",
      "top_risks": [{"description": "risk 7777", "severity": "RED", "cost": "low", "apps": "Acme Core"}],
      "findings": []
    }"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: body.to_string(),
        finish_reason: None,
    });
    let result = Synthesizer::new(llm, "stub/model").synthesize(&model).await;
    assert_eq!(
        result.status,
        SynthesisStatus::Unavailable("no verifiable content".to_string())
    );
    assert_eq!(result.notes.len(), 2, "both rejections recorded");
}

/// Why: the status note must carry the exact `synthesis:` banners the report and
/// its readers key on.
/// What: asserts the available and unavailable banner strings.
/// Test: this test itself.
#[test]
fn status_lines_render_banners() {
    let avail = Synthesis {
        status: SynthesisStatus::Available,
        ..Default::default()
    };
    assert_eq!(avail.status_lines()[0], "synthesis: available");

    let unavail = Synthesis::unavailable("provider timeout");
    assert_eq!(
        unavail.status_lines()[0],
        "synthesis: unavailable (provider timeout)"
    );
}
