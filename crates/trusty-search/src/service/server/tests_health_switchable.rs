//! Tests for `/health`'s `SwitchableEmbedder`-sourced `embedder_info` /
//! `embedder_bootstrap` fields (epic #3524 slice 6 — PR 2/5, closes #3530 /
//! #3493 P1).
//!
//! Why: split out of `tests_health.rs` (issue #610 line-cap — that file has
//! no allowlist headroom) rather than growing it further; these tests are
//! self-contained and share no fixtures with the rest of that file.
//! What: installs a `SwitchableEmbedder` with a concrete `ActiveBackend` and
//! asserts `/health` reports it verbatim (python and ort arms) instead of
//! the old provider/quantized PREDICTION.
//! Test: this file.
use super::*;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::registry::IndexRegistry;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};
use axum::extract::State;
use axum::Json;
use serde_json::Value;

/// With a Python/MPS `ActiveBackend` installed via `SwitchableEmbedder`,
/// `/health` must report the REAL backend — `backend=python`,
/// `provider=MPS`, `quantized=false`, `model=all-MiniLM-L6-v2` — instead of
/// the old prediction-based path (which would have reported `CoreML`, the
/// ORT-oriented guess). Also verifies `embedder_bootstrap` mirrors
/// `ActiveBackend::bootstrap`.
/// What: installs a `SwitchableEmbedder` with a `Python`/`Mps`/`Ready`
/// `ActiveBackend`, calls `/health`, and asserts the JSON body.
/// Test: this test.
#[tokio::test]
async fn health_reports_switchable_python_backend_info() {
    let state = SearchAppState::new(IndexRegistry::new());
    let inner: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
    let active = ActiveBackend {
        kind: BackendKind::Python,
        provider: trusty_common::embedder::ExecutionProvider::Mps,
        model: "all-MiniLM-L6-v2".to_string(),
        quantized: false,
        bootstrap: BootstrapState::Ready,
    };
    let switchable = Arc::new(SwitchableEmbedder::new(inner, active));
    state.install_switchable_embedder(Arc::clone(&switchable));

    let state_arc = Arc::new(state);
    let Json(resp) = health_handler(State(state_arc)).await;
    assert_eq!(
        resp.embedder_bootstrap, "ready",
        "embedder_bootstrap must mirror ActiveBackend::bootstrap"
    );

    let json: Value = serde_json::to_value(&resp).expect("serialize");
    let info = json
        .get("embedder_info")
        .expect("embedder_info must be present");
    assert_eq!(info["backend"].as_str(), Some("python"));
    assert_eq!(info["provider"].as_str(), Some("MPS"));
    assert_eq!(info["quantized"].as_bool(), Some(false));
    assert_eq!(info["model"].as_str(), Some("all-MiniLM-L6-v2"));
}

/// The same switchable-backend path for the default ort backend —
/// `backend=ort`, and `quantized` reflects `ActiveBackend::quantized`
/// verbatim (true when the operator opted into `TRUSTY_EMBEDDER_MODEL=int8`,
/// false for the fp32 default), not the old always-true `dimension == 384`
/// prediction.
/// Test: this test.
#[tokio::test]
async fn health_reports_switchable_ort_backend_info() {
    for quantized in [false, true] {
        let state = SearchAppState::new(IndexRegistry::new());
        let inner: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
        let active = ActiveBackend {
            kind: BackendKind::Ort,
            provider: trusty_common::embedder::ExecutionProvider::CoreMLAne,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized,
            bootstrap: BootstrapState::NotApplicable,
        };
        let switchable = Arc::new(SwitchableEmbedder::new(inner, active));
        state.install_switchable_embedder(Arc::clone(&switchable));

        let state_arc = Arc::new(state);
        let Json(resp) = health_handler(State(state_arc)).await;
        assert_eq!(resp.embedder_bootstrap, "n/a");

        let json: Value = serde_json::to_value(&resp).expect("serialize");
        let info = json
            .get("embedder_info")
            .expect("embedder_info must be present");
        assert_eq!(info["backend"].as_str(), Some("ort"));
        assert_eq!(
            info["quantized"].as_bool(),
            Some(quantized),
            "quantized must reflect ActiveBackend::quantized exactly, not a dimension guess"
        );
    }
}

