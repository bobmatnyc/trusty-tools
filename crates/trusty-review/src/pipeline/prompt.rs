//! Review prompt construction.
//!
//! Why: keeping the prompt text in its own module makes it easy to iterate on
//! the wording, the output format spec, and the context-block layout without
//! touching pipeline logic.  The prompt is the primary lever for review quality.
//!
//! What: exposes `build_review_prompt` which assembles the `LlmRequest` for the
//! reviewer role from the diff, PR metadata, and optional context blocks.  The
//! system prompt encodes the fail-safe APPROVE-default policy (spec REV-130)
//! and the structured output format the parser expects.
//!
//! Structured output contract (required by parser):
//!   The LLM MUST end its response with a JSON block delimited exactly as:
//!   ```json
//!   { "verdict": "<VERDICT>", "summary": "<one-line summary>",
//!     "findings": [ { "title": "...", "body": "...", "severity": "...",
//!                     "confidence": 0.0, "file": "...", "line": null } ] }
//!   ```
//!   Where `<VERDICT>` ∈ {"APPROVE","APPROVE*","REQUEST_CHANGES","BLOCK","UNKNOWN"}.
//!
//! Test: `build_review_prompt_includes_diff`, `system_prompt_contains_policy`,
//! `prompt_includes_context_blocks`.

use crate::{
    coverage::CoverageVerdictContrib,
    integrations::{
        analyze_client::{ComplexityHotspot, Smell},
        apex_context::ApexContextResult,
        search_client::SearchResult,
    },
    llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix},
    models::ReviewResult,
    voice::VoiceConfig,
};

// System prompt templates are in a separate file to keep this module under the
// 500-line cap (#610) — the two large prompt constants are ~160 lines combined.
use super::prompt_templates::{SYSTEM_PROMPT_COVERAGE_GATING, SYSTEM_PROMPT_STOCK};
// User-message builder extracted to keep this module under the 500-line cap (#610).
use super::prompt_user_msg::build_user_message;

// ─── Prompt constants ─────────────────────────────────────────────────────────

/// Reviewer temperature — tighter than chat for more deterministic verdicts.
const REVIEWER_TEMPERATURE: f32 = 0.3;

/// Maximum tokens for the review response.
const REVIEWER_MAX_TOKENS: u32 = 4096;

// ─── Review output schema ─────────────────────────────────────────────────────

/// The name used for the structured-output tool/schema.
const REVIEW_SCHEMA_NAME: &str = "review_output";

/// Build the JSON Schema for the review output structure.
///
/// Why: the provider uses this schema to force the model to emit a clean JSON
/// object rather than free text with a JSON block embedded in it.  This
/// eliminates the fail-safe APPROVE problem (Haiku always fail-safes; Sonnet
/// sometimes does) that occurs when the model ignores the output format
/// instruction in the system prompt.
/// What: returns a `ResponseSchema` whose `schema` field is a JSON Schema
/// object describing the `review_output` shape expected by `parse_review_response`.
/// The schema matches the fields that `LlmOutputBlock` deserializes.
/// The `grade` and `grade_justification` fields were added in 0.3.4 (#732).
/// Test: `build_review_prompt_includes_response_schema` in this module.
pub fn review_response_schema() -> ResponseSchema {
    ResponseSchema {
        name: REVIEW_SCHEMA_NAME.to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "grade": {
                    "type": "string",
                    "enum": ["A+", "A", "A-", "B+", "B", "B-", "C+", "C", "C-", "D+", "D", "D-", "F"],
                    "description": "Letter grade for overall PR quality (A+ = best, F = worst)"
                },
                "grade_justification": {
                    "type": "string",
                    "description": "One-line justification for the assigned grade"
                },
                "verdict": {
                    "type": "string",
                    "enum": ["APPROVE", "APPROVE*", "REQUEST_CHANGES", "BLOCK", "UNKNOWN"],
                    "description": "Review verdict — one of the five board grades"
                },
                "summary": {
                    "type": "string",
                    "description": "One-line summary of the review"
                },
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "body": {"type": "string"},
                            "severity": {
                                "type": "string",
                                "enum": ["low", "medium", "high", "critical"]
                            },
                            "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                            "file": {"type": "string"},
                            "line": {"type": ["integer", "null"]}
                        },
                        "required": ["title", "body"]
                    }
                }
            },
            "required": ["grade", "grade_justification", "verdict", "summary", "findings"]
        }),
    }
}

