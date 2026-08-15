//! Tests for report M2 synthesis (#2314; compact digest + layered section
//! instructions + truncation retry, #2357 follow-up).
//!
//! Why: extracted from `synthesize.rs` to keep that file under the 500-line cap.
//! What: covers the numeric guardrail primitives, the fail-closed paths
//! (provider error, malformed JSON, timeout, truncation, still-truncated after
//! retry), the one-shot truncation retry recovering, happy-path injection,
//! guardrail rejection of a fabricated figure, the structural greens exclusion,
//! the forced-output JSON schema shape (incl. the `maxItems` bounds), and the
//! layered section-instruction resolution (generic default vs. template
//! override) reaching the built system prompt.  No live LLM calls.
//! Test: included as `#[cfg(test)] mod tests` from `synthesize.rs`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::report::metrics::{
    AnalyzeMetrics, CountMetrics, LanguageLoc, LocMetrics, MetricFinding, Severity,
};
use crate::report::model::{ReportModel, RepositoryReport};
use crate::report::synthesize_guard::{allowed_numbers, numbers_in, verify_prose};
use crate::report::synthesize_prompt::{build_synthesis_prompt, synthesis_schema};

use super::{Synthesis, SynthesisError, Synthesizer};

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

/// A provider that returns pre-scripted `(body, finish_reason)` pairs in call
/// order and counts how many times it was called — the harness for the
/// one-shot truncation-retry tests (#2357 follow-up).
struct QueuedLlm {
    queue: Mutex<VecDeque<(String, Option<String>)>>,
    calls: std::sync::atomic::AtomicUsize,
}

impl QueuedLlm {
    fn new(responses: Vec<(&str, Option<&str>)>) -> Self {
        QueuedLlm {
            queue: Mutex::new(
                responses
                    .into_iter()
                    .map(|(b, f)| (b.to_string(), f.map(str::to_string)))
                    .collect(),
            ),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmProvider for QueuedLlm {
    fn name(&self) -> &str {
        "queued"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (body, finish_reason) = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ("{}".to_string(), Some("stop".to_string())));
        Ok(LlmResponse {
            text: body,
            model: "queued".to_string(),
            input_tokens: 10,
            output_tokens: 10,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason,
        })
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
        client: None,
        vendor_methodology: crate::report::model::vendor_methodology(),
        instructions: None,
        instructions_source: None,
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
            local_path: None,
            scan: None,
            metrics: Some(metrics),
        }],
        gaps: Vec::new(),
        synthesis: None,
        benchmark: None,
        investigation: None,
        section_instructions: Default::default(),
        // #5405: synthesis does not read the board figures.
        ticketing: None,
    }
}

fn red(title: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity: Severity::Red,
        category: "security".to_string(),
        component: "auth".to_string(),
        ..Default::default()
    }
}

fn green(title: &str) -> MetricFinding {
    MetricFinding {
        title: title.to_string(),
        severity: Severity::Green,
        category: "maintainability".to_string(),
        component: "core".to_string(),
        ..Default::default()
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
    let req = build_synthesis_prompt(&model, "stub/model", false);
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
    let req = build_synthesis_prompt(&model, "bedrock/us.anthropic.claude-sonnet-4-6", false);
    assert_eq!(req.model, "us.anthropic.claude-sonnet-4-6");
    let req2 = build_synthesis_prompt(&model, "openrouter/openai/gpt-5.4-mini", false);
    assert_eq!(req2.model, "openai/gpt-5.4-mini");
}

/// Why: forced structured output requires a well-formed schema, and the
/// `top_risks`/`findings` arrays must carry the #2357-follow-up structural
/// size bounds (`maxItems`) — not merely a polite description.
/// What: asserts the schema name, the three top-level narrative properties,
/// and the maxItems caps (5 for a normal top_risks pass, 10 for findings).
/// Test: this test itself.
#[test]
fn synthesis_schema_shape() {
    let schema = synthesis_schema(5);
    assert_eq!(schema.name, "report_synthesis");
    let props = &schema.schema["properties"];
    assert!(props["executive_summary"].is_object());
    assert!(props["top_risks"].is_object());
    assert!(props["findings"].is_object());
    assert_eq!(props["top_risks"]["maxItems"], 5);
    assert_eq!(props["findings"]["maxItems"], 10);
    let req = build_synthesis_prompt(&fixture_model(vec![]), "stub/model", false);
    assert!(
        req.response_schema.is_some(),
        "every synthesis request must force structured output"
    );
}

/// Why: the schema's `top_risks` cap must shrink on the one-shot truncation
/// retry (mirroring the wave-3 batch investigation's cap-shrinking retry).
/// What: `retry_concise = true` yields `maxItems: 3` and a retry directive in
/// the system prompt.
/// Test: this test itself.
#[test]
fn retry_concise_shrinks_top_risks_cap_and_adds_directive() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(&model, "stub/model", true);
    let schema = req.response_schema.expect("schema present");
    assert_eq!(schema.schema["properties"]["top_risks"]["maxItems"], 3);
    assert!(req.system.contains("Retry directive"));
    assert!(req.system.to_lowercase().contains("truncated"));
}

