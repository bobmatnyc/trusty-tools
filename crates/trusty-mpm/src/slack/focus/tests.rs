//! Unit tests for the Slack proxy binding (TELUI-6, #2549).
//!
//! Why: this lives in a `focus/tests.rs` (a recognized test-file path) so the
//! production `slack/focus.rs` stays under the 500-SLOC production cap while the
//! suite shares its private render functions via `use super::*`.
//! What: covers the `mrkdwn` render mapping for each proxy outcome, the
//! conversation-key convention, the proxy-verb classifier, and the no-focus
//! handler wiring. The focus state machine and daemon paths are covered by
//! `client::proxy::tests`; the inbound-event routing by `slack::tests`.
//! Test: this *is* the test module.

use super::*;
use std::sync::Arc;

use crate::client::{
    CommandExecutor, FocusOutcome, FocusTarget, InjectOutcome, SessionProxy, SummarizeOutcome,
};

/// A proxy over an unreachable daemon (no focus set).
fn offline_proxy() -> SessionProxy {
    SessionProxy::new(Arc::new(CommandExecutor::new("http://127.0.0.1:0")))
}

fn target() -> FocusTarget {
    FocusTarget {
        id: "aaaa1111bbbb2222".into(),
        name: "tmpm-api".into(),
    }
}

#[test]
fn conv_keys_by_channel_and_thread() {
    // No thread → the bare channel id; a thread → channel:thread_ts. An empty
    // thread string is treated as "no thread" (defensive) so a threadless event
    // that carried "" still keys by the channel alone.
    assert_eq!(conv("C1", None), "C1");
    assert_eq!(conv("C1", Some("169.42")), "C1:169.42");
    assert_eq!(conv("C1", Some("")), "C1");
}

#[test]
fn proxy_verb_classifies_the_three_verbs() {
    // The three proxy verbs classify (with or without the leading slash, any
    // case); everything else is None so it falls through to normal dispatch.
    assert_eq!(proxy_verb("/focus"), Some(ProxyVerb::Focus));
    assert_eq!(proxy_verb("unfocus"), Some(ProxyVerb::Unfocus));
    assert_eq!(proxy_verb("/SUMMARY"), Some(ProxyVerb::Summary));
    assert_eq!(proxy_verb("/fleet"), None);
    assert_eq!(proxy_verb(""), None);
}

#[test]
fn render_focus_focused_names_session() {
    let body = render_focus(&FocusOutcome::Focused(target()));
    assert!(body.contains("Focused on *tmpm-api*"), "{body}");
    assert!(body.contains("`/unfocus`"), "{body}");
}

#[test]
fn render_focus_current_none_hints_usage() {
    let body = render_focus(&FocusOutcome::Current(None));
    assert!(body.contains("Usage"), "{body}");
}

#[test]
fn render_focus_not_found_reports() {
    let body = render_focus(&FocusOutcome::NotFound {
        target: "no-such".into(),
        error: "managed session no-such not found".into(),
    });
    assert!(body.contains("Cannot focus"), "{body}");
    assert!(body.contains("no-such"), "{body}");
}

#[test]
fn render_inject_sent_echoes_text() {
    let body = render_inject(&InjectOutcome::Sent {
        target: target(),
        text: "run the tests".into(),
    });
    assert!(body.contains("tmpm-api"), "{body}");
    assert!(body.contains("run the tests"), "{body}");
}

#[test]
fn render_inject_auto_unfocused_signals_gone() {
    let body = render_inject(&InjectOutcome::AutoUnfocused {
        target: target(),
        error: "managed session x not found".into(),
    });
    assert!(body.contains("gone"), "{body}");
    assert!(body.contains("tmpm-api"), "{body}");
}

#[test]
fn render_inject_failed_keeps_focus_prose() {
    let body = render_inject(&InjectOutcome::Failed {
        target: target(),
        error: "connection refused".into(),
    });
    assert!(body.contains("failed"), "{body}");
    assert!(body.contains("Still focused"), "{body}");
}

#[test]
fn render_inject_no_focus_hints() {
    let body = render_inject(&InjectOutcome::NoFocus);
    assert!(body.contains("No session is focused"), "{body}");
}

#[test]
fn render_summary_ok_shows_state_and_pending() {
    let body = render_summary(&SummarizeOutcome::Summary {
        target: target(),
        state: "active".into(),
        summary: "running cargo test".into(),
        pending_decision: Some("apply patch?".into()),
    });
    assert!(body.contains("tmpm-api"), "{body}");
    assert!(body.contains("active"), "{body}");
    assert!(body.contains("running cargo test"), "{body}");
    assert!(body.contains("pending: apply patch?"), "{body}");
}

#[test]
fn render_summary_no_focus_hints() {
    let body = render_summary(&SummarizeOutcome::NoFocus);
    assert!(body.contains("No session is focused"), "{body}");
}

#[test]
fn truncate_summary_leaves_short_text_untouched() {
    let short = "running cargo test, all green";
    assert_eq!(truncate_summary(short).as_ref(), short);
}

#[test]
fn truncate_summary_caps_long_text() {
    // A digest far longer than the budget is capped and marked, never passed
    // through verbatim (defensive ceiling against Slack's 40,000-char limit).
    let long: String = "x".repeat(MAX_SUMMARY_CHARS * 3);
    let truncated = truncate_summary(&long);
    assert!(
        truncated.chars().count() <= MAX_SUMMARY_CHARS + " […truncated]".chars().count(),
        "truncated length {} exceeds budget",
        truncated.chars().count()
    );
    assert!(truncated.ends_with("[…truncated]"), "{truncated}");
}

#[tokio::test]
async fn handle_unfocus_when_none() {
    let proxy = offline_proxy();
    assert!(handle_unfocus(&proxy, "C1").contains("No session was focused"));
}

#[tokio::test]
async fn handle_focus_empty_hints() {
    // `/focus` with no argument and no current focus shows the usage hint and
    // never touches the daemon (the offline proxy would otherwise error).
    let proxy = offline_proxy();
    let body = handle_focus(&proxy, "C1", "   ").await;
    assert!(body.contains("Usage"), "{body}");
}
