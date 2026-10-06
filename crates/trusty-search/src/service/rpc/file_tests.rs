//! `search.file.get` over the daemon's real socket (#9029).
//!
//! Why: the core is tested in `service/file_view_tests.rs`; these cases prove
//! the wire contract a client reads through `DaemonClient` — the full router
//! `service::socket` serves, a real Unix socket, and each refusal's code.
//! Test: this module.

use std::sync::Arc;

use serde_json::json;
use trusty_common::uds::server::CODE_INVALID_PARAMS;

use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::daemon_client::DaemonClient;
use crate::service::file_view::tests::repo;
use crate::service::rpc::error::{CODE_FORBIDDEN, CODE_NOT_FOUND};
use crate::service::rpc::file::METHOD_FILE_GET;
use crate::service::server::SearchAppState;

const INDEX: &str = "file-get-9029";

/// A served socket over one registered repo index.
struct Served {
    tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    client: DaemonClient,
    _stop: tokio::sync::oneshot::Sender<()>,
}

async fn serve() -> Served {
    let (tmp, root) = repo();
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(INDEX),
        Arc::new(tokio::sync::RwLock::new(CodeIndexer::new(INDEX, &root))),
        root.clone(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    let socket = tmp.path().join("ts.sock");
    let bound = crate::service::socket::bind(&socket)
        .await
        .expect("bind the scratch socket");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(crate::service::socket::serve_until_shutdown(
        bound,
        state,
        async {
            let _ = stopped.await;
        },
    ));
    Served {
        tmp,
        root,
        client: DaemonClient::at(socket),
        _stop: stop,
    }
}

/// #9029: the method is reachable through `DaemonClient`; content alone by
/// default, content plus a `HEAD` diff on `diff: "head"`.
#[tokio::test]
async fn file_get_over_the_daemon_socket_returns_content_and_head_diff() {
    let s = serve().await;
    let plain = s
        .client
        .call(
            METHOD_FILE_GET,
            json!({ "index_id": INDEX, "path": "src/lib.rs" }),
        )
        .await
        .expect("an indexed file is served");
    assert_eq!(plain["content"], "pub fn one() {}\n", "{plain}");
    assert!(plain["diff"].is_null(), "diff defaults to none: {plain}");

    std::fs::write(s.root.join("src/lib.rs"), "pub fn changed() {}\n").expect("edit");
    let with_diff = s
        .client
        .call(
            METHOD_FILE_GET,
            json!({ "index_id": INDEX, "path": "src/lib.rs", "diff": "head" }),
        )
        .await
        .expect("served with a diff");
    assert_eq!(with_diff["content"], "pub fn changed() {}\n");
    assert_eq!(with_diff["diff"]["status"], "changed", "{with_diff}");
    assert!(
        with_diff["diff"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("-pub fn one() {}") && t.contains("+pub fn changed() {}")),
        "{with_diff}"
    );
}

/// #9029: each refusal keeps its code over the socket and carries its body as
/// `data`; a missing file and an outside file are indistinguishable.
#[tokio::test]
async fn file_get_refusals_keep_their_codes_over_the_socket() {
    let s = serve().await;
    let outside = s.tmp.path().join("outside.rs");
    std::fs::write(&outside, "secret\n").expect("outside file");
    let sops = format!(
        "token: {}\nsops:\n    version: 3.8.1\n",
        crate::core::sops::enc("aGk=")
    );
    std::fs::write(s.root.join("secrets.yaml"), sops).expect("sops file");

    let call = |params: serde_json::Value| s.client.call(METHOD_FILE_GET, params);

    let missing = call(json!({ "index_id": INDEX, "path": "src/nope.rs" }))
        .await
        .expect_err("missing");
    let beyond = call(json!({ "index_id": INDEX, "path": outside }))
        .await
        .expect_err("outside the root");
    assert_eq!(missing.code(), Some(CODE_NOT_FOUND), "{missing}");
    assert_eq!(beyond.code(), missing.code());
    assert_eq!(beyond.message(), missing.message());
    assert_eq!(beyond.data(), missing.data());
    assert_eq!(
        missing.data().map(|d| &d["error"]),
        Some(&json!("file_not_found"))
    );

    let traversal = call(json!({ "index_id": INDEX, "path": "../outside.rs" }))
        .await
        .expect_err("traversal");
    assert_eq!(traversal.code(), Some(CODE_INVALID_PARAMS), "{traversal}");
    assert_eq!(
        traversal.data().map(|d| &d["reason"]),
        Some(&json!("path_traversal"))
    );

    let refused = call(json!({ "index_id": INDEX, "path": "secrets.yaml" }))
        .await
        .expect_err("sops");
    assert_eq!(refused.code(), Some(CODE_FORBIDDEN), "{refused}");
    assert_eq!(
        refused.data().map(|d| &d["reason"]),
        Some(&json!("sops_encrypted"))
    );

    let unknown = call(json!({ "index_id": "no-such-index", "path": "src/lib.rs" }))
        .await
        .expect_err("unknown index");
    assert_eq!(unknown.code(), Some(CODE_NOT_FOUND), "{unknown}");

    for bad in [
        json!({ "index_id": INDEX, "path": "src/lib.rs", "diff": "staged" }),
        json!({ "index_id": INDEX, "path": "src/lib.rs", "extra": true }),
        json!({ "index_id": INDEX }),
    ] {
        let err = call(bad.clone()).await.expect_err("bad params");
        assert_eq!(err.code(), Some(CODE_INVALID_PARAMS), "{bad}: {err}");
    }
}
