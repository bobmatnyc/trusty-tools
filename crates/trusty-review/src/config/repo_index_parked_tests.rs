//! A parked index is present to both index resolvers (#11417, #11545).
//!
//! Why: the daemon lists a parked index in its `parked` array, not in
//! `indexes`, so `review_pr` and the checkout resolver reported "no index"
//! for an index that one search would reload (#11545).
//! Test: this module.

use serde_json::{Value, json};
use trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;

use super::resolve_repo_index;
use crate::config::ReviewConfig;
use crate::integrations::search_client::HttpSearchClient;
use crate::integrations::search_transport::TRUSTY_SEARCH_URL_ENV;
use crate::integrations::search_transport::fixture::{EnvGuard, FakeSearchSocket, Reply};

/// The root the parked row records; the checkout test rewrites it per run.
const PARKED_ROOT_ENV: &str = "R11545_PARKED_ROOT";

/// A daemon whose only registration for the repo is a parked row.
fn parked_only(method: &str, _: &Value) -> Reply {
    let root = std::env::var(PARKED_ROOT_ENV).unwrap_or_else(|_| "/src/tga".to_string());
    match method {
        "search.indexes.list" => Ok(json!({
            "indexes": [{ "id": "other", "root_path": "/src/other", "repo_identity": "acme/other" }],
            "parked": [{
                "id": "trusty-git-analytics",
                "root_path": root,
                "root_state": "present",
            }],
        })),
        _ => Err((-32601, "method not found".to_string(), None)),
    }
}

/// Serve `parked_only` on a fresh socket and build a client over it.
fn parked_client(prefix: &str) -> (tempfile::TempDir, FakeSearchSocket, Vec<EnvGuard>) {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    let socket = dir.path().join("search.sock");
    let fake = FakeSearchSocket::serve(&socket, parked_only);
    let guards = vec![
        EnvGuard::set(TRUSTY_SEARCH_SOCKET_ENV, &socket.to_string_lossy()),
        EnvGuard::unset(TRUSTY_SEARCH_URL_ENV),
    ];
    (dir, fake, guards)
}

/// Why: #11545 error arm — `review_pr` must resolve an index the daemon lists
/// only under `parked`, not fail with "no index".
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn review_pr_resolves_an_index_listed_only_as_parked() {
    let (_dir, _fake, _env) = parked_client("r11545r");
    let client = HttpSearchClient::from_config(&ReviewConfig::load(None)).expect("client builds");

    let got = resolve_repo_index(&client, "bobmatnyc", "trusty-git-analytics", None).await;
    assert_eq!(
        got.expect("a parked index is present"),
        "trusty-git-analytics"
    );
}

/// Why: #11545 error arm — the checkout resolver behind the context gate must
/// pick a parked index covering the checkout, not clear the index.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn checkout_resolver_counts_a_parked_index_as_present() {
    let checkout = tempfile::tempdir().expect("checkout dir");
    std::fs::create_dir(checkout.path().join(".git")).expect(".git");
    let root = checkout.path().canonicalize().expect("canonical root");
    let _root = EnvGuard::set(PARKED_ROOT_ENV, &root.to_string_lossy());
    let (_dir, _fake, _env) = parked_client("r11545c");
    let client = HttpSearchClient::from_config(&ReviewConfig::load(None)).expect("client builds");
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let original = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&root).expect("enter checkout");
    config.resolve_index(&client).await;
    std::env::set_current_dir(original).expect("restore cwd");

    assert_eq!(config.search_index, "trusty-git-analytics");
}

/// Why: #11417 — an id that is neither live nor parked keeps the index remedy.
/// Test: this test.
#[serial_test::serial]
#[tokio::test]
async fn no_index_for_an_unknown_id_keeps_the_index_remedy() {
    let (_dir, _fake, _env) = parked_client("r11417u");
    let client = HttpSearchClient::from_config(&ReviewConfig::load(None)).expect("client builds");

    let err = resolve_repo_index(&client, "acme", "widget", None)
        .await
        .expect_err("no index");
    let text = err.to_string();
    assert!(text.contains("trusty-search index <path>"), "{text}");
}
