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
use crate::report::synthesize_prompt::{
    build_synthesis_prompt, schema_contract_statement, synthesis_schema,
};

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

/// Why: #6009 shape 3 — the prompt-text contract must be derived FROM the
/// schema, not hand-typed beside it, or the two can drift exactly the way
/// the three live shapes drifted from the schema itself.
/// What: asserts the statement built from `synthesis_schema(5)` names every
/// canonical top-level field and every `top_risks`/`findings` item field.
/// Test: this test itself.
#[test]
fn schema_contract_statement_lists_every_field_name() {
    let schema = synthesis_schema(5);
    let contract = schema_contract_statement(&schema.schema);
    for needle in [
        "executive_summary",
        "top_risks",
        "findings",
        "description",
        "severity",
        "cost",
        "apps",
        "app_slug",
        "title",
        "evidence",
        "component",
        "business_impact",
        "remediation",
        "cost_effort",
    ] {
        assert!(
            contract.contains(needle),
            "contract statement must name field {needle:?}: {contract}"
        );
    }
}

/// Why: the derived contract text is worthless unless it actually reaches
/// the system prompt the provider receives.
/// What: asserts `req.system` contains the "Required JSON object shape"
/// section and the canonical field names it lists.
/// Test: this test itself.
#[test]
fn schema_contract_statement_reaches_system_prompt() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(&model, "stub/model", false);
    assert!(req.system.contains("Required JSON object shape"));
    assert!(req.system.contains("executive_summary"));
    assert!(req.system.contains("top_risks"));
    assert!(req.system.contains("findings"));
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

/// Why: an audit is only as complete as the set of targets the operator
/// registered, and the report is the only place that gap becomes visible.  The
/// instruction must be static — a template that overrides `executive_summary`
/// must not be able to drop it — and it must name schema/migration
/// repositories, the kind operators forget most often.
/// What: asserts the coverage-gaps block reaches `req.system` with the
/// register-and-re-run recommendation, the schema-repository call-out, and the
/// no-speculation constraint; asserts it survives an `executive_summary`
/// override; and asserts the digest emits the "Applications assessed" heading
/// the instruction tells the model to compare references against.
/// Test: this test itself.
#[test]
fn system_prompt_asks_for_unregistered_target_gaps() {
    let mut model = fixture_model(vec![]);
    model.section_instructions.insert(
        "executive_summary".to_string(),
        "OVERRIDE: lead with the TQI posture.".to_string(),
    );
    let req = build_synthesis_prompt(&model, "stub/model", false);
    for needle in [
        "Coverage gaps in the assessed set",
        "coverage-gaps note",
        "register the named targets and re-run",
        "schema and migration repositories",
        "Never list one you infer probably exists",
    ] {
        assert!(
            req.system.contains(needle),
            "coverage-gaps instruction must survive a template override and contain {needle:?}: {}",
            req.system
        );
    }
    assert!(
        req.messages[0].content.contains("Applications assessed"),
        "the digest must name the assessed set the instruction compares against: {}",
        req.messages[0].content
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

// ── #6009: markdown-heading fallback + raw-capture regression ──────────────

/// A faithful reconstruction of the live 4/4 repro captured against
/// `anthropic/claude-opus-4.8` (#6009): `strict:true` `response_format` was
/// silently ignored (200 OK, `finish_reason: "stop"`) and the model wrote the
/// system prompt's own `## Output` field labels back as markdown headings
/// around free prose instead of the requested JSON object. The figures here
/// (8,200 / 120 / 640) are the fixture model's allowed set, so the recovered
/// text exercises the numeric guardrail rather than bypassing it.
fn markdown_fallback_response() -> String {
    "## Executive Summary\n\n\
     The 8,200 LoC Rust codebase spans 120 files and 640 functions with one \
     critical security gap that must be remediated before close.\n\n\
     ## Top Risks\n\n\
     No RED or AMBER risks were identified in the provided data.\n\n\
     ## Findings\n\n\
     No findings require elaboration.\n"
        .to_string()
}

/// Why: #6009 — before this fix, `parse_raw` had no fallback for a schema the
/// provider ignored entirely, so this exact live shape hard-failed the whole
/// due-diligence run as `Unparseable` with a real finding count sitting
/// unused behind it. RED against pre-fix `parse_raw` (no markdown fallback),
/// GREEN after.
/// What: recovers `executive_summary` from the markdown-headed response;
/// `top_risks`/`findings` stay empty (never reconstructed from prose).
/// Test: this test itself.
#[tokio::test]
async fn synthesize_recovers_executive_summary_from_markdown_fallback() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: markdown_fallback_response(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("the markdown fallback must recover a usable executive summary");

    assert_eq!(
        result.executive_summary.as_deref(),
        Some(
            "The 8,200 LoC Rust codebase spans 120 files and 640 functions with one \
             critical security gap that must be remediated before close."
        )
    );
    assert!(
        result.top_risks.is_empty(),
        "prose is never reconstructed into risk rows"
    );
    assert!(
        result.findings.is_empty(),
        "prose is never reconstructed into findings"
    );
    assert!(
        result.notes.is_empty(),
        "a clean recovered summary records no notes"
    );
}

/// Why: the fallback must not turn every unparseable response into a silent
/// pass — a genuinely different shape (no heading at all, e.g. a refusal or
/// an error string) must still fail closed exactly like today.
/// What: a response with no `## Executive Summary` heading; asserts
/// `Err(SynthesisError::Unparseable)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_markdown_fallback_rejects_response_with_no_heading() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: "I'm unable to provide a structured response for this request.".to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a headingless response must not be silently accepted");
    assert!(
        matches!(err, SynthesisError::Unparseable),
        "expected Unparseable, got {err:?}"
    );
}

/// Why: recovering the markdown fallback text must not create a second path
/// around the numeric guardrail — a fabricated figure in the recovered
/// executive summary must be rejected exactly like a fabricated figure in a
/// normal JSON response.
/// What: the heading is present, but the body cites `999` — absent from the
/// fixture model's allowed set — so nothing survives and the whole pass fails
/// as `NoVerifiableContent`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_markdown_fallback_still_applies_numeric_guardrail() {
    let model = fixture_model(vec![red("x")]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: "## Executive Summary\n\n999 critical vulnerabilities were found.\n".to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a fabricated figure recovered via the fallback must still be rejected");
    assert!(
        matches!(err, SynthesisError::NoVerifiableContent),
        "expected NoVerifiableContent, got {err:?}"
    );
}

/// Why: #6009 — the only prior evidence of an unparseable response was a WARN
/// log line with no body; a live repro had to spend a fresh OpenRouter call to
/// see what the model actually sent. This proves the capture wiring writes
/// that evidence to disk instead.
/// What: configures a capture directory, sends a response with no recoverable
/// heading, and asserts both the hard failure AND the persisted file.
/// Test: this test itself.
#[tokio::test]
async fn capture_dir_persists_raw_response_on_parse_failure() {
    let model = fixture_model(vec![red("x")]);
    let body = "totally free text with no JSON and no recognised heading".to_string();
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: body.clone(),
        finish_reason: Some("stop".to_string()),
    });
    let dir = tempfile::tempdir().expect("tempdir");
    let err = Synthesizer::new(llm, "stub/model")
        .with_raw_capture_dir(dir.path())
        .synthesize(&model)
        .await
        .expect_err("still unparseable — capture must not change the verdict");
    assert!(matches!(err, SynthesisError::Unparseable));

    let captured = std::fs::read_to_string(dir.path().join(super::UNPARSEABLE_CAPTURE_FILENAME))
        .expect("the raw response must be persisted next to the report output");
    assert_eq!(
        captured, body,
        "the captured file must hold the verbatim (scrubbed) response"
    );
}

