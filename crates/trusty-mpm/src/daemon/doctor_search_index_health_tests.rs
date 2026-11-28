//! Tests for the `search_index_*` doctor rows (#10535).
//!
//! Why: an unreadable answer must never read as `Ok`, and the orphan count
//! must reach the row.
//! What: a mock socket answers `search.project.health`; a dead socket stands
//! for a daemon that does not answer.
//! Test: this module IS the test suite.

use serde_json::Value;
use trusty_common::search_rpc::METHOD_PROJECT_HEALTH;

use super::*;
use crate::uds_mock::{self, MockFuture, RpcError};

/// A daemon that answers `search.project.health` with `answer`.
async fn daemon(answer: Result<Value, RpcError>) -> uds_mock::MockUdsDaemon {
    uds_mock::spawn(move |method: &str, _params: Value| -> MockFuture {
        let answer = if method == METHOD_PROJECT_HEALTH {
            answer.clone()
        } else {
            Err(RpcError::new(CODE_METHOD_NOT_FOUND, "method not found"))
        };
        Box::pin(async move { answer })
    })
    .await
}

fn counts(parked: u64, orphaned: u64) -> Value {
    serde_json::json!({
        "registered": 6, "resident": 2, "parked": parked,
        "orphaned": orphaned, "never_queried": 1,
    })
}

#[tokio::test]
async fn orphaned_parks_are_warn_with_the_count() {
    let d = daemon(Ok(counts(3, 2))).await;
    let rows = check_search_index_health_at(d.socket()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, ORPHAN_PARKED);
    assert_eq!(rows[0].status, CheckStatus::Warn, "{}", rows[0].message);
    assert!(
        rows[0].message.starts_with("2 parked"),
        "{}",
        rows[0].message
    );
    assert!(rows[0].message.contains("trusty-search project map"));
}

#[tokio::test]
async fn no_orphaned_park_is_ok() {
    let d = daemon(Ok(counts(1, 0))).await;
    let rows = check_search_index_health_at(d.socket()).await;
    assert_eq!(rows[0].status, CheckStatus::Ok, "{}", rows[0].message);
}

#[tokio::test]
async fn a_daemon_without_the_method_is_not_checked() {
    let d = daemon(Err(RpcError::new(
        CODE_METHOD_NOT_FOUND,
        "method not found",
    )))
    .await;
    let rows = check_search_index_health_at(d.socket()).await;
    assert_eq!(rows[0].status, CheckStatus::Warn);
    assert!(
        rows[0].message.starts_with("not checked") && rows[0].message.contains("predates"),
        "{}",
        rows[0].message
    );
}

#[tokio::test]
async fn a_body_without_the_counts_is_not_checked() {
    let d = daemon(Ok(serde_json::json!({ "registered": 6 }))).await;
    let rows = check_search_index_health_at(d.socket()).await;
    assert_eq!(rows[0].status, CheckStatus::Warn);
    assert!(
        rows[0].message.starts_with("not checked"),
        "{}",
        rows[0].message
    );
}

#[tokio::test]
async fn a_down_daemon_leaves_orphan_parked_not_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rows = check_search_index_health_at(&dir.path().join("none.sock")).await;
    assert_eq!(rows[0].name, ORPHAN_PARKED);
    assert_eq!(rows[0].status, CheckStatus::Warn);
    assert!(
        rows[0].message.starts_with("not checked"),
        "{}",
        rows[0].message
    );
}
