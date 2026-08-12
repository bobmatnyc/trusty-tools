//! Per-period prompt assembly, transport, and finding-response parsing.
//!
//! Why: reviewing every period in one prompt would blow the context budget and
//! blur the periods together, so each period is reviewed on its own — and the
//! two halves that decide what a period's prompt says and how its answer is
//! read stay pure functions with no network in them.
//! What: [`period_reviewer_system_prompt`], [`period_findings_schema`], and
//! [`build_period_user_message`] produce the request text; [`parse_period_findings`]
//! turns a response body into [`LongitudinalFinding`] values;
//! [`build_period_request`] assembles the two into a
//! [`trusty_common::inference::ChatRequest`] and [`PeriodReviewer`] sends it.
//!
//! #5464: the transport is `trusty_common::inference` — the workspace's one
//! inference entry point, which tga already consumes in
//! [`crate::classify::tiers::bedrock`]. Profiling therefore reaches a model with
//! no Cargo edge to trusty-review, and prefix routing (`bedrock/…`,
//! `openrouter/…`) stays the commons' job rather than being re-implemented here.
//!
//! Test: `batch_reviewer_tests.rs`.

use std::sync::Arc;
use std::time::Instant;

use serde::Deserialize;
use tracing::{debug, warn};
use trusty_common::credentials::{default_store, KeyStore};
use trusty_common::inference::{
    register_default_factories, ChatMessage, ChatRequest, Configurator, InferenceAdapter,
    InferenceError,
};

use super::types::{Effort, Finding, LongitudinalFinding, PeriodBatch, TokenCostSummary};

// ─── Request parameters ───────────────────────────────────────────────────────

/// Sampling temperature for a period-review call.
///
/// Low, because the task is extraction into a fixed schema rather than prose.
pub const PERIOD_REVIEWER_TEMPERATURE: f32 = 0.2;

/// Output-token ceiling for a period-review call.
pub const PERIOD_REVIEWER_MAX_TOKENS: u32 = 2048;

/// Diffs included in one period prompt, however many the sampler collected.
const MAX_DIFFS_IN_PROMPT: usize = 10;

// ─── Prompt construction ──────────────────────────────────────────────────────

/// JSON Schema for the period-findings response.
///
/// Why: with a schema attached the model must emit a parseable object, which is
/// what keeps [`parse_period_findings`] from silently returning nothing on a
/// prose answer.
/// What: returns the bare schema value. The caller wraps it in whatever
/// structured-output type its provider takes.
/// Test: `period_findings_schema_has_findings_property`.
pub fn period_findings_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
}

/// System prompt for the period-review role.
///
/// Test: `batch_reviewer_system_prompt_contains_schema`.
pub fn period_reviewer_system_prompt() -> &'static str {
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

