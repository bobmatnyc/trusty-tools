//! Issue #8105: the MCP `reindex` tool reports a write-quarantined index as an
//! error naming the quarantine, not as a queued run.
//!
//! Why: the owner ruling on #8105 names both surfaces — HTTP 409, and an MCP
//! error result naming the quarantine reason. The daemon's real socket router
//! renders the refusal (409's twin, `conflict`) and a real `McpServer` relays
//! it over a scratch socket (#9168), so the assertion is what an MCP client
//! receives. Against pre-fix code the tool call succeeds with `queued: true`.
//! What: one quarantined index, raised by a DIRECTORY at the colocated redb
//! path, and one `tools/call reindex` against it.
//! Test: `cargo test -p trusty-search --test mcp_reindex_quarantine_8105`

#[path = "support/socket_daemon.rs"]
mod socket_daemon;

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::RwLock;
use trusty_common::embedder::MockEmbedder;

use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::core::Embedder;
use trusty_search::mcp::{McpServer, Request};
use trusty_search::service::persistence::PersistedIndex;
use trusty_search::service::persistence_loader::build_indexer_from_entry;
use trusty_search::service::server::SearchAppState;

/// The MCP error result for a reindex of a write-quarantined index names the
/// conflict refusal (409's twin), the `index_write_quarantined` code, and the
/// quarantine itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_reindex_of_a_write_quarantined_index_is_an_error_naming_the_quarantine() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".trusty-search").join("index.redb"))
        .expect("create a directory at the redb path");
    let mut entry =
        PersistedIndex::new("quarantined-mcp-8105".to_string(), dir.path().to_path_buf());
    entry.colocated = true;
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let indexer = build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("build indexer");
    assert!(
        indexer.is_write_quarantined(),
        "precondition: a directory at the redb path must quarantine the index"
    );
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new("quarantined-mcp-8105"),
        Arc::new(RwLock::new(indexer)),
        dir.path().to_path_buf(),
    ));

    let daemon = socket_daemon::serve_state(SearchAppState::new(registry)).await;
    let server = McpServer::new(daemon.client.clone());

    let resp = server
        .dispatch(Request {
            jsonrpc: Some("2.0".into()),
            id: Some(Value::from(1u64)),
            method: "tools/call".into(),
            params: Some(json!({
                "name": "reindex",
                "arguments": { "index_id": "quarantined-mcp-8105" },
            })),
        })
        .await;

    let result = resp.result.expect("tools/call always returns a result");
    assert_eq!(
        result["isError"],
        Value::Bool(true),
        "#8105: the reindex must be reported as refused: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("a prose content node");
    assert!(
        text.contains("conflict") && text.contains("index_write_quarantined"),
        "the error must carry the conflict refusal and its code: {text}"
    );
    assert!(
        text.contains("write-quarantined") && text.contains("restart the daemon"),
        "the error must name the quarantine and how to clear it: {text}"
    );
}