// ── #6009 shape 2: drifted `top_risks` field names ──────────────────────────

/// Verbatim (structure and field names unchanged) reconstruction of the live
/// capture at `synthesis-unparseable-response.txt` (#6009 shape 2): valid
/// top-level JSON, but every `top_risks` item uses `risk` instead of
/// `description` and `applications` instead of `apps`, and omits
/// `severity`/`cost` entirely.
fn shape2_capture_response() -> String {
    r#"{
  "executive_summary": "Due diligence of 00-bobmatnyc-trusty-tools surfaced 27 AMBER findings and no RED findings.",
  "top_risks": [
    {
      "risk": "Plaintext secrets at rest across credential, OAuth, and MCP config stores, with world-readable fallback on non-unix targets; broad remediation across multiple modules to move to encryption/keyring-backed storage.",
      "applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Unauthenticated API on the default loopback bind reachable by any local process including browser pages hitting 127.0.0.1; moderate effort to enforce auth by default.",
      "applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Non-atomic memory writes — vector mutation and metadata commit as separate transactions with no rollback, and non-atomic move_segment that can duplicate records — risking silent data corruption; significant effort to introduce transactional guarantees.",
      "applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Pervasive silent error suppression in credential reads, config parsing, and memory recall collapsing failures to None/defaults/empty results; moderate effort to surface and log failure paths.",
      "applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Scalability constraints in the memory subsystem — per-session redb+usearch files accumulating unbounded handles, single mutex serializing all segment writes, blocking flock in async context, and uncached config re-reads per prompt; moderate refactoring effort under load.",
      "applications": "00-bobmatnyc-trusty-tools"
    }
  ],
  "findings": []
}"#
    .to_string()
}