/// Build the user-turn message for one period.
///
/// Why: the model needs the period's numbers next to its diffs — a quality
/// score alone is not reviewable, and diffs alone lose the period context that
/// makes a trend visible.
/// What: renders the period label and bounds, the commit statistics, and up to
/// [`MAX_DIFFS_IN_PROMPT`] sampled diffs in fenced blocks.
/// Test: `batch_reviewer_prompt_contains_period_label`,
/// `batch_reviewer_prompt_handles_empty_diffs`.
pub fn build_period_user_message(batch: &PeriodBatch) -> String {
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

// ─── Request assembly ─────────────────────────────────────────────────────────

/// Assemble the shared-inference request for one period.
///
/// Why: [`trusty_common::inference::ChatRequest`] carries no structured-output
/// field, so the schema only reaches the model if the system turn spells it
/// out. Stating it there keeps [`parse_period_findings`]'s direct-JSON path —
/// the one that needs no fence — the likely outcome rather than the lucky one.
/// What: system turn = the reviewer role plus [`period_findings_schema`]
/// rendered as JSON; user turn = [`build_period_user_message`]. `model` is
/// passed through UNCHANGED: a `bedrock/` or `openrouter/` prefix is what
/// `provider_for` routes on, and the adapter strips it for the wire in
/// `ProviderId::wire_model_id`.
/// Test: `period_request_preserves_routing_prefix`,
/// `period_request_carries_schema_and_sampling`.
pub fn build_period_request(batch: &PeriodBatch, model: &str) -> ChatRequest {
    // `to_string_pretty` over a `Value` this module built cannot fail; an empty
    // string would still leave the prose instructions in the system turn.
    let schema = serde_json::to_string_pretty(&period_findings_schema()).unwrap_or_default();
    let system = format!(
        "{}\n\n## Response schema\nReturn ONLY a JSON object conforming to this schema:\n\
         ```json\n{schema}\n```",
        period_reviewer_system_prompt()
    );

    let mut req = ChatRequest::new(
        model,
        vec![
            ChatMessage::system(system),
            ChatMessage::user(build_period_user_message(batch)),
        ],
    );
    req.temperature = Some(PERIOD_REVIEWER_TEMPERATURE);
    req.max_tokens = Some(PERIOD_REVIEWER_MAX_TOKENS);
    req
}

// ─── Transport ────────────────────────────────────────────────────────────────

/// What one period's review produced, and whether it happened at all.
///
/// Why: a bare `Vec` cannot tell a caller "the model read this period and found
/// nothing" from "the provider never answered" — both are zero findings, and a
/// Bedrock outage across twelve quarters would render as twelve clean quarters.
/// Since the profile's trajectory is derived from the periods that WERE
/// reviewed, a caller that cannot see the difference publishes a trend over a
/// silently smaller sample. #5464: the shape is inherited from trusty-review's
/// original; it is fixed here rather than after #5465 wires the first caller to
/// it.
/// What: the findings, plus [`Self::skipped`] carrying the provider error when
/// the call failed. Deliberately NOT a `Result`, so no caller can `?` a single
/// period's outage into aborting the whole run — the audit must survive one bad
/// period. `#[non_exhaustive]` because the parse-failure case wants the same
/// treatment once `ChatRequest` can enforce a schema.
/// Test: `period_review_distinguishes_provider_failure_from_a_clean_period`.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct PeriodReview {
    /// Findings the model reported. Empty whenever [`Self::skipped`] is set.
    pub findings: Vec<LongitudinalFinding>,

    /// The provider error that skipped this period, or `None` when the model
    /// answered — in which case `findings` is what it said, including,
    /// legitimately, nothing.
    pub skipped: Option<InferenceError>,
}

impl PeriodReview {
    /// Whether the provider call failed and this period was never reviewed.
    pub fn was_skipped(&self) -> bool {
        self.skipped.is_some()
    }
}

// ─── Run coverage ─────────────────────────────────────────────────────────────

/// A period the provider never answered for.
///
/// The reason is rendered from the [`InferenceError`] at record time rather than
/// held as the error itself, so a summary stays `Clone` and can be written into
/// a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPeriod {
    /// The period's label, e.g. `2026Q2`.
    pub period_label: String,
    /// The provider failure, as rendered by its `Display`.
    pub reason: String,
}

/// How much of a profile run actually reached the model.
///
/// Why (#5465): [`PeriodReview::skipped`] exists so a provider outage on one
/// period cannot abort the run, but a caller that merely collects
/// `review.findings` throws that distinction away again — twelve failed calls
/// then render as twelve clean quarters, and the trajectory is computed over a
/// sample the reader cannot see is smaller. This type is the call site's half of
/// that contract: every review passes through [`Self::record`], so a skip is
/// counted rather than silently flattened into "no findings".
/// What: a reviewed count and the skipped periods, plus [`Self::coverage_line`]
/// for stderr and [`Self::coverage_note`] for the report.
/// Test: `period_run_summary_separates_a_skipped_period_from_a_clean_one`,
/// `period_run_summary_coverage_note_absent_when_complete`.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct PeriodRunSummary {
    /// Periods the model answered for — including, legitimately, with nothing.
    pub reviewed: usize,
    /// Periods whose provider call failed, in the order they were attempted.
    pub skipped: Vec<SkippedPeriod>,
}

