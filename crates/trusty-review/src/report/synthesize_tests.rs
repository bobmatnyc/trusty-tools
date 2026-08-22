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
use crate::report::synthesize_guard::{
    allowed_numbers, allowed_numbers_with, numbers_in, verify_prose,
};
use crate::report::synthesize_prompt::{
    SYNTHESIS_DEFAULT_MAX_TOKENS, SYNTHESIS_ESCALATED_MAX_TOKENS, SYNTHESIS_MIN_MAX_TOKENS,
    SynthesisTier, build_synthesis_prompt, schema_contract_statement, synthesis_schema,
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
        inference: None,
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
            analyze_gap: None,
            authorship: None,
            inspect_priority: Vec::new(),
            crate_topology: None,
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

/// Why: #6030 — the guardrail compares canonical strings, so a measured 85.19
/// written as "85%" looked fabricated and the whole narrative was dropped. The
/// allowed set must therefore carry the coarser spellings of every measured
/// figure.
/// What: asserts both the truncation and the round-half-up form at each coarser
/// precision are admitted (including a carry, 9.99 → 10), and that neighbouring
/// values that are no rounding of the source are still absent.
/// Test: this test itself.
#[test]
fn allowed_numbers_admits_rounded_forms() {
    let allowed = allowed_numbers(&serde_json::json!({
        "top_author_share_pct": 85.19,
        "score": 9.99,
    }));
    // The set holds canonical keys, so 10.0 is present under the key "10".
    for form in ["85.19", "85.1", "85.2", "85", "9.99", "9.9", "10"] {
        assert!(allowed.contains(form), "{form} is a rounding of the source");
    }
    for absent in ["85.3", "84", "86", "8.9", "11"] {
        assert!(
            !allowed.contains(absent),
            "{absent} is no rounding of any source figure"
        );
    }
}

/// Why: #6030 regression — live verification of #6037 failed because the
/// authorship narrative said "85%" for a measured `top_author_share_pct` of
/// 85.19 and the guardrail reported "rejected (unverified figure): 85".
/// What: the rounded spellings verify; a figure that is no rounding of any
/// measured value is still rejected with the offending token surfaced.
/// Test: this test itself.
#[test]
fn verify_prose_accepts_rounding() {
    let allowed = allowed_numbers(&serde_json::json!({ "top_author_share_pct": 85.19 }));
    for prose in [
        "The top author owns 85% of the code.",
        "The top author owns 85.2% of the code.",
        "The top author owns 85.1% of the code.",
        "The top author owns 85.19% of the code.",
    ] {
        assert!(
            verify_prose(prose, &allowed).is_ok(),
            "a conventional rounding must verify: {prose}"
        );
    }
    assert_eq!(
        verify_prose("The top author owns 84% of the code.", &allowed),
        Err("84".to_string())
    );
    assert_eq!(
        verify_prose("The top author owns 85.3% of the code.", &allowed),
        Err("85.3".to_string())
    );
}

/// Why: #6137 — `rounded_forms` widens decimal precision only, so an integer
/// key admitted nothing coarser than itself and "1.55 million lines" for a
/// measured 1,553,771 was read as a fabricated 1.55. That rejection vetoed five
/// LLM-written fields of one report.
/// What: the million- and thousand-scale restatements are admitted; a
/// single-significant-digit form and a genuinely different figure are not.
/// Test: this test itself.
#[test]
fn allowed_numbers_admits_scaled_unit_forms() {
    let allowed = allowed_numbers(&serde_json::json!({ "total_loc": 1553771 }));
    for form in ["1553771", "1.55", "1.6", "1.553771", "1553", "1554"] {
        assert!(
            allowed.contains(form),
            "{form} is a scaled restatement of 1,553,771"
        );
    }
    for absent in ["2", "1.7", "1.4", "9999"] {
        assert!(
            !allowed.contains(absent),
            "{absent} is no scaled restatement of 1,553,771"
        );
    }
}

/// Why: #6137 regression — the exact live rejection. The report's Key Facts row
/// states 1553771 lines and the model wrote "1.55 million lines"; the status
/// note read `rejected (unverified figure) in executive summary: 1.55`.
/// What: the million-scale prose verifies, and a fabricated figure of the same
/// shape still does not.
/// Test: this test itself.
#[test]
fn verify_prose_accepts_a_million_scale_restatement() {
    let allowed = allowed_numbers(&serde_json::json!({ "total_loc": 1553771 }));
    for prose in [
        "The codebase is roughly 1.55 million lines.",
        "The codebase is roughly 1.6 million lines.",
        "1,553,771 lines across the workspace.",
    ] {
        assert!(
            verify_prose(prose, &allowed).is_ok(),
            "a scaled restatement must verify: {prose}"
        );
    }
}

/// Why: widening the allowed set must not open a hole — a figure that restates
/// nothing measured is still the thing the guardrail exists to catch.
/// What: a fabricated million-scale figure and a fabricated small integer are
/// both rejected, with the offending token surfaced.
/// Test: this test itself.
#[test]
fn verify_prose_still_rejects_a_fabricated_figure() {
    let allowed = allowed_numbers(&serde_json::json!({ "total_loc": 1553771 }));
    assert_eq!(
        verify_prose("The codebase is roughly 2.4 million lines.", &allowed),
        Err("2.4".to_string())
    );
    assert_eq!(
        verify_prose("There are 42 services.", &allowed),
        Err("42".to_string())
    );
}

/// Why: #6137 — the investigation coverage percentage is computed at render
/// time, printed in the report's own Investigation Coverage section, and quoted
/// verbatim into the synthesis prompt. The model was being asked to cite a
/// figure the guardrail then rejected.
/// What: a figure supplied as printed report text is admitted; one that is
/// neither in the model nor printed is not.
/// Test: this test itself.
#[test]
fn allowed_numbers_admits_a_printed_derived_figure() {
    let printed = vec!["- files examined: 73 of 6664 tracked (1.1% coverage)".to_string()];
    let allowed = allowed_numbers_with(&serde_json::json!({ "files": 73 }), &printed);
    assert!(allowed.contains("1.1"), "a printed figure is in-model");
    assert!(verify_prose("Coverage was 1.1%.", &allowed).is_ok());
    assert_eq!(
        verify_prose("Coverage was 9.7%.", &allowed),
        Err("9.7".to_string())
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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

// ── #6180: custom auditor instructions in the prompt ────────────────────────

/// Build the same fixture with an `instructions.md` already loaded onto it, as
/// `cli_report::load_report_instructions` leaves the model after discovery.
fn fixture_model_with_instructions(text: &str) -> ReportModel {
    let mut model = fixture_model(vec![red("SQL injection risk")]);
    model.instructions = Some(text.to_string());
    model.instructions_source = Some("engagement/instructions.md".to_string());
    model
}

/// Why: #6180's whole point is that the file reaches the auditor. A discovered
/// `instructions.md` that loads but never lands in the prompt would satisfy every
/// other test and deliver nothing.
/// What: asserts the verbatim text and the additive-overlay heading appear in the
/// synthesis user message.
/// Test: this test itself.
#[test]
fn discovered_instructions_reach_the_synthesis_prompt() {
    let model =
        fixture_model_with_instructions("Weigh secrets handling above every other dimension.");
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let digest = &req.messages[0].content;
    assert!(
        digest.contains("Weigh secrets handling above every other dimension."),
        "instructions must reach the prompt verbatim: {digest}"
    );
    assert!(
        digest.contains("Analyst focus directives"),
        "instructions must arrive under the additive-overlay heading: {digest}"
    );
}

/// Why: "missing instructions.md = zero behavior change" is only a claim until
/// the two prompts are compared byte for byte.
/// What: builds the prompt from the same fixture with and without instructions
/// and asserts every request field is identical except the one added block.
/// Test: this test itself.
#[test]
fn no_instructions_leaves_the_prompt_byte_identical() {
    let bare = fixture_model(vec![red("SQL injection risk")]);
    let with = fixture_model_with_instructions("Weigh secrets handling above all.");

    let a = build_synthesis_prompt(
        &bare,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let b = build_synthesis_prompt(
        &with,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );

    // The system prompt, model, and budget are untouched by instructions — only
    // the user digest gains a block.
    assert_eq!(a.system, b.system);
    assert_eq!(a.model, b.model);
    assert_eq!(a.max_tokens, b.max_tokens);
    assert_ne!(a.messages[0].content, b.messages[0].content);
    assert!(
        !a.messages[0].content.contains("Analyst focus directives"),
        "a model with no instructions must carry no directives block"
    );

    // And the bare prompt is exactly the with-instructions prompt minus the
    // appended block — nothing earlier in the assembly shifted.
    let extra = b.messages[0]
        .content
        .strip_prefix(
            a.messages[0]
                .content
                .strip_suffix(TRAILING_SYNTHESIS_DIRECTIVE)
                .expect("bare digest ends with the closing directive"),
        )
        .expect("the with-instructions digest extends the bare one");
    assert!(
        extra.starts_with("\n## Analyst focus directives"),
        "{extra}"
    );
}

/// The closing line `build_digest` appends after the optional directives block;
/// everything before it must be identical with and without instructions.
const TRAILING_SYNTHESIS_DIRECTIVE: &str = "\nSynthesise the narrative sections from the data above, obeying every rule in the system prompt. Where the analyst focus directives above are relevant, weight the executive summary and top risks toward them — but only using figures and findings actually present in the data.\n";

/// Why: a routing prefix must be stripped so the bare id reaches the provider.
/// What: asserts `bedrock/` and `openrouter/` are stripped from `req.model`.
/// Test: this test itself.
#[test]
fn prompt_strips_prefix() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(
        &model,
        "bedrock/us.anthropic.claude-sonnet-4-6",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    assert_eq!(req.model, "us.anthropic.claude-sonnet-4-6");
    let req2 = build_synthesis_prompt(
        &model,
        "openrouter/openai/gpt-5.4-mini",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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
    let schema = synthesis_schema(5, 10);
    assert_eq!(schema.name, "report_synthesis");
    let props = &schema.schema["properties"];
    assert!(props["executive_summary"].is_object());
    // #6004: the two additional narrative slots must be forced via the same
    // schema value as executive_summary.
    assert!(props["code_quality_summary"].is_object());
    assert!(props["security_summary"].is_object());
    // #5453: same for the authorship narrative slot.
    assert!(props["authorship_summary"].is_object());
    assert!(props["top_risks"].is_object());
    assert!(props["findings"].is_object());
    assert_eq!(props["top_risks"]["maxItems"], 5);
    assert_eq!(props["findings"]["maxItems"], 10);
    let req = build_synthesis_prompt(
        &fixture_model(vec![]),
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    assert!(
        req.response_schema.is_some(),
        "every synthesis request must force structured output"
    );
}

/// Why: #6009 shape 3 — the prompt-text contract must be derived FROM the
/// schema, not hand-typed beside it, or the two can drift exactly the way
/// the three live shapes drifted from the schema itself.
/// What: asserts the statement built from `synthesis_schema(5, 10)` names every
/// canonical top-level field and every `top_risks`/`findings` item field.
/// Test: this test itself.
#[test]
fn schema_contract_statement_lists_every_field_name() {
    let schema = synthesis_schema(5, 10);
    let contract = schema_contract_statement(&schema.schema);
    for needle in [
        "executive_summary",
        // #6004: the contract statement is derived from the same schema
        // value, so these must be named alongside executive_summary.
        "code_quality_summary",
        "security_summary",
        // #5453: same for the authorship narrative slot.
        "authorship_summary",
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    assert!(req.system.contains("Required JSON object shape"));
    assert!(req.system.contains("executive_summary"));
    // #6004: the same derivation must carry the new narrative slots into the
    // system prompt text, not just `response_format`.
    assert!(req.system.contains("code_quality_summary"));
    assert!(req.system.contains("security_summary"));
    // #5453: same for the authorship narrative slot.
    assert!(req.system.contains("authorship_summary"));
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Concise,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
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

/// Why (#5453/#6004): the `authorship_summary` slot asks the model for a
/// key-person narrative, and the ONLY route real ownership figures take into
/// the prompt is the per-repository digest block. Every other fixture leaves
/// `authorship: None`, so nothing proved the wiring — a regression that dropped
/// it would leave the model writing that section from no data at all, which is
/// exactly the ungrounded prose the guardrail exists to reject.
/// What: attaches a loaded artifact to the fixture repository and asserts the
/// digest carries the ownership figures and the trajectory line; then asserts a
/// repository WITHOUT an artifact contributes neither, so the absence is a real
/// absence rather than a zeroed row.
/// Test: this test itself.
#[test]
fn authorship_figures_reach_the_synthesis_prompt() {
    use crate::report::authorship::{AuthorshipSummary, MonthlyActivity};

    let mut model = fixture_model(vec![]);
    model.repositories[0].authorship = Some(AuthorshipSummary {
        schema_version: "v0".to_string(),
        repository: "acme-core".to_string(),
        distinct_authors: 4,
        bus_factor: 1,
        top_author_share_pct: 71.0,
        single_author_subsystems: vec!["migrations".to_string()],
        monthly_trajectory: vec![
            MonthlyActivity {
                month: "2026-01".to_string(),
                active_authors: 2,
                commits: 4,
            },
            MonthlyActivity {
                month: "2026-02".to_string(),
                active_authors: 3,
                commits: 20,
            },
        ],
        unresolved_authors: 0,
        caveats: vec![],
    });

    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let digest = &req.messages[0].content;
    assert!(
        digest.contains("Authorship:"),
        "the ownership figures must reach the prompt: {digest}"
    );
    for needle in ["4 distinct author(s)", "bus factor 1", "migrations"] {
        assert!(
            digest.contains(needle),
            "the digest must carry {needle:?}: {digest}"
        );
    }
    assert!(
        digest.contains("Authorship trajectory: increasing"),
        "the trailing-window trend must reach the prompt too: {digest}"
    );

    let without = build_synthesis_prompt(
        &fixture_model(vec![]),
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    assert!(
        !without.messages[0].content.contains("Authorship:"),
        "a repository with no artifact must contribute no authorship line at all: {}",
        without.messages[0].content
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

/// Why: regression for the rebase seam between #6009/#6014's whitelist
/// normalizer and the three narrative slots #6004/#5453 added on top of it —
/// a normalizer whitelist keyed only off the pre-#6004 field set silently
/// drops `code_quality_summary`/`security_summary`/`authorship_summary` as
/// "unrecognized" even though both the schema and `RawSynthesis` declare
/// them.
/// What: a full-shape response carrying all three new fields, grounded in
/// figures present in the fixture model; asserts each survives parse →
/// normalize → guardrail into `Synthesis`, and that no drop note was
/// recorded for any of them.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_happy_path_injects_new_narrative_fields() {
    let model = fixture_model(vec![red("SQL injection risk")]);
    let body = r#"{
      "executive_summary": "The 8,200 LoC Rust codebase spans 120 files and 640 functions with one critical security gap.",
      "code_quality_summary": "The 8,200 LoC codebase spans 120 files, a moderate footprint.",
      "security_summary": "640 functions were assessed; one RED finding stands out.",
      "authorship_summary": "120 files were assessed for authorship concentration.",
      "top_risks": [],
      "findings": []
    }"#
    .to_string();
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body,
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("a clean response synthesizes");

    assert_eq!(
        result.code_quality_summary.as_deref(),
        Some("The 8,200 LoC codebase spans 120 files, a moderate footprint.")
    );
    assert_eq!(
        result.security_summary.as_deref(),
        Some("640 functions were assessed; one RED finding stands out.")
    );
    assert_eq!(
        result.authorship_summary.as_deref(),
        Some("120 files were assessed for authorship concentration.")
    );
    assert!(
        result.notes.is_empty(),
        "none of the three new fields is unrecognized or unverified: {:?}",
        result.notes
    );
}

/// Build the fixture model with an authorship artifact whose top-author share is
/// `pct` — the shape behind #6030's live failure.
fn fixture_model_with_top_author_share(pct: f64) -> ReportModel {
    use crate::report::authorship::AuthorshipSummary;

    let mut model = fixture_model(vec![]);
    model.repositories[0].authorship = Some(AuthorshipSummary {
        schema_version: "v0".to_string(),
        repository: "acme-core".to_string(),
        distinct_authors: 4,
        bus_factor: 1,
        top_author_share_pct: pct,
        single_author_subsystems: vec!["migrations".to_string()],
        monthly_trajectory: vec![],
        unresolved_authors: 0,
        caveats: vec![],
    });
    model
}

/// Why: #6030 end-to-end regression — the run that failed live verification of
/// #6037 rendered "rejected (unverified figure) in authorship summary: 85"
/// against a measured `top_author_share_pct` of 85.19, so the narrative was
/// dropped for restating a real figure at report precision.
/// What: drives a full synthesize with an authorship narrative that writes 85.19
/// as "85%"; asserts the field survives and no rejection note was recorded.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_accepts_rounded_authorship_figure() {
    let model = fixture_model_with_top_author_share(85.19);
    let body = r#"{
      "executive_summary": "The 8,200 LoC Rust codebase spans 120 files.",
      "authorship_summary": "One author owns 85% of the code, leaving a bus factor of 1.",
      "top_risks": [],
      "findings": []
    }"#
    .to_string();
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body,
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("a rounded but faithful figure synthesizes");

    assert_eq!(
        result.authorship_summary.as_deref(),
        Some("One author owns 85% of the code, leaving a bus factor of 1.")
    );
    assert!(
        result.notes.is_empty(),
        "a rounding of a measured figure records no rejection note: {:?}",
        result.notes
    );
}

/// Why: widening the guardrail for roundings must not widen it for wrong
/// figures — 84 is neither the truncation nor the round-half-up form of 85.19.
/// What: the same shape as the accepting case with the share misstated; asserts
/// the field is dropped and the rejection note names the offending token.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_still_rejects_a_wrong_authorship_figure() {
    let model = fixture_model_with_top_author_share(85.19);
    let body = r#"{
      "executive_summary": "The 8,200 LoC Rust codebase spans 120 files.",
      "authorship_summary": "One author owns 84% of the code.",
      "top_risks": [],
      "findings": []
    }"#
    .to_string();
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm {
        body,
        finish_reason: Some("stop".to_string()),
    });
    let result = Synthesizer::new(llm, "stub/model")
        .synthesize(&model)
        .await
        .expect("the executive summary still verifies, so the pass succeeds");

    assert!(
        result.authorship_summary.is_none(),
        "a figure matching no rounding must still drop the field"
    );
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("authorship summary: 84")),
        "the rejection note must name the offending token: {:?}",
        result.notes
    );
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

/// Why: the retry ladder is a bounded escalation, never an open-ended loop.
/// #5454 decides what happens when it is spent — an error, not a degraded
/// report. #6093 changed the bound from two calls to three (full → concise →
/// narrative-only), so a provider that truncates unconditionally now costs
/// three calls and no more.
/// What: the queued stub truncates on every call; asserts
/// `Err(SynthesisError::Truncated)` and exactly 3 calls.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_still_truncated_after_retry_is_a_hard_error() {
    let model = fixture_model(vec![red("x")]);
    let llm = Arc::new(QueuedLlm::new(vec![
        ("{}", Some("length")),
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
        3,
        "the ladder must stop after its three rungs"
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

/// (c) #6004: `code_quality_summary` is rejected exactly like
/// `executive_summary` when it cites a figure absent from the model —
/// constructed by calling the guardrail directly (`apply_guardrail`), not by
/// routing a fabricated figure through a stub LLM whose trigger condition is
/// the same as `synthesize_rejects_unverified_figure` already covers.
/// What: one clean top-risk row (so the overall pass still succeeds) plus a
/// `code_quality_summary` citing 9999; asserts the field is dropped, a
/// rejection note names it, and the clean row survives untouched.
/// Test: this test itself.
#[test]
fn code_quality_summary_guardrail_rejects_unverified_figure() {
    let allowed: std::collections::HashSet<String> = ["moderate".to_string(), "Acme".to_string()]
        .into_iter()
        .collect();
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: "Complexity sits at 9999 on average.".to_string(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: vec![super::RiskRow {
            description: "moderate".to_string(),
            severity: "AMBER".to_string(),
            cost: "moderate".to_string(),
            apps: "Acme".to_string(),
        }],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &Default::default())
        .expect("the clean top-risk row keeps this Ok");

    assert!(
        result.code_quality_summary.is_none(),
        "a code_quality_summary citing 9999 must be rejected"
    );
    assert_eq!(result.top_risks.len(), 1, "the clean row must survive");
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("rejected (unverified figure)") && n.contains("code quality")),
        "a guardrail rejection note must name the code quality summary: {:?}",
        result.notes
    );
}

/// (c) #6004: `security_summary` is rejected exactly like
/// `code_quality_summary` when it cites a figure absent from the model — same
/// construction as `code_quality_summary_guardrail_rejects_unverified_figure`,
/// mirrored onto the sibling field.
/// What: one clean top-risk row (so the overall pass still succeeds) plus a
/// `security_summary` citing 9999; asserts the field is dropped, a rejection
/// note names it, and the clean row survives untouched.
/// Test: this test itself.
#[test]
fn security_summary_guardrail_rejects_unverified_figure() {
    let allowed: std::collections::HashSet<String> = ["moderate".to_string(), "Acme".to_string()]
        .into_iter()
        .collect();
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: String::new(),
        security_summary: "9999 vulnerable dependencies were flagged.".to_string(),
        authorship_summary: String::new(),
        top_risks: vec![super::RiskRow {
            description: "moderate".to_string(),
            severity: "AMBER".to_string(),
            cost: "moderate".to_string(),
            apps: "Acme".to_string(),
        }],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &Default::default())
        .expect("the clean top-risk row keeps this Ok");

    assert!(
        result.security_summary.is_none(),
        "a security_summary citing 9999 must be rejected"
    );
    assert_eq!(result.top_risks.len(), 1, "the clean row must survive");
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("rejected (unverified figure)") && n.contains("security")),
        "a guardrail rejection note must name the security summary: {:?}",
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
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
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
    // #6004/#5453: the markdown fallback recovers executive_summary ONLY —
    // the three new narrative slots are absent from this response shape
    // entirely, so they stay `None` (the reporter's honesty-marker path),
    // same as top_risks/findings above.
    assert!(
        result.code_quality_summary.is_none(),
        "the markdown fallback never recovers code_quality_summary"
    );
    assert!(
        result.security_summary.is_none(),
        "the markdown fallback never recovers security_summary"
    );
    assert!(
        result.authorship_summary.is_none(),
        "the markdown fallback never recovers authorship_summary"
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

/// Why (#6030): the owner ruled that an executive summary "should describe
/// what this codebase does (as well as analyze the major components)", and
/// today's is risk-only. Like #5886's coverage-gaps note, this requirement is
/// static rather than a section instruction, so a template `instruct:`
/// override must not be able to drop it while restyling the section.
/// What: overrides `executive_summary`, then asserts the purpose-and-
/// components instruction still reaches the system prompt, and that the
/// digest carries the assessed set the instruction names components from.
#[test]
fn system_prompt_asks_what_the_codebase_does() {
    let mut model = fixture_model(vec![]);
    model.section_instructions.insert(
        "executive_summary".to_string(),
        "OVERRIDE: lead with the TQI posture.".to_string(),
    );
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    for needle in [
        "What the codebase does",
        "what the audited system IS and what it DOES",
        "analyze the major components",
        "Never invent a product, a market, a customer, or a component the data does not name",
        "Risk remains the summary's core",
    ] {
        assert!(
            req.system.contains(needle),
            "the purpose/components instruction must survive a template override and contain \
             {needle:?}: {}",
            req.system
        );
    }
    // The forced-output schema states the same requirement in the field's own
    // description, so a model steering off `response_format` alone cannot
    // revert to a risk-only summary.
    let schema = req.response_schema.as_ref().expect("forced schema");
    let description = schema.schema["properties"]["executive_summary"]["description"]
        .as_str()
        .expect("executive_summary description");
    assert!(
        description.contains("open by stating what the audited codebase IS and DOES"),
        "the forced schema must state the requirement too: {description}"
    );
    assert!(
        req.messages[0].content.contains("Applications assessed"),
        "the digest must name the set whose components the instruction asks about: {}",
        req.messages[0].content
    );
}

/// Why (#6030/#6029): the instruction above can only be obeyed from data in
/// the prompt. A sweep without `--analyze` carries no `AnalyzeMetrics`, and
/// the digest previously wrote "No metrics available for this application"
/// for exactly that run — starving the summary of density AND component
/// evidence while `RepoScan` held both.
/// What: metrics absent, scan present; asserts the scan's LoC, languages and
/// file count all reach `messages[0]`.
#[test]
fn digest_carries_scan_profile_without_metrics() {
    let mut model = fixture_model(vec![]);
    let repo = model.repositories.first_mut().expect("one repository");
    repo.metrics = None;
    repo.scan = Some(crate::report::scan::RepoScan {
        total_loc: 1_500_000,
        file_count: 8_432,
        by_language: vec![LanguageLoc {
            language: "Rust".to_string(),
            loc: 1_500_000,
        }],
        frameworks: vec![],
    });

    let digest = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    )
    .messages[0]
        .content
        .clone();
    assert!(digest.contains("Total LoC: 1500000"), "{digest}");
    assert!(digest.contains("Primary languages: Rust"), "{digest}");
    assert!(digest.contains("Files: 8432"), "{digest}");
    assert!(
        !digest.contains("No metrics available"),
        "a scanned application is not a metrics-free one: {digest}"
    );
}

/// The component evidence itself: the build manifests, the project each
/// declares, and its top dependencies are what the executive summary names
/// components from, and they never reached the prompt before #6030.
#[test]
fn digest_names_build_manifests_as_component_evidence() {
    let mut model = fixture_model(vec![]);
    let repo = model.repositories.first_mut().expect("one repository");
    repo.scan = Some(crate::report::scan::RepoScan {
        total_loc: 8200,
        file_count: 120,
        by_language: vec![],
        frameworks: vec![crate::report::scan::Framework {
            manifest: "package.json".to_string(),
            name: "acme-web".to_string(),
            deps: vec!["react".to_string(), "vite".to_string()],
        }],
    });

    let digest = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    )
    .messages[0]
        .content
        .clone();
    assert!(digest.contains("Build manifests / frameworks:"), "{digest}");
    assert!(digest.contains("package.json"), "{digest}");
    assert!(digest.contains("acme-web"), "{digest}");
    assert!(digest.contains("react"), "{digest}");
}

// ── #6093: the output-budget retry ladder ───────────────────────────────────

/// A provider that models a real output-token ceiling: it answers only when the
/// response the request ASKS FOR fits inside the request's own `max_tokens`.
///
/// Why: the #6093 defect is not that a stub said "length" twice — it is that
/// the ask stayed the same size while the ceiling stayed at a hardcoded 3072,
/// so no retry could ever converge. A stub returning a scripted `finish_reason`
/// cannot express that; this one derives the verdict from the request, so a fix
/// that only shrinks content, or only raises the ceiling, still fails it if the
/// two never meet.
/// What: reads `max_tokens` and the schema's own `top_risks`/`findings`
/// `maxItems` off the request, prices the response at
/// `overhead + narrative + risks * per_risk + findings * per_finding`, and
/// returns `finish_reason: "length"` with an empty body when that exceeds
/// `max_tokens`. `overhead` stands in for the reasoning tokens an
/// Anthropic-family model spends before the JSON starts.
struct CeilingLlm {
    overhead: u32,
    narrative: u32,
    per_risk: u32,
    per_finding: u32,
    calls: std::sync::atomic::AtomicUsize,
    budgets: Mutex<Vec<u32>>,
}

impl CeilingLlm {
    fn new(overhead: u32, narrative: u32, per_risk: u32, per_finding: u32) -> Self {
        CeilingLlm {
            overhead,
            narrative,
            per_risk,
            per_finding,
            calls: std::sync::atomic::AtomicUsize::new(0),
            budgets: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The `max_tokens` seen on each call, in call order.
    fn budgets(&self) -> Vec<u32> {
        self.budgets.lock().unwrap().clone()
    }

    /// `maxItems` for one top-level array property, or 0 when the property is
    /// absent from the schema entirely (the narrative-only rung).
    fn max_items(req: &LlmRequest, property: &str) -> u32 {
        req.response_schema
            .as_ref()
            .and_then(|s| s.schema["properties"][property]["maxItems"].as_u64())
            .unwrap_or(0) as u32
    }
}

#[async_trait]
impl LlmProvider for CeilingLlm {
    fn name(&self) -> &str {
        "ceiling"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.budgets.lock().unwrap().push(req.max_tokens);

        let risks = Self::max_items(&req, "top_risks");
        let findings = Self::max_items(&req, "findings");
        let needed =
            self.overhead + self.narrative + risks * self.per_risk + findings * self.per_finding;

        let truncated = needed > req.max_tokens;
        Ok(LlmResponse {
            text: if truncated {
                String::new()
            } else {
                good_response()
            },
            model: "ceiling".to_string(),
            input_tokens: 4000,
            output_tokens: needed.min(req.max_tokens),
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some(if truncated { "length" } else { "stop" }.to_string()),
        })
    }
}

/// Why: the reported defect (#6093, reproduced twice on 0.21.0 against a 44-
/// and a 45-finding investigation). Before the ladder, the first call and its
/// "concise" retry both asked for ten per-finding elaborations at a fixed
/// 3072-token ceiling, so both truncated and an eight-minute investigation
/// exited with no report at all.
/// What: 45 RED findings, none verified, against a provider that truncates
/// whenever the ask outgrows the request's own `max_tokens`. Asserts synthesis
/// SUCCEEDS, that it took more than one call to get there, and that each retry
/// raised the budget — pre-fix code sends 3072 on both calls and fails this.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_ladder_recovers_a_large_finding_set() {
    let findings: Vec<MetricFinding> = (0..45).map(|i| red(&format!("finding {i}"))).collect();
    let model = fixture_model(findings);
    // Prices roughly matched to a real run: ~1200 tokens of narrative, ~120 per
    // risk row, ~320 per nine-field elaboration, ~900 of reasoning overhead.
    let llm = Arc::new(CeilingLlm::new(900, 1200, 120, 320));

    let result = Synthesizer::new(llm.clone(), "stub/model")
        .synthesize(&model)
        .await
        .expect("a 45-finding investigation must synthesize to a complete report");

    assert!(
        result.executive_summary.is_some(),
        "the narrative must survive the ladder: {:?}",
        result.notes
    );
    let budgets = llm.budgets();
    assert!(
        llm.call_count() > 1,
        "this fixture must exercise a retry, not pass on the first call"
    );
    assert!(
        budgets.windows(2).all(|w| w[1] > w[0]),
        "each retry must raise the output budget, got {budgets:?}"
    );
}

/// Why: the ladder must converge even when per-finding prose is ruinously
/// expensive — that is what the final narrative-only rung exists for.
/// What: a per-finding price no budget can absorb; asserts synthesis still
/// succeeds and that it took all three rungs.
/// Test: this test itself.
#[tokio::test]
async fn synthesize_ladder_falls_back_to_narrative_only() {
    let findings: Vec<MetricFinding> = (0..45).map(|i| red(&format!("finding {i}"))).collect();
    let model = fixture_model(findings);
    let llm = Arc::new(CeilingLlm::new(900, 1200, 120, 100_000));

    let result = Synthesizer::new(llm.clone(), "stub/model")
        .synthesize(&model)
        .await
        .expect("the narrative-only rung must still produce a report");

    assert!(result.executive_summary.is_some());
    assert_eq!(
        llm.call_count(),
        3,
        "the ladder is three rungs: full, concise, narrative-only"
    );
}

/// Why: #6093 closure condition 2 — a configured `[models.reviewer].max_tokens`
/// must demonstrably reach the synthesis request. It never did: the request
/// carried a hardcoded 3072 whatever the operator configured.
/// What: sets 9000 via `with_max_tokens` and asserts the first request carried
/// exactly that; then sets a value below the floor and asserts it is raised to
/// the floor rather than sent as an unmeetable ceiling.
/// Test: this test itself.
#[tokio::test]
async fn configured_max_tokens_reaches_the_request() {
    let model = fixture_model(vec![red("x")]);

    let llm = Arc::new(CeilingLlm::new(0, 10, 1, 1));
    Synthesizer::new(llm.clone(), "stub/model")
        .with_max_tokens(9000)
        .synthesize(&model)
        .await
        .expect("synthesis succeeds");
    assert_eq!(llm.budgets().first().copied(), Some(9000));

    let small = Arc::new(CeilingLlm::new(0, 10, 1, 1));
    Synthesizer::new(small.clone(), "stub/model")
        .with_max_tokens(64)
        .synthesize(&model)
        .await
        .expect("synthesis succeeds");
    assert_eq!(
        small.budgets().first().copied(),
        Some(SYNTHESIS_MIN_MAX_TOKENS),
        "a configured ceiling below the floor is raised to it"
    );
}

/// Why: the ladder only converges if every rung both raises the budget and
/// shrinks the ask; a rung that does one without the other reintroduces #6093.
/// What: asserts the budget rises and the two array caps fall across the
/// ladder, and that the escalation stays bounded.
/// Test: this test itself.
#[test]
fn tier_ladder_raises_budget_and_shrinks_ask() {
    let tiers = [
        SynthesisTier::Full,
        SynthesisTier::Concise,
        SynthesisTier::NarrativeOnly,
    ];
    let budgets: Vec<u32> = tiers.iter().map(|t| t.budget(4096)).collect();
    assert!(
        budgets.windows(2).all(|w| w[1] > w[0]),
        "budgets must rise: {budgets:?}"
    );
    assert!(
        budgets.iter().all(|b| *b <= SYNTHESIS_ESCALATED_MAX_TOKENS),
        "escalation must stay bounded: {budgets:?}"
    );

    let asks: Vec<(usize, usize)> = tiers
        .iter()
        .map(|t| (t.top_risks_cap(), t.elaboration_cap()))
        .collect();
    assert!(
        asks.windows(2).all(|w| w[1].0 <= w[0].0 && w[1].1 < w[0].1),
        "each rung must ask for strictly fewer elaborations: {asks:?}"
    );
    assert_eq!(
        SynthesisTier::NarrativeOnly.elaboration_cap(),
        0,
        "the final rung asks for no elaboration prose"
    );
}

/// Why: on the final rung the `findings` array must be absent from the schema,
/// not merely capped at zero — a zero-length array the model is still shown is
/// an invitation to fill it.
/// What: builds the narrative-only request; asserts the schema and its derived
/// contract statement both omit `findings`, and that the digest says
/// elaboration was deferred rather than claiming everything is verified.
/// Test: this test itself.
#[test]
fn narrative_only_tier_omits_findings_from_schema() {
    let findings: Vec<MetricFinding> = (0..12).map(|i| red(&format!("finding {i}"))).collect();
    let req = build_synthesis_prompt(
        &fixture_model(findings),
        "stub/model",
        SynthesisTier::NarrativeOnly,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let schema = req.response_schema.as_ref().expect("schema forced");
    assert!(
        schema.schema["properties"]["findings"].is_null(),
        "findings must be absent: {}",
        schema.schema
    );
    assert!(
        !schema_contract_statement(&schema.schema).contains("findings"),
        "the prompt-text contract must not name a field the schema dropped"
    );
    assert!(schema.schema["properties"]["executive_summary"].is_object());

    let digest = &req.messages[0].content;
    assert!(
        digest.contains("none this pass"),
        "the digest must say elaboration was deferred, not that all are verified: {digest}"
    );
    assert!(
        !digest.contains("every RED/AMBER finding already has verified"),
        "that line would be false here: {digest}"
    );
}

/// Why (#6147): the architecture paragraph is only as good as what the prompt
/// states. Before this the digest carried complexity buckets and a language
/// list, so the model inferred a structure; now a Cargo workspace's own graph
/// reaches it as measured fact.
/// What: attaches a topology to the fixture model and asserts the digest names
/// its member count, its shared core, and the section heading that marks the
/// facts as measured.
/// Test: this test itself.
#[test]
fn prompt_carries_the_crate_topology() {
    use crate::report::topology::{CrateNode, CrateTopology};

    let mut model = fixture_model(vec![]);
    model.repositories[0].crate_topology = Some(CrateTopology {
        members: 3,
        edges: 3,
        cycles: Vec::new(),
        crates: vec![
            CrateNode {
                name: "acme-core".to_string(),
                deps: Vec::new(),
                inbound: 2,
            },
            CrateNode {
                name: "acme-app".to_string(),
                deps: vec!["acme-core".to_string()],
                inbound: 0,
            },
        ],
    });

    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let digest = &req.messages[0].content;

    assert!(
        digest.contains("Crate topology (measured from cargo metadata"),
        "the facts must be marked measured, not inferred: {digest}"
    );
    assert!(digest.contains("3 crates"), "{digest}");
    assert!(
        digest.contains("Most depended on: acme-core (2)"),
        "{digest}"
    );
    assert!(
        digest.contains("`acme-core`: depends on 0 internal crate(s); depended on by 2"),
        "{digest}"
    );
}

/// Why: a repository with no Cargo workspace must add no headed-but-empty
/// section to the digest — an empty heading invites the model to fill it.
/// What: asserts the fixture model, which declares no topology, carries none.
/// Test: this test itself.
#[test]
fn prompt_omits_the_topology_block_when_there_is_none() {
    let model = fixture_model(vec![]);
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );

    assert!(
        !req.messages[0].content.contains("Crate topology"),
        "no topology, no heading"
    );
}

// ── Claim-level grounding guardrail (#6082 lap 4) ───────────────────────────

/// A one-repo model whose only finding scopes itself to localhost in its own
/// remediation, and whose topology makes `acme-leaf` a crate nothing depends on.
fn grounded_fixture_model() -> ReportModel {
    let mut model = fixture_model(vec![MetricFinding {
        title: "Control-plane HTTP session endpoints have no authentication".to_string(),
        severity: Severity::Red,
        category: "security".to_string(),
        component: "crates/acme-daemon/src/control_routes.rs".to_string(),
        description: "The session handlers accept requests with no auth check".to_string(),
        remediation: "Add an auth middleware layer before exposing them beyond localhost"
            .to_string(),
    }]);
    model.repositories[0].crate_topology = Some(crate::report::topology::CrateTopology {
        members: 4,
        edges: 3,
        cycles: Vec::new(),
        crates: vec![
            crate::report::topology::CrateNode {
                name: "acme-core".to_string(),
                deps: Vec::new(),
                inbound: 3,
            },
            crate::report::topology::CrateNode {
                name: "acme-leaf".to_string(),
                deps: vec!["acme-core".to_string()],
                inbound: 0,
            },
        ],
    });
    model
}

/// #6082 lap 4 (FIX 1): the blocking defect — a loopback-only endpoint written
/// up as remote code execution.
///
/// Why: the finding is correct and RED; the reachability word is not, and the
/// finding's own remediation says so. Pre-fix `apply_guardrail` had no claim
/// check at all and shipped the contradiction verbatim.
/// What: asserts the executive summary survives with the remote wording
/// rewritten, and that a note records the correction.
/// Test: this test itself.
#[test]
fn synthesize_rewrites_a_remote_claim_about_a_local_finding() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    let raw = super::RawSynthesis {
        executive_summary: "The acme-daemon control plane has no auth — together an \
                            unauthenticated remote-code-execution path."
            .to_string(),
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: vec![],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the corrected summary keeps this Ok");

    let exec = result
        .executive_summary
        .expect("the summary is corrected, not dropped");
    assert!(
        !exec.to_lowercase().contains("remote"),
        "the remote claim must not survive: {exec}"
    );
    assert!(exec.contains("local-process-reachable code-execution"));
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("reachability corrected")),
        "the correction must be recorded: {:?}",
        result.notes
    );
}

/// #6082 lap 4 (FIX 1): a remote claim this crate cannot rewrite drops the
/// field rather than shipping it.
///
/// Why: fail-closed is the same posture the numeric guardrail takes — the
/// deterministic composition fills the placeholder.
/// What: prose whose remote wording survives every known rewrite; asserts the
/// field is absent and the note names the grounding rejection.
/// Test: this test itself.
#[test]
fn synthesize_rejects_an_uncorrectable_remote_claim() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: String::new(),
        security_summary: "The acme-daemon control_routes surface offers remote code execution \
                           and remote-management access to the internet."
            .to_string(),
        authorship_summary: "Two authors carry the estate.".to_string(),
        top_risks: vec![],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the authorship paragraph keeps this Ok");

    assert!(result.security_summary.is_none());
    assert!(
        result.notes.iter().any(|n| {
            n.contains("claim contradicts the report's own data") && n.contains("security summary")
        }),
        "the rejection must be recorded: {:?}",
        result.notes
    );
}

