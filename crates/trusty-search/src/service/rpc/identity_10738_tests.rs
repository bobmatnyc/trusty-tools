//! #10738 (SPEC-OKF-01 §5.6 D1): index identity on `search.index.status`, the
//! put reply's `content_sha256` and `identity`, and `search.index.file.status`.
//!
//! Every case drives the socket router (reads + writes on ONE state), so the
//! wire shape is what is asserted. Ids are unique per case: the identity file
//! lives in the per-index data dir, which parallel cases share.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use trusty_common::uds::server::{RpcRouter, CODE_INVALID_PARAMS};

use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::service::rpc::error::CODE_NOT_FOUND;
use crate::service::rpc::test_support::{rpc_err, rpc_ok};
use crate::service::rpc::{reads, writes};
use crate::service::server::SearchAppState;

const DIM: usize = 8;
const FILE: &str = "src/auth.rs";
const CONTENT: &str = "fn authenticate(token: &str) -> bool { verify(token) }\n";
const EDITED: &str = "fn authenticate(token: &str) -> bool { verify_v2(token) }\n";

/// Lowercase hex SHA-256, computed independently of the daemon's helper.
fn sha256_hex(content: &str) -> String {
    Sha256::digest(content.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One resident index with an embedder, a vector store and a durable corpus.
fn registry_with(id: &str, root: &Path, corpus_dir: Option<&Path>) -> IndexRegistry {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(DIM).expect("usearch"));
    let mut indexer =
        CodeIndexer::new(id, root.to_str().expect("utf8 root")).with_components(embedder, store);
    if let Some(dir) = corpus_dir {
        let corpus = CorpusStore::open(&dir.join("index.redb")).expect("open corpus");
        indexer.set_corpus_store(Arc::new(corpus));
    }
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    ));
    registry
}

/// The read and write surfaces mounted on one state.
async fn served(registry: IndexRegistry) -> (Arc<SearchAppState>, RpcRouter) {
    let state = Arc::new(SearchAppState::new(registry));
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIM));
    state.install_embedder(embedder).await;
    let rpc = writes::register(reads::register(RpcRouter::new(), &state), &state);
    (state, rpc)
}

async fn status_identity(rpc: &RpcRouter, id: &str) -> serde_json::Value {
    let status = rpc_ok(
        rpc,
        reads::METHOD_INDEX_STATUS,
        serde_json::json!({ "index_id": id }),
    )
    .await;
    status["identity"].clone()
}

async fn put(rpc: &RpcRouter, id: &str, path: &str, content: &str) -> serde_json::Value {
    rpc_ok(
        rpc,
        writes::METHOD_INDEX_FILE_PUT,
        serde_json::json!({ "index_id": id, "body": { "path": path, "content": content } }),
    )
    .await
}

async fn file_status(rpc: &RpcRouter, id: &str, path: &str) -> serde_json::Value {
    rpc_ok(
        rpc,
        reads::METHOD_INDEX_FILE_STATUS,
        serde_json::json!({ "index_id": id, "body": { "path": path } }),
    )
    .await
}

/// Why: a journal compares the identity it stored with the live one; a value
/// that changed on restart would re-push every file after every boot.
/// What: status reports a 128-bit hex `instance_id` at generation 1; a put
/// does not change it; a second daemon state (new registry, new indexer, same
/// data dir) reports the same value.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn identity_is_stable_across_a_daemon_restart() {
    let id = "identity-restart-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let _ = crate::service::index_identity::retire(id);
    let (state, rpc) = served(registry_with(id, tmp.path(), None)).await;

    let first = status_identity(&rpc, id).await;
    let instance = first["instance_id"].as_str().expect("instance_id string");
    assert_eq!(instance.len(), 32, "{first}");
    assert!(instance.bytes().all(|b| b.is_ascii_hexdigit()), "{first}");
    assert_eq!(first["generation"], 1, "{first}");
    let reply = put(&rpc, id, FILE, CONTENT).await;
    assert_eq!(
        reply["identity"], first,
        "a put must not change the identity"
    );
    assert_eq!(status_identity(&rpc, id).await, first);
    drop(rpc);
    drop(state);

    let (_state, rpc) = served(registry_with(id, tmp.path(), None)).await;
    assert_eq!(
        status_identity(&rpc, id).await,
        first,
        "a restart must not change the identity"
    );
}

