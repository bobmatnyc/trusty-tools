//! Unit tests for the Telegram proxy binding (TELUI-6, #1440).
//!
//! Why: this lives in a `focus/tests.rs` (a recognized test-file path) so the
//! production `telegram/focus.rs` stays under the 500-SLOC production cap while
//! the suite shares its private render functions via `use super::*`.
//! What: covers the HTML render mapping for each proxy outcome and the no-focus
//! handler wiring. The focus state machine and daemon paths are covered by
//! `client::proxy::tests`.
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
fn render_focus_focused_names_session() {
    let html = render_focus(&FocusOutcome::Focused(target()));
    assert!(html.contains("Focused on <b>tmpm-api</b>"), "{html}");
    assert!(html.contains("/unfocus"), "{html}");
}

#[test]
fn render_focus_current_none_hints_usage() {
    let html = render_focus(&FocusOutcome::Current(None));
    assert!(html.contains("Usage"), "{html}");
}

#[test]
fn render_focus_not_found_escapes_and_reports() {
    let html = render_focus(&FocusOutcome::NotFound {
        target: "no-such".into(),
        error: "managed session no-such not found".into(),
    });
    assert!(html.contains("Cannot focus"), "{html}");
    assert!(html.contains("no-such"), "{html}");
}

#[test]
fn render_inject_sent_echoes_text() {
    let html = render_inject(&InjectOutcome::Sent {
        target: target(),
        text: "run the tests".into(),
    });
    assert!(html.contains("tmpm-api"), "{html}");
    assert!(html.contains("run the tests"), "{html}");
}

#[test]
fn render_inject_auto_unfocused_signals_gone() {
    let html = render_inject(&InjectOutcome::AutoUnfocused {
        target: target(),
        error: "managed session x not found".into(),
    });
    assert!(html.contains("gone"), "{html}");
    assert!(html.contains("tmpm-api"), "{html}");
}

#[test]
fn render_inject_failed_keeps_focus_prose() {
    let html = render_inject(&InjectOutcome::Failed {
        target: target(),
        error: "connection refused".into(),
    });
    assert!(html.contains("failed"), "{html}");
    assert!(html.contains("Still focused"), "{html}");
}

#[test]
fn render_inject_no_focus_hints() {
    let html = render_inject(&InjectOutcome::NoFocus);
    assert!(html.contains("No session is focused"), "{html}");
}

#[test]
fn render_summary_ok_shows_state_and_pending() {
    let html = render_summary(&SummarizeOutcome::Summary {
        target: target(),
        state: "active".into(),
        summary: "running cargo test".into(),
        pending_decision: Some("apply patch?".into()),
    });
    assert!(html.contains("tmpm-api"), "{html}");
    assert!(html.contains("active"), "{html}");
    assert!(html.contains("running cargo test"), "{html}");
    assert!(html.contains("pending: apply patch?"), "{html}");
}

#[test]
fn render_summary_no_focus_hints() {
    let html = render_summary(&SummarizeOutcome::NoFocus);
    assert!(html.contains("No session is focused"), "{html}");
}

#[tokio::test]
async fn handle_unfocus_when_none() {
    let proxy = offline_proxy();
    assert!(handle_unfocus(&proxy, 1).contains("No session was focused"));
}

#[tokio::test]
async fn handle_focus_empty_hints() {
    // `/focus` with no argument and no current focus shows the usage hint and
    // never touches the daemon (offline proxy would otherwise error).
    let proxy = offline_proxy();
    let html = handle_focus(&proxy, 1, "   ").await;
    assert!(html.contains("Usage"), "{html}");
}
