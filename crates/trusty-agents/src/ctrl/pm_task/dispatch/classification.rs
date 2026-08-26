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
//!   - [`build_turn_context`] — when `[workstreams].enabled` (a REAL master
//!     switch, #3840 critic HIGH-2) — fetches the closed label vocabulary
//!     ([`crate::api::server::workstreams::list_workstream_labels_at`]),
//!     renders the in-band classification instruction block (§9.6.1), and —
//!     when a workstream is focused — assembles the focused-mode context
//!     block in the spec's stable order (§9.6.3): global summary (a stub —
//!     no cheap global-summary source exists yet in this slice, honestly
//!     labeled) → per-workstream summary (cached, `ws-summary:<label>` tag)
//!     → last `recent_window` raw turns (`ws:<label>` tag). Disabled = no
//!     vocabulary fetch, no classification block, no focused assembly.
//!   - [`finish_turn`] parses the trailing `[[task: <label>]]` /
//!     `[[task: new: <label>]]` marker out of the raw response — anchored to
//!     the response's own last line, with the inner label charset-validated
//!     ([`is_valid_label`]) before it is ever treated as real (#3840 critic
//!     HIGH-1) — applies the task-bleed nudge (§9.6.4: a gentle log line +
//!     response-appended note when the turn's honest classification differs
//!     from the focused workstream, never forced/dropped), then returns the
//!     display text to the caller IMMEDIATELY. Persisting the turn as a
//!     `ws:<label>`-tagged drawer and refreshing the cached per-workstream
//!     summary every `summarize_every` turns (one extra LLM call on the SAME
//!     credentials/model — `llm::chat_adapter_aware`, no new provider
//!     plumbing) both happen in a detached `tokio::spawn`ed task AFTER the
//!     response is returned (#3840 critic HIGH-4 — a cadence turn used to
//!     make the user wait through a second full LLM round trip before
//!     seeing their answer).
//! Deferred (documented, not implemented in this slice): lazy summary
//! invalidation on manual re-tag (§9.6.2 — re-tagging is a future sidebar
//! affordance, not built here); a real global prompt-history summary source
//! (currently an honestly-labeled stub); periodic near-duplicate label
//! consolidation (§9.6.1); an unbounded vocabulary in the classification
//! block (no label-count/char cap — filed as a follow-up, see PR body);
//! `should_refresh_summary`'s turn count derives from a
//! [`SUMMARY_SCAN_LIMIT`]-capped fetch, so a workstream past that many turns
//! sticks at the cap and re-fires the summary EVERY turn instead of on
//! cadence (filed as a follow-up, see PR body — needs a real stored turn
//! counter, not a capped re-scan).
//! Live-verified end to end on OpenRouter/Sonnet (the product-policy
//! default, per DOC-54's provider decision log) against a real
//! trusty-memory daemon: classification, `ws:`-tagged persistence,
//! `/api/workstreams` rollup, focused-mode assembly in stable order, the
//! task-bleed nudge, and the `summarize_every` cadence (including a real
//! generated `ws-summary:<label>` drawer at the turn-5 boundary) all
//! confirmed working — see the PR body for full transcripts.
//! NOTE: [`maybe_summarize_workstream`]'s one-shot summary call
//! (`llm::chat_adapter_aware`/`single_turn::chat`) only special-cases
//! Bedrock, not an `ollama/`-prefixed persona model — that combination
//! fails with an OpenRouter error (caught and logged; the turn's own
//! response/persistence is unaffected, only the summary cache stays stale).
//! Moot under the current "no Ollama" operating policy; not fixed here.
//! Test: `parse_marker_*`, `is_valid_label_*`, `classification_block_*`,
//! `bleed_nudge_*`, `should_refresh_summary_*` (pure), `build_turn_context_*`
//! (hermetic), `finish_turn_*` (hermetic + mock-daemon,
//! `classification_tests.rs`).

use std::path::Path;

use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};

use crate::agents::AgentConfig;
use crate::api::server::workstreams::{self, workstream_summary_tag, workstream_tag};
use crate::llm;