// ── Layered section instructions (#2357 follow-up) ─────────────────────────

/// Why: a bare `--synthesize` run (no template override) must use the crate's
/// own generic section-instruction defaults.
/// What: the built system prompt embeds the generic `executive_summary`
/// default text verbatim.
/// Test: this test itself.
#[test]
fn system_prompt_uses_generic_defaults_when_no_override() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(&model, "stub/model", false);
    assert!(
        req.system.contains("deal-analytic paragraph"),
        "generic executive_summary default must appear verbatim: {}",
        req.system
    );
}

/// Why: a template's `<!-- instruct:<id> ... -->` override must WIN over the
/// generic default and actually reach the built request — this is the
/// coordinator's explicit acceptance test for the template-override tier.
/// What: sets `model.section_instructions["executive_summary"]` to a
/// CAST-flavoured override string; asserts it appears verbatim in `req.system`
/// and the generic default's own distinguishing phrase does not.
/// Test: this test itself.
#[test]
fn template_override_reaches_system_prompt() {
    let mut model = fixture_model(vec![]);
    model.section_instructions.insert(
        "executive_summary".to_string(),
        "CAST-FLAVOURED OVERRIDE: lead with the TQI and health-factor posture.".to_string(),
    );
    let req = build_synthesis_prompt(&model, "stub/model", false);
    assert!(
        req.system.contains("CAST-FLAVOURED OVERRIDE"),
        "template override must reach the system prompt: {}",
        req.system
    );
    assert!(
        !req.system.contains("deal-analytic paragraph"),
        "the override must REPLACE the generic default, not merge with it"
    );
}

/// Why: overriding one section must not disturb the others' generic defaults.
/// What: an override for `top_risks` only leaves `executive_summary` generic.
/// Test: this test itself.
#[test]
fn partial_template_override_leaves_other_sections_generic() {
    let mut model = fixture_model(vec![]);
    model.section_instructions.insert(
        "top_risks".to_string(),
        "TOP RISKS OVERRIDE TEXT".to_string(),
    );
    let req = build_synthesis_prompt(&model, "stub/model", false);
    assert!(req.system.contains("TOP RISKS OVERRIDE TEXT"));
    assert!(req.system.contains("deal-analytic paragraph"));
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

/// Why: verified prose must be injected and the pass must succeed.
/// What: runs synthesize with a clean response; asserts exec/risks/findings.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_happy_path_injects() {
    let model = fixture_model(vec![red("SQL injection risk")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: good_response(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("a clean response synthesizes");

    assert!(result.executive_summary.is_some());
    assert_eq!(result.top_risks.len(), 1);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].app_slug, "acme-core");
    assert!(result.notes.is_empty(), "clean response records no notes");
}

/// Why: a malformed response must never be partial-trusted. #5454 regression —
/// this used to return `Ok(Synthesis::unavailable(..))` and the report shipped
/// deterministic-only; it is now a hard error.
/// What: returns non-JSON; asserts `Err(SynthesisError::Unparseable)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_malformed_json_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: "this is not json at all {{{".to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("an unparseable response must not yield a usable synthesis");
    assert!(
        matches!(err, SynthesisError::Unparseable),
        "expected Unparseable, got {err:?}"
    );
}

/// Why: #5454 — a provider failure is the single most likely way an audit loses
/// its narrative (rate limit, bad model id, network). It used to degrade the
/// report silently; it must now stop the run.
/// What: uses ErrorLlm; asserts `Err(SynthesisError::Provider(..))` carrying the
/// provider's own text.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_provider_error_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(ErrorLlm);
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a provider error must not yield a usable synthesis");
    match err {
        SynthesisError::Provider(reason) => {
            assert!(!reason.is_empty(), "the provider's reason must be carried")
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

/// Why: a truncated response is incomplete and must never be partial-trusted.
/// What: returns a valid body but `finish_reason = "length"` on both the initial
/// call and the retry; asserts `Err(SynthesisError::Truncated)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_truncation_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: good_response(),
        finish_reason: Some("length".to_string()),
    });
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a truncated response must not yield a usable synthesis");
    assert!(
        matches!(err, SynthesisError::Truncated),
        "expected Truncated, got {err:?}"
    );
}

