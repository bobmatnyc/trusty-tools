//! `NoIndex` for a parked index names the park and `ensure_project` (#11417).
//!
//! Why: the daemon lists a parked index in its `parked` array, not in
//! `indexes`, so `review_pr` reported "not registered" and told the operator
//! to index the checkout — a reindex, which never clears a park.
//! Test: this module.

use serde_json::{Value, json};
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

use super::{RepoIndexError, resolve_repo_index};
use crate::config::ReviewConfig;
use crate::integrations::search_client::HttpSearchClient;
use crate::integrations::search_transport::TRUSTY_SEARCH_URL_ENV;
use crate::integrations::search_transport::fixture::{EnvGuard, FakeSearchSocket, Reply};

/// A daemon whose only registration for the repo is a parked row.
fn parked_only(method: &str, _: &Value) -> Reply {
    match method {
        "search.indexes.list" => Ok(json!({
            "indexes": [{ "id": "other", "root_path": "/src/other", "repo_identity": "acme/other" }],
            "parked": [{
                "id": "trusty-git-analytics",
                "root_path": "/src/trusty-git-analytics",
                "root_state": "present",
            }],
        })),
        _ => Err((-32601, "method not found".to_string(), None)),
    }
}

/// Why: #11417 criterion 3 — the miss says the id is parked and points at
/// `ensure_project`, never at a reindex.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn no_index_names_a_parked_id_and_points_at_ensure_project() {
    let dir = tempfile::Builder::new()
        .prefix("r11417")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let socket = dir.path().join("search.sock");
    let _fake = FakeSearchSocket::serve(&socket, parked_only);
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket.to_string_lossy());
    let _url = EnvGuard::unset(TRUSTY_SEARCH_URL_ENV);
    let client = HttpSearchClient::from_config(&ReviewConfig::load(None)).expect("client builds");

    let err = resolve_repo_index(&client, "bobmatnyc", "trusty-git-analytics", None)
        .await
        .expect_err("a parked index is not resolved");
    assert!(matches!(err, RepoIndexError::NoIndex { .. }), "{err}");
    let text = err.to_string();
    assert!(text.contains("is parked"), "{text}");
    assert!(text.contains("ensure_project"), "{text}");
    assert!(!text.contains("trusty-search index <path>"), "{text}");
}

/// Why: #11417 — an id that is neither live nor parked keeps the old remedy.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn no_index_for_an_unknown_id_keeps_the_index_remedy() {
    let dir = tempfile::Builder::new()
        .prefix("r11417u")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let socket = dir.path().join("search.sock");
    let _fake = FakeSearchSocket::serve(&socket, parked_only);
    let _pin = EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket.to_string_lossy());
    let _url = EnvGuard::unset(TRUSTY_SEARCH_URL_ENV);
    let client = HttpSearchClient::from_config(&ReviewConfig::load(None)).expect("client builds");

    let err = resolve_repo_index(&client, "acme", "widget", None)
        .await
        .expect_err("no index");
    let text = err.to_string();
    assert!(!text.contains("is parked"), "{text}");
    assert!(text.contains("trusty-search index <path>"), "{text}");
}
