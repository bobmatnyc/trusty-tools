//! Per-period LLM finding extraction for the contributor-profile pipeline (#565).
//!
//! Why: each period batch (statistics + sampled diffs) must be sent to an LLM
//! to extract code-quality findings so they can be synthesised into longitudinal
//! trends.  Doing this per-period keeps individual prompts focused and token
//! counts manageable.
//! What: `BatchReviewer` calls the LLM with a per-period "reviewer" prompt,
//! parses the JSON findings block (same strategy as the MVP parser), and
//! accumulates telemetry into `TokenCostSummary`.  Fail-safe: any LLM or parse
//! error records the error and returns an empty finding list — never panics.
//! Verification round is OUT OF SCOPE for v0.1 (TODO: add a verification pass
//! in a future PR to cross-check high-confidence findings against the diff).
//! Test: `tests` module below covers prompt content, JSON parsing, fail-safe
//! on empty/malformed LLM response, and telemetry accumulation — all using a
//! fake `LlmProvider` injected via `Arc<dyn LlmProvider>`.

use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::llm::{
    ChatMessage, LlmProvider, LlmRequest, LlmResponse, ResponseSchema, strip_provider_prefix,
};
use crate::models::{Effort, Finding};
use crate::profile::types::{LongitudinalFinding, PeriodBatch, TokenCostSummary};

// ─── Prompt constants ─────────────────────────────────────────────────────────

/// Temperature for the period-review LLM call — tighter than chat for
/// consistent structured output.
const PERIOD_REVIEWER_TEMPERATURE: f32 = 0.2;

/// Max tokens per period-review LLM call.
const PERIOD_REVIEWER_MAX_TOKENS: u32 = 2048;

// ─── BatchReviewer ────────────────────────────────────────────────────────────

/// Per-period LLM finding extractor.
///
/// Why: each period batch must be analysed independently so findings can later
/// be compared across periods (recurrence, worsening, resolution).
/// What: holds a reference to an `LlmProvider` and the model slug used for
/// period-review calls.  `review_period` sends one LLM request per period,
/// parses the JSON block, and returns `Vec<LongitudinalFinding>`.  The
/// `trend_tag` on each finding is intentionally left `None` here — the
/// synthesiser assigns tags after seeing all periods.
/// Test: see `tests` module below.
pub struct BatchReviewer {
    llm: Arc<dyn LlmProvider>,
    model: String,
}

impl BatchReviewer {
    /// Create a `BatchReviewer` from an injected provider and model slug.
    ///
    /// Why: dependency injection allows tests to supply a fake provider without
    /// spawning real network connections.
    /// What: stores the provider and model for reuse across period calls.
    /// Test: exercised by all `tests::*` tests.
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            llm,
            model: model.into(),
        }
    }

    /// Extract findings for one `PeriodBatch` via LLM.
    ///
    /// Why: each period is reviewed independently so per-period findings can
    /// be compared for recurrence/trend in a later synthesis step.
    /// What: builds the review prompt for the period, calls the LLM, tries to
    /// parse the JSON findings block, and appends the parsed findings (with
    /// `period_label` set and `trend_tag = None`) to the output.  On any error
    /// the function logs and returns an empty `Vec` — never propagates the
    /// error so a single bad period does not abort the entire profile run.
    /// Accumulates telemetry into `cost_out` in-place.
    /// Test: `tests::batch_reviewer_parses_findings_from_json`,
    /// `tests::batch_reviewer_fail_safe_on_empty_response`,
    /// `tests::batch_reviewer_fail_safe_on_malformed_json`.
    pub async fn review_period(
        &self,
        batch: &PeriodBatch,
        cost_out: &mut TokenCostSummary,
    ) -> Vec<LongitudinalFinding> {
        let req = build_period_prompt(batch, &self.model);
        let start = Instant::now();

        let resp = match self.llm.complete(req).await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    period = %batch.stats.period_label,
                    error = %e,
                    "batch_reviewer: LLM call failed — returning empty findings (fail-safe)"
                );
                return Vec::new();
            }
        };

        let latency = start.elapsed().as_millis() as u64;
        cost_out.accumulate(
            resp.input_tokens as u64,
            resp.output_tokens as u64,
            resp.cost_usd,
            latency,
        );

        debug!(
            period = %batch.stats.period_label,
            input_tokens = resp.input_tokens,
            output_tokens = resp.output_tokens,
            latency_ms = latency,
            "batch_reviewer: LLM call complete"
        );

        parse_period_findings(&resp, &batch.stats.period_label)
    }
}

// ─── Prompt builder ───────────────────────────────────────────────────────────

