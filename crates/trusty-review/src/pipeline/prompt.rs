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
    integrations::{
        analyze_client::{ComplexityHotspot, Smell},
        search_client::SearchResult,
    },
    llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix},
    models::ReviewResult,
};

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
/// Test: `build_review_prompt_includes_response_schema` in this module.
pub fn review_response_schema() -> ResponseSchema {
    ResponseSchema {
        name: REVIEW_SCHEMA_NAME.to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
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
            "required": ["verdict", "summary", "findings"]
        }),
    }
}

// ─── Context inputs ───────────────────────────────────────────────────────────

/// Context assembled from trusty-search and trusty-analyze before the LLM call.
///
/// Why: the pipeline gathers context in parallel from multiple sources then
/// bundles it into a single struct for prompt construction.
/// What: all fields are optional / empty-defaulted so the pipeline degrades
/// gracefully when a source is unavailable.
/// Test: `build_review_prompt_includes_context_blocks`.
#[derive(Debug, Default)]
pub struct ReviewContext {
    /// Code search results from trusty-search (may be empty if unavailable).
    pub search_results: Vec<SearchResult>,
    /// Complexity hotspots from trusty-analyze (may be empty).
    pub complexity_hotspots: Vec<ComplexityHotspot>,
    /// Code smells from trusty-analyze (may be empty).
    pub smells: Vec<Smell>,
}

// ─── System prompt ────────────────────────────────────────────────────────────

/// Return the system prompt for the reviewer role.
///
/// Why: the system prompt encodes the fail-safe verdict policy (spec REV-130),
/// the output format contract, and the quality bar for REQUEST_CHANGES/BLOCK.
/// Keeping it as a function (not a constant) allows conditional sections in the
/// future (e.g. copilot-mode conditioning from spec REV-104).
/// What: returns a static string; future versions may accept a context param.
/// The output-format section no longer instructs the model to emit a fenced
/// JSON block — with forced structured output enabled (via `response_schema`),
/// the provider guarantees the response IS the JSON object, so fence-based
/// instructions are unnecessary and would confuse models that try to
/// literally wrap output in backticks.
/// Test: `system_prompt_contains_policy`.
pub fn reviewer_system_prompt() -> &'static str {
    r#"You are a senior software engineer performing a pull-request code review.

## Verdict grades (MANDATORY — pick exactly one)

| Grade           | Severity anchor | When to use |
|-----------------|-----------------|-------------|
| BLOCK           | Critical        | Compile error introduced by this diff, data corruption, security/auth bypass. Use BLOCK whenever this diff BREAKS something that was working. |
| REQUEST_CHANGES | High            | Confirmed correctness bug, silent data loss, missing required migration/backfill, resource leak, unhandled exception path with real failure consequence. |
| APPROVE*        | Medium          | Advisory concern the author may reasonably disagree with; the code ships but you want the note on record. |
| APPROVE         | Low / none      | No significant concerns; the change is clean and correct. |
| UNKNOWN         | —               | The diff was too truncated, context-free, or otherwise insufficient to assess. Use this instead of guessing. |

- Your default verdict is APPROVE. You bear the burden of proof to escalate.
- APPROVE* requires at least one Medium finding. Do not emit APPROVE* with only Low findings.
- REQUEST_CHANGES requires ALL THREE: (a) a specific wrong line cited verbatim,
  (b) a traceable failure path, (c) a concrete fix proposed.
- Do NOT emit UNKNOWN just because the PR is large; use it only when you
  genuinely cannot tell if the change is correct.
- **Do not under-rate a clearly blocking issue as advisory.** If it would break
  a build or corrupt data in production, assign it severity=critical and
  verdict=BLOCK.

## Compile-break rule (CRITICAL)
If the diff REMOVES a symbol (enum value, method, constant, field, function
signature change) AND the same diff still shows remaining references or
call-sites to that removed symbol elsewhere in the codebase, that is a
compile-time regression.  Assign the finding severity=critical and
verdict=BLOCK.  No other context (e.g. "it might be unused") softens this.

## Severity anchors for findings
Every finding MUST have a `severity` from:
- **critical** — compile error, data corruption, security bypass, auth failure.
- **high**     — confirmed correctness bug, silent data loss, unhandled exception
  path, missing required migration, resource leak with real consequence.
- **medium**   — advisory: code smell, suboptimal pattern, minor risk, the author
  may reasonably disagree.
- **low**      — cosmetic, documentation gap, style preference.

## What to review
Focus on: correctness bugs, security issues, data-loss risks, logic errors.
Note but do not block on: style, minor naming, documentation gaps, test coverage.

## Output (REQUIRED — populate the structured response fields)
- `verdict`: one of APPROVE, APPROVE*, REQUEST_CHANGES, BLOCK, UNKNOWN.
- `summary`: one sentence summary of the review.
- `findings`: array of issues found (empty array if none).
  Each finding has: title, body (detailed description), severity (low/medium/high/critical),
  confidence (0.0–1.0), file (source file path), line (null if not applicable).

`confidence` is a float in [0.0, 1.0].
`line` may be null if no specific line is applicable.
`findings` may be an empty array if there are no issues."#
}