/// `HealthResponse::embedder_bootstrap` must mirror every
/// `BootstrapState` variant `ActiveBackend::bootstrap` can carry, not just
/// the two values the switchable-backend tests above happen to exercise
/// (`Ready` / `NotApplicable`) — covers `Bootstrapping`, `Failed`, and
/// `FellBackToOrt` too.
/// What: installs a `SwitchableEmbedder` with each `BootstrapState` variant
/// in turn and asserts `/health`'s `embedder_bootstrap` field via
/// `bootstrap_state_str`'s mapping.
/// Test: this test.
#[tokio::test]
async fn health_reports_embedder_bootstrap_state() {
    for (bootstrap, expected) in [
        (BootstrapState::NotApplicable, "n/a"),
        (BootstrapState::Bootstrapping, "bootstrapping"),
        (BootstrapState::Ready, "ready"),
        (BootstrapState::Failed, "failed"),
        (BootstrapState::FellBackToOrt, "fell_back_to_ort"),
    ] {
        let state = SearchAppState::new(IndexRegistry::new());
        let inner: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
        let active = ActiveBackend {
            kind: BackendKind::Ort,
            provider: trusty_common::embedder::ExecutionProvider::Cpu,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap,
        };
        let switchable = Arc::new(SwitchableEmbedder::new(inner, active));
        state.install_switchable_embedder(Arc::clone(&switchable));

        let state_arc = Arc::new(state);
        let Json(resp) = health_handler(State(state_arc)).await;
        assert_eq!(
            resp.embedder_bootstrap, expected,
            "BootstrapState::{bootstrap:?} must report {expected:?}"
        );
    }
}

/// Why (#4125): a permanent embedder capability downgrade reached NO field a
/// monitor gates on. `embedder_bootstrap: "failed"` (the graceful python
/// bootstrap gave up for this daemon's lifetime) and `"fell_back_to_ort"` (the
/// swap-back watchdog abandoned a dead python/MPS sidecar) both sat next to
/// `status: "ok"` forever, so a silent MPS -> CPU regression was
/// indistinguishable from a healthy daemon. `embedder: "ready"` is not the
/// misreport — it describes the currently-active backend, which really is
/// ready; nothing aggregated "we did not get the backend we asked for".
/// What: drives every `BootstrapState` through `/health` and asserts the exact
/// degraded/ok partition — the two terminal downgrades degrade, while
/// `Bootstrapping` (transient, every Apple-Silicon boot passes through it),
/// `Ready`, and `NotApplicable` stay `"ok"`. Against the pre-fix aggregation
/// the first two assertions fail; the last three pin the fix against
/// over-flagging. A daemon with no switchable handle installed yet is checked
/// too, since that path reports `"n/a"` through a different branch.
/// Test: this test.
#[tokio::test]
async fn health_is_degraded_when_the_embedder_backend_permanently_downgraded() {
    for (bootstrap, expected_status) in [
        (BootstrapState::Failed, "degraded"),
        (BootstrapState::FellBackToOrt, "degraded"),
        (BootstrapState::Bootstrapping, "ok"),
        (BootstrapState::Ready, "ok"),
        (BootstrapState::NotApplicable, "ok"),
    ] {
        let state = SearchAppState::new(IndexRegistry::new());
        let inner: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(384));
        let active = ActiveBackend {
            kind: BackendKind::Ort,
            provider: trusty_common::embedder::ExecutionProvider::Cpu,
            model: "all-MiniLM-L6-v2".to_string(),
            quantized: false,
            bootstrap,
        };
        let switchable = Arc::new(SwitchableEmbedder::new(inner, active));
        state.install_switchable_embedder(Arc::clone(&switchable));

        let state_arc = Arc::new(state);
        let Json(resp) = health_handler(State(state_arc)).await;
        assert_eq!(
            resp.status, expected_status,
            "BootstrapState::{bootstrap:?} must drive status {expected_status:?}, \
             got {:?} (embedder_bootstrap={:?})",
            resp.status, resp.embedder_bootstrap
        );
    }

    // No switchable handle installed at all — the `"n/a"` branch must stay ok.
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(resp.embedder_bootstrap, "n/a");
    assert_eq!(resp.status, "ok");
}