/// Opening delimiter of the trailing in-band classification marker
/// (DOC-54 §9.6.1). Chosen to be vanishingly unlikely inside normal prose so
/// [`parse_marker`] never mis-fires on ordinary double-bracket text.
const MARKER_OPEN: &str = "[[task: ";
const MARKER_CLOSE: &str = "]]";
/// The closed-vocabulary escape hatch prefix — `new: <label>` inside the
/// marker signals a deliberate new workstream rather than a drift-prone
/// free-form label.
const NEW_PREFIX: &str = "new: ";
/// Maximum accepted label length — matches trusty-memory's own
/// `is_valid_workstream_name` bound (DOC-53 §4.3) so a classification-minted
/// label is never rejected downstream for being too long.
const LABEL_MAX_LEN: usize = 64;

/// A parsed classification decision for one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classification {
    pub(crate) label: String,
    pub(crate) is_new: bool,
}

/// Validate a candidate label against the charset allow-list before it is
/// EVER treated as a real classification (#3840 critic HIGH-1).
///
/// Why: the marker's inner text is untrusted model output — a persona
/// echoing the marker SYNTAX back verbatim (e.g. answering "how does
/// classification work?" with an example ending in `[[task: <label>]]`)
/// must never have that literal placeholder persisted as a real `ws:<label>`
/// trusty-memory tag. Gating on a strict charset allow-list, not just
/// "non-empty", closes that hole.
/// What: non-empty, at most [`LABEL_MAX_LEN`] bytes, matches
/// `^[a-z0-9-]{1,64}$` — lowercase ASCII letters, digits, and hyphens only.
/// Test: `is_valid_label_accepts_kebab_case`,
/// `is_valid_label_rejects_placeholder_and_unsafe_text`.
fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= LABEL_MAX_LEN
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parse the trailing `[[task: <label>]]` / `[[task: new: <label>]]` marker
/// out of a raw LLM response (#3840 critic HIGH-1 hardening).
///
/// Why: the marker must be ANCHORED to the end of the response — only the
/// last line of the (trimmed) text is ever considered a classification
/// attempt — rather than a bare `rfind` anywhere in the string. A bare
/// `rfind` had two failure modes: (1) a persona explaining the marker syntax
/// mid-prose (e.g. `` the format is `[[task: <label>]]` `` followed by more
/// prose) could have that lookalike text mistaken for the real decision, and
/// (2) when a persona emitted the marker TWICE, the first occurrence stayed
/// visible in the displayed text (only content before the LAST occurrence
/// was stripped). Anchoring to "last line only" plus looping backward over
/// CONSECUTIVE trailing marker-shaped lines fixes both: every trailing
/// marker-shaped line is stripped from the display, and the LAST one
/// (closest to the true end of the response) is authoritative — documented
/// choice, not the first. A trailing line that structurally matches the
/// marker syntax but whose inner label fails [`is_valid_label`] (e.g. a
/// literal `<label>` placeholder) is still stripped from display (the model
/// clearly attempted a classification) but yields `None` — logged by the
/// caller, never persisted as a tag.
/// What: Returns `(display_text, None)` unchanged when the LAST line isn't a
/// well-formed marker at all (not anchored — e.g. marker syntax appears only
/// mid-sentence, not as the response's own trailing line).
/// Test: `parse_marker_extracts_existing_label`,
/// `parse_marker_extracts_new_label`, `parse_marker_missing_is_none`,
/// `parse_marker_strips_surrounding_whitespace`,
/// `parse_marker_ignores_earlier_lookalike_text`,
/// `parse_marker_ignores_trailing_syntax_echo_mid_sentence`,
/// `parse_marker_double_marker_strips_both_last_wins`,
/// `parse_marker_trailing_syntax_echo_is_stripped_but_unclassified`.
pub(crate) fn parse_marker(raw: &str) -> (String, Option<Classification>) {
    let mut remaining = raw.trim_end();
    let mut classification: Option<Classification> = None;
    let mut is_first_line = true;

    loop {
        let line_start = remaining.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = remaining[line_start..].trim();

        let Some(rest) = line
            .strip_prefix(MARKER_OPEN)
            .and_then(|s| s.strip_suffix(MARKER_CLOSE))
        else {
            break;
        };
        let inner = rest.trim();

        if is_first_line && !inner.is_empty() {
            let (label, is_new) = match inner.strip_prefix(NEW_PREFIX) {
                Some(new_label) => (new_label.trim().to_string(), true),
                None => (inner.to_string(), false),
            };
            if is_valid_label(&label) {
                classification = Some(Classification { label, is_new });
            } else {
                tracing::warn!(
                    label = %label.chars().take(80).collect::<String>(),
                    "classification marker label failed charset validation ([a-z0-9-]{{1,64}}); turn left unclassified"
                );
            }
        }
        is_first_line = false;

        // Strip this marker-shaped line (and its leading newline) from the
        // displayed text regardless of label validity, then keep looking
        // backward for another consecutive trailing marker line.
        remaining = remaining[..line_start].trim_end();
    }

    (remaining.to_string(), classification)
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
         End your response with exactly one trailing line — the LAST line, on \
         its own, nothing after it — classifying this turn:\n\
         `{MARKER_OPEN}<label>{MARKER_CLOSE}` to reuse an existing label, or \
         `{MARKER_OPEN}{NEW_PREFIX}<label>{MARKER_CLOSE}` to start a new one. \
         `<label>` must be lowercase-kebab-case: letters, digits, and hyphens \
         only (e.g. `pr-review-tips`), at most {LABEL_MAX_LEN} characters — never \
         the literal placeholder text. \
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
/// Why: `enabled` (`[workstreams].enabled`, default `true`) is a REAL master
/// switch (#3840 critic HIGH-2), not merely consulted by the summarizer —
/// when `false`, this function does no work at all: no vocabulary fetch, no
/// classification block, and focused-mode is treated as unfocused (assembling
/// a focused-context block only makes sense alongside active classification).
/// What: When `enabled`, fetches the closed label vocabulary and renders the
/// classification block unconditionally; when `focused` is also `Some`,
/// assembles the focused-mode block in the spec's stable order: global
/// summary (stub) → per-workstream summary (cached) → last `recent_window`
/// raw turns. Fails open exactly like every other trusty-memory read in this
/// crate — an unreachable daemon yields empty labels/history, never an error.
/// Test: `build_turn_context_unfocused_has_no_context_block`,
/// `build_turn_context_focused_assembles_stable_order`,
/// `build_turn_context_disabled_is_a_real_no_op`.
pub(crate) async fn build_turn_context(
    project_path: &Path,
    focused: Option<&str>,
    recent_window: usize,
    enabled: bool,
    memory_socket: &Path,
) -> TurnContext {
    if !enabled {
        return TurnContext {
            classification_block: String::new(),
            focused_label: None,
            focused_context_block: None,
        };
    }

    let labels = workstreams::list_workstream_labels_at(project_path, memory_socket).await;
    let classification_block = classification_block(&labels);

    let focused_context_block = match focused {
        Some(label) => {
            let block =
                assemble_focused_block(project_path, memory_socket, label, recent_window).await;
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
    memory_socket: &Path,
    label: &str,
    recent_window: usize,
) -> String {
    // Global summary: no cheap always-available global prompt-history
    // summary source exists in this slice yet (deferred — see module doc);
    // an honestly-labeled stub preserves the spec's "agent has full memory
    // access" framing without fabricating content.
    let global_summary =
        "(global summary not yet wired in this slice — full memory remains queryable on demand)";

    let ws_summary = workstreams::drawers_by_tag_at(
        project_path,
        memory_socket,
        &workstream_summary_tag(label),
        1,
    )
    .await;
    let ws_summary_text = ws_summary
        .first()
        .map(|h| h.content.as_str())
        .unwrap_or("(no summary yet for this task)");

    let recent = workstreams::drawers_by_tag_at(
        project_path,
        memory_socket,
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

/// Finish a persona turn: parse the classification marker, apply the
/// task-bleed nudge, and return the text to display to the user IMMEDIATELY
/// — turn persistence and the periodic summary LLM call happen off the
/// response path (#3840 critic HIGH-4).
///
/// Why: `create_tagged_drawer_at` is two sequential HTTP writes
/// (ensure-palace + create-drawer) and, on a cadence turn,
/// `maybe_summarize_workstream` adds a SECOND full LLM round trip — both
/// awaited inline used to mean the user's answer sat behind that latency
/// even though neither result affects what they see. Detaching them into a
/// `tokio::spawn`ed background task after parsing/nudging (but before
/// returning) means the user sees their answer as soon as the persona's own
/// LLM call completes; the write/summary land a beat later, logged exactly
/// like the previous inline failures on error, with no caller left awaiting
/// them (fire-and-forget, matching this module's existing fail-open posture
/// for persistence).
/// What: Called from every `run_pm_task_with_persona` return path
/// (streaming and non-streaming) so classification/persistence happen
/// exactly once regardless of which path produced the raw response. A
/// response with no parseable marker is returned unchanged (logged, not
/// persisted) — a persona that forgets the marker still answers the user.
/// `[workstreams].enabled = false` is a real master switch (#3840 critic
/// HIGH-2): even if a marker somehow parses, nothing is persisted.
/// Test: `finish_turn_no_marker_returns_unchanged`,
/// `finish_turn_bleed_nudge_appended_when_focused_mismatch`,
/// `finish_turn_disabled_does_not_persist`,
/// `finish_turn_persists_via_detached_task` (mock-daemon, `classification_tests.rs`).
// 8 params is one over clippy's default `too_many_arguments` threshold;
// each is independently meaningful at every one of `finish_turn`'s two call
// sites in `persona.rs` (streaming + non-streaming) and bundling them into a
// request struct would only move the same fields one level of indirection
// without reducing what a caller has to supply — allowed deliberately
// rather than restructured under the demo-day time budget.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finish_turn(
    project_path: &Path,
    persona_name: &str,
    client: &Client<OpenAIConfig>,
    persona_cfg: &AgentConfig,
    user_input: &str,
    raw_response: String,
    ctx: &TurnContext,
    memory_socket: &Path,
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

    if !persona_cfg.workstreams.enabled {
        tracing::debug!(
            persona = %persona_name,
            label = %classification.label,
            "workstream context disabled ([workstreams].enabled = false); turn not persisted"
        );
        return Ok(display);
    }

    // Off the response path: persistence + the cadence summary call, in a
    // detached task. Everything captured must be owned ('static) — cheap
    // clones (`Client<OpenAIConfig>` and `AgentConfig` are both `Clone`).
    let project_path_owned = project_path.to_path_buf();
    let persona_name_owned = persona_name.to_string();
    let user_input_owned = user_input.to_string();
    let display_for_persist = display.clone();
    let client_owned = client.clone();
    let persona_cfg_owned = persona_cfg.clone();
    let label = classification.label.clone();
    let memory_socket = memory_socket.to_path_buf();
    tokio::spawn(async move {
        let content = format!(
            "### {persona_name_owned}\n\n**User:** {user_input_owned}\n\n**Assistant:** {display_for_persist}"
        );
        if let Err(e) = workstreams::create_tagged_drawer_at(
            &project_path_owned,
            &memory_socket,
            &content,
            vec![workstream_tag(&label)],
        )
        .await
        {
            tracing::warn!(persona = %persona_name_owned, error = %e, "failed to persist workstream-tagged turn");
        }

        if let Err(e) = maybe_summarize_workstream(
            &project_path_owned,
            &memory_socket,
            &label,
            &client_owned,
            &persona_cfg_owned,
        )
        .await
        {
            tracing::warn!(persona = %persona_name_owned, error = %e, "workstream summary refresh failed");
        }
    });

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
    memory_socket: &Path,
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
        memory_socket,
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
    // #3867 (epic #3866 Slice A, instrumentation point 3): time the LLM
    // round-trip for the durable `agents-ws-summary` compression record
    // below — `duration_ms` is this call's wall-clock, not the whole
    // function's (drawer read/write I/O is excluded, matching the issue's
    // "duration_ms: ... the LLM round-trip for agents-ws-summary" spec).
    let summary_started = std::time::Instant::now();
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
    let summary_duration_ms =
        u64::try_from(summary_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let Some(summary_text) = resp.content else {
        anyhow::bail!("workstream summary call returned no content");
    };

    workstreams::create_tagged_drawer_at(
        project_path,
        memory_socket,
        &summary_text,
        vec![workstream_summary_tag(label)],
    )
    .await?;
    tracing::info!(label = %label, turn_count = count, "workstream summary refreshed");

    // #3867 (epic #3866 Slice A): durable compression-effectiveness record
    // for the agents-ws-summary surface, sharing RTK's `compression.jsonl`
    // sink — one writer, two surfaces. See `classification_ws_summary`'s
    // doc comment for the full rationale.
    classification_ws_summary::record(
        project_path,
        label,
        &joined,
        &summary_text,
        summary_duration_ms,
    )
    .await;

    Ok(())
}

#[path = "classification_ws_summary.rs"]
mod classification_ws_summary;

#[cfg(test)]
#[path = "classification_tests.rs"]
mod tests;
