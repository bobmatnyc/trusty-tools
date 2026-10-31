//! `search.project.ensure` from tm's side (#10531).
//!
//! Why: registration must hand the daemon exactly the wire shape it expects,
//! read an overlap refusal's index id, and treat a dead daemon as a fact to log.
//! Test: this module.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::{EnsureOutcome, ensure_call, plan};
use crate::project::Project;
use crate::uds_mock::{self, MockFuture, RpcError};

const T: Duration = Duration::from_secs(5);

fn project(repo_url: &str, key: Option<&str>) -> Project {
    Project {
        name: "demo".into(),
        repo_url: repo_url.into(),
        default_branch: "main".into(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
        project_key: key.map(str::to_string),
    }
}

/// Why: the wire shape is the contract with the daemon's method.
/// Test: this test.
#[tokio::test]
async fn ensure_call_sends_the_wire_shape_and_reads_the_index_id() {
    let seen: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::default();
    let rec = Arc::clone(&seen);
    let daemon = uds_mock::spawn(move |m, p| -> MockFuture {
        rec.lock().expect("lock").push((m.to_string(), p));
        Box::pin(async { Ok(json!({ "index_id": "demo", "created": true })) })
    })
    .await;
    let out = ensure_call(
        daemon.socket(),
        "o/demo",
        "demo",
        Path::new("/work/demo"),
        T,
    )
    .await;
    assert_eq!(
        out,
        EnsureOutcome::Ensured {
            index_id: "demo".into(),
            created: true
        }
    );
    let calls = seen.lock().expect("lock").clone();
    assert_eq!(
        calls,
        vec![(
            "search.project.ensure".to_string(),
            json!({ "project_key": "o/demo", "name": "demo", "root": "/work/demo" })
        )]
    );
}

/// Why: an overlap refusal names the index in the way; tm must surface it.
/// Test: this test.
#[tokio::test]
async fn an_overlap_refusal_names_the_overlapping_index() {
    let daemon = uds_mock::spawn(|_, _| -> MockFuture {
        Box::pin(async {
            Err(
                RpcError::new(-32009, "project_root_overlap: inside cto").with_data(
                    json!({ "error": "project_root_overlap", "overlapping_index_id": "cto" }),
                ),
            )
        })
    })
    .await;
    let out = ensure_call(daemon.socket(), "o/a", "a", Path::new("/work/cto/a"), T).await;
    assert_eq!(
        out,
        EnsureOutcome::Refused {
            code: -32009,
            message: "project_root_overlap: inside cto".into(),
            overlapping_index_id: Some("cto".into()),
        }
    );
}

/// Why: daemon down must be an outcome, not a raised error (acceptance 3).
/// Test: this test.
#[tokio::test]
async fn a_dead_socket_is_unreachable() {
    let dir = tempfile::tempdir().expect("dir");
    let out = ensure_call(
        &dir.path().join("none.sock"),
        "o/a",
        "a",
        Path::new("/w/a"),
        T,
    )
    .await;
    assert!(matches!(out, EnsureOutcome::Unreachable(_)), "{out:?}");
}

/// Why: no key or no checkout means nothing to send.
/// Test: this test.
#[test]
fn plan_is_empty_without_a_key_or_a_checkout() {
    let dir = tempfile::tempdir().expect("dir");
    let here = dir.path().to_str().expect("utf8");
    assert_eq!(
        plan(&project(here, Some("o/demo"))),
        Some(dir.path().to_path_buf())
    );
    assert_eq!(plan(&project(here, None)), None, "no key");
    assert_eq!(plan(&project(here, Some("  "))), None, "blank key");
    assert_eq!(
        plan(&project("/no/such/dir-10531", Some("o/demo"))),
        None,
        "no checkout"
    );
}