// ─── Context inputs ───────────────────────────────────────────────────────────

/// Context assembled from trusty-search, trusty-analyze, and APEX before the LLM call.
///
/// Why: the pipeline gathers context in parallel from multiple sources then
/// bundles it into a single struct for prompt construction.
/// What: all fields are optional / empty-defaulted so the pipeline degrades
/// gracefully when a source is unavailable.
/// Test: `build_review_prompt_includes_context_blocks`,
/// `prompt_includes_apex_context` (prompt_tests.rs).
#[derive(Debug, Default)]
pub struct ReviewContext {
    /// Code search results from trusty-search (may be empty if unavailable).
    pub search_results: Vec<SearchResult>,
    /// Complexity hotspots from trusty-analyze (may be empty).
    pub complexity_hotspots: Vec<ComplexityHotspot>,
    /// Code smells from trusty-analyze (may be empty).
    pub smells: Vec<Smell>,
    /// APEX/KB product spec snippets (Phase 6 PR-B, REV-420, #550).
    ///
    /// Retrieved from the configured `apex_index` using the PR title+description
    /// as the cross-query, then filtered by `apex_path_prefixes`.  Empty when
    /// APEX is disabled (`apex_index` not configured) or no matching docs are
    /// found.  Fail-open: a search error produces an empty vec, never an error.
    pub apex_results: Vec<ApexContextResult>,
    /// Coverage verdict contribution from the coverage policy (#1014).
    ///
    /// `None` when coverage gating is disabled (the default) or when no LCOV
    /// file was available.  When `Some`, the summary string is injected into the
    /// user message as an informational block so the LLM can reference it in
    /// findings; the `floor` is applied deterministically by the runner AFTER
    /// the LLM response, not by the model itself.
    pub coverage_contrib: Option<CoverageVerdictContrib>,
}

// ─── System prompt ────────────────────────────────────────────────────────────

/// Return the stock base system prompt for the reviewer role (no layering).
///
/// Why: the stock system prompt encodes the fail-safe verdict policy (spec
/// REV-130), the output format contract, and the quality bar for
/// REQUEST_CHANGES/BLOCK.  Kept as a function for backward compatibility and
/// for tests that need only the stock text.  For the full 3-layer prompt
/// (stock → principles → voice) use `build_system_prompt(voice_config)`.
/// The `coverage_gating_enabled` parameter controls whether the prompt tells
/// the model that coverage can gate the verdict (#1014).  When `false`, the
/// stock advisory text ("do not block on coverage") is preserved unchanged.
/// What: returns a static string; the output-format section uses structured
/// output language — the provider forces JSON via `response_schema` so the
/// model need not emit a fenced block.
/// Test: `system_prompt_contains_policy`, `system_prompt_coverage_gating_on`,
/// `system_prompt_coverage_gating_off`.
pub fn reviewer_system_prompt() -> &'static str {
    reviewer_system_prompt_with_coverage(false)
}

