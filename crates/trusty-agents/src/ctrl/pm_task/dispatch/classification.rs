//! In-band workstream classification, turn persistence, and per-workstream
//! summary caching for persona chat turns (DOC-54 §9.6 / SPEC-AGENTS-08,
//! demo-day 2026-07-24).
//!
//! Why: `run_pm_task_with_persona` is already the largest dispatch body in
//! this crate (near the 500-line file cap) — the filterable-context slice
//! (in-band classification marker, workstream-tagged persistence, focused-
//! mode context assembly, periodic per-workstream summarization) is
//! deliberately pulled into this sibling module rather than grown inline,
//! mirroring how `persona.rs` already splits its tests into `persona_tests.rs`.
//! `persona.rs` calls exactly two entry points here: [`build_turn_context`]
//! (before the system prompt is built) and [`finish_turn`] (on every return
//! path, streaming and non-streaming alike).
//! What:
//!   - [`build_turn_context`] fetches the closed label vocabulary
//!     ([`crate::api::server::workstreams::list_workstream_labels_at`]),
//!     renders the in-band classification instruction block (§9.6.1), and —
//!     when a workstream is focused — assembles the focused-mode context
//!     block in the spec's stable order (§9.6.3): global summary (a stub —
//!     no cheap global-summary source exists yet in this slice, honestly
//!     labeled) → per-workstream summary (cached, `ws-summary:<label>` tag)
//!     → last `recent_window` raw turns (`ws:<label>` tag).
//!   - [`finish_turn`] parses the trailing `[[task: <label>]]` /
//!     `[[task: new: <label>]]` marker out of the raw response, persists
//!     the turn as a `ws:<label>`-tagged drawer, refreshes the cached
//!     per-workstream summary every `summarize_every` turns (one extra LLM
//!     call on the SAME credentials/model — `llm::chat_adapter_aware`, no
//!     new provider plumbing), and applies the task-bleed nudge (§9.6.4): a
//!     gentle log line + response-appended note when the turn's honest
//!     classification differs from the focused workstream — never forced,
//!     never dropped.
//! Deferred (documented, not implemented in this slice): lazy summary
//! invalidation on manual re-tag (§9.6.2 — re-tagging is a future sidebar
//! affordance, not built here); a real global prompt-history summary source
//! (currently an honestly-labeled stub); periodic near-duplicate label
//! consolidation (§9.6.1).
//! KNOWN GAP (found during live demo-day verification, not fixed here —
//! filed as a follow-up): [`maybe_summarize_workstream`]'s one-shot call
//! goes through `llm::chat_adapter_aware`/`single_turn::chat`, which only
//! special-cases Bedrock — an `ollama/`-prefixed persona model (unlike the
//! main turn, which goes through the ollama-aware `chat_with_tools_gated`/
//! streaming paths) fails this call with "OpenRouter chat request failed".
//! The failure is caught and logged (`finish_turn` never propagates it —
//! the turn's own response/persistence is unaffected), so this degrades to
//! "summary cache stays stale" rather than breaking the turn. Works
//! correctly for any OpenRouter/Bedrock/Anthropic-direct-routed persona
//! (the product-policy default, per DOC-54's provider decision log).
//! Test: `parse_marker_*`, `classification_block_*`, `bleed_nudge_*`,
//! `should_refresh_summary_*` (pure), `build_turn_context_*` /
//! `finish_turn_*` (hermetic, `classification_tests.rs`).

use std::path::Path;

use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};

use crate::agents::AgentConfig;
use crate::api::server::workstreams::{self, workstream_summary_tag, workstream_tag};
use crate::llm;
use crate::memory::trusty_client::default_trusty_url;

/// Opening delimiter of the trailing in-band classification marker
/// (DOC-54 §9.6.1). Chosen to be vanishingly unlikely inside normal prose so
/// [`parse_marker`] never mis-fires on ordinary double-bracket text.
const MARKER_OPEN: &str = "[[task: ";
const MARKER_CLOSE: &str = "]]";
/// The closed-vocabulary escape hatch prefix — `new: <label>` inside the
/// marker signals a deliberate new workstream rather than a drift-prone
/// free-form label.
const NEW_PREFIX: &str = "new: ";

