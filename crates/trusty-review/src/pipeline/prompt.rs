//! Review prompt construction.
//!
//! Why: keeping the prompt text in its own module makes it easy to iterate on
//! the wording, the output format spec, and the context-block layout without
//! touching pipeline logic.  The prompt is the primary lever for review quality.
//!
//! What: exposes `build_review_prompt` which assembles the `LlmRequest` for the
//! reviewer role from the diff, PR metadata, and optional context blocks.  The
//! system prompt encodes the verdict policy (the pipeline now fails CLOSED to
//! UNKNOWN on parse/truncation errors — #1241 supersedes spec REV-130's fail-open
//! APPROVE) and the structured output format the parser expects.
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
    llm::{ChatMessage, LlmRequest, ResponseSchema, enforce_strict_mode, strip_provider_prefix},
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

/// Maximum tokens for the review response (default for most models).
const REVIEWER_MAX_TOKENS: u32 = 4096;

/// Higher output-token ceiling for Gemini models (#1241).
///
/// Why: Gemini models are noticeably more verbose in their structured JSON than
/// the OpenAI/Anthropic reviewers — at the 4096 default their `findings` array is
/// frequently cut off mid-object, which the #1241 truncation guard now (correctly)
/// converts to UNKNOWN.  Raising Gemini's ceiling to 8192 lets the full structured
/// JSON land so the review actually completes instead of failing closed.
/// What: applied by `max_tokens_for_model` when the bare slug contains `gemini`.
const GEMINI_MAX_TOKENS: u32 = 8192;

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
///
/// OpenAI strict mode (forwarded by OpenRouter for `openai/*` models with
/// `strict: true`) requires EVERY `object` node to set
/// `"additionalProperties": false` AND to list every property key in
/// `"required"`.  Rather than hand-maintain those on each nested object — the
/// omission on `findings.items` is exactly what blocked all OpenAI reviews —
/// the schema is declared in its natural shape and then made strict-compliant
/// in one pass by [`enforce_strict_mode`] (#1235).  Fields that are
/// semantically optional are expressed as nullable types (`line`) or carry a
/// safe default value the model emits; the `LlmOutputBlock` deserializer uses
/// `#[serde(default)]` so both lenient (Bedrock/Anthropic, Gemini) and strict
/// (OpenAI) responses round-trip.
/// Test: `build_review_prompt_includes_response_schema` and
/// `review_schema_is_openai_strict_compliant` in this module.
pub fn review_response_schema() -> ResponseSchema {
    let mut schema = serde_json::json!({
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
                        "line": {"type": ["integer", "null"]},
                        // `category` (#1359) is intentionally listed here so
                        // `enforce_strict_mode` adds it to `findings.items.required`.
                        // OpenAI strict mode REQUIRES every declared property to be
                        // in `required` (an omission rejects the whole request), so it
                        // MUST stay required for the `openai/*` path. Non-strict
                        // providers (Bedrock/Anthropic tool-use, Gemini) and older
                        // models that omit `category` are NOT rejected client-side:
                        // both backends funnel their structured output through the
                        // SAME `parse_review_response` serde path, where
                        // `LlmFinding.category` is `#[serde(default)]` → `Correctness`.
                        // So "required" here is a strict-mode requirement, not a
                        // hard contract that breaks lenient providers; the serde
                        // default is the cross-provider safety net. See
                        // `parse_finding_without_category_defaults_correctness`
                        // (parser_tests.rs) and the assertion in
                        // `review_schema_is_openai_strict_compliant` (prompt_tests.rs).
                        "category": {
                            "type": "string",
                            "enum": ["correctness", "method-conformance", "test-coverage"],
                            "description": "Almost always \"correctness\". Use \"method-conformance\" ONLY when the diff explicitly contradicts a method stated in the \"Intended method (ticket/spec)\" context. Use \"test-coverage\" ONLY for findings derived from an \"Unmet AC:\" snippet in the context (see test-plan-gaps instructions). Never use \"method-conformance\" for a missing method or stale-spec conflict."
                        },
                        // `consequence` (#1416) is a plain required string;
                        // pre-#1416 models / lenient providers that omit it funnel
                        // through serde where `LlmFinding.consequence` is
                        // `#[serde(default)]` → "". Empty is fine (renderer skips it).
                        "consequence": {
                            "type": "string",
                            "description": "Brief failure mechanism: what goes wrong in practice if this is NOT addressed (e.g. \"panics on empty input\", \"leaks the connection under load\"). One short phrase. Empty string if there is no concrete consequence."
                        },
                        // `suggested_replacement` (#1415) is nullable (like `line`)
                        // so it can be `null` while still satisfying OpenAI strict
                        // mode (every property must be in `required`).  Non-strict
                        // providers and pre-#1415 models simply omit it → serde
                        // default `None`. See `LlmFinding.suggested_replacement`.
                        "suggested_replacement": {
                            "type": ["string", "null"],
                            "description": "EXACT replacement code for the line(s) at `line`, suitable for a one-click GitHub suggestion block. Provide ONLY when the fix is a concrete code replacement that maps to specific line(s) at this location; otherwise null and describe the fix in `body`. Do NOT include a code fence or surrounding prose — just the literal replacement line(s)."
                        }
                    }
                }
            }
        }
    });
    // Make every object node OpenAI strict-mode compliant in one pass. This is
    // what adds `category` (and every other property) to `findings.items.required`
    // for OpenAI strict mode; non-strict providers rely on the serde default
    // instead (see the `category` note above).
    enforce_strict_mode(&mut schema);
    ResponseSchema {
        name: REVIEW_SCHEMA_NAME.to_string(),
        schema,
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
/// Why: the stock system prompt encodes the verdict policy (the pipeline fails
/// CLOSED to UNKNOWN on parse/truncation errors per #1241, which supersedes spec
/// REV-130's fail-open APPROVE), the output format contract, and the quality bar
/// for REQUEST_CHANGES/BLOCK.  Kept as a function for backward compatibility and
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
        max_tokens: max_tokens_for_model(reviewer_model),
        response_schema: Some(review_response_schema()),
    }
}

/// Pick the output-token ceiling for a given reviewer model id (#1241).
///
/// Why: a single 4096 default truncates Gemini's verbose structured JSON, which
/// the truncation guard then fails closed to UNKNOWN — the review never completes.
/// Gemini needs a larger ceiling; other models keep the leaner default.
/// What: strips any `bedrock/`/`openrouter/` routing prefix, lowercases the bare
/// slug, and returns `GEMINI_MAX_TOKENS` (8192) when it contains `gemini`, else
/// `REVIEWER_MAX_TOKENS` (4096).
/// Test: `max_tokens_gemini_is_raised`, `max_tokens_default_for_non_gemini`.
fn max_tokens_for_model(reviewer_model: &str) -> u32 {
    let bare = strip_provider_prefix(reviewer_model).to_ascii_lowercase();
    if bare.contains("gemini") {
        GEMINI_MAX_TOKENS
    } else {
        REVIEWER_MAX_TOKENS
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