/// Build the JSON Schema for the period findings output structure.
///
/// Why: forced structured output eliminates parse failures in the period
/// reviewer (batch_reviewer); with a schema, the model MUST emit valid JSON
/// conforming to the `PeriodFindingsBlock` shape.
/// What: returns a `ResponseSchema` with name `"period_findings"` and a
/// JSON Schema matching `PeriodFindingsBlock` / `PeriodFindingWire`.
/// Test: `tests::build_period_prompt_includes_schema`.
fn period_findings_schema() -> ResponseSchema {
    ResponseSchema {
        name: "period_findings".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "description": {"type": "string"},
                            "suggestion": {"type": "string"},
                            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                            "file": {"type": "string"},
                            "severity": {
                                "type": "string",
                                "enum": ["low", "medium", "high", "critical"]
                            }
                        },
                        "required": ["kind", "description"]
                    }
                }
            },
            "required": ["findings"]
        }),
    }
}

/// Build the LLM request for reviewing a single period batch.
///
/// Why: centralises prompt assembly so the wording can be iterated without
/// touching reviewer logic.
/// What: assembles a system prompt (reviewer role instructions + JSON output
/// schema) and a user message containing the period stats summary + sampled
/// diff snippets.  Includes `response_schema` so the provider forces structured
/// output — eliminating parse failures in the profile pipeline.
/// `model` may carry a `bedrock/` or `openrouter/` routing prefix; this
/// function strips it before setting `LlmRequest.model` so the bare id reaches
/// the provider API.
/// Test: `tests::batch_reviewer_prompt_contains_period_label`,
/// `tests::batch_period_prompt_strips_bedrock_prefix`,
/// `tests::build_period_prompt_includes_schema`.
pub fn build_period_prompt(batch: &PeriodBatch, model: &str) -> LlmRequest {
    let system = period_reviewer_system_prompt();
    let user = build_period_user_message(batch);
    LlmRequest {
        model: strip_provider_prefix(model).to_string(),
        system: system.to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user,
        }],
        temperature: PERIOD_REVIEWER_TEMPERATURE,
        max_tokens: PERIOD_REVIEWER_MAX_TOKENS,
        response_schema: Some(period_findings_schema()),
    }
}

/// System prompt for the period-batch reviewer role.
///
/// Why: the system prompt defines the review criteria and the structured
/// output shape the parser depends on.  With forced structured output active,
/// the model populates the response fields directly rather than emitting a
/// fenced JSON block.
/// What: instructs the LLM to act as a senior engineer reviewing a sample of
/// one engineer's commits and populate the `findings` array.
/// Test: asserted in `tests::batch_reviewer_system_prompt_contains_schema`.
pub(super) fn period_reviewer_system_prompt() -> &'static str {
    r#"You are a senior software engineer reviewing a sample of one engineer's commits
over a specific time window as part of a longitudinal quality analysis.

## Task
Identify code-quality findings present in the sampled diffs. Focus on:
- Correctness bugs, error-handling gaps, resource leaks
- Security weaknesses (injection, auth, secrets in code)
- Logic errors, off-by-one issues, data-loss risks
- Missing tests or test-quality issues
- Recurring anti-patterns visible across multiple commits in this window

## Output (REQUIRED)
Populate the structured response with a `findings` array.
Each finding must include:
- `kind`: short category label (e.g. error_handling, security, logic)
- `description`: concise description of the issue observed
- `suggestion`: concrete improvement suggestion
- `confidence`: float in [0.0, 1.0]
- `file`: most relevant file path; use "multiple" if the issue spans files
- `severity`: one of low, medium, high, critical

`findings` may be an empty array if the sample looks clean."#
}

/// Build the user-turn message for a period review.
///
/// Why: the user message carries the period statistics and sampled diff
/// snippets that the LLM needs to produce findings.
/// What: formats the period label/dates, commit stats summary, and up to
/// `MAX_DIFFS_IN_PROMPT` sampled diffs each in a fenced code block.
/// Test: `tests::batch_reviewer_prompt_contains_period_label`.
fn build_period_user_message(batch: &PeriodBatch) -> String {
    const MAX_DIFFS_IN_PROMPT: usize = 10;
    let s = &batch.stats;
    let mut msg = String::with_capacity(4096);

    msg.push_str(&format!(
        "## Period: {}\nFrom {} to {}\n\n",
        s.period_label, s.since, s.until
    ));

    msg.push_str("### Statistics\n");
    msg.push_str(&format!("- Commits: {}\n", s.commit_count));
    msg.push_str(&format!("- Quality score: {:.2}\n", s.quality_score));
    msg.push_str(&format!("- Ticketed %: {:.0}%\n", s.ticketed_pct * 100.0));

    if !s.categories.is_empty() {
        let mut cats: Vec<(&String, &u64)> = s.categories.iter().collect();
        cats.sort_by_key(|(k, _)| k.as_str());
        let cat_str: Vec<String> = cats.iter().map(|(k, v)| format!("{k}={v}")).collect();
        msg.push_str(&format!("- Categories: {}\n", cat_str.join(", ")));
    }

    if !s.repositories.is_empty() {
        msg.push_str(&format!("- Repositories: {}\n", s.repositories.join(", ")));
    }
    msg.push('\n');

    if batch.sampled_diffs.is_empty() {
        msg.push_str("### Sampled diffs\n*(no diffs available for this period)*\n\n");
    } else {
        msg.push_str("### Sampled diffs\n\n");
        for (i, diff) in batch
            .sampled_diffs
            .iter()
            .enumerate()
            .take(MAX_DIFFS_IN_PROMPT)
        {
            let cat = diff.category.as_deref().unwrap_or("unknown");
            let effort = diff.effort.as_deref().unwrap_or("?");
            msg.push_str(&format!(
                "#### Diff {} — {} ({repo}) [category={cat}, effort={effort}]\n",
                i + 1,
                &diff.sha[..8.min(diff.sha.len())],
                repo = diff.repository,
            ));
            msg.push_str(&format!("Commit: {}\n\n", diff.message));
            msg.push_str("```diff\n");
            msg.push_str(&diff.diff_text);
            if !diff.diff_text.ends_with('\n') {
                msg.push('\n');
            }
            msg.push_str("```\n\n");
        }
    }

    msg.push_str(
        "Please review the diffs above and populate the structured `findings` \
         array as specified in the system prompt.\n",
    );

    msg
}

