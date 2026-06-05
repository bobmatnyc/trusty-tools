//! Status-computation tests for `service::handlers` (#722).
//!
//! Why: split from `handlers_tests.rs` to keep both files under the 500-line
//! cap while preserving full coverage of the `compute_status` helper and the
//! HTTP handler integration for dep-reachability degradation.
//! What: pure unit tests for `compute_status` (no async / HTTP) and two async
//! integration tests that exercise `handle_health` with fake search clients.
//! Test: this is the test module; each function is a self-contained unit test.

use std::sync::Arc;

use axum::{body::to_bytes, extract::State, http::StatusCode, response::IntoResponse as _};

use crate::service::handlers::{AppState, DepInfo, DepStatus, compute_status, handle_health};
use crate::service::inference_probe::InferenceStatus;

// Re-use the fakes defined in `handlers_tests.rs` which is loaded first.
use super::tests::{FailSearch, FakeAnalyze, FakeLlm, FakeSearch};

// ── compute_status unit tests (#722) ──────────────────────────────────────────

/// compute_status returns "ok" when inference is ok and all required deps reachable.
///
/// Why: the happy-path gate for #722 — verifies the base "ok" case is unbroken.
/// What: calls compute_status with Ok inference + both deps reachable; asserts "ok".
/// Test: this test itself.
#[test]
fn health_status_ok_all_good() {
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: true,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: true,
        },
    };
    assert_eq!(
        compute_status(InferenceStatus::Ok, &deps),
        "ok",
        "all good → status must be ok"
    );
}

/// compute_status returns "degraded" when a required dep is unreachable (#722).
///
/// Why: the core of the #722 fix — a required dep being down must flip status
/// to "degraded" even when inference itself is fine.
/// What: calls compute_status with Ok inference + trusty_search unreachable;
/// asserts "degraded".
/// Test: this test itself.
#[test]
fn health_status_degraded_required_dep_down() {
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: false, // required dep is down
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: true,
        },
    };
    assert_eq!(
        compute_status(InferenceStatus::Ok, &deps),
        "degraded",
        "required dep down → status must be degraded"
    );
}

/// compute_status returns "degraded" when inference fails, even if all deps reachable.
///
/// Why: preserves the existing #719 behavior — inference failure alone must degrade.
/// What: calls compute_status with AuthError inference + all deps reachable; asserts
/// "degraded".
/// Test: this test itself.
#[test]
fn health_status_degraded_inference_auth_error() {
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: true,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: true,
        },
    };
    assert_eq!(
        compute_status(InferenceStatus::AuthError, &deps),
        "degraded",
        "auth_error inference → status must be degraded"
    );
}

/// compute_status stays "ok" when inference is `Unknown` (probe timed out) (#739).
///
/// Why: a slow Bedrock cold-start causes the probe to time out and return
/// `Unknown`.  This must NOT degrade health — real review calls have a much
/// longer budget than the probe window.  The probe reports "could not confirm"
/// rather than "definitely unreachable", so the top-level status should not
/// penalise the operator.
/// What: calls compute_status with Unknown inference + all deps reachable;
/// asserts "ok".
/// Test: this test itself.
#[test]
fn health_status_ok_inference_unknown() {
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: true,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: true,
        },
    };
    assert_eq!(
        compute_status(InferenceStatus::Unknown, &deps),
        "ok",
        "Unknown inference (probe timed out) must not degrade status (#739)"
    );
}

/// compute_status stays "ok" when only a non-required dep is unreachable (#722).
///
/// Why: a non-required dep being down must NOT degrade status — only required deps matter.
/// What: calls compute_status with Ok inference + trusty_analyze (required=false)
/// unreachable; asserts "ok".
/// Test: this test itself.
#[test]
fn health_status_ok_optional_dep_down() {
    let deps = DepStatus {
        trusty_search: DepInfo {
            required: true,
            reachable: true,
        },
        trusty_analyze: DepInfo {
            required: false,
            reachable: false, // optional dep down — must not degrade
        },
    };
    assert_eq!(
        compute_status(InferenceStatus::Ok, &deps),
        "ok",
        "optional dep down → status must remain ok"
    );
}

/// /health sets `status: "degraded"` when trusty_search (required) is unreachable.
///
/// Why: validates the full HTTP handler path for #722 — the integration between
/// handle_health, dep probing, and compute_status.
/// What: uses FailSearch (health() returns Err) + FakeLlm (ok inference); deserialises
/// response body; asserts status is "degraded" and deps.trusty_search.reachable is false.
/// Test: this test itself.
#[tokio::test]
async fn health_required_dep_down_sets_degraded() {
    let state = AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FailSearch),
        None,
    );
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "HTTP status must be 200 even when degraded (spec REV-706)"
    );

    let body_bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(
        body["status"], "degraded",
        "required dep (trusty_search) unreachable → status must be degraded"
    );
    assert_eq!(
        body["inference"], "ok",
        "inference must be ok (FakeLlm always succeeds)"
    );
    assert_eq!(
        body["deps"]["trusty_search"]["reachable"], false,
        "trusty_search.reachable must be false when search is down"
    );
    assert_eq!(
        body["deps"]["trusty_search"]["required"], true,
        "trusty_search.required must remain true"
    );
}

/// /health stays "ok" when only the optional dep (trusty_analyze) is unreachable.
///
/// Why: validates that only required deps can degrade status — optional dep failures
/// must appear only in deps.<name>.reachable, not in top-level status.
/// What: uses FakeSearch (ok) + FakeAnalyze (health() returns Err) + FakeLlm (ok);
/// asserts status is "ok" and deps.trusty_analyze.reachable is false.
/// Test: this test itself.
#[tokio::test]
async fn health_optional_dep_down_stays_ok() {
    let state = AppState::new(
        crate::config::ReviewConfig::load(None),
        Arc::new(FakeLlm),
        Arc::new(FakeSearch),
        Some(Arc::new(FakeAnalyze)),
    );
    let response = handle_health(State(state)).await;
    let resp: axum::response::Response = response.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(
        body["status"], "ok",
        "optional dep (trusty_analyze) unreachable → status must remain ok"
    );
    assert_eq!(
        body["deps"]["trusty_analyze"]["reachable"], false,
        "trusty_analyze.reachable must be false (FakeAnalyze.health() fails)"
    );
    assert_eq!(
        body["deps"]["trusty_search"]["reachable"], true,
        "trusty_search.reachable must be true (FakeSearch.health() succeeds)"
    );
}