/// Why: #6009 shape 2 — before the `RiskRow` `#[serde(alias)]`/`#[serde(default)]`
/// fix, this exact live capture failed `serde_json::from_str::<RawSynthesis>`
/// because every `top_risks` item used `risk`/`applications` instead of
/// `description`/`apps` and omitted `severity`/`cost` entirely, so the whole
/// response was classified `Unparseable` even though it was valid top-level
/// JSON with real, verifiable risk content. RED against pre-fix `RiskRow`
/// (missing field errors on `description`/`severity`/`cost`/`apps`), GREEN
/// after.
/// What: parses the verbatim capture and asserts all 5 rows recover their
/// drifted-name fields (`description` from `risk`, `apps` from
/// `applications`) while `severity`/`cost` default to `""` — never a
/// fabricated band or figure.
/// Test: this test itself.
#[test]
fn parse_raw_recovers_shape2_field_name_drift() {
    let (raw, notes) =
        super::parse_raw(&shape2_capture_response()).expect("shape-2 field drift must still parse");
    assert!(
        notes.is_empty(),
        "recognised shape-2 synonyms record no drop notes: {notes:?}"
    );
    assert_eq!(raw.top_risks.len(), 5, "all 5 captured rows must survive");
    assert_eq!(
        raw.top_risks[0].description,
        "Plaintext secrets at rest across credential, OAuth, and MCP config stores, with world-readable fallback on non-unix targets; broad remediation across multiple modules to move to encryption/keyring-backed storage.",
        "`risk` must alias onto `description`"
    );
    assert_eq!(
        raw.top_risks[0].apps, "00-bobmatnyc-trusty-tools",
        "`applications` must alias onto `apps`"
    );
    for (i, row) in raw.top_risks.iter().enumerate() {
        assert_eq!(
            row.severity, "",
            "row {i}: an omitted severity must default to empty, never be fabricated"
        );
        assert_eq!(
            row.cost, "",
            "row {i}: an omitted cost must default to empty, never be fabricated"
        );
    }
}

// ── #6009 shape 3: a THIRD field-name drift on the same forced schema ──────

/// Verbatim (structure and field names unchanged) reconstruction of the third
/// consecutive live capture at `synthesis-unparseable-response.txt` (#6009
/// shape 3): valid top-level JSON, but every `top_risks` item uses `risk`
/// instead of `description`, `cost_effort_framing` instead of `cost`, and
/// `affected_applications` instead of `apps` — a THIRD distinct set of
/// drifted names from the SAME provider/model, proving per-shape
/// `#[serde(alias)]` additions can never converge and motivating the
/// whitelist-synonym-table class fix in
/// `crate::report::synthesize_normalize`.
fn shape3_capture_response() -> String {
    r#"{
  "executive_summary": "This diligence pass on 00-bobmatnyc-trusty-tools surfaced 32 findings, all AMBER, concentrated in authentication & secrets, error handling, state management, and scalability.",
  "top_risks": [
    {
      "risk": "API keys and OAuth client secrets persisted as plaintext files, with hardening that is Unix-only and unverified/absent on Windows.",
      "cost_effort_framing": "Moderate remediation: introduce OS keyring/secret-manager integration and enforce cross-platform permission or encryption controls before any Windows deployment.",
      "affected_applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Pervasive silent-failure error handling — corrupt or malformed config/credential files degrade to defaults or None, poisoned locks recover silently, and home resolution falls back to CWD.",
      "cost_effort_framing": "Moderate, dispersed effort: convert silent fallbacks to surfaced errors across multiple modules and add operator-visible diagnostics.",
      "affected_applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Non-atomic persistence in the memory store — cross-segment moves and payload/vector index writes commit in separate steps, risking duplicated or orphaned records on crash.",
      "cost_effort_framing": "Higher effort: requires transactional or reconciliation design across redb and usearch to guarantee cross-store consistency.",
      "affected_applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Agent tool-capability surface defaults to unrestricted, granting all registered tools including shell-capable ones when a config omits an allowlist; combined with denylist-style traversal checks and an auth-exempt SSE route.",
      "cost_effort_framing": "Moderate: flip defaults to deny-by-default, adopt allowlist validation, and confirm downstream SSE auth gating.",
      "affected_applications": "00-bobmatnyc-trusty-tools"
    },
    {
      "risk": "Persistence layer is single-node demo-scale — full-file JSONL re-reads with no fsync/checksum, cross-process locks that cannot serialize the redb writer, blocking flock on the async runtime, and a per-segment write mutex.",
      "cost_effort_framing": "Higher effort: durability and concurrency rework needed before multi-node or production-scale operation.",
      "affected_applications": "00-bobmatnyc-trusty-tools"
    }
  ],
  "findings": []
}"#
    .to_string()
}