/// #6082 lap 4 (FIX 2): a crate with zero dependents called load-bearing
/// contradicts the report's own topology table and drops the field.
///
/// Why: the last report named trusty-mpm load-bearing while its own table two
/// sections down showed it with 0 dependents.
/// What: prose naming the leaf crate as load-bearing; asserts the field is
/// dropped and the note names the crate.
/// Test: this test itself.
#[test]
fn synthesize_rejects_a_load_bearing_claim_about_a_leaf_crate() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    let raw = super::RawSynthesis {
        executive_summary: "acme-core and acme-leaf are the load-bearing crates the estate \
                            depends on."
            .to_string(),
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: "Two authors carry the estate.".to_string(),
        top_risks: vec![],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the authorship paragraph keeps this Ok");

    assert!(result.executive_summary.is_none());
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("acme-leaf") && n.contains("most-depended-on")),
        "the rejection must name the crate: {:?}",
        result.notes
    );
}

/// #6082 lap 4: a top-risk row's description takes the same grounding pass the
/// paragraphs do — row 1 of the graded report carried the same contradiction.
#[test]
fn synthesize_grounds_the_top_risk_row_description() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed: std::collections::HashSet<String> = ["RED".to_string()].into_iter().collect();
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: vec![super::RiskRow {
            description: "acme-daemon control_routes endpoints have no auth — an unauthenticated \
                          remote-code-execution path."
                .to_string(),
            severity: "RED".to_string(),
            cost: String::new(),
            apps: String::new(),
        }],
        findings: vec![],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the corrected row keeps this Ok");

    assert_eq!(result.top_risks.len(), 1);
    assert!(
        !result.top_risks[0]
            .description
            .to_lowercase()
            .contains("remote"),
        "row 1 must be corrected: {}",
        result.top_risks[0].description
    );
}