/// Why: the one-shot truncation retry (#2357 follow-up) must recover a
/// response that only truncated on the FIRST attempt — the cheap-resilience
/// path that keeps a transient truncation from discarding the whole narrative.
/// What: the queued stub truncates on call 1 and returns a clean response on
/// call 2; asserts the final status is `Available` with the exec summary
/// present, and that exactly 2 calls were made (one retry, not more).
/// Test: this test itself.
#[tokio::test]
async fn synthesize_retry_recovers_from_truncation() {
    let model = fixture_model(vec![red("SQL injection risk")]);
    let good = good_response();
    let llm = Arc::new(QueuedLlm::new(vec![
        ("{}", Some("length")),
        (good.as_str(), Some("stop")),
    ]));
    let result = Synthesizer::new(llm.clone(), "stub/model")
        .synthesize(&model)
        .await
        .expect("the retry recovers a first-call truncation");

    assert!(result.executive_summary.is_some());
    assert_eq!(llm.call_count(), 2, "exactly one retry must have occurred");
}

/// Why: the retry is a single cheap attempt, never an open-ended loop. #5454
/// changed only what happens when it is spent — an error, not a degraded report.
/// What: the queued stub truncates on both calls; asserts
/// `Err(SynthesisError::Truncated)` and exactly 2 calls (no further retries).
/// Test: this test itself.
#[tokio::test]
async fn synthesize_still_truncated_after_retry_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm = Arc::new(QueuedLlm::new(vec![
        ("{}", Some("length")),
        ("{}", Some("length")),
    ]));
    let err = Synthesizer::new(llm.clone(), "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a still-truncated retry must not yield a usable synthesis");
    assert!(
        matches!(err, SynthesisError::Truncated),
        "expected Truncated, got {err:?}"
    );
    assert_eq!(
        llm.call_count(),
        2,
        "no more than one retry must be attempted"
    );
}

/// Why: a hung provider must stop the report rather than hang it — and #5454
/// makes that stop an error rather than a narrative-free report.
/// What: uses a 30s-sleeping provider with a 10ms timeout; asserts
/// `Err(SynthesisError::Timeout)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_timeout_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(SleepLlm);
    let err = Synthesizer::new(llm, "stub/model")
        .with_timeout(Duration::from_millis(10))
        .synthesize(&model)
        .await
        .expect_err("a timeout must not yield a usable synthesis");
    assert!(
        matches!(err, SynthesisError::Timeout),
        "expected Timeout, got {err:?}"
    );
}

/// Why: the numeric guardrail must drop any field citing a figure not in source.
/// What: exec summary cites 9999 (rejected); the finding is clean (kept).
/// Asserts exec is dropped with a `rejected (unverified figure)` note while the
/// finding survives, and the pass as a whole still succeeds — per-FIELD rejection
/// remains a correctness property under #5454's required-inference rule, and the
/// dropped summary falls through to the deterministic composition (#5374).
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
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("one clean finding keeps the pass successful");

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

/// Why: when nothing survives verification there is no narrative, which #5454
/// makes a failed report rather than a deterministic-only one.
/// What: every field cites 9999; asserts
/// `Err(SynthesisError::NoVerifiableContent)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_all_rejected_is_a_hard_error() {
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
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a wholly-rejected response must not yield a usable synthesis");
    assert!(
        matches!(err, SynthesisError::NoVerifiableContent),
        "expected NoVerifiableContent, got {err:?}"
    );
}

/// Why: this is the direct acceptance-QA regression test — a large, real finding
/// count (mirroring the "26 verified findings" scenario) must still produce a
/// populated executive summary rather than a blank one from a truncated
/// top-level synthesis call.
/// What: builds a model with 45 RED/AMBER findings (exceeding the 40-finding
/// context cap) across two repos; a stub provider returns a clean, concise
/// response citing only the model's own figures; asserts `Available` with a
/// non-empty executive summary.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_with_many_findings_produces_exec_summary() {
    let mut model = fixture_model(vec![]);
    let mut findings: Vec<MetricFinding> = Vec::new();
    for i in 0..45 {
        findings.push(red(&format!("Finding {i}")));
    }
    model.repositories[0].metrics.as_mut().unwrap().findings = findings;

    let body = r#"{
      "executive_summary": "The 8,200 LoC Rust codebase (120 files, 640 functions) carries a large volume of RED/AMBER findings across authentication, dependency, and error-handling dimensions that an acquirer must remediate before close.",
      "top_risks": [
        {"description": "Widespread unauthenticated routes", "severity": "RED", "cost": "high", "apps": "Acme Core"}
      ],
      "findings": []
    }"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: body.to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("a clean response at scale synthesizes");

    assert!(
        result.executive_summary.is_some(),
        "executive summary must not be blank at scale: notes={:?}",
        result.notes
    );
    assert!(!result.executive_summary.unwrap().is_empty());
}

/// Why: the status note carries the `synthesis:` banner the report's readers key
/// on, plus every guardrail rejection.
/// What: asserts the banner line and that notes follow it in order.
/// Test: this test itself.
#[test]
fn status_lines_render_banners() {
    let syn = Synthesis {
        notes: vec!["synthesis: rejected (unverified figure) in top-risk row 1: 42".to_string()],
        ..Default::default()
    };
    let lines = syn.status_lines();
    assert_eq!(lines[0], "synthesis: available");
    assert_eq!(lines[1], syn.notes[0]);
}
