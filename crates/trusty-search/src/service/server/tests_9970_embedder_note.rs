//! #9970: the cross-index search body and `/health` carry the embedder note.
//!
//! Why: D1b — every search response tells the agent while the python embedder
//! is not serving; Q5 — `/health` says degraded and why, never silence.
//! What: drives the `*_report` cores (#9939), not the handler shims, with a
//! switchable embedder in each bootstrap state. Written against pre-#9970
//! APIs, so each test compiles on the base commit and fails on the missing
//! `embedder` / `python_bootstrap.note`. The per-index `search.query` path is
//! covered with a real `drive_bootstrap` in `commands/start/tests_9970.rs`.
//! Test: this module.

use std::sync::Arc;

use serde_json::Value;

use super::health::health_report;
use super::search_global::{global_search_report, GlobalSearchRequest};
use super::SearchAppState;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::IndexRegistry;
use crate::service::embedder_supervisor::bootstrap_control::BootstrapReport;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};

fn backend(kind: BackendKind, bootstrap: BootstrapState) -> ActiveBackend {
    ActiveBackend {
        kind,
        provider: trusty_common::embedder::ExecutionProvider::Cpu,
        model: "all-MiniLM-L6-v2".to_string(),
        quantized: false,
        bootstrap,
    }
}

/// A state with no index and an ort switchable embedder in `bootstrap`.
fn state_with(bootstrap: BootstrapState) -> (Arc<SearchAppState>, Arc<SwitchableEmbedder>) {
    let inner: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
    let switchable = Arc::new(SwitchableEmbedder::new(
        inner,
        backend(BackendKind::Ort, bootstrap),
    ));
    let state = SearchAppState::new(IndexRegistry::new());
    state.install_switchable_embedder(Arc::clone(&switchable));
    (Arc::new(state), switchable)
}

async fn fan_out(state: &Arc<SearchAppState>) -> Value {
    let req: GlobalSearchRequest =
        serde_json::from_value(serde_json::json!({ "query": "anything" })).expect("body");
    global_search_report(state, req)
        .await
        .unwrap_or_else(|(status, body)| panic!("fan-out refused {status}: {body}"))
}

/// The fan-out body names the embedder and carries the note while the
/// bootstrap runs, and drops the note once python serves.
#[tokio::test]
#[serial_test::parallel]
async fn search_carries_the_note_while_the_bootstrap_runs() {
    let (state, switchable) = state_with(BootstrapState::Bootstrapping);
    let during = fan_out(&state).await;
    assert_eq!(during["embedder"]["active"], "ort", "{during}");
    assert_eq!(during["embedder"]["intended"], "python", "{during}");
    assert!(
        during["embedder"]["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("Degraded:")),
        "{during}"
    );

    switchable.swap_to(
        Arc::new(MockEmbedder::new(384)),
        backend(BackendKind::Python, BootstrapState::Ready),
    );
    let after = fan_out(&state).await;
    assert_eq!(after["embedder"]["active"], "python", "{after}");
    assert!(after["embedder"]["note"].is_null(), "{after}");
}

/// Q5: a bootstrap waiting on a retry is degraded on `/health`, and
/// `python_bootstrap` carries the step and the note naming the cause.
#[tokio::test]
#[serial_test::parallel]
async fn health_carries_the_note_naming_the_cause() {
    let (state, switchable) = state_with(BootstrapState::Retrying);
    switchable
        .bootstrap_control()
        .update_report(|r| BootstrapReport {
            attempts: 2,
            last_error: Some("python readiness probe timed out after 60s".to_string()),
            ..r.clone()
        });
    let health = health_report(Arc::clone(&state)).await;
    assert_eq!(health["status"], "degraded", "{health}");
    let py = &health["python_bootstrap"];
    assert_eq!(py["phase"], "retrying", "{py}");
    assert!(
        py["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("Degraded:") && n.contains("timed out after 60s")),
        "{py}"
    );
}

/// #9970 AC2 / Q5: the eager python path's two ort outcomes — a startup
/// failure (`Failed`) and a fallback-latch trip (`FellBackToOrt`) — are
/// degraded on `/health`, and `python_bootstrap.note` names the cause.
#[tokio::test]
#[serial_test::parallel]
async fn health_reports_an_eager_python_fallback_as_degraded_with_the_cause() {
    let cases = [
        (
            BootstrapState::Failed,
            "failed",
            "py-embedder venv bootstrap: uv not found and TRUSTY_UV_FETCH=0",
        ),
        (
            BootstrapState::FellBackToOrt,
            "fell_back_to_ort",
            "its supervisor gave up restarting it; last error: broken pipe",
        ),
    ];
    for (bootstrap, step, cause) in cases {
        let (state, switchable) = state_with(bootstrap);
        switchable
            .bootstrap_control()
            .update_report(|r| BootstrapReport {
                last_error: Some(cause.to_string()),
                ..r.clone()
            });
        let health = health_report(Arc::clone(&state)).await;
        assert_eq!(health["status"], "degraded", "{step}: {health}");
        let py = &health["python_bootstrap"];
        assert_eq!(py["phase"], step, "{py}");
        assert!(
            py["note"]
                .as_str()
                .is_some_and(|n| n.starts_with("Degraded:") && n.contains(cause)),
            "{step}: {py}"
        );
    }
}