/// Why: #6009 shape 3 — this is the THIRD distinct field-name shape the same
/// live model produced across three consecutive calls, and it is neither of
/// the two `#[serde(alias)]`s the previous round of this fix added. A
/// per-shape alias can never converge; the whitelist synonym table in
/// `synthesize_normalize` recognises `cost_effort_framing`/
/// `affected_applications` alongside the shape-2 `risk`/`applications`
/// names. RED against `f6102c50a` (pre-fix `RiskRow` has no
/// `cost_effort_framing`/`affected_applications` alias, so this capture fails
/// `serde_json::from_str::<RawSynthesis>` and the whole response is
/// classified `Unparseable`), GREEN after.
/// What: parses the verbatim shape-3 capture; asserts all 5 rows recover
/// `description` (from `risk`), `cost` (from `cost_effort_framing`), and
/// `apps` (from `affected_applications`), with `severity` defaulted to `""`
/// (omitted in this shape, same as shape 2) — never fabricated.
/// Test: this test itself.
#[test]
fn parse_raw_recovers_shape3_field_name_drift() {
    let (raw, notes) =
        super::parse_raw(&shape3_capture_response()).expect("shape-3 field drift must still parse");
    assert!(
        notes.is_empty(),
        "recognised shape-3 synonyms record no drop notes: {notes:?}"
    );
    assert_eq!(raw.top_risks.len(), 5, "all 5 captured rows must survive");
    assert_eq!(
        raw.top_risks[0].description,
        "API keys and OAuth client secrets persisted as plaintext files, with hardening that is Unix-only and unverified/absent on Windows.",
        "`risk` must normalize onto `description`"
    );
    assert_eq!(
        raw.top_risks[0].cost,
        "Moderate remediation: introduce OS keyring/secret-manager integration and enforce cross-platform permission or encryption controls before any Windows deployment.",
        "`cost_effort_framing` must normalize onto `cost`"
    );
    assert_eq!(
        raw.top_risks[0].apps, "00-bobmatnyc-trusty-tools",
        "`affected_applications` must normalize onto `apps`"
    );
    for (i, row) in raw.top_risks.iter().enumerate() {
        assert_eq!(
            row.severity, "",
            "row {i}: an omitted severity must default to empty, never be fabricated"
        );
    }
}

/// Why: the shape-3 fixture must also synthesize end to end — not merely
/// parse — so the numeric guardrail and reporter path are exercised exactly
/// as they would be against the live response.  The captured executive
/// summary cites "32 findings", a figure absent from the fixture model's
/// allowed set (8200/120/640), so the guardrail correctly rejects THAT field
/// on its own numeric-fabrication grounds — this test is not asserting
/// normalization bypasses the guardrail, only that the 5 top-risk rows (whose
/// prose carries no digits, aside from the "00" embedded in the repo name
/// itself — matched here by naming the fixture repo
/// "00-bobmatnyc-trusty-tools", exactly as the live capture named it, so that
/// digit sequence is a legitimately allowed figure) recover and survive
/// independently.
/// What: runs the full `Synthesizer::synthesize` against the shape-3
/// capture; asserts the executive summary is rejected with a note citing
/// "32", while all 5 top-risk rows survive intact.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_recovers_shape3_end_to_end() {
    let mut model = fixture_model(vec![red("x")]);
    model.repositories[0].name = "00-bobmatnyc-trusty-tools".to_string();
    model.repositories[0].slug = "00-bobmatnyc-trusty-tools".to_string();
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: shape3_capture_response(),
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("the shape-3 capture must synthesize end to end (top-risk rows survive)");
    assert!(
        result.executive_summary.is_none(),
        "the captured summary cites an unverifiable figure (32) and must be rejected"
    );
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("rejected (unverified figure)") && n.contains("32")),
        "the rejection must be recorded: {:?}",
        result.notes
    );
    assert_eq!(result.top_risks.len(), 5, "all 5 rows must survive intact");
}

/// Why: normalization must never turn a genuinely wrong response shape into
/// an accepted one — the contract stays strict. A response using entirely
/// different top-level field names (no `executive_summary` key at all) must
/// still fail, exactly as it did before this change, just via
/// `NoVerifiableContent` instead of a parse error (the JSON itself is
/// syntactically valid; every key is simply unrecognized and dropped).
/// What: a response with `summary`/`risks` instead of
/// `executive_summary`/`top_risks`; asserts
/// `Err(SynthesisError::NoVerifiableContent)`.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_rejects_a_wholly_unrecognized_shape() {
    let model = fixture_model(vec![red("x")]);
    let body = r#"{
      "summary": "The codebase looks fine overall.",
      "risks": [{"issue": "something", "severity_band": "RED"}]
    }"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body: body.to_string(),
        finish_reason: Some("stop".to_string()),
    });
    let err = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect_err("a wholly unrecognized shape must never be accepted");
    assert!(
        matches!(err, SynthesisError::NoVerifiableContent),
        "expected NoVerifiableContent, got {err:?}"
    );
}