/// A parsed classification decision for one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classification {
    pub(crate) label: String,
    pub(crate) is_new: bool,
}

/// Parse the trailing `[[task: <label>]]` / `[[task: new: <label>]]` marker
/// out of a raw LLM response.
///
/// What: Scans from the LAST occurrence of [`MARKER_OPEN`] so a label that
/// coincidentally echoes the marker syntax earlier in the response (e.g. the
/// model explaining the convention) doesn't get mistaken for the real
/// decision. Returns `(display_text, None)` unchanged when no well-formed
/// marker is found — a persona that forgets the marker still answers the
/// user; the turn just isn't classified/persisted (logged by the caller).
/// Test: `parse_marker_extracts_existing_label`,
/// `parse_marker_extracts_new_label`, `parse_marker_missing_is_none`,
/// `parse_marker_strips_surrounding_whitespace`,
/// `parse_marker_ignores_earlier_lookalike_text`.
pub(crate) fn parse_marker(raw: &str) -> (String, Option<Classification>) {
    let Some(open_idx) = raw.rfind(MARKER_OPEN) else {
        return (raw.trim_end().to_string(), None);
    };
    let after_open = &raw[open_idx + MARKER_OPEN.len()..];
    let Some(close_rel) = after_open.find(MARKER_CLOSE) else {
        return (raw.trim_end().to_string(), None);
    };
    let inner = after_open[..close_rel].trim();
    if inner.is_empty() {
        return (raw.trim_end().to_string(), None);
    }
    let display = raw[..open_idx].trim_end().to_string();
    let (label, is_new) = match inner.strip_prefix(NEW_PREFIX) {
        Some(new_label) => (new_label.trim().to_string(), true),
        None => (inner.to_string(), false),
    };
    if label.is_empty() {
        return (display, None);
    }
    (display, Some(Classification { label, is_new }))
}

/// Render the compact in-band classification instruction block (DOC-54
/// §9.6.1) appended to the persona's system prompt on every turn.
///
/// What: Presents the closed vocabulary of existing labels plus the
/// explicit `new: <label>` escape, and the exact marker syntax
/// [`parse_marker`] expects — the two are deliberately coupled so a prompt
/// edit can't silently drift out of sync with the parser.
/// Test: `classification_block_lists_existing_labels`,
/// `classification_block_empty_labels_still_offers_new`.
pub(crate) fn classification_block(labels: &[String]) -> String {
    let vocab = if labels.is_empty() {
        "(none yet)".to_string()
    } else {
        labels.join(", ")
    };
    format!(
        "## Task classification\nExisting task labels for this agent: {vocab}.\n\
         End your response with exactly one trailing line classifying this turn:\n\
         `{MARKER_OPEN}<label>{MARKER_CLOSE}` to reuse an existing label, or \
         `{MARKER_OPEN}{NEW_PREFIX}<label>{MARKER_CLOSE}` to start a new one. \
         Pick honestly based on what the turn is actually about — never force-fit \
         a turn into a label just because it's focused."
    )
}

/// Gentle task-bleed nudge text (DOC-54 §9.6.4) appended to the displayed
/// response when a turn's honest classification differs from the focused
/// workstream. Never forces or drops the turn — the classification stands.
/// Test: `bleed_nudge_none_when_matching`, `bleed_nudge_some_when_different`.
pub(crate) fn bleed_nudge(focused: Option<&str>, classified: &str) -> Option<String> {
    let focused = focused?;
    if focused == classified {
        return None;
    }
    Some(format!(
        "\n\n_(This looks like a different task — \"{classified}\", not the focused \"{focused}\". \
         Noted honestly rather than forced into the focused task.)_"
    ))
}

/// Precomputed per-turn context assembled BEFORE the system prompt is built.
pub(crate) struct TurnContext {
    /// Instruction block appended to the system prompt on every turn.
    pub(crate) classification_block: String,
    /// The workstream this turn is focused on, if any (`SessionOverrides::focused_workstream`).
    pub(crate) focused_label: Option<String>,
    /// Focused-mode assembled context block (global + per-WS summary +
    /// recent turns), appended to the system prompt only when focused.
    pub(crate) focused_context_block: Option<String>,
}

