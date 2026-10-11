//! `run_doctor` end to end against a stand-in trusty-search daemon (#9169 B2).
//!
//! Why: these drive the whole report, not the project-map module, so they
//! compile against a tree that has no such module and fail there because the
//! rows are missing — the red-first proof for B2.
//! What: a mock socket answers `search.project.map` and
//! `search.project.resolve`; `TRUSTY_SEARCH_SOCKET` points `run_doctor` at it.
//! Test: this module IS the test suite.

use super::run_doctor;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::daemon::search_rpc;
use crate::uds_mock::{self, MockFuture, RpcError};

/// The method names B1 ships; spelled here so this file needs nothing new.
const MAP: &str = "search.project.map";
const RESOLVE: &str = "search.project.resolve";

/// One pin for `bobmatnyc/demo`, held by `migration`.
fn one_pin_map() -> serde_json::Value {
    serde_json::json!({
        "path": "/data/project-map.toml",
        "schema": 1,
        "projects": [{
            "key": "bobmatnyc/demo", "index_id": "demo-1a2b3c4d",
            "root_path": "/src/demo", "pinned_by": "migration", "pinned_unix": 1,
        }],
        "parked": [],
    })
}

/// Run the whole doctor with the search socket pointed at `handler`, and
/// return the named row.
async fn doctor_row<F>(handler: F, name: &str) -> DoctorCheck
where
    F: Fn(&str, serde_json::Value) -> MockFuture + Send + Sync + 'static,
{
    let daemon = uds_mock::spawn(handler).await;
    // SAFETY: `#[serial]` on every caller keeps env writers apart.
    unsafe {
        std::env::set_var(search_rpc::TRUSTY_SEARCH_SOCKET_ENV, daemon.socket());
    }
    let report = run_doctor(None, None, &[], None).await;
    unsafe {
        std::env::remove_var(search_rpc::TRUSTY_SEARCH_SOCKET_ENV);
    }
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no `{name}` row in the doctor report: {names:?}"))
}

/// A live worktree index beside its group's pinned main checkout is a
/// duplicate the operator should park (#9169 B2 red-first).
#[tokio::test]
#[serial_test::serial]
async fn duplicate_index_group_is_warn() {
    let handler = |method: &str, _params: serde_json::Value| -> MockFuture {
        let answer = match method {
            MAP => Ok(one_pin_map()),
            RESOLVE => Ok(serde_json::json!({
                "index_id": "demo-1a2b3c4d", "root_path": "/src/demo",
                "repo_identity": "bobmatnyc/demo", "kind": "main_checkout",
                "matched_by": "index_id", "pinned_by": "migration",
                "duplicates": [{
                    "index_id": "demo-wt-9f8e7d6c", "root_path": "/src/demo/.worktrees/wt",
                    "repo_identity": "bobmatnyc/demo", "kind": "worktree", "parked": false,
                }],
            })),
            _ => Err(RpcError::new(-32601, "method not found")),
        };
        Box::pin(async move { answer })
    };
    let row = doctor_row(handler, "search_project_duplicate_index").await;
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("demo-wt-9f8e7d6c"), "{}", row.message);
    assert!(row.message.contains("project park"), "{}", row.message);
}

/// Fail-Open error arm: a map the daemon cannot read is `Fail`, and the
/// checks that need it say they did not run instead of reporting `Ok`.
#[tokio::test]
#[serial_test::serial]
async fn unreadable_project_map_is_fail_not_ok() {
    let handler = |method: &str, _params: serde_json::Value| -> MockFuture {
        let answer = match method {
            MAP | RESOLVE => Err(RpcError::new(
                -32603,
                "project map /data/project-map.toml is unreadable: expected `=`; \
                 run `trusty-search project rebuild-map --yes`",
            )),
            _ => Err(RpcError::new(-32601, "method not found")),
        };
        Box::pin(async move { answer })
    };
    let row = doctor_row(handler, "search_project_map_unreadable").await;
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message.contains("/data/project-map.toml"),
        "{}",
        row.message
    );
}