/// Why: a delete and re-create of the same index id leaves an empty index, so
/// a journal that matched the old identity would report files Searchable that
/// the index no longer holds (SPEC-OKF-01 criterion 134).
/// What: delete with `delete_data: false` (the data dir survives), register the
/// id again, and the new identity has a different `instance_id` at generation 1.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn identity_changes_when_the_index_is_deleted_and_recreated() {
    let _isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let id = "identity-recreate-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let (state, rpc) = served(registry_with(id, tmp.path(), None)).await;
    let before = status_identity(&rpc, id).await;

    let deleted = rpc_ok(
        &rpc,
        writes::METHOD_INDEX_DELETE,
        serde_json::json!({ "index_id": id }),
    )
    .await;
    assert_eq!(deleted["removed"], true, "{deleted}");
    assert_eq!(deleted["data_deleted"], false, "{deleted}");

    let fresh = CodeIndexer::new(id, tmp.path().to_str().expect("utf8 root"));
    state.registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(fresh)),
        tmp.path().to_path_buf(),
    ));
    let after = status_identity(&rpc, id).await;
    assert_ne!(
        after["instance_id"], before["instance_id"],
        "{before} vs {after}"
    );
    assert_eq!(after["generation"], 1, "{after}");
}

/// Why: the caller counts a put as acknowledged only when the daemon names
/// the hash of the bytes it sent (SPEC-OKF-01 F4).
/// What: the put reply's `content_sha256` equals an independent SHA-256 of
/// the content, and `identity` equals the status identity. A tombstone reply
/// carries both too.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn put_reply_carries_the_sha256_of_the_content_and_the_identity() {
    let id = "identity-put-hash-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("store");
    let (_state, rpc) = served(registry_with(id, tmp.path(), Some(store.path()))).await;

    let reply = put(&rpc, id, FILE, CONTENT).await;
    assert_eq!(reply["indexed"], true, "{reply}");
    assert_eq!(reply["content_sha256"], sha256_hex(CONTENT), "{reply}");
    assert_eq!(
        reply["identity"],
        status_identity(&rpc, id).await,
        "{reply}"
    );

    let tombstone = "---\nsource_status: deleted\nsource_id: auth-10738\n---\n";
    let gone = put(&rpc, id, FILE, tombstone).await;
    assert_eq!(gone["removed"], true, "{gone}");
    assert_eq!(gone["content_sha256"], sha256_hex(tombstone), "{gone}");
    assert_eq!(gone["identity"], reply["identity"], "{gone}");
}

/// Why: the probe needs revision evidence for one path; chunk coverage cannot
/// tell an old revision from the current one (SPEC-OKF-01 criterion 133).
/// What: after a put, `file.status` reports the put's hash and chunk count;
/// after an edit, the new hash; after a remove, `present: false`, a null hash
/// and zero chunks.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn file_status_reports_the_put_hash_then_absent_after_remove() {
    let id = "identity-file-status-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("store");
    let (_state, rpc) = served(registry_with(id, tmp.path(), Some(store.path()))).await;
    let identity = status_identity(&rpc, id).await;

    let reply = put(&rpc, id, FILE, CONTENT).await;
    let after_put = file_status(&rpc, id, FILE).await;
    assert_eq!(after_put["path"], FILE, "{after_put}");
    assert_eq!(after_put["present"], true, "{after_put}");
    assert_eq!(
        after_put["content_sha256"],
        sha256_hex(CONTENT),
        "{after_put}"
    );
    assert_eq!(after_put["chunk_count"], reply["chunks"], "{after_put}");
    assert_eq!(after_put["identity"], identity, "{after_put}");

    put(&rpc, id, FILE, EDITED).await;
    let after_edit = file_status(&rpc, id, FILE).await;
    assert_eq!(
        after_edit["content_sha256"],
        sha256_hex(EDITED),
        "{after_edit}"
    );

    rpc_ok(
        &rpc,
        writes::METHOD_INDEX_FILE_REMOVE,
        serde_json::json!({ "index_id": id, "body": { "path": FILE } }),
    )
    .await;
    let after_remove = file_status(&rpc, id, FILE).await;
    assert_eq!(after_remove["present"], false, "{after_remove}");
    assert!(after_remove["content_sha256"].is_null(), "{after_remove}");
    assert_eq!(after_remove["chunk_count"], 0, "{after_remove}");
    assert_eq!(after_remove["identity"], identity, "{after_remove}");
}