/// Build [`TurnContext`] for one persona turn (DOC-54 §9.6.1/§9.6.3).
///
/// What: Fetches the closed label vocabulary and renders the classification
/// block unconditionally. When `focused` is `Some`, additionally assembles
/// the focused-mode block in the spec's stable order: global summary (stub)
/// → per-workstream summary (cached) → last `recent_window` raw turns.
/// Fails open exactly like every other trusty-memory read in this crate — an
/// unreachable daemon yields empty labels/history, never an error.
/// Test: `build_turn_context_unfocused_has_no_context_block`,
/// `build_turn_context_focused_assembles_stable_order`.
pub(crate) async fn build_turn_context(
    project_path: &Path,
    focused: Option<&str>,
    recent_window: usize,
) -> TurnContext {
    let base_url = default_trusty_url();
    let labels = workstreams::list_workstream_labels_at(project_path, &base_url).await;
    let classification_block = classification_block(&labels);

    let focused_context_block = match focused {
        Some(label) => {
            let block = assemble_focused_block(project_path, &base_url, label, recent_window).await;
            tracing::info!(
                label = %label,
                recent_window,
                block_chars = block.len(),
                "focused-mode context assembled"
            );
            tracing::debug!(label = %label, block = %block, "focused-mode context block (full)");
            Some(block)
        }
        None => None,
    };

    TurnContext {
        classification_block,
        focused_label: focused.map(str::to_string),
        focused_context_block,
    }
}

