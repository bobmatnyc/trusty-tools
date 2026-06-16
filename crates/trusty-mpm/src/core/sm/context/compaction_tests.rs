//! Unit tests for §7.3 compaction call rendering + token estimation.
//!
//! Why: the fixed faithful-summary prompt, the chars/4 heuristic, and the request
//! shape (Haiku model, temperature 0.0) are normative; pin them so a refactor
//! can't silently weaken the fidelity contract or change the trigger math.
//! What: asserts the prompt anchors, the estimate, the rendered fold/resummarise
//! messages, and a mock-provider fold that captures the request.
//! Test: this is the test module.

use super::*;
use crate::core::sm::context::mock_provider::MockProvider;
use crate::core::sm::context::model::{Round, ToolTrace};
use chrono::{TimeZone, Utc};

/// Fixed deterministic timestamp for the round fixtures.
fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("valid ts")
}

/// Why: the §7.2 trigger relies on chars/4; pin the divisor behaviour.
/// What: asserts a few exact values of the integer-divided estimate.
/// Test: this is the test.
#[test]
fn estimate_tokens_uses_chars_over_four() {
    assert_eq!(estimate_tokens(0), 0);
    assert_eq!(estimate_tokens(4), 1);
    assert_eq!(estimate_tokens(7), 1);
    assert_eq!(estimate_tokens(8), 2);
    assert_eq!(estimate_tokens(40_000), 10_000);
}

/// Why: §7.3 mandates the prompt explicitly preserve goal ids, session ids,
/// decisions, and blockers; assert those anchors are literally present so the
/// contract can't be edited away unnoticed.
/// What: substring-checks the fixed faithful-summary prompt.
/// Test: this is the test.
#[test]
fn faithful_prompt_mentions_required_anchors() {
    let p = FAITHFUL_SUMMARY_SYSTEM_PROMPT;
    assert!(p.contains("goal ids"), "must preserve goal ids");
    assert!(p.contains("session ids"), "must preserve session ids");
    assert!(p.contains("decisions"), "must preserve decisions");
    assert!(p.contains("blockers"), "must preserve blockers");
    assert!(p.contains("open questions"), "must preserve open questions");
}

/// Why: the re-summarisation pass (§7.6) shares the fidelity contract; assert its
/// anchors too.
/// What: substring-checks the compress-summary prompt.
/// Test: this is the test.
#[test]
fn resummarise_prompt_mentions_required_anchors() {
    let p = COMPRESS_SUMMARY_SYSTEM_PROMPT;
    assert!(p.contains("goal id"));
    assert!(p.contains("session id"));
    assert!(p.contains("decision"));
    assert!(p.contains("blocker"));
    assert!(p.contains("open question"));
}

/// Why: the fold message must carry both the prior summary and the verbatim
/// evicted rounds (incl. tool traces) so the model can integrate them.
/// What: renders a fold message and asserts the summary + round text + trace are
/// all present.
/// Test: this is the test.
#[test]
fn render_fold_message_includes_summary_and_rounds() {
    let rounds = vec![Round::new(
        "deploy goal g-42",
        "spawned s-7",
        ts(),
        vec![ToolTrace::new("session_new", "s-7 created")],
    )];
    let msg = render_compaction_user_message("prior summary text", &rounds);
    assert!(msg.contains("prior summary text"));
    assert!(msg.contains("deploy goal g-42"));
    assert!(msg.contains("spawned s-7"));
    assert!(msg.contains("session_new"));
    assert!(msg.contains("s-7 created"));
}

/// Why: the first compaction has no prior summary; the renderer must mark that
/// explicitly rather than emit a blank.
/// What: renders with an empty prior summary and asserts the "none yet" marker.
/// Test: this is the test.
#[test]
fn render_fold_message_marks_empty_prior_summary() {
    let rounds = vec![Round::new("u", "a", ts(), Vec::new())];
    let msg = render_compaction_user_message("", &rounds);
    assert!(msg.contains("none yet"));
}

/// Why: the re-summarise wrapper must label the input so the model knows its task.
/// What: asserts the label + summary are present.
/// Test: this is the test.
#[test]
fn render_resummarise_message_wraps_summary() {
    let msg = render_resummarise_user_message("big summary g-1 s-2");
    assert!(msg.contains("SUMMARY TO COMPACT"));
    assert!(msg.contains("big summary g-1 s-2"));
}

/// Why: the fold call must go through the injected provider with the resolved
/// (Haiku) model id and temperature 0.0 — the §7.3 determinism + cost contract.
/// What: folds one round via a mock, then asserts the captured request's model,
/// temperature, system prompt, and that exactly one call was made.
/// Test: this is the test.
#[tokio::test]
async fn fold_rounds_builds_haiku_request_at_temp_zero() {
    let mock = MockProvider::fixed("updated summary");
    let rounds = vec![Round::new("u", "a", ts(), Vec::new())];
    let resp = fold_rounds(&mock, "claude-haiku", "", &rounds)
        .await
        .expect("fold succeeds");
    assert_eq!(resp.text, "updated summary");

    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1, "exactly one compaction call");
    assert_eq!(reqs[0].model, "claude-haiku");
    assert!((reqs[0].temperature - COMPACTION_TEMPERATURE).abs() < f32::EPSILON);
    assert_eq!(reqs[0].max_tokens, COMPACTION_MAX_TOKENS);
    assert_eq!(reqs[0].system, FAITHFUL_SUMMARY_SYSTEM_PROMPT);
}

/// Why: a `Fixed` mock returns its canned text regardless of input — the seam the
/// "evicted content survives" engine test relies on.
/// What: folds and asserts the returned text equals the canned summary.
/// Test: this is the test.
#[tokio::test]
async fn fold_rounds_returns_mock_summary() {
    let mock = MockProvider::fixed("CANNED-123");
    let rounds = vec![Round::new("hello", "world", ts(), Vec::new())];
    let resp = fold_rounds(&mock, "m", "prior", &rounds)
        .await
        .expect("fold");
    assert_eq!(resp.text, "CANNED-123");
}

/// Why: the re-summarise pass must also route through the injected provider at
/// temperature 0.0 with the compress prompt.
/// What: re-summarises via a mock and asserts the captured request.
/// Test: this is the test.
#[tokio::test]
async fn resummarise_returns_mock_summary() {
    let mock = MockProvider::fixed("shorter");
    let resp = resummarise(&mock, "m", "long summary")
        .await
        .expect("resummarise");
    assert_eq!(resp.text, "shorter");
    let reqs = mock.requests();
    assert_eq!(reqs[0].system, COMPRESS_SUMMARY_SYSTEM_PROMPT);
    assert!((reqs[0].temperature - COMPACTION_TEMPERATURE).abs() < f32::EPSILON);
}
