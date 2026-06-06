//! Single-shot `claude` CLI dispatch for OAuth-only credential paths.
//!
//! Why: When only `CLAUDE_CODE_OAUTH_TOKEN` is configured (no OpenRouter key,
//! no Anthropic API key), the ctrl orchestrator can't reach api.anthropic.com
//! or OpenRouter directly — OAuth tokens are only valid for the `claude` CLI.
//! What: `run_pm_task_via_claude_cli`, the prompt-filtering helper
//! `filter_project_index_in_prompt`, and the post-processing helper
//! `strip_cli_artifacts`.
//! Test: `strip_cli_artifacts_*` and `filter_project_index_in_prompt_*` cover
//! the pure helpers; the CLI path itself is exercised manually.

use std::path::Path;

use anyhow::{Context, Result};

use crate::agents::AgentConfig;
use crate::events::{self, Event};

use super::state::ConversationTurn;

/// Drive a PM/ctrl turn through the local `claude` CLI subprocess (#250).
///
/// Why: When only `CLAUDE_CODE_OAUTH_TOKEN` is configured (no OpenRouter key,
/// no Anthropic API key), the ctrl orchestrator can't reach api.anthropic.com
/// or OpenRouter directly — OAuth tokens are only valid for the `claude` CLI.
/// What: Builds a single concatenated prompt (system + history + user turn)
/// and dispatches via `ClaudeCodeAgentRunner::new()` (a slim wrapper that
/// already speaks stream-json and surfaces the final result string). Tools
/// are intentionally NOT registered here: the claude CLI brings its own tool
/// surface, and trying to graft trusty-agents's `delegate_to_agent` onto it would
/// require a second Claude Max session inside the CLI's own session.
/// Test: Compilation-tested; the CLI path is exercised manually via
/// `cargo run` with only `CLAUDE_CODE_OAUTH_TOKEN` set.
pub async fn run_pm_task_via_claude_cli(
    _project_path: &Path,
    pm_cfg: &AgentConfig,
    user_input: &str,
    history: &[ConversationTurn],
    sid: &str,
) -> Result<String> {
    // #283 follow-up: latency instrumentation for the ctrl direct-CLI path.
    // Why: Helps operators correlate "spinner stuck" feelings with real
    // wall-clock spend on the claude CLI subprocess.
    let t0 = std::time::Instant::now();

    // Emit `AgentStarted` so the TUI shows `⟳ ctrl · running…` while the
    // CLI subprocess is in flight. Mirrors the pattern in `subprocess.rs`
    // and `claude_code_runner.rs` so the same UI affordance lights up
    // regardless of which dispatch path is used.
    let agent_name = pm_cfg.agent.name.clone();
    let session_id = sid.to_string();
    events::publish(Event::AgentStarted {
        session_id: session_id.clone(),
        agent_name: agent_name.clone(),
        runner_type: "claude-code".to_string(),
    });

    let runner = match crate::agents::claude_code_runner::ClaudeCodeAgentRunner::new()
        .await
        .context("ctrl: failed to locate `claude` CLI for CLAUDE_CODE_OAUTH_TOKEN routing")
    {
        Ok(r) => r,
        Err(e) => {
            events::publish(Event::AgentDone {
                session_id: session_id.clone(),
                agent: agent_name.clone(),
                status: "error".to_string(),
            });
            return Err(e);
        }
    };

    // Compose history into a single prompt — claude CLI is single-shot per
    // invocation. Prefix each turn with a clear marker so the model can read
    // the dialogue chronologically.
    let mut composed = String::new();
    for turn in history {
        composed.push_str("User: ");
        composed.push_str(&turn.user);
        composed.push_str("\n\nAssistant: ");
        composed.push_str(&turn.assistant);
        composed.push_str("\n\n");
    }
    composed.push_str("User: ");
    composed.push_str(user_input);

    // #280: Apply relevance-first project-index filtering to the embedded
    // system prompt before handing it to the claude CLI. Mirrors
    // `WorkflowEngine::run_phase`'s `filter_index_entries(.., 15)` call so
    // both dispatch paths burn the same token budget on context for the
    // same task. Graceful no-op when the prompt has no `## Project Context
    // (auto-indexed)` section.
    let mut filtered_cfg = pm_cfg.clone();
    filtered_cfg.system_prompt.content =
        filter_project_index_in_prompt(&pm_cfg.system_prompt.content, user_input, 15);

    // Build a config tweak that forces the runner to use the resolved model
    // verbatim (already set on pm_cfg).
    let result = match runner
        .run_with_config_public(&filtered_cfg, &composed)
        .await
        .context("ctrl: claude CLI invocation failed")
    {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(
                duration_ms = t0.elapsed().as_millis() as u64,
                agent = %agent_name,
                "ctrl CLI dispatch failed"
            );
            events::publish(Event::AgentDone {
                session_id: session_id.clone(),
                agent: agent_name.clone(),
                status: "error".to_string(),
            });
            return Err(e);
        }
    };

    tracing::info!(
        duration_ms = t0.elapsed().as_millis() as u64,
        agent = %agent_name,
        "ctrl CLI dispatch complete"
    );
    events::publish(Event::AgentDone {
        session_id,
        agent: agent_name,
        status: "success".to_string(),
    });

    Ok(strip_cli_artifacts(result.content))
}