impl PeriodRunSummary {
    /// Fold one period's result in, returning its findings.
    ///
    /// A skipped period contributes no findings and increments nothing but
    /// [`Self::skipped`]; a period the model answered counts as reviewed even
    /// when it found nothing, because that is a real result.
    ///
    /// Test: `period_run_summary_separates_a_skipped_period_from_a_clean_one`.
    pub fn record(&mut self, period_label: &str, review: PeriodReview) -> Vec<LongitudinalFinding> {
        // #5465: the first caller of `review_period` — the branch below is what
        // keeps a provider outage from rendering as a clean period.
        match review.skipped {
            Some(err) => {
                self.skipped.push(SkippedPeriod {
                    period_label: period_label.to_string(),
                    reason: err.to_string(),
                });
                Vec::new()
            }
            None => {
                self.reviewed += 1;
                review.findings
            }
        }
    }

    /// Periods attempted, reviewed or not.
    pub fn attempted(&self) -> usize {
        self.reviewed + self.skipped.len()
    }

    /// Whether every attempted period was actually reviewed.
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty()
    }

    /// One line naming the coverage, for stderr.
    ///
    /// Test: `period_run_summary_separates_a_skipped_period_from_a_clean_one`.
    pub fn coverage_line(&self) -> String {
        let attempted = self.attempted();
        if self.is_complete() {
            return format!("{}/{attempted} period(s) reviewed", self.reviewed);
        }
        let labels: Vec<&str> = self
            .skipped
            .iter()
            .map(|s| s.period_label.as_str())
            .collect();
        format!(
            "{}/{attempted} period(s) reviewed — {} SKIPPED (provider failure): {}",
            self.reviewed,
            self.skipped.len(),
            labels.join(", ")
        )
    }

    /// A Markdown section naming the skipped periods, or `None` when complete.
    ///
    /// Why: the report outlives the terminal it was produced in, and a reader
    /// deciding from it must be able to see that the trajectory covers fewer
    /// periods than the window suggests.
    /// Test: `period_run_summary_coverage_note_absent_when_complete`.
    pub fn coverage_note(&self) -> Option<String> {
        if self.is_complete() {
            return None;
        }
        let mut note = String::from("## Coverage\n\n");
        note.push_str(&format!(
            "{} of {} period(s) were reviewed. The following period(s) were \
             **skipped** because the inference provider call failed — they are \
             absent from the findings and the trajectory, and are NOT evidence \
             of clean work:\n\n",
            self.reviewed,
            self.attempted()
        ));
        for s in &self.skipped {
            note.push_str(&format!("- `{}` — {}\n", s.period_label, s.reason));
        }
        note.push('\n');
        Some(note)
    }
}

/// The period-review transport: one model call per period, over the shared
/// inference stack.
///
/// Why: #5464 — contributor profiling is tga's domain, so the call that turns a
/// period's diffs into findings routes through `trusty_common::inference`
/// directly instead of trusty-review's `crate::llm`. Any residual interaction
/// with trusty-review stays a process boundary (its `review_diff` MCP tool, or
/// the `trusty-review` binary `tga audit` already spawns) — never a Cargo edge.
/// What: holds a [`InferenceAdapter`] and the routing slug it was resolved from.
/// [`Self::review_period`] survives a provider failure and reports it as a
/// skipped period rather than a clean one.
/// Test: `period_reviewer_routes_through_shared_inference`,
/// `period_review_distinguishes_provider_failure_from_a_clean_period`.
pub struct PeriodReviewer {
    adapter: Arc<dyn InferenceAdapter>,
    model: String,
}

