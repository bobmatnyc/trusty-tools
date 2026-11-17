//! Tests for [`IndexRestore`] (#10989): a new session unparks only its
//! project's decommission park, and an unconfirmed unpark is a failure.
//!
//! Hermetic: trusty-search is a `uds_mock` stand-in that keeps a project map.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{IndexRestore, METHOD_MAP, METHOD_UNPARK, RestoreState, RestoreTarget};
use crate::session_manager::index_shutdown::tests::{Answer, healthy_memory, rig};
use crate::session_manager::index_shutdown::{METHOD_PARK, METHOD_RESOLVE};
use crate::uds_mock::RpcError;

/// trusty-search's "not found" code.
const NOT_FOUND: i64 = -32004;

/// The parked rows a [`stateful_search`] holds.
pub(crate) type Parked = Arc<Mutex<Vec<Value>>>;

/// A parked row for `widget-idx` of `acme/widget`, with `reason`.
pub(crate) fn row(index_id: &str, key: &str, reason: &str) -> Value {
    json!({
        "index_id": index_id, "key": key, "root_path": "/repos/acme/widget",
        "reason": reason, "parked_unix": 1,
    })
}

/// A trusty-search that keeps a map: resolve misses a parked `widget-idx`,
/// a decommission park adds its row, unpark removes a row and confirms.
pub(crate) fn stateful_search(parked: &Parked) -> Answer {
    let parked = Arc::clone(parked);
    Arc::new(move |method, params| {
        let mut rows = parked.lock().expect("parked");
        let is_parked = |rows: &Vec<Value>, id: &str| rows.iter().any(|r| r["index_id"] == id);
        match method {
            METHOD_RESOLVE if is_parked(&rows, "widget-idx") => {
                Err(RpcError::new(NOT_FOUND, "no live index"))
            }
            METHOD_RESOLVE => Ok(json!({ "index_id": "widget-idx" })),
            METHOD_PARK => {
                let reason = if params["decommission"] == true {
                    "decommissioned"
                } else {
                    "operator"
                };
                let id = params["index_id"].as_str().unwrap_or_default().to_string();
                rows.push(row(&id, "acme/widget", reason));
                Ok(json!({ "index_id": id, "parked": true, "unloaded": true }))
            }
            METHOD_MAP => Ok(json!({ "projects": [], "parked": *rows })),
            METHOD_UNPARK => {
                let id = params["index_id"].as_str().unwrap_or_default().to_string();
                let before = rows.len();
                rows.retain(|r| r["index_id"] != id.as_str());
                if rows.len() == before {
                    return Err(RpcError::new(NOT_FOUND, format!("{id} is not parked")));
                }
                Ok(json!({ "index_id": id, "unparked": true, "watching": true }))
            }
            other => Err(RpcError::new(-32601, format!("unexpected {other}"))),
        }
    })
}

fn target() -> RestoreTarget {
    RestoreTarget {
        identity: Some("acme/widget".into()),
        root: Some(PathBuf::from("/elsewhere/session")),
    }
}

/// Why: #10989 acceptance 4 — only a `decommissioned` park of this project is
/// undone; an operator park of the same project and another project's
/// decommission park stay parked.
/// Test: this test. Fails if `RestoreTarget::owns` drops the reason check or
/// the project match.
#[tokio::test]
async fn restore_unparks_only_the_projects_decommission_park() {
    let parked: Parked = Arc::new(Mutex::new(vec![
        row("widget-idx", "acme/widget", "decommissioned"),
        row("widget-wt", "acme/widget", "operator"),
        row("gadget-idx", "acme/gadget", "decommissioned"),
    ]));
    let rig = rig(stateful_search(&parked), healthy_memory()).await;

    let state = IndexRestore::at(Some(rig.search_socket()))
        .restore(&target())
        .await;

    assert_eq!(
        state,
        RestoreState::Unparked {
            ids: vec!["widget-idx".into()]
        }
    );
    let left: Vec<Value> = parked.lock().expect("parked").clone();
    let left: Vec<&str> = left.iter().filter_map(|r| r["index_id"].as_str()).collect();
    assert_eq!(
        left,
        vec!["widget-wt", "gadget-idx"],
        "the others stay parked"
    );
    assert_eq!(
        rig.params_of(METHOD_UNPARK),
        Some(json!({ "index_id": "widget-idx" }))
    );
}