/// Assemble the focused-mode context block in DOC-54 §9.6.3's stable order.
async fn assemble_focused_block(
    project_path: &Path,
    base_url: &str,
    label: &str,
    recent_window: usize,
) -> String {
    // Global summary: no cheap always-available global prompt-history
    // summary source exists in this slice yet (deferred — see module doc);
    // an honestly-labeled stub preserves the spec's "agent has full memory
    // access" framing without fabricating content.
    let global_summary =
        "(global summary not yet wired in this slice — full memory remains queryable on demand)";

    let ws_summary =
        workstreams::drawers_by_tag_at(project_path, base_url, &workstream_summary_tag(label), 1)
            .await;
    let ws_summary_text = ws_summary
        .first()
        .map(|h| h.content.as_str())
        .unwrap_or("(no summary yet for this task)");

    let recent = workstreams::drawers_by_tag_at(
        project_path,
        base_url,
        &workstream_tag(label),
        recent_window,
    )
    .await;
    let recent_text = if recent.is_empty() {
        "(no prior turns recorded for this task yet)".to_string()
    } else {
        recent
            .iter()
            .rev()
            .map(|h| h.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    format!(
        "## Focused task: \"{label}\"\nWorking on task \"{label}\" — history below is filtered to this \
         task; your full memory (all tasks) remains queryable on demand.\n\n\
         ### Global summary\n{global_summary}\n\n\
         ### Task summary\n{ws_summary_text}\n\n\
         ### Recent turns on this task\n{recent_text}"
    )
}

/// Finish a persona turn: parse the classification marker, persist the
/// turn, maybe refresh the workstream summary, apply the task-bleed nudge,
/// and return the text to display to the user.
///
/// What: Called from every `run_pm_task_with_persona` return path
/// (streaming and non-streaming) so classification/persistence happen
/// exactly once regardless of which path produced the raw response. A
/// response with no parseable marker is returned unchanged (logged, not
/// persisted) — a persona that forgets the marker still answers the user.
/// Test: `finish_turn_no_marker_returns_unchanged`,
/// `finish_turn_persists_and_returns_display_text`,
/// `finish_turn_bleed_nudge_appended_when_focused_mismatch`.
pub(crate) async fn finish_turn(
    project_path: &Path,
    persona_name: &str,
    client: &Client<OpenAIConfig>,
    persona_cfg: &AgentConfig,
    user_input: &str,
    raw_response: String,
    ctx: &TurnContext,
) -> Result<String> {
    let (mut display, classification) = parse_marker(&raw_response);
    let Some(classification) = classification else {
        tracing::warn!(
            persona = %persona_name,
            "run_pm_task_with_persona: no classification marker found; turn not tagged"
        );
        return Ok(display);
    };

    tracing::info!(
        persona = %persona_name,
        label = %classification.label,
        is_new = classification.is_new,
        focused = ?ctx.focused_label,
        "workstream classification decision"
    );

    if let Some(nudge) = bleed_nudge(ctx.focused_label.as_deref(), &classification.label) {
        tracing::info!(
            persona = %persona_name,
            focused = ?ctx.focused_label,
            classified = %classification.label,
            "task bleed: turn classified outside the focused workstream"
        );
        display.push_str(&nudge);
    }

    let base_url = default_trusty_url();
    let content =
        format!("### {persona_name}\n\n**User:** {user_input}\n\n**Assistant:** {display}");
    if let Err(e) = workstreams::create_tagged_drawer_at(
        project_path,
        &base_url,
        &content,
        vec![workstream_tag(&classification.label)],
    )
    .await
    {
        tracing::warn!(persona = %persona_name, error = %e, "failed to persist workstream-tagged turn");
    }

    if let Err(e) = maybe_summarize_workstream(
        project_path,
        &base_url,
        &classification.label,
        client,
        persona_cfg,
    )
    .await
    {
        tracing::warn!(persona = %persona_name, error = %e, "workstream summary refresh failed");
    }

    Ok(display)
}

/// Upper bound on turns scanned when deciding whether a summary refresh is
/// due — bounds worst-case request size for a very long-lived workstream
/// (mirrors `WORKSTREAM_SCAN_LIMIT`'s rationale in `api::server::workstreams`).
const SUMMARY_SCAN_LIMIT: usize = 200;

/// Pure cadence decision for [`maybe_summarize_workstream`] — whether a
/// summary refresh is due for a workstream currently at `turn_count` turns.
///
/// What: `false` when disabled, cadence is 0, there are no turns yet, or
/// `turn_count` isn't an exact multiple of `summarize_every`. Pulled out as
/// a pure function (mirroring `persona::persona_max_turns`) so the cadence
/// logic is unit-testable without a mock daemon or LLM call.
/// Test: `should_refresh_summary_skips_when_disabled`,
/// `should_refresh_summary_skips_zero_cadence`,
/// `should_refresh_summary_skips_off_cadence`,
/// `should_refresh_summary_skips_zero_turns`,
/// `should_refresh_summary_true_on_cadence_boundary`.
fn should_refresh_summary(enabled: bool, summarize_every: u32, turn_count: usize) -> bool {
    if !enabled || summarize_every == 0 || turn_count == 0 {
        return false;
    }
    turn_count.is_multiple_of(summarize_every as usize)
}

/// Refresh the cached `ws-summary:<label>` drawer every `summarize_every`
/// turns (DOC-54 §9.6.2), per [`should_refresh_summary`]'s cadence decision.
/// Lazy invalidation on manual re-tag is deferred (module doc).
async fn maybe_summarize_workstream(
    project_path: &Path,
    base_url: &str,
    label: &str,
    client: &Client<OpenAIConfig>,
    persona_cfg: &AgentConfig,
) -> Result<()> {
    let cfg = &persona_cfg.workstreams;
    if !cfg.enabled || cfg.summarize_every == 0 {
        return Ok(());
    }
    let turns = workstreams::drawers_by_tag_at(
        project_path,
        base_url,
        &workstream_tag(label),
        SUMMARY_SCAN_LIMIT,
    )
    .await;
    let count = turns.len();
    if !should_refresh_summary(cfg.enabled, cfg.summarize_every, count) {
        return Ok(());
    }

    let window = &turns[..(cfg.summarize_every as usize).min(turns.len())];
    let joined = window
        .iter()
        .rev()
        .map(|h| h.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let system = "Summarize this workstream's recent turns in 3-5 sentences: capture decisions \
                  made, open questions, and current status. Plain prose only, no preamble.";
    let resp = llm::chat_adapter_aware(
        client,
        &persona_cfg.agent.model,
        system,
        &joined,
        0.2,
        400,
        Vec::new(),
    )
    .await?;
    let Some(summary_text) = resp.content else {
        anyhow::bail!("workstream summary call returned no content");
    };

    workstreams::create_tagged_drawer_at(
        project_path,
        base_url,
        &summary_text,
        vec![workstream_summary_tag(label)],
    )
    .await?;
    tracing::info!(label = %label, turn_count = count, "workstream summary refreshed");
    Ok(())
}

#[cfg(test)]
#[path = "classification_tests.rs"]
mod tests;
