//! User-turn message builder for the review prompt.
//!
//! Why: extracted from `prompt.rs` to keep that file under the 500-line cap
//! (#610) after the coverage-gating additions (#1014) caused it to grow.
//! What: the single public-within-pipeline function `build_user_message` formats
//! the PR identity, diff, and all context blocks into the user-turn text.
//! Test: `prompt_includes_context_blocks`, `prompt_includes_external_context`
//! in `prompt_tests.rs`.

use super::prompt::{ReviewContext, ReviewPrMeta};

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
pub(super) fn build_user_message(
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

    // APEX product-spec context block (Phase 6 PR-B, REV-420).
    // Each result is a snippet from the spec/docs corpus that semantically
    // matches the PR content.  Cite format: [apex: `path:line` — "excerpt"].
    if !context.apex_results.is_empty() {
        msg.push_str("## Related APEX product specs\n\n");
        // defensive: apex_results already capped in fetch_apex_context; guard against future refactors
        for (i, apex) in context
            .apex_results
            .iter()
            .enumerate()
            .take(crate::config::constants::MAX_APEX_RESULTS)
        {
            let line_suffix = apex.start_line.map(|l| format!(":{l}")).unwrap_or_default();
            msg.push_str(&format!(
                "### APEX {} — `{}{}`\n",
                i + 1,
                apex.file,
                line_suffix
            ));
            if !apex.snippet.is_empty() {
                msg.push_str("```\n");
                msg.push_str(&apex.snippet);
                if !apex.snippet.ends_with('\n') {
                    msg.push('\n');
                }
                msg.push_str("```\n");
            }
            msg.push('\n');
        }
        msg.push_str(
            "When citing an APEX spec, use the format: \
             [apex: `path/to/spec.md:15` — \"brief excerpt\"]\n\n",
        );
    }

    // External context block (rendered `## Related <source>` markdown from the
    // context orchestrator — JIRA / Confluence / GitHub Issues).
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

    // Coverage context block (#1014): inject the coverage summary when available.
    // The runner applies the deterministic floor AFTER the LLM call; we surface
    // the numbers here so the LLM can reference them in findings if relevant.
    if let Some(ref cov) = context.coverage_contrib {
        msg.push_str("## Test coverage context\n\n");
        msg.push_str(&cov.summary);
        msg.push_str("\n\n");
    }

    // Structured-output instruction (schema-enforced; no need to emit a fence).
    msg.push_str(
        "Please review the diff above and populate the structured response \
         fields (verdict, summary, findings) as specified in the system prompt.\n",
    );

    msg
}
