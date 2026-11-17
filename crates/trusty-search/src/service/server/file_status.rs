//! Per-file revision evidence: the put reply's hash and `search.index.file.status`
//! (#10738, SPEC-OKF-01 §5.6 D1).
//!
//! Why: a caller that pushes a file needs to know which bytes the index holds
//! for it, under which index identity. Chunk coverage alone cannot tell an old
//! revision from the current one.
//! What: [`file_status_report`], the core behind `search.index.file.status`.
//! The shared write core keeps the hash it reads (#10738).
//! Test: `service::rpc::identity_10738_tests`.

use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;

use crate::core::registry::IndexId;

use super::state::SearchAppState;

/// The body of `search.index.file.status`.
#[derive(Debug, Deserialize)]
pub struct FileStatusRequest {
    /// Index-relative path, or an absolute path inside one of the index's roots.
    pub path: String,
}

/// The body `search.index.file.status` serves.
///
/// Why: SPEC-OKF-01 §5.6 D1 item 3 — revision evidence for one path, under the
/// live index identity, without a search.
/// What: resolves the index (404 for an unknown id), maps the path to its
/// stored key (400 outside every root), counts the path's chunks and reads its
/// persisted content hash. `present` is true when chunks exist.
/// `content_sha256` is the stored hash when present and known, else `null`; a
/// hash with no chunks behind it is never reported. A failed corpus read is a
/// 500, never `present: false`.
/// Test: `file_status_reports_the_put_hash_then_absent_after_remove`,
/// `file_status_for_an_unknown_path_or_index_never_fabricates_a_hash`.
pub(crate) async fn file_status_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: FileStatusRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = super::index_resolve::resolve_or_load_index(state, &index_id)
        .await
        .map_err(|(status, body)| (status, body.0))?;
    let key = super::remove_path::index_key(&index_id.0, &handle.roots(), &req.path)?;
    let failed = |e: anyhow::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({
                "error": "file_status_failed",
                "index_id": index_id.0,
                "path": req.path,
                "message": format!("{e:#}"),
            }),
        )
    };
    let indexer = handle.indexer.read().await;
    let chunk_count = indexer.file_chunk_ids(&key).await.map_err(failed)?.len();
    let present = chunk_count > 0;
    let content_sha256 = if !present {
        None
    } else if let Some(corpus) = indexer.corpus_store() {
        let k = key.clone();
        tokio::task::spawn_blocking(move || corpus.file_hash(&k))
            .await
            .map_err(|e| failed(anyhow::anyhow!("file-hash read task did not complete: {e}")))?
            .map_err(failed)?
    } else {
        crate::service::reindex::hash::hashes_for(&index_id)
            .get(std::path::Path::new(&key))
            .map(|h| h.value().clone())
    };
    drop(indexer);
    let mut body = serde_json::Map::new();
    body.insert("index_id".into(), index_id.0.clone().into());
    body.insert("path".into(), req.path.into());
    body.insert("present".into(), present.into());
    body.insert("content_sha256".into(), content_sha256.into());
    body.insert("chunk_count".into(), chunk_count.into());
    crate::service::index_identity::insert_into(&index_id.0, &mut body);
    Ok(serde_json::Value::Object(body))
}