impl PeriodReviewer {
    /// Resolve `model` against the shared credential store and build its adapter.
    ///
    /// Why: the slug → provider → credential → adapter ladder is
    /// `trusty_common`'s, and routing it through [`Configurator`] is what keeps
    /// tga from growing a second copy of the "which backend, whose key" decision.
    /// What: registers the default HTTP provider factories (plus the Bedrock
    /// Converse factory when tga is built `--features bedrock`), then resolves
    /// `model` against [`default_store`].
    ///
    /// # Errors
    ///
    /// [`super::ProfileError::Inference`] when no credential resolves for the
    /// slug's provider family, or when no factory is registered for it.
    ///
    /// Test: `from_slug_with_store_builds_an_adapter_for_a_stored_credential`,
    /// `from_slug_with_store_errors_when_no_credential_resolves`.
    pub fn from_slug(model: &str) -> super::Result<Self> {
        Self::from_slug_with_store(model, default_store().as_ref())
    }

    /// [`Self::from_slug`] against an explicit credential store.
    ///
    /// Why: [`default_store`] reads the machine's real keychain, so a test built
    /// on it asserts whatever the developer happens to have exported. Injecting
    /// the store is how `trusty_common`'s own `provider_for` tests stay
    /// deterministic, and taking the same seam here is what makes both arms of
    /// this resolution testable.
    /// What: registers the default HTTP provider factories (plus the Bedrock
    /// Converse factory under tga's `bedrock` feature), then resolves `model`
    /// against `store` — env tier first, then the store, per `resolve_key_with`.
    ///
    /// # Errors
    ///
    /// As [`Self::from_slug`].
    pub fn from_slug_with_store(model: &str, store: &dyn KeyStore) -> super::Result<Self> {
        let mut configurator = Configurator::new();
        register_default_factories(&mut configurator);
        // #5464: the Bedrock Converse factory is registered separately from the
        // OpenAI-dialect ones, and only exists under tga's `bedrock` feature.
        #[cfg(feature = "bedrock")]
        trusty_common::inference::register_bedrock_factory(&mut configurator);

        let adapter = configurator.build(model, store)?;
        Ok(Self {
            adapter: Arc::from(adapter),
            model: model.to_string(),
        })
    }

    /// Bind a reviewer to an already-built adapter.
    ///
    /// Why: tests drive the real transport with the commons' `ScriptedAdapter`,
    /// and a caller that already resolved an adapter should reuse it rather than
    /// re-running credential resolution per period.
    /// Test: `period_reviewer_routes_through_shared_inference`.
    pub fn with_adapter(adapter: Arc<dyn InferenceAdapter>, model: impl Into<String>) -> Self {
        Self {
            adapter,
            model: model.into(),
        }
    }

    /// Review one period.
    ///
    /// Why: a run over twelve quarters must not be lost because the eleventh
    /// call timed out, so a provider failure never propagates — but it is
    /// REPORTED, in [`PeriodReview::skipped`], so the caller can say "9 of 12
    /// periods reviewed" instead of publishing a trend over a silently smaller
    /// sample.
    /// What: sends [`build_period_request`] through the adapter, accumulates the
    /// call's usage into `cost_out`, and parses the body with
    /// [`parse_period_findings`]. `cost_usd` is the provider's authoritative
    /// figure; a provider that reports none contributes 0 and leaves the token
    /// counts as the record of what the call cost. A failed call bills nothing
    /// and returns no findings.
    /// Test: `period_reviewer_routes_through_shared_inference`,
    /// `period_review_distinguishes_provider_failure_from_a_clean_period`.
    pub async fn review_period(
        &self,
        batch: &PeriodBatch,
        cost_out: &mut TokenCostSummary,
    ) -> PeriodReview {
        let period = &batch.stats.period_label;
        let request = build_period_request(batch, &self.model);
        let start = Instant::now();

        let response = match self.adapter.chat(&request).await {
            Ok(r) => r,
            Err(e) => {
                // #5464: the error travels back to the caller — a logged warning
                // alone made a skipped period indistinguishable from a clean one.
                warn!(
                    period = %period,
                    model = %self.model,
                    error = %e,
                    "batch_reviewer: inference call failed — period skipped"
                );
                return PeriodReview {
                    findings: Vec::new(),
                    skipped: Some(e),
                };
            }
        };

        let latency_ms = start.elapsed().as_millis() as u64;
        let usage = response.usage();
        cost_out.accumulate(
            u64::from(usage.prompt_tokens),
            u64::from(usage.completion_tokens),
            usage.cost_usd.unwrap_or(0.0),
            latency_ms,
        );

        debug!(
            period = %period,
            model = %response.resolved_model(&self.model),
            input_tokens = usage.prompt_tokens,
            output_tokens = usage.completion_tokens,
            latency_ms,
            "batch_reviewer: inference call complete"
        );

        PeriodReview {
            findings: parse_period_findings(&response.first_text().unwrap_or_default(), period),
            skipped: None,
        }
    }
}

