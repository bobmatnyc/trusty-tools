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
//!   Where `<VERDICT>` ∈ {"APPROVE","APPROVE*","REQUEST_CHANGES","BLOCK","N/A"}.
//!
//! Test: `build_review_prompt_includes_diff`, `system_prompt_contains_policy`,
//! `prompt_includes_context_blocks`.

use crate::{
    integrations::{
        analyze_client::{ComplexityHotspot, Smell},
        search_client::SearchResult,
    },
    llm::{ChatMessage, LlmRequest},
    models::ReviewResult,
};

// ─── Prompt constants ─────────────────────────────────────────────────────────

/// Reviewer temperature — tighter than chat for more deterministic verdicts.
const REVIEWER_TEMPERATURE: f32 = 0.3;

/// Maximum tokens for the review response.
const REVIEWER_MAX_TOKENS: u32 = 4096;

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
/// Test: `system_prompt_contains_policy`.
pub fn reviewer_system_prompt() -> &'static str {
    r#"You are a senior software engineer performing a pull-request code review.

## Verdict policy (MANDATORY)
- Your default verdict is APPROVE. You bear the burden of proof to escalate.
- REQUEST_CHANGES requires ALL THREE from the visible diff:
    (a) a specific wrong line cited verbatim,
    (b) a traceable failure path,
    (c) a concrete fix proposed.
- BLOCK is reserved for undisputed evidence of data loss, auth bypass, or
  irreversible production breakage introduced by this PR.
- When in doubt, APPROVE or APPROVE*.

## What to review
Focus on: correctness bugs, security issues, data-loss risks, logic errors.
Note but do not block on: style, minor naming, documentation gaps, test coverage.

## Output format (REQUIRED)
After your review narrative, output EXACTLY ONE JSON block as your final content,
using the following schema. Do not include any text after the JSON block.

```json
{
  "verdict": "APPROVE",
  "summary": "One sentence summary of the review.",
  "findings": [
    {
      "title": "Short finding title",
      "body": "Detailed description of the issue.",
      "severity": "low|medium|high|critical",
      "confidence": 0.85,
      "file": "src/path/to/file.rs",
      "line": 42
    }
  ]
}
```

`verdict` must be one of: APPROVE, APPROVE*, REQUEST_CHANGES, BLOCK, N/A.
`confidence` is a float in [0.0, 1.0].
`line` may be null if no specific line is applicable.
`findings` may be an empty array if there are no issues.
Emit the raw JSON block with no additional prose after it."#
}

// ─── Prompt builder ───────────────────────────────────────────────────────────