// ─── JSON block parser ────────────────────────────────────────────────────────

/// Wire type for the per-period findings JSON block.
#[derive(Debug, Deserialize)]
struct PeriodFindingsBlock {
    #[serde(default)]
    findings: Vec<PeriodFindingWire>,
}

/// Wire type for a single finding in the JSON block.
#[derive(Debug, Deserialize)]
struct PeriodFindingWire {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    suggestion: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    file: String,
    #[serde(default)]
    severity: String,
}

/// Parse findings from an LLM response for one period.
///
/// Why: the MVP parser (`pipeline::parser`) targets `ReviewResult` verdict +
/// findings; the period parser targets just findings with slightly different
/// field names.  A separate function keeps the schemas independent.
/// What: tries direct JSON parse first (structured output path where the body
/// IS the JSON object), then falls back to the legacy fence-based extraction.
/// Converts wire findings to `LongitudinalFinding` (with `trend_tag = None`
/// and the given `period_label`).  Fail-safe: any parse error logs and returns
/// empty — never panics.
/// Test: `tests::batch_reviewer_parses_findings_from_json`,
/// `tests::batch_reviewer_parses_direct_json`.
fn parse_period_findings(resp: &LlmResponse, period_label: &str) -> Vec<LongitudinalFinding> {
    let body = resp.text.trim();
    if body.is_empty() {
        warn!(period = %period_label, "batch_reviewer: empty LLM response — returning empty findings");
        return Vec::new();
    }

    // Strategy 1: direct JSON (structured output path).
    if body.starts_with('{')
        && let Ok(block) = serde_json::from_str::<PeriodFindingsBlock>(body)
    {
        debug!(
            period = %period_label,
            findings = block.findings.len(),
            "batch_reviewer: parsed via direct JSON (structured output)"
        );
        return convert_period_block(block, period_label);
    }

    // Strategy 2: legacy fence-based extraction.
    let Some(fence_start) = body.rfind("```json") else {
        warn!(
            period = %period_label,
            "batch_reviewer: no JSON block found in LLM response — returning empty findings"
        );
        return Vec::new();
    };

    let after = &body[fence_start + 7..];
    let Some(fence_end) = after.find("```") else {
        warn!(period = %period_label, "batch_reviewer: unclosed JSON block — returning empty findings");
        return Vec::new();
    };

    let json_text = after[..fence_end].trim();
    let block: PeriodFindingsBlock = match serde_json::from_str(json_text) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                period = %period_label,
                error = %e,
                "batch_reviewer: JSON parse error — returning empty findings"
            );
            return Vec::new();
        }
    };

    convert_period_block(block, period_label)
}

/// Convert a `PeriodFindingsBlock` into `LongitudinalFinding` values.
///
/// Why: extracted to eliminate duplication between the two parse strategies.
/// What: maps each wire finding to `LongitudinalFinding` with `trend_tag = None`.
/// Test: covered by all parse-path tests in the test module.
fn convert_period_block(
    block: PeriodFindingsBlock,
    period_label: &str,
) -> Vec<LongitudinalFinding> {
    block
        .findings
        .into_iter()
        .map(|f| {
            let effort = severity_to_effort(&f.severity);
            let file = if f.file.is_empty() {
                "unknown".to_string()
            } else {
                f.file
            };
            let kind = if f.kind.is_empty() {
                "general".to_string()
            } else {
                f.kind
            };
            LongitudinalFinding {
                period_label: period_label.to_string(),
                finding: Finding::new(
                    file,
                    kind,
                    f.description,
                    f.suggestion,
                    f.confidence,
                    effort,
                ),
                trend_tag: None,
            }
        })
        .collect()
}

/// Map a severity string to a `Finding` effort level.
fn severity_to_effort(severity: &str) -> Effort {
    match severity.to_lowercase().as_str() {
        "high" | "critical" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests live in batch_reviewer_tests.rs to keep this file under 500 lines.

#[cfg(test)]
#[path = "batch_reviewer_tests.rs"]
mod tests;
