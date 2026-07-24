//! Tests for `classification` — split out following the `persona.rs`/
//! `persona_tests.rs` pattern already established in this directory (keeps
//! `classification.rs` well clear of the 500-SLOC production cap).
//! What: pure-function tests for `parse_marker`/`classification_block`/
//! `bleed_nudge`, plus `build_turn_context`/`finish_turn` tests against a
//! no-project-root tempdir (hermetic — no network, no LLM credentials
//! required, since every code path that would need them short-circuits
//! before reaching them when there's no project root to write into).
//! Test: This module IS the test coverage.

use super::*;
use async_openai::{Client, config::OpenAIConfig};

fn test_client() -> Client<OpenAIConfig> {
    Client::with_config(OpenAIConfig::new())
}

fn test_persona_cfg() -> AgentConfig {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    toml::from_str(toml_str).expect("parses")
}

// ---------------------------------------------------------------------
// parse_marker
// ---------------------------------------------------------------------

#[test]
fn parse_marker_extracts_existing_label() {
    let raw = "Here's your answer.\n\n[[task: feat-x]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Here's your answer.");
    let c = c.expect("classification present");
    assert_eq!(c.label, "feat-x");
    assert!(!c.is_new);
}

#[test]
fn parse_marker_extracts_new_label() {
    let raw = "Sure, I'll help with that.\n\n[[task: new: onboarding-flow]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Sure, I'll help with that.");
    let c = c.expect("classification present");
    assert_eq!(c.label, "onboarding-flow");
    assert!(c.is_new);
}

#[test]
fn parse_marker_missing_is_none() {
    let raw = "Just a plain response with no marker.";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, raw);
    assert!(c.is_none());
}

#[test]
fn parse_marker_strips_surrounding_whitespace() {
    let raw = "Answer text.\n\n[[task:   feat-y   ]]  \n";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "Answer text.");
    assert_eq!(c.expect("classification present").label, "feat-y");
}

#[test]
fn parse_marker_ignores_earlier_lookalike_text() {
    // The marker syntax mentioned earlier in prose must not be mistaken for
    // the real trailing decision — only the LAST occurrence counts.
    let raw = "I'll end with `[[task: <label>]]` as instructed.\n\n[[task: feat-z]]";
    let (display, c) = parse_marker(raw);
    assert_eq!(display, "I'll end with `[[task: <label>]]` as instructed.");
    assert_eq!(c.expect("classification present").label, "feat-z");
}

#[test]
fn parse_marker_empty_label_is_none() {
    let raw = "Answer.\n\n[[task: ]]";
    let (_, c) = parse_marker(raw);
    assert!(c.is_none());
}

// ---------------------------------------------------------------------
// classification_block
// ---------------------------------------------------------------------

#[test]
fn classification_block_lists_existing_labels() {
    let block = classification_block(&["feat-x".to_string(), "feat-y".to_string()]);
    assert!(block.contains("feat-x, feat-y"));
    assert!(block.contains("[[task: <label>]]"));
    assert!(block.contains("[[task: new: <label>]]"));
}

#[test]
fn classification_block_empty_labels_still_offers_new() {
    let block = classification_block(&[]);
    assert!(block.contains("(none yet)"));
    assert!(block.contains("new:"));
}

// ---------------------------------------------------------------------
// bleed_nudge
// ---------------------------------------------------------------------

#[test]
fn bleed_nudge_none_when_unfocused() {
    assert_eq!(bleed_nudge(None, "feat-x"), None);
}

#[test]
fn bleed_nudge_none_when_matching() {
    assert_eq!(bleed_nudge(Some("feat-x"), "feat-x"), None);
}

#[test]
fn bleed_nudge_some_when_different() {
    let nudge = bleed_nudge(Some("feat-x"), "feat-y").expect("nudge present");
    assert!(nudge.contains("feat-y"));
    assert!(nudge.contains("feat-x"));
}