/// Build the base system prompt with optional coverage-gating language.
///
/// Why: when coverage gating is enabled, the "do not block on coverage" advisory
/// in the stock prompt becomes inaccurate (the runner WILL lower the verdict if
/// coverage is insufficient).  This function is the single source of truth for
/// both variants.
/// What: when `coverage_gating_enabled` is false, the prompt is identical to the
/// pre-#1014 stock text.  When true, the "Note but do not block on" coverage line
/// is replaced with an informational note about the coverage context block.
/// Test: `system_prompt_coverage_gating_on`, `system_prompt_coverage_gating_off`.
pub fn reviewer_system_prompt_with_coverage(coverage_gating_enabled: bool) -> &'static str {
    if coverage_gating_enabled {
        SYSTEM_PROMPT_COVERAGE_GATING
    } else {
        SYSTEM_PROMPT_STOCK
    }
}

// ─── Layered system prompt ────────────────────────────────────────────────────

/// Build the layered system prompt: stock → principles → voice.
///
/// Why: the 3-layer composition (issues #754 + #756) is the production system
/// prompt; this function is the single assembly point so callers only need to
/// supply a `VoiceConfig`.
/// What: appends principles then voice addenda to the stock base when they are
/// non-empty; a blank separator line is inserted between layers.  When
/// `voice_config` is all-None (stock-only), the output equals `reviewer_system_prompt()`.
/// `coverage_gating_enabled` selects the stock base variant (#1014): when true the
/// "do not block on coverage" advisory is replaced with an informational note.
/// Test: `build_system_prompt_stock_only`, `build_system_prompt_with_principles`,
/// `build_system_prompt_full_pipeline` in `prompt_tests.rs`.
pub fn build_system_prompt(voice_config: &VoiceConfig) -> String {
    build_system_prompt_with_coverage(voice_config, false)
}

/// Build the layered system prompt with an explicit coverage-gating flag.
///
/// Why: the runner calls this with `coverage_gating_enabled = config.coverage.enabled`
/// so the system prompt accurately reflects whether coverage can gate the verdict.
/// What: selects the stock base via `reviewer_system_prompt_with_coverage`, then
/// appends the principles and voice addenda exactly as `build_system_prompt` does.
/// Test: `build_system_prompt_coverage_gating_on`.
pub fn build_system_prompt_with_coverage(
    voice_config: &VoiceConfig,
    coverage_gating_enabled: bool,
) -> String {
    let stock = reviewer_system_prompt_with_coverage(coverage_gating_enabled);
    let addendum = voice_config.combined_addendum();
    if addendum.is_empty() {
        return stock.to_string();
    }
    format!("{stock}\n\n{addendum}")
}

// ─── Prompt builder ───────────────────────────────────────────────────────────

/// Build the `LlmRequest` for the reviewer role.
///
/// Why: centralises all prompt-assembly logic so pipeline code stays clean and
/// prompt iteration doesn't require touching pipeline logic.
/// What: assembles a layered system prompt (stock → principles → voice via
/// `voice_config`) + user message containing the PR metadata, truncated diff,
/// code search context (if any), and static-analysis annotations (if any).
/// Includes `response_schema` so the provider forces structured output via
/// Bedrock tool-use or OpenRouter json_schema.
/// `reviewer_model` may carry a `bedrock/` or `openrouter/` routing prefix;
/// this function strips it before setting `LlmRequest.model`.
/// `coverage_gating_enabled` selects the coverage-aware system prompt variant
/// (#1014): when true, the "do not block on coverage" advisory is replaced.
/// Test: `build_review_prompt_includes_diff`, `prompt_includes_context_blocks`,
/// `build_review_prompt_strips_bedrock_prefix`,
/// `build_review_prompt_includes_response_schema`,
/// `build_review_prompt_with_voice_config_principles`,
/// `build_review_prompt_with_voice_config_full`,
/// `build_review_prompt_coverage_gating_injects_block`.
// Nine arguments are required to fully specify the review (PR identity, diff,
// context, model, voice, coverage flag).  The parameter count is structural;
// splitting would make the API less ergonomic without improving cohesion.
#[allow(clippy::too_many_arguments)]
pub fn build_review_prompt(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    external_context: &str,
    reviewer_model: &str,
    voice_config: &VoiceConfig,
) -> LlmRequest {
    build_review_prompt_inner(
        owner,
        repo,
        pr_meta,
        diff,
        context,
        external_context,
        reviewer_model,
        voice_config,
        false,
    )
}