/// Strip claude CLI artifacts from the end of a response.
///
/// DEFENSIVE-ONLY (Feature A): As of the conversational-output-mode rewrite,
/// the conversational agents (ctrl, izzie, cto-assistant) are configured with
/// stop_sequences and prose-only system prompts that should prevent any
/// `## Summary` block from appearing in the first place. This helper is kept
/// in place as a safety net for legacy agents and CLI subprocess outputs that
/// may still emit the artifact; do not remove it without a deprecation pass.
///
/// Why: The claude CLI appends a trailing `\n\n## Summary\n…` block to its
/// final output, plus stray trailing whitespace. The TUI renders this verbatim
/// and it makes ctrl chat replies look like build reports rather than
/// conversational answers. We strip it here so the helper has a single home
/// and unit tests can pin down the trim semantics.
/// What: Removes everything from the first occurrence of `\n\n## Summary` (or
/// a `## Summary` header at start-of-line preceded by a single newline) to end
/// of string, then trims any trailing whitespace/newlines.
/// Test: `strip_cli_artifacts_*` unit tests in `mod tests` cover the both-
/// newline form, the single-newline form, the no-summary case, and trailing
/// whitespace trimming.
pub(crate) fn strip_cli_artifacts(s: String) -> String {
    let cut = if let Some(idx) = s.find("\n\n## Summary") {
        Some(idx)
    } else if let Some(idx) = s.find("\n## Summary") {
        Some(idx)
    } else if s.starts_with("## Summary") {
        Some(0)
    } else {
        None
    };
    match cut {
        Some(idx) => s[..idx].trim_end().to_string(),
        None => s.trim_end().to_string(),
    }
}

/// Apply relevance-first filtering to the project-index section of a system
/// prompt, matching `WorkflowEngine`'s behavior (#280).
///
/// Why: The ctrl direct-CLI dispatch path (`run_pm_task_via_claude_cli`) was
/// injecting the full project-index whenever the loaded agent TOML had one
/// embedded — but the workflow engine already filters its index by task
/// keywords before injection, so the two paths burned different token budgets
/// for the same user intent. This helper closes the divergence by running
/// the same `filter_index_entries` over the section in-place.
/// What: Locates `## Project Context (auto-indexed)` in `system_prompt`,
/// extracts its body up to the next `## ` heading or `---` separator,
/// runs `filter_index_entries(body, task, top_n)`, and splices the
/// filtered body back. If the marker section isn't present the prompt
/// is returned unchanged (graceful fallback for agents that never
/// embed an index).
/// Test: `filter_project_index_in_prompt_*` unit tests below.
pub(crate) fn filter_project_index_in_prompt(
    system_prompt: &str,
    task: &str,
    top_n: usize,
) -> String {
    const HEADER: &str = "## Project Context (auto-indexed)";
    let Some(header_start) = system_prompt.find(HEADER) else {
        return system_prompt.to_string();
    };
    let body_start = header_start + HEADER.len();
    // Skip the blank line(s) after the header so the filter only sees bullet
    // entries — `filter_index_entries` preserves any leading non-bullet
    // preamble verbatim, which would re-emit the header redundantly.
    let after_header = &system_prompt[body_start..];
    let body_offset = after_header
        .char_indices()
        .find(|(_, c)| *c != '\n')
        .map(|(i, _)| i)
        .unwrap_or(after_header.len());
    let body_abs_start = body_start + body_offset;

    // Section ends at the next `## ` heading OR the next `---\n` separator,
    // whichever comes first. Both are produced by `InitContext::to_prompt_prefix`
    // and appear in the wild for the workflow-engine path. If neither marker
    // is found, the section runs to end-of-prompt.
    let tail = &system_prompt[body_abs_start..];
    let next_section = tail
        .find("\n## ")
        .map(|i| body_abs_start + i + 1) // +1 to keep the leading `\n`
        .unwrap_or(system_prompt.len());
    let next_separator = tail
        .find("\n---")
        .map(|i| body_abs_start + i + 1)
        .unwrap_or(system_prompt.len());
    let body_end = next_section.min(next_separator);

    let body = &system_prompt[body_abs_start..body_end];
    let filtered = crate::agents::context_filter::filter_index_entries(body, task, top_n);

    let mut out = String::with_capacity(system_prompt.len());
    out.push_str(&system_prompt[..body_abs_start]);
    out.push_str(&filtered);
    // Preserve a trailing newline before the next section so headings stay
    // separated; `filter_index_entries` may strip its own trailing whitespace.
    if !filtered.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&system_prompt[body_end..]);
    out
}