// ─── Response parsing ─────────────────────────────────────────────────────────

/// Wire shape of the period-findings response body.
#[derive(Debug, Deserialize)]
struct PeriodFindingsBlock {
    #[serde(default)]
    findings: Vec<PeriodFindingWire>,
}

/// Wire shape of one finding in that body.
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

/// Parse a period-review response body into findings.
///
/// Why: one unparseable period must cost that period's findings, not the whole
/// profile — a run over twelve quarters should not be lost because the eleventh
/// answer came back as prose.
/// What: tries a direct JSON parse first (structured output returns the object
/// as the whole body), then falls back to extracting the last ```` ```json ````
/// fence. Every failure path logs and returns an empty `Vec`.
/// `trend_tag` is left `None` — only the synthesizer, which sees every period,
/// can assign it.
/// Test: `batch_reviewer_parses_findings_from_json`,
/// `batch_reviewer_parses_direct_json`,
/// `batch_reviewer_fail_safe_on_empty_response`,
/// `batch_reviewer_fail_safe_on_malformed_json`,
/// `batch_reviewer_fail_safe_on_prose_response`.
pub fn parse_period_findings(body: &str, period_label: &str) -> Vec<LongitudinalFinding> {
    let body = body.trim();
    if body.is_empty() {
        warn!(period = %period_label, "batch_reviewer: empty response — returning empty findings");
        return Vec::new();
    }

    // Strategy 1: the body IS the JSON object (structured-output path).
    // #5463: tga is edition 2021, so this cannot be the source's let-chain.
    if body.starts_with('{') {
        if let Ok(block) = serde_json::from_str::<PeriodFindingsBlock>(body) {
            debug!(
                period = %period_label,
                findings = block.findings.len(),
                "batch_reviewer: parsed via direct JSON"
            );
            return convert_period_block(block, period_label);
        }
    }

    // Strategy 2: a fenced JSON block inside free text.
    let Some(fence_start) = body.rfind("```json") else {
        warn!(
            period = %period_label,
            "batch_reviewer: no JSON block in response — returning empty findings"
        );
        return Vec::new();
    };

    let after = &body[fence_start + 7..];
    let Some(fence_end) = after.find("```") else {
        warn!(period = %period_label, "batch_reviewer: unclosed JSON block — returning empty findings");
        return Vec::new();
    };

    match serde_json::from_str::<PeriodFindingsBlock>(after[..fence_end].trim()) {
        Ok(block) => convert_period_block(block, period_label),
        Err(e) => {
            warn!(
                period = %period_label,
                error = %e,
                "batch_reviewer: JSON parse error — returning empty findings"
            );
            Vec::new()
        }
    }
}

/// Convert wire findings into [`LongitudinalFinding`] values.
///
/// An empty `file` or `kind` becomes a sentinel rather than an empty string, so
/// the Markdown report never renders a blank table cell.
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

/// Map a severity label to a remediation [`Effort`].
///
/// Unrecognised severities fall to `Low` — an unknown label must not inflate a
/// finding's weight.
///
/// Test: `severity_to_effort_mapping`.
pub fn severity_to_effort(severity: &str) -> Effort {
    match severity.to_lowercase().as_str() {
        "high" | "critical" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "batch_reviewer_tests.rs"]
mod tests;
