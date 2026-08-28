//! Tests for the HTTP-refusal-to-JSON-RPC-error mapping (#6285 slice 2).
//!
//! Test: this file IS the test module for `super`.

use axum::http::StatusCode;
use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS};

use super::{
    code_for, refusal_is_permanent, rpc_error_from_http, CODE_NOT_FOUND, CODE_UNAVAILABLE,
    CODE_UNAVAILABLE_PERMANENT,
};

/// Why: the code is the ONLY thing a socket client can branch on — `RpcError`
/// carries no body — so the whole index-scoped contract's retryable/permanent
/// split rides on this table. A 503 that will clear and one that never will
/// must not collapse onto one code.
/// Test: this function IS the test.
#[test]
fn status_and_permanence_pick_the_code() {
    assert_eq!(code_for(400, false), CODE_INVALID_PARAMS);
    assert_eq!(code_for(404, false), CODE_NOT_FOUND);
    assert_eq!(code_for(503, false), CODE_UNAVAILABLE);
    assert_eq!(code_for(503, true), CODE_UNAVAILABLE_PERMANENT);
    assert_eq!(code_for(500, false), CODE_INTERNAL_ERROR);
    // A status this surface does not produce is internal rather than a silent
    // mis-classification as one of the coded families.
    assert_eq!(code_for(418, true), CODE_INTERNAL_ERROR);
}

/// Why: `retryable` settles permanence wherever it is present, and both arms
/// have to be read — a body that said `retryable: true` and was read as
/// permanent would tell a caller to give up on a cold-parked index a search
/// would restore.
/// Test: this function IS the test.
#[test]
fn a_retryable_field_settles_permanence() {
    assert!(!refusal_is_permanent(
        &serde_json::json!({ "retryable": true })
    ));
    assert!(refusal_is_permanent(
        &serde_json::json!({ "retryable": false })
    ));
    // `retryable` wins over `reason` — an explicit field is never overridden by
    // the fallback that exists because it is missing.
    assert!(!refusal_is_permanent(
        &serde_json::json!({ "retryable": true, "reason": "skipped_by_config" })
    ));
}

/// Why: `kg_unavailable` is the one refusal on this surface whose body predates
/// the `retryable` field, and a `skip_kg` index answers it identically forever.
/// This drives the body `call_chain_report` actually builds, so adding
/// `retryable` to that body later does not silently make this test vacuous —
/// the arm above covers that case and this one keeps covering today's.
/// Test: this function IS the test.
#[test]
fn a_kg_disabled_lane_is_permanent_without_a_retryable_field() {
    let body = serde_json::json!({
        "error": "kg_unavailable",
        "reason": "skipped_by_config",
        "index": "demo",
    });
    assert!(refusal_is_permanent(&body));
    assert_eq!(
        rpc_error_from_http(StatusCode::SERVICE_UNAVAILABLE, &body).code,
        CODE_UNAVAILABLE_PERMANENT
    );
}

/// Why: a 503 body with neither discriminant means "not now", and telling a
/// caller to stop retrying on that basis would strand it.
/// Test: this function IS the test.
#[test]
fn an_unrecognised_503_body_is_treated_as_retryable() {
    let body = serde_json::json!({ "error": "something_new" });
    assert!(!refusal_is_permanent(&body));
    assert_eq!(
        rpc_error_from_http(StatusCode::SERVICE_UNAVAILABLE, &body).code,
        CODE_UNAVAILABLE
    );
}

/// Why: `error` names the refusal class and `message` carries the operator
/// detail, and the two live in separate fields. Picking one drops the other —
/// `index_not_resident` alone never says a search reloads it, and the prose
/// alone never gives a client a token to match.
/// Test: this function IS the test.
#[test]
fn refusal_message_joins_error_and_message() {
    let body = serde_json::json!({
        "error": "index_not_resident",
        "index_id": "demo",
        "retryable": true,
        "message": "cold-parked; a search reloads it",
    });
    let err = rpc_error_from_http(StatusCode::SERVICE_UNAVAILABLE, &body);
    assert_eq!(err.code, CODE_UNAVAILABLE);
    assert_eq!(
        err.message,
        "index_not_resident: cold-parked; a search reloads it"
    );
}

/// Why: `residency_miss_response`'s 404 arm carries `error` and no `message`,
/// and it is the most-hit refusal on this surface. It must not render as
/// `"index_not_resident: "` or as an empty string.
/// Test: this function IS the test.
#[test]
fn refusal_with_only_an_error_field_renders_that_field() {
    let body = serde_json::json!({ "error": "unknown index: demo", "index_id": "demo" });
    let err = rpc_error_from_http(StatusCode::NOT_FOUND, &body);
    assert_eq!(err.code, CODE_NOT_FOUND);
    assert_eq!(err.message, "unknown index: demo");
}

/// Why: a body shape no current route produces must still say something an
/// operator can act on. Rendering the whole body is never nothing; an empty
/// message would be.
/// Test: this function IS the test.
#[test]
fn refusal_without_an_error_field_renders_the_whole_body() {
    let body = serde_json::json!({ "unexpected": true });
    let err = rpc_error_from_http(StatusCode::INTERNAL_SERVER_ERROR, &body);
    assert_eq!(err.code, CODE_INTERNAL_ERROR);
    assert!(
        err.message.contains("unexpected"),
        "an unrecognised body must still name what it carried: {}",
        err.message
    );
}

/// Why: the permanent arm is what a caller reads as "stop polling". This drives
/// the real `index_restore_failed` body rather than a hand-built one, so a
/// change to that builder's `retryable` field fails here.
/// Test: this function IS the test.
#[test]
fn a_permanently_failed_restore_reports_the_permanent_code() {
    let body = serde_json::json!({
        "error": "index_restore_failed",
        "index_id": "demo",
        "retryable": false,
        "message": "restart the daemon or re-register to retry",
    });
    let err = rpc_error_from_http(StatusCode::SERVICE_UNAVAILABLE, &body);
    assert_eq!(
        err.code, CODE_UNAVAILABLE_PERMANENT,
        "a refusal that never clears must not share a code with one that does"
    );
}