/// Why: #10989 acceptance 4 — a project with only a non-decommission park is
/// not touched: no unpark is sent.
/// Test: this test.
#[tokio::test]
async fn an_operator_park_is_left_parked() {
    let parked: Parked = Arc::new(Mutex::new(vec![row(
        "widget-idx",
        "acme/widget",
        "operator",
    )]));
    let rig = rig(stateful_search(&parked), healthy_memory()).await;

    let state = IndexRestore::at(Some(rig.search_socket()))
        .restore(&target())
        .await;

    assert_eq!(state, RestoreState::NothingParked);
    assert_eq!(parked.lock().expect("parked").len(), 1, "still parked");
    assert!(
        !rig.methods().contains(&format!("search:{METHOD_UNPARK}")),
        "{:?}",
        rig.methods()
    );
}

/// Why: a project with no identity matches a row by the checkout it contains.
/// Test: this test. Fails if the root arm of `RestoreTarget::owns` is removed.
#[tokio::test]
async fn a_project_without_identity_matches_by_root() {
    let parked: Parked = Arc::new(Mutex::new(vec![row(
        "widget-idx",
        "root:/repos/acme/widget",
        "decommissioned",
    )]));
    let rig = rig(stateful_search(&parked), healthy_memory()).await;
    let target = RestoreTarget {
        identity: None,
        root: Some(PathBuf::from("/repos/acme/widget/.claude/worktrees/s1")),
    };

    let state = IndexRestore::at(Some(rig.search_socket()))
        .restore(&target)
        .await;

    assert_eq!(
        state,
        RestoreState::Unparked {
            ids: vec!["widget-idx".into()]
        }
    );
}

/// Why: Fail-Open Check (#10989) — an unpark the daemon refuses, or answers
/// without `unparked: true`, is a failure naming the index; so is a map that
/// cannot be read.
/// Test: this test, one row per arm. Treating any arm as success fails it.
#[tokio::test]
async fn a_refused_unpark_is_a_failure_naming_the_index() {
    let decommissioned =
        || json!({ "parked": [row("widget-idx", "acme/widget", "decommissioned")] });
    let arms: Vec<(&str, Answer, Vec<String>)> = vec![
        (
            "unpark refused",
            Arc::new(move |m: &str, _: &Value| match m {
                METHOD_MAP => Ok(decommissioned()),
                _ => Err(RpcError::new(-32603, "project map unwritable")),
            }),
            vec!["widget-idx".into()],
        ),
        (
            "unpark unconfirmed",
            Arc::new(move |m: &str, _: &Value| match m {
                METHOD_MAP => Ok(decommissioned()),
                _ => Ok(json!({ "index_id": "widget-idx", "unparked": false })),
            }),
            vec!["widget-idx".into()],
        ),
        (
            "map unreadable",
            Arc::new(|_: &str, _: &Value| Err(RpcError::new(-32603, "map corrupt"))),
            vec![],
        ),
    ];
    for (name, search, want_ids) in arms {
        let rig = rig(search, healthy_memory()).await;
        let state = IndexRestore::at(Some(rig.search_socket()))
            .restore(&target())
            .await;
        match state {
            RestoreState::Failed { ids, error } => {
                assert_eq!(ids, want_ids, "{name}: {error}");
                assert!(!error.is_empty(), "{name}");
            }
            other => panic!("{name}: expected Failed, got {other:?}"),
        }
    }
}

/// Why: no trusty-search daemon means nothing can be parked-and-loaded; it is
/// reported, never a failure and never "unparked".
/// Test: this test.
#[tokio::test]
async fn no_daemon_is_not_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = IndexRestore::at(Some(dir.path().join("absent.sock")))
        .restore(&target())
        .await;
    assert_eq!(state, RestoreState::NotRunning);
}