/// One RED narrative against the grounded fixture, varying only the field the
/// test is about.
fn grounded_finding(business_impact: &str) -> super::FindingProse {
    super::FindingProse {
        trace_verdict: String::new(),
        app_slug: "acme-web".to_string(),
        title: "Run handler executes an arbitrary caller-supplied executable".to_string(),
        severity: "RED".to_string(),
        description: "the request body overrides the executable path with no validation"
            .to_string(),
        evidence: "pub claude_cmd: Option<String>,".to_string(),
        component: "crates/acme-daemon/src/control_routes.rs".to_string(),
        business_impact: business_impact.to_string(),
        remediation: "validate the path and require auth".to_string(),
        cost_effort: "medium".to_string(),
        evidence_measured: true,
    }
}

/// #6082 lap 5 (BLOCKER 2): a finding's own business impact takes the grounding
/// pass, and the hedged `remote/local` wording is a claim the guard corrects.
///
/// Why: the guard was wired to the summaries and the top-risk rows only, so RED
/// finding 3's business impact shipped "enabling remote/local code execution"
/// two lines from a Security Posture paragraph the same guard had corrected to
/// "not remotely" — the one remaining RCE-class string in the document,
/// contradicting the corrected paragraph. The hedged spelling also matched no
/// rewrite pattern, so the sentence never triggered the check at all.
/// What: the graded report's own wording as a finding's `business_impact`.
/// Asserts it survives with the remote claim rewritten and the correction noted.
/// Test: this test itself.
#[test]
fn synthesize_grounds_a_finding_business_impact() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: vec![],
        findings: vec![grounded_finding(
            "An attacker reaching the acme-daemon control_routes endpoint could cause it to \
             execute an arbitrary binary, enabling remote/local code execution",
        )],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the corrected finding keeps this Ok");

    assert_eq!(result.findings.len(), 1, "{:?}", result.notes);
    let impact = &result.findings[0].business_impact;
    assert!(
        !impact.to_lowercase().contains("remote"),
        "the remote claim must not survive in the business impact: {impact}"
    );
    assert!(
        impact.contains("local-process-reachable code execution"),
        "the corrected wording must replace it: {impact}"
    );
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("reachability corrected")),
        "the correction must be recorded: {:?}",
        result.notes
    );
}