/// Why: a fabricated hash for a path the index never held would read as a
/// confirmed revision.
/// What: an unknown path answers `present: false` with a null hash; an unknown
/// index is a NOT_FOUND error; a path outside every root is refused.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn file_status_for_an_unknown_path_or_index_never_fabricates_a_hash() {
    let id = "identity-unknown-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_state, rpc) = served(registry_with(id, tmp.path(), None)).await;

    let unknown = file_status(&rpc, id, "src/never.rs").await;
    assert_eq!(unknown["present"], false, "{unknown}");
    assert!(unknown["content_sha256"].is_null(), "{unknown}");
    assert_eq!(unknown["chunk_count"], 0, "{unknown}");

    let no_index = rpc_err(
        &rpc,
        reads::METHOD_INDEX_FILE_STATUS,
        serde_json::json!({ "index_id": "no-such-index-10738", "body": { "path": FILE } }),
    )
    .await;
    assert_eq!(no_index.code, CODE_NOT_FOUND, "{no_index:?}");

    let outside = rpc_err(
        &rpc,
        reads::METHOD_INDEX_FILE_STATUS,
        serde_json::json!({ "index_id": id, "body": { "path": "/definitely/not/in/root.rs" } }),
    )
    .await;
    assert_eq!(outside.code, CODE_INVALID_PARAMS, "{outside:?}");
}

/// The variant number whose chunks the index holds for `path`; chunk ids carry
/// the symbol name, and each variant defines `variant_<n>_10738`.
async fn committed_variant(state: &SearchAppState, id: &str, path: &str) -> Vec<usize> {
    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    let indexer = handle.indexer.read().await;
    let ids = indexer.file_chunk_ids(path).await.expect("chunk ids");
    (0..VARIANTS)
        .filter(|n| {
            let name = format!("variant_{n}_10738");
            ids.iter().any(|c| c.contains(&name))
        })
        .collect()
}

const VARIANTS: usize = 8;

fn variant(n: usize) -> String {
    format!("fn variant_{n}_10738(x: u32) -> u32 {{ x + {n} }}\n")
}

/// Why: H3 — the put core forgot and recorded the path's hash outside the
/// per-path write lock, so two puts to one path could leave one's hash beside
/// the other's chunks (SPEC-OKF-01 criterion 133).
/// What: with the path's write lock held, a queued put must not touch the
/// stored hash, so `file.status` still names the committed content's hash;
/// then eight concurrent puts finish, and the stored hash equals the hash of
/// the one variant whose chunks the index holds.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn concurrent_puts_to_one_path_store_the_hash_of_the_committed_chunks() {
    let id = "identity-put-race-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("store");
    let (state, rpc) = served(registry_with(id, tmp.path(), Some(store.path()))).await;
    let rpc = Arc::new(rpc);
    put(&rpc, id, FILE, &variant(0)).await;

    let handle = state.registry.get(&IndexId::new(id)).expect("registered");
    let indexer = handle.indexer.read().await;
    let held = indexer.lock_path_for_write(FILE).await;
    let queued = {
        let rpc = Arc::clone(&rpc);
        tokio::spawn(async move { put(&rpc, id, FILE, &variant(1)).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let during = file_status(&rpc, id, FILE).await;
    assert_eq!(during["present"], true, "{during}");
    assert_eq!(
        during["content_sha256"],
        sha256_hex(&variant(0)),
        "a put waiting on the path lock must not change the stored hash: {during}"
    );
    drop(held);
    drop(indexer);
    queued.await.expect("queued put");
    assert_eq!(
        file_status(&rpc, id, FILE).await["content_sha256"],
        sha256_hex(&variant(1))
    );

    let puts: Vec<_> = (0..VARIANTS)
        .map(|n| {
            let rpc = Arc::clone(&rpc);
            tokio::spawn(async move { put(&rpc, id, FILE, &variant(n)).await })
        })
        .collect();
    for p in puts {
        p.await.expect("concurrent put");
    }
    let committed = committed_variant(&state, id, FILE).await;
    assert_eq!(committed.len(), 1, "one variant's chunks: {committed:?}");
    let status = file_status(&rpc, id, FILE).await;
    assert_eq!(
        status["content_sha256"],
        sha256_hex(&variant(committed[0])),
        "the stored hash must name the committed chunks' content: {status}"
    );
}
