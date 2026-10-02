//! The walker's admission decision for a pushed `index_file` write (#8922).
//!
//! Why: `index_file` (HTTP, socket, MCP) indexed any path it was given, so a
//! caller could put a file the reindex walk excludes into the index, and the
//! reply said `indexed: true`.
//! What: [`gate`] runs [`admits_pushed`] before the write and turns a refusal
//! into the response both transports return; [`refusal`] builds that response
//! for the sops refusal the indexer reports after the write.
//! Test: `crate::service::write_admission_tests`.

use std::path::{Component, Path, PathBuf};

use axum::http::StatusCode;

use crate::core::registry::IndexHandle;
use crate::core::CodeIndexer;
use crate::service::index_admission::{self, Admission};
use crate::service::walker;

/// Why a pushed write was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The walker would skip this path.
    Excluded,
    /// The content is a sops-encrypted file.
    SopsEncrypted,
    /// The filesystem could not say whether the walker would admit the path.
    Undetermined,
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the walker would admit the file a pushed write names.
///
/// Why: a pushed write must not reach a file the reindex walk skips (#8922).
/// What: `path` resolves against the index root. A `..` segment, or content
/// over the walker's size caps, is excluded. A path on disk gets
/// [`index_admission::admits`], the walker's own answer with ignore files. A
/// path that is not on disk — a network-mounted root the caller reads for the
/// daemon — gets every rule that needs no directory listing: configured
/// subtrees, `exclude_globs`, `extensions`, `path_filter`, skip dirs, source
/// extensions, name skips and doc exclusion. A path whose presence on disk
/// cannot be read is undetermined, never admitted.
/// Test: `pushed_write_to_an_excluded_path_is_refused_and_purged`,
/// `pushed_write_over_a_size_cap_is_refused`,
/// `pushed_write_the_filesystem_cannot_resolve_is_refused`.
pub(crate) fn admits_pushed(handle: &IndexHandle, path: &str, content_len: usize) -> Admission {
    let given = Path::new(path);
    if given
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Admission::Excluded;
    }
    let opts = index_admission::walk_options(handle);
    if walker::len_exceeds_caps(given, content_len as u64, opts.data_file_max_bytes) {
        return Admission::Excluded;
    }
    let abs = if given.is_absolute() {
        given.to_path_buf()
    } else {
        canonical_or_raw(&handle.root_path).join(given)
    };
    match abs.symlink_metadata() {
        Ok(_) => return index_admission::admits(handle, &abs),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Admission::Undetermined,
    }
    if !index_admission::configured_file(handle, &abs) {
        return Admission::Excluded;
    }
    let admitted = index_admission::configured_roots(handle)
        .iter()
        .flat_map(|root| [root.clone(), canonical_or_raw(root)])
        .any(|root| abs.starts_with(&root) && walker::path_admitted(&root, &abs, &opts));
    if admitted {
        Admission::Included
    } else {
        Admission::Excluded
    }
}

/// Refuse a pushed write the walker would not admit, before it is indexed.
///
/// Why: see [`admits_pushed`]. An excluded file may also hold chunks from an
/// earlier write, which must not keep answering searches.
/// What: a tombstone only removes, so it passes. An excluded path has its
/// chunks removed and answers 403; an undetermined one answers 503 and removes
/// nothing, since the file may still be admitted (#7396).
/// Test: `pushed_write_to_an_excluded_path_is_refused_and_purged`,
/// `pushed_write_the_filesystem_cannot_resolve_is_refused`.
pub(crate) async fn gate(
    handle: &IndexHandle,
    indexer: &CodeIndexer,
    path: &str,
    content: &str,
) -> Result<(), (StatusCode, serde_json::Value)> {
    if trusty_common::knowledge_document::is_tombstone(content) {
        return Ok(());
    }
    let index_id = handle.id.0.as_str();
    match admits_pushed(handle, path, content.len()) {
        Admission::Included => Ok(()),
        Admission::Undetermined => Err(refusal(index_id, path, Refusal::Undetermined, 0)),
        Admission::Excluded => {
            let removed = purge_pushed(handle, indexer, path).await.map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({
                        "error": "index_file_failed",
                        "index_id": index_id,
                        "path": path,
                        "indexed": false,
                        "message": format!(
                            "the path is excluded, and its old chunks could not be removed: {e}"
                        ),
                    }),
                )
            })?;
            Err(refusal(index_id, path, Refusal::Excluded, removed))
        }
    }
}

/// Purge an excluded pushed path: chunks, content hash, then one graph rebuild.
///
/// Why (#8922): a hash left behind makes a reindex hash-skip the file once the
/// policy admits it again. The reindex keys hashes by root-relative path, so an
/// absolute pushed path also forgets its root-relative form.
async fn purge_pushed(
    handle: &IndexHandle,
    indexer: &CodeIndexer,
    path: &str,
) -> anyhow::Result<usize> {
    use crate::service::reindex::hash::{forget_file_hash, purge_file};
    let removed = purge_file(&handle.id, indexer, path).await?;
    if let Ok(rel) = Path::new(path).strip_prefix(canonical_or_raw(&handle.root_path)) {
        forget_file_hash(&handle.id, indexer, &rel.to_string_lossy()).await?;
    }
    if removed > 0 {
        indexer.rebuild_symbol_graph_now().await;
    }
    Ok(removed)
}

/// The response a refused pushed write returns on every transport.
///
/// What: 403 `index_file_excluded` for an excluded path or sops content, 503
/// `index_file_admission_undetermined` (retryable) when the filesystem could
/// not answer. Every body says `indexed: false`.
pub(crate) fn refusal(
    index_id: &str,
    path: &str,
    refusal: Refusal,
    removed_chunks: usize,
) -> (StatusCode, serde_json::Value) {
    let (status, error, reason, message) = match refusal {
        Refusal::Excluded => (
            StatusCode::FORBIDDEN,
            "index_file_excluded",
            "excluded_path",
            "the index's walker excludes this path (exclude_globs, extensions, include_paths, \
             ignore files, skip dirs or size caps), so it is not indexed",
        ),
        Refusal::SopsEncrypted => (
            StatusCode::FORBIDDEN,
            "index_file_excluded",
            "sops_encrypted",
            "the content is a sops-encrypted file, which is never indexed",
        ),
        Refusal::Undetermined => (
            StatusCode::SERVICE_UNAVAILABLE,
            "index_file_admission_undetermined",
            "admission_undetermined",
            "the filesystem could not say whether this path is admitted; nothing was indexed",
        ),
    };
    tracing::warn!(index_id, path, reason, "index-file refused (#8922)");
    let mut body = serde_json::json!({
        "error": error,
        "index_id": index_id,
        "path": path,
        "indexed": false,
        "chunks": 0,
        "reason": reason,
        "message": message,
    });
    if refusal == Refusal::Undetermined {
        body["retryable"] = true.into();
    } else {
        body["removed_chunks"] = removed_chunks.into();
    }
    (status, body)
}

#[cfg(test)]
#[path = "write_admission_tests.rs"]
mod write_admission_tests;
