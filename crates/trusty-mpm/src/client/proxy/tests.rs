//! Unit tests for the channel-agnostic session-manager proxy (TELUI-6, #1440).
//!
//! Why: this lives in a `proxy/tests.rs` (a recognized test-file path) so the
//! production `client/proxy.rs` stays under the 500-SLOC production cap while the
//! suite shares its private items via `use super::*`.
//! What: covers the pure inject-vs-coordinator routing, the focus store
//! round-trip, the missing-session predicate, and the focus/inject/summarize
//! paths (incl. dead-session auto-unfocus) against an in-process test daemon.
//! Test: this *is* the test module.

use super::*;
use std::future::IntoFuture;

use crate::client::CommandExecutor;

/// Spawn the daemon's real HTTP API on a random loopback port (empty fleet).
///
/// Why: lets the proxy be tested against genuine daemon routes with no live
/// daemon, tmux, or network. With no managed sessions, every resolve yields the
/// "not found" error the auto-unfocus path keys on.
/// What: mirrors the isolated-managed helper in `executor/tests.rs`.
async fn spawn_test_daemon() -> String {
    use crate::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// A proxy over an unreachable daemon (for the pure / transient-error tests).
fn offline_proxy() -> SessionProxy {
    SessionProxy::new(Arc::new(CommandExecutor::new("http://127.0.0.1:0")))
}

#[test]
fn route_free_text_focused_injects() {
    assert_eq!(route_free_text("build it now", true), FreeTextRoute::Inject);
}

#[test]
fn route_free_text_unfocused_coordinates() {
    assert_eq!(
        route_free_text("spin up a session", false),
        FreeTextRoute::Coordinator
    );
}

#[test]
fn route_free_text_slash_never_injects() {
    // A `/`-prefixed line reaching the router is an UNKNOWN command; never inject.
    assert_eq!(
        route_free_text("/typo arg", true),
        FreeTextRoute::Coordinator
    );
    assert_eq!(route_free_text("  /typo", true), FreeTextRoute::Coordinator);
}

#[test]
fn is_missing_session_detects_not_found() {
    assert!(is_missing_session("managed session foo not found"));
    assert!(!is_missing_session("send failed: connection refused"));
}

#[test]
fn set_get_clear_round_trip() {
    let proxy = offline_proxy();
    assert!(proxy.current_focus("c1").is_none());
    proxy.lock().insert(
        "c1".to_string(),
        FocusTarget {
            id: "id-1".into(),
            name: "api".into(),
        },
    );
    assert_eq!(proxy.current_focus("c1").map(|f| f.id), Some("id-1".into()));
    // unfocus returns and removes the entry.
    let removed = proxy.unfocus("c1");
    assert_eq!(removed.map(|f| f.name), Some("api".to_string()));
    assert!(proxy.current_focus("c1").is_none());
    // Unfocusing an empty conversation is a no-op returning None.
    assert!(proxy.unfocus("c1").is_none());
}

#[tokio::test]
async fn focus_empty_reports_current() {
    // An empty target never touches the daemon; it reports the current focus.
    let proxy = offline_proxy();
    assert_eq!(proxy.focus("c1", "   ").await, FocusOutcome::Current(None));
}

#[tokio::test]
async fn focus_unknown_is_not_found() {
    // Focusing a session that does not exist reports NotFound and sets no focus.
    let url = spawn_test_daemon().await;
    let proxy = SessionProxy::new(Arc::new(CommandExecutor::new(url)));
    match proxy.focus("c1", "no-such-session").await {
        FocusOutcome::NotFound { target, .. } => assert_eq!(target, "no-such-session"),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(proxy.current_focus("c1").is_none());
}

#[tokio::test]
async fn inject_no_focus() {
    let proxy = offline_proxy();
    assert_eq!(proxy.inject("c1", "hi").await, InjectOutcome::NoFocus);
}

#[tokio::test]
async fn inject_auto_unfocuses_dead_session() {
    // A focused session that no longer exists: the send resolves to "not found",
    // so focus is auto-cleared and the outcome names the vanished session.
    let url = spawn_test_daemon().await;
    let proxy = SessionProxy::new(Arc::new(CommandExecutor::new(url)));
    proxy.lock().insert(
        "c1".to_string(),
        FocusTarget {
            id: "dead-id".into(),
            name: "ghost".into(),
        },
    );
    match proxy.inject("c1", "are you there?").await {
        InjectOutcome::AutoUnfocused { target, .. } => assert_eq!(target.name, "ghost"),
        other => panic!("expected AutoUnfocused, got {other:?}"),
    }
    assert!(proxy.current_focus("c1").is_none(), "focus auto-cleared");
}

#[tokio::test]
async fn inject_transient_error_keeps_focus() {
    // An unreachable daemon is a transient failure, not a missing session: focus
    // must be preserved so a blip does not discard the operator's context.
    let proxy = offline_proxy();
    proxy.lock().insert(
        "c1".to_string(),
        FocusTarget {
            id: "id-1".into(),
            name: "api".into(),
        },
    );
    match proxy.inject("c1", "hello").await {
        InjectOutcome::Failed { target, .. } => assert_eq!(target.name, "api"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(proxy.current_focus("c1").is_some(), "focus preserved");
}

#[tokio::test]
async fn summarize_no_focus() {
    let proxy = offline_proxy();
    assert_eq!(proxy.summarize("c1").await, SummarizeOutcome::NoFocus);
}

#[tokio::test]
async fn summarize_auto_unfocuses_dead_session() {
    // Summarizing a vanished focused session auto-clears the focus.
    let url = spawn_test_daemon().await;
    let proxy = SessionProxy::new(Arc::new(CommandExecutor::new(url)));
    proxy.lock().insert(
        "c1".to_string(),
        FocusTarget {
            id: "dead-id".into(),
            name: "ghost".into(),
        },
    );
    match proxy.summarize("c1").await {
        SummarizeOutcome::AutoUnfocused { target, .. } => assert_eq!(target.name, "ghost"),
        other => panic!("expected AutoUnfocused, got {other:?}"),
    }
    assert!(proxy.current_focus("c1").is_none(), "focus auto-cleared");
}