// ─── Prompt builder ───────────────────────────────────────────────────────────

/// Build the `LlmRequest` for the reviewer role.
///
/// Why: centralises all prompt-assembly logic so pipeline code stays clean and
/// prompt iteration doesn't require touching pipeline logic.
/// What: assembles a system prompt + user message containing the PR metadata,
/// truncated diff, code search context (if any), and static-analysis annotations
/// (if any).  Includes `response_schema` so the provider forces structured
/// output via Bedrock tool-use or OpenRouter json_schema — eliminating the
/// silent fail-safe APPROVE problem.
/// `reviewer_model` may carry a `bedrock/` or `openrouter/` routing prefix;
/// this function strips it before setting `LlmRequest.model` so the bare id is
/// what reaches the provider's API (the prefix is used only for routing).
/// Test: `build_review_prompt_includes_diff`, `prompt_includes_context_blocks`,
/// `build_review_prompt_strips_bedrock_prefix`,
/// `build_review_prompt_includes_response_schema`.
pub fn build_review_prompt(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    external_context: &str,
    reviewer_model: &str,
) -> LlmRequest {
    let user_message = build_user_message(owner, repo, pr_meta, diff, context, external_context);
    LlmRequest {
        model: strip_provider_prefix(reviewer_model).to_string(),
        system: reviewer_system_prompt().to_string(),
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
            author: String::new(),
            url: result.pr_url.clone(),
        }
    }
}

/// Build the user-turn message for the review prompt.
///
/// Why: the user message carries all the review input: PR identity, diff, and
/// context blocks.  Splitting it from the system prompt makes each independently
/// tweakable.
/// What: formats PR metadata as a header, then the diff block, then optional
/// context sections for code search and static analysis, then any external
/// context (`## Related <source>` markdown already rendered by the context
/// orchestrator — JIRA / Confluence / GitHub Issues; APEX in PR-B).
/// Test: `prompt_includes_context_blocks`, `prompt_includes_external_context`.
fn build_user_message(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    external_context: &str,
) -> String {
    let mut msg = String::with_capacity(diff.len() + 2048);

    // PR header.
    msg.push_str(&format!("## PR: {owner}/{repo}"));
    if !pr_meta.title.is_empty() {
        msg.push_str(&format!(" — {}", pr_meta.title));
    }
    msg.push('\n');
    if !pr_meta.author.is_empty() {
        msg.push_str(&format!("Author: @{}\n", pr_meta.author));
    }
    if !pr_meta.url.is_empty() {
        msg.push_str(&format!("URL: {}\n", pr_meta.url));
    }
    msg.push('\n');

    // Diff block.
    msg.push_str("## Unified diff\n\n");
    msg.push_str("```diff\n");
    msg.push_str(diff);
    if !diff.ends_with('\n') {
        msg.push('\n');
    }
    msg.push_str("```\n\n");

    // Code search context block.
    if !context.search_results.is_empty() {
        msg.push_str("## Related code (from trusty-search)\n\n");
        for (i, result) in context.search_results.iter().enumerate().take(10) {
            msg.push_str(&format!("### Context {} — {}\n", i + 1, result.file));
            if let Some(ref snippet) = result.snippet {
                msg.push_str("```\n");
                msg.push_str(snippet);
                if !snippet.ends_with('\n') {
                    msg.push('\n');
                }
                msg.push_str("```\n");
            }
            msg.push('\n');
        }
    }

    // Static-analysis context block.
    if !context.complexity_hotspots.is_empty() {
        msg.push_str("## Complexity hotspots (from trusty-analyze)\n\n");
        for h in context.complexity_hotspots.iter().take(5) {
            let fn_part = h
                .function_name
                .as_deref()
                .map(|f| format!(" `{f}`"))
                .unwrap_or_default();
            msg.push_str(&format!(
                "- `{}`{fn_part}: cyclomatic={}, cognitive={}\n",
                h.file, h.cyclomatic, h.cognitive
            ));
        }
        msg.push('\n');
    }

    if !context.smells.is_empty() {
        msg.push_str("## Code smells (from trusty-analyze)\n\n");
        for s in context.smells.iter().take(10) {
            let line_part = s.line.map(|l| format!(" (line {l})")).unwrap_or_default();
            msg.push_str(&format!(
                "- `{}` — {} [{}]{line_part}\n",
                s.file, s.category, s.severity
            ));
        }
        msg.push('\n');
    }

    // External context block (rendered `## Related <source>` markdown from the
    // context orchestrator — JIRA / Confluence / GitHub Issues; APEX in PR-B).
    // It is appended verbatim because the orchestrator already owns the heading
    // + bullet format, keeping this builder source-agnostic.
    let external = external_context.trim();
    if !external.is_empty() {
        msg.push_str(external);
        if !external.ends_with('\n') {
            msg.push('\n');
        }
        msg.push('\n');
    }

    // Structured-output instruction (schema-enforced; no need to emit a fence).
    msg.push_str(
        "Please review the diff above and populate the structured response \
         fields (verdict, summary, findings) as specified in the system prompt.\n",
    );

    msg
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to prompt_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