// ---------------------------------------------------------------------
// build_turn_context / finish_turn — hermetic (no project root -> every
// trusty-memory call short-circuits to empty before any network attempt).
// ---------------------------------------------------------------------

#[tokio::test]
async fn build_turn_context_unfocused_has_no_context_block() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = build_turn_context(tmp.path(), None, 12).await;
    assert!(ctx.focused_context_block.is_none());
    assert!(ctx.focused_label.is_none());
    assert!(ctx.classification_block.contains("(none yet)"));
}

#[tokio::test]
async fn build_turn_context_focused_assembles_stable_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = build_turn_context(tmp.path(), Some("feat-x"), 12).await;
    assert_eq!(ctx.focused_label.as_deref(), Some("feat-x"));
    let block = ctx.focused_context_block.expect("focused block present");
    let global_pos = block
        .find("Global summary")
        .expect("global summary section");
    let ws_pos = block.find("Task summary").expect("task summary section");
    let recent_pos = block
        .find("Recent turns on this task")
        .expect("recent turns section");
    assert!(
        global_pos < ws_pos && ws_pos < recent_pos,
        "DOC-54 §9.6.3 stable order: global -> per-workstream summary -> recent turns"
    );
}

#[tokio::test]
async fn finish_turn_no_marker_returns_unchanged() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), None, 12).await;
    let raw = "Plain response, no classification marker.".to_string();
    let out = finish_turn(tmp.path(), "izzie", &client, &cfg, "hi", raw.clone(), &ctx)
        .await
        .expect("finish_turn must not error even without a marker");
    assert_eq!(out, raw);
}

#[tokio::test]
async fn finish_turn_with_marker_and_no_project_root_still_returns_display_text() {
    // No project root -> `create_tagged_drawer_at` fails (logged, not
    // propagated) and `maybe_summarize_workstream` sees zero turns and
    // never attempts an LLM call — the whole thing stays hermetic.
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), None, 12).await;
    let raw = "Here you go.\n\n[[task: feat-x]]".to_string();
    let out = finish_turn(tmp.path(), "izzie", &client, &cfg, "hi", raw, &ctx)
        .await
        .expect("finish_turn must fail open on a persistence error");
    assert_eq!(out, "Here you go.");
}

#[tokio::test]
async fn finish_turn_bleed_nudge_appended_when_focused_mismatch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let client = test_client();
    let cfg = test_persona_cfg();
    let ctx = build_turn_context(tmp.path(), Some("feat-a"), 12).await;
    let raw = "Doing something else.\n\n[[task: feat-b]]".to_string();
    let out = finish_turn(tmp.path(), "izzie", &client, &cfg, "hi", raw, &ctx)
        .await
        .expect("finish_turn must not error");
    assert!(out.starts_with("Doing something else."));
    assert!(out.contains("feat-b"));
    assert!(out.contains("feat-a"));
}

// ---------------------------------------------------------------------
// should_refresh_summary — pure cadence decision (DOC-54 §9.6.2)
// ---------------------------------------------------------------------

#[test]
fn should_refresh_summary_skips_when_disabled() {
    assert!(!should_refresh_summary(false, 5, 5));
    assert!(!should_refresh_summary(false, 5, 10));
}

#[test]
fn should_refresh_summary_skips_zero_cadence() {
    assert!(!should_refresh_summary(true, 0, 5));
}

#[test]
fn should_refresh_summary_skips_zero_turns() {
    assert!(!should_refresh_summary(true, 5, 0));
}

#[test]
fn should_refresh_summary_skips_off_cadence() {
    assert!(!should_refresh_summary(true, 5, 4));
    assert!(!should_refresh_summary(true, 5, 6));
    assert!(!should_refresh_summary(true, 5, 11));
}

#[test]
fn should_refresh_summary_true_on_cadence_boundary() {
    assert!(should_refresh_summary(true, 5, 5));
    assert!(should_refresh_summary(true, 5, 10));
    assert!(should_refresh_summary(true, 1, 3));
}