/// #6082 lap 5 (BLOCKER 2): an uncorrectable remote claim in a finding field
/// empties that field rather than shipping it.
///
/// Why: fail-closed is the posture every other grounded field takes, and an
/// emptied field renders as the honesty marker the deterministic composition
/// already fills.
/// What: business-impact prose whose remote wording survives every rewrite.
/// Asserts the finding still ships (the measurement is real), its business
/// impact is empty, and the note names the grounding rejection.
/// Test: this test itself.
#[test]
fn synthesize_empties_an_uncorrectable_remote_claim_in_a_finding() {
    let model = grounded_fixture_model();
    let grounding = crate::report::synthesize_grounding::Grounding::from_model(&model);
    let allowed = allowed_numbers(&serde_json::to_value(&model).unwrap());
    let raw = super::RawSynthesis {
        executive_summary: String::new(),
        code_quality_summary: String::new(),
        security_summary: String::new(),
        authorship_summary: String::new(),
        top_risks: vec![],
        findings: vec![grounded_finding(
            "The acme-daemon control_routes surface offers remote code execution and \
             remote-management access to the internet.",
        )],
    };

    let result = super::apply_guardrail(raw, &allowed, Vec::new(), &grounding)
        .expect("the finding's other fields keep this Ok");

    assert_eq!(result.findings.len(), 1, "{:?}", result.notes);
    assert!(
        result.findings[0].business_impact.is_empty(),
        "an uncorrectable claim must be dropped: {}",
        result.findings[0].business_impact
    );
    assert!(
        result.notes.iter().any(|n| {
            n.contains("claim contradicts the report's own data") && n.contains("business impact")
        }),
        "the rejection must be recorded: {:?}",
        result.notes
    );
}

/// #6082 lap 4: the prompt states both grounding facts, so the model is told
/// the answer rather than only judged for guessing.
#[test]
fn prompt_carries_the_grounding_facts() {
    let model = grounded_fixture_model();
    let req = build_synthesis_prompt(
        &model,
        "stub/model",
        SynthesisTier::Full,
        SYNTHESIS_DEFAULT_MAX_TOKENS,
    );
    let digest = &req.messages[0].content;

    assert!(digest.contains("Reachability of these findings"));
    assert!(digest.contains("Control-plane HTTP session endpoints have no authentication"));
    assert!(digest.contains("Load-bearing crates (measured from cargo metadata"));
    assert!(digest.contains("`acme-core` — 3 dependent(s)"));
    assert!(
        !digest.contains("`acme-leaf` — "),
        "a leaf crate must not be offered as load-bearing"
    );
}