/// Build the `LlmRequest` with coverage-gating flag exposed (used by the runner).
///
/// Why: the runner calls this variant when `config.coverage.enabled` is true so
/// the system prompt reflects that coverage can gate the verdict (#1014).
/// What: identical to `build_review_prompt` but passes `coverage_gating_enabled`
/// through to `build_system_prompt_with_coverage`.
/// Test: `build_review_prompt_coverage_gating_injects_block`.
#[allow(clippy::too_many_arguments)]
pub fn build_review_prompt_with_coverage(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    external_context: &str,
    reviewer_model: &str,
    voice_config: &VoiceConfig,
    coverage_gating_enabled: bool,
) -> LlmRequest {
    build_review_prompt_inner(
        owner,
        repo,
        pr_meta,
        diff,
        context,
        external_context,
        reviewer_model,
        voice_config,
        coverage_gating_enabled,
    )
}

/// Internal implementation shared by both `build_review_prompt` variants.
///
/// Why: avoids code duplication between the public API-stable function and the
/// coverage-aware variant while keeping the public interface clean.
/// What: assembles the full `LlmRequest` from all inputs.
/// Test: covered transitively by all `build_review_prompt_*` tests.
#[allow(clippy::too_many_arguments)]
fn build_review_prompt_inner(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    external_context: &str,
    reviewer_model: &str,
    voice_config: &VoiceConfig,
    coverage_gating_enabled: bool,
) -> LlmRequest {
    let user_message = build_user_message(owner, repo, pr_meta, diff, context, external_context);
    LlmRequest {
        model: strip_provider_prefix(reviewer_model).to_string(),
        system: build_system_prompt_with_coverage(voice_config, coverage_gating_enabled),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_message,
        }],
        temperature: REVIEWER_TEMPERATURE,
        max_tokens: REVIEWER_MAX_TOKENS,
        response_schema: Some(review_response_schema()),
    }
}

/// Minimal PR metadata needed for prompt construction.
///
/// Why: avoids pulling the full `PrMetadata` struct from the GitHub integration
/// into the prompt module; the prompt only needs title, author, and PR URL.
/// What: three string fields; set to empty strings if not available (e.g. for
/// `--local-diff` mode where there is no PR).
/// Test: covered transitively by `build_review_prompt_includes_diff`.
#[derive(Debug, Default, Clone)]
pub struct ReviewPrMeta {
    /// PR title (empty string for local-diff mode).
    pub title: String,
    /// PR description / body (empty string for local-diff mode or when null).
    ///
    /// Why: the external context sources (#599 Fix 3) regex-scan the body for
    /// JIRA ticket keys and fold its prose into their keyword query, matching the
    /// incumbent's `title + "\n" + description` signal.
    pub body: String,
    /// Author login (empty string for local-diff mode).
    pub author: String,
    /// PR URL (empty string for local-diff mode).
    pub url: String,
}

impl ReviewPrMeta {
    /// Construct from a `ReviewResult` (used to create a prompt from an
    /// existing result skeleton).
    ///
    /// Why: convenience constructor for round-trip test scenarios.
    /// What: copies `pr_title`, `pr_url`, and `owner`/`repo` from the result.
    /// Test: covered transitively.
    pub fn from_result(result: &ReviewResult) -> Self {
        Self {
            title: result.pr_title.clone(),
            body: String::new(),
            author: String::new(),
            url: result.pr_url.clone(),
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

// Tests extracted to prompt_tests.rs to keep this file under the 500-line cap.
// Voice-layering tests are in prompt_voice_tests.rs (split to keep prompt_tests.rs
// under the cap after adding the voice_config parameter).

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "prompt_voice_tests.rs"]
mod voice_tests;
