//! A 404 from an index-scoped handler must mean "no such index anywhere"
//! (issue #4715).
//!
//! Why: the MCP layer reads a 404 on the index it advertised as this session's
//! default and reports `INDEX_NOT_READY` — "never built". That is only true if
//! the handler's 404 rules out the cold store too. `search_handler` always did;
//! `index_status_handler`, `get_index_chunks_handler`, and `grep_handler`
//! did a bare hot-registry lookup, so a cold-parked index — a real index that
//! merely is not resident — would have been reported as one that never existed.
//! That is the same class of lie #4715 exists to remove, pointed the other way.
//!
//! Reachability is not hypothetical or gated: `restore_eager_entry`
//! (`commands/start/restore.rs`) parks any index whose eager restore times out
//! into the cold store on the DEFAULT configuration (#4087/#4250), no env var
//! involved. `TRUSTY_MAX_RESIDENT_INDEXES` (#2161) is a second, opt-in route to
//! the same state.
//!
//! What: drives each handler directly against a state whose index is only in
//! the cold store, and asserts 503 rather than 404.
//! Test: this file.

use super::*;
use crate::core::registry::{IndexId, IndexRegistry};
use crate::service::persistence::PersistedIndex;
use axum::extract::State;
use axum::http::StatusCode;
use std::path::PathBuf;

/// Build the two request types from JSON rather than adding a `Default` derive
/// to a production struct for a test's convenience — every field on both is
/// `#[serde(default)]`, so `{}` is the canonical empty request.
fn chunks_params() -> super::files::ChunksParams {
    serde_json::from_value(serde_json::json!({})).expect("empty ChunksParams")
}

fn grep_request() -> crate::service::grep::GrepRequest {
    serde_json::from_value(serde_json::json!({ "pattern": "fn main" })).expect("GrepRequest")
}

fn cold_entry(id: &str) -> PersistedIndex {
    PersistedIndex {
        id: id.to_string(),
        root_path: PathBuf::from(format!("/tmp/trusty-4715-{id}")),
        ..Default::default()
    }
}

/// Build a state whose only knowledge of `id` is a cold-store entry.
fn state_with_cold_index(id: &str) -> Arc<SearchAppState> {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    state.cold_store.register_cold_entries(vec![cold_entry(id)]);
    assert!(
        state.cold_store.contains(&IndexId::new(id.to_string())),
        "precondition: the entry is cold, not resident"
    );
    state
}

/// `index_status` on a cold-parked index is 503, not 404.
///
/// Why: 404 would let the MCP layer classify a BUILT index as `not_indexed`.
#[tokio::test]
async fn cold_parked_index_status_is_503_not_404() {
    let state = state_with_cold_index("cold-a");
    let err = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-a".to_string()),
    )
    .await
    .expect_err("a non-resident index cannot be reported");
    assert_eq!(err, StatusCode::SERVICE_UNAVAILABLE);
}

/// A permanently restore-failed index is also 503 — it exists, it just cannot
/// be served (#1106).
#[tokio::test]
async fn restore_failed_index_status_is_503_not_404() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let id = IndexId::new("failed-a".to_string());
    state
        .cold_store
        .register_cold_entries(vec![cold_entry("failed-a")]);
    state.cold_store.mark_failed(&id);
    assert!(state.cold_store.is_failed(&id));

    let err = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("failed-a".to_string()),
    )
    .await
    .expect_err("a failed index cannot be reported");
    assert_eq!(err, StatusCode::SERVICE_UNAVAILABLE);
}

/// The 404 survives for an id that is in NO store — the case the MCP layer is
/// entitled to read as "never indexed".
#[tokio::test]
async fn status_404_only_when_absent_from_every_store() {
    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let err = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("never-existed".to_string()),
    )
    .await
    .expect_err("genuinely unknown");
    assert_eq!(err, StatusCode::NOT_FOUND);
}

/// `list_chunks` follows the same rule as `index_status` — it is routed
/// through the same MCP classification.
#[tokio::test]
async fn cold_parked_index_chunks_is_503_not_404() {
    let state = state_with_cold_index("cold-b");
    let err = super::files::get_index_chunks_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-b".to_string()),
        axum::extract::Query(chunks_params()),
    )
    .await
    .expect_err("a non-resident index has no enumerable chunks");
    assert_eq!(err, StatusCode::SERVICE_UNAVAILABLE);

    let absent = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let err = super::files::get_index_chunks_handler(
        State(absent),
        axum::extract::Path("never-existed".to_string()),
        axum::extract::Query(chunks_params()),
    )
    .await
    .expect_err("genuinely unknown");
    assert_eq!(err, StatusCode::NOT_FOUND);
}

/// Per-index `grep` follows the same rule, and says why in the body.
#[tokio::test]
async fn cold_parked_index_grep_is_503_not_404() {
    let state = state_with_cold_index("cold-c");
    let (code, axum::Json(body)) = super::files::grep_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("cold-c".to_string()),
        axum::Json(grep_request()),
    )
    .await
    .expect_err("a non-resident index has no files to grep");
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_not_resident");

    let absent = Arc::new(SearchAppState::new(IndexRegistry::new()));
    let (code, _) = super::files::grep_handler(
        State(absent),
        axum::extract::Path("never-existed".to_string()),
        axum::Json(grep_request()),
    )
    .await
    .expect_err("genuinely unknown");
    assert_eq!(code, StatusCode::NOT_FOUND);
}

/// A permanently-failed restore never clears on its own, so grep's body must
/// name the operator action (`search_handler`'s wording) rather than telling
/// the caller to wait.
///
/// The state built here is the OVERLAP — in both `entries` and
/// `failed_entries` — which is what makes the arm order load-bearing: a
/// `contains`-first match would answer "retry after it restores" for an index
/// that never will. `mark_failed` alone does not produce it (it removes the
/// entry, `store.rs:541`); a later `register_cold_entries` does, because
/// `register_with_reason` never consults the failed set and nothing ever
/// clears it.
#[tokio::test]
async fn restore_failed_index_grep_says_restart_not_retry() {
    let state = state_with_cold_index("failed-c");
    let id = IndexId::new("failed-c".to_string());
    state.cold_store.mark_failed(&id);
    assert!(
        !state.cold_store.contains(&id),
        "mark_failed drops the entry"
    );
    state
        .cold_store
        .register_cold_entries(vec![cold_entry("failed-c")]);
    assert!(
        state.cold_store.contains(&id),
        "re-registered as a cold entry"
    );
    assert!(state.cold_store.is_failed(&id), "and still a failed one");

    let (code, axum::Json(body)) = super::files::grep_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("failed-c".to_string()),
        axum::Json(grep_request()),
    )
    .await
    .expect_err("a failed index cannot be grepped");
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "index_restore_failed");
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("restart the daemon or re-register"),
        "must name the operator action, got: {message}"
    );
    assert!(
        !message.contains("retry after it restores"),
        "must not tell a permanently-failed index to wait, got: {message}"
    );
}