/// Build the `LlmRequest` for the reviewer role.
///
/// Why: centralises all prompt-assembly logic so pipeline code stays clean and
/// prompt iteration doesn't require touching pipeline logic.
/// What: assembles a system prompt + user message containing the PR metadata,
/// truncated diff, code search context (if any), and static-analysis annotations
/// (if any).  The user message ends with the structured-output reminder.
/// Test: `build_review_prompt_includes_diff`, `prompt_includes_context_blocks`.
pub fn build_review_prompt(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
    reviewer_model: &str,
) -> LlmRequest {
    let user_message = build_user_message(owner, repo, pr_meta, diff, context);
    LlmRequest {
        model: reviewer_model.to_string(),
        system: reviewer_system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: user_message,
        }],
        temperature: REVIEWER_TEMPERATURE,
        max_tokens: REVIEWER_MAX_TOKENS,
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
/// context sections for code search and static analysis.
/// Test: `prompt_includes_context_blocks`.
fn build_user_message(
    owner: &str,
    repo: &str,
    pr_meta: &ReviewPrMeta,
    diff: &str,
    context: &ReviewContext,
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

    // Structured-output reminder.
    msg.push_str(
        "Please review the diff above and end your response with the structured \
         JSON verdict block exactly as specified in the system prompt.\n",
    );

    msg
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> ReviewPrMeta {
        ReviewPrMeta {
            title: "Add authentication".to_string(),
            author: "alice".to_string(),
            url: "https://github.com/acme/backend/pull/42".to_string(),
        }
    }

    fn empty_context() -> ReviewContext {
        ReviewContext::default()
    }

    #[test]
    fn system_prompt_contains_policy() {
        let prompt = reviewer_system_prompt();
        assert!(
            prompt.contains("default verdict is APPROVE"),
            "system prompt must state APPROVE-default policy"
        );
        assert!(
            prompt.contains("REQUEST_CHANGES requires ALL THREE"),
            "system prompt must specify the REQUEST_CHANGES gate"
        );
        assert!(
            prompt.contains("BLOCK"),
            "system prompt must describe the BLOCK tier"
        );
        assert!(
            prompt.contains("\"verdict\""),
            "system prompt must include the JSON output schema"
        );
    }

    #[test]
    fn build_review_prompt_includes_diff() {
        let diff = "+fn hello() { println!(\"hi\"); }\n";
        let req = build_review_prompt(
            "acme",
            "backend",
            &sample_meta(),
            diff,
            &empty_context(),
            "openai/gpt-5.4-mini-20260317",
        );
        assert_eq!(req.model, "openai/gpt-5.4-mini-20260317");
        assert_eq!(req.messages.len(), 1);
        let content = &req.messages[0].content;
        assert!(
            content.contains("fn hello"),
            "user message must include the diff"
        );
        assert!(
            content.contains("acme/backend"),
            "user message must include owner/repo"
        );
        assert!(
            content.contains("Add authentication"),
            "user message must include PR title"
        );
        assert!((req.temperature - REVIEWER_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[test]
    fn prompt_includes_context_blocks() {
        use crate::integrations::search_client::SearchResult;

        let context = ReviewContext {
            search_results: vec![SearchResult {
                file: "src/auth.rs".to_string(),
                snippet: Some("pub fn verify() {}".to_string()),
                score: 0.9,
                start_line: Some(10),
                end_line: Some(12),
            }],
            complexity_hotspots: vec![ComplexityHotspot {
                file: "src/auth.rs".to_string(),
                function_name: Some("verify".to_string()),
                cyclomatic: 12,
                cognitive: 8,
            }],
            smells: vec![Smell {
                file: "src/auth.rs".to_string(),
                category: "long_method".to_string(),
                severity: "medium".to_string(),
                line: Some(20),
            }],
        };

        let req = build_review_prompt(
            "acme",
            "repo",
            &sample_meta(),
            "+fn foo() {}",
            &context,
            "openai/gpt-5.4-mini-20260317",
        );
        let content = &req.messages[0].content;
        assert!(
            content.contains("Related code"),
            "user message must include search context section"
        );
        assert!(
            content.contains("pub fn verify"),
            "user message must include search snippet"
        );
        assert!(
            content.contains("Complexity hotspots"),
            "user message must include hotspot section"
        );
        assert!(
            content.contains("Code smells"),
            "user message must include smells section"
        );
    }

    #[test]
    fn prompt_empty_context_omits_sections() {
        let req = build_review_prompt(
            "o",
            "r",
            &sample_meta(),
            "+fn x() {}",
            &empty_context(),
            "openai/gpt-5.4-nano-20260317",
        );
        let content = &req.messages[0].content;
        assert!(
            !content.contains("Related code"),
            "empty context must not include search section"
        );
        assert!(
            !content.contains("Complexity hotspots"),
            "empty context must not include hotspot section"
        );
    }

    #[test]
    fn prompt_local_diff_mode_no_pr_metadata() {
        // In --local-diff mode, pr_meta has empty fields.
        let meta = ReviewPrMeta::default();
        let req = build_review_prompt(
            "local",
            "local",
            &meta,
            "+fn local_fn() {}",
            &empty_context(),
            "openai/gpt-5.4-mini-20260317",
        );
        let content = &req.messages[0].content;
        // Must still include the diff.
        assert!(content.contains("local_fn"));
    }
}
