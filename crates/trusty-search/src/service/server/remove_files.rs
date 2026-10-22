//! `search.index.files.remove`: remove every file under a path prefix, or in a
//! file list, in one call (#10110).
//!
//! Why: purging an excluded repository through `remove-file` cost one corpus
//! rehydrate, one durable dirty mark and one graph rebuild per file. This is
//! the batch verb; `remove-file` stays as it was.
//! What: [`remove_files_report`] validates the selection, maps absolute paths
//! to stored keys with the `remove-file` rules, then runs the dry run or the
//! batch removal under the teardown guard, stamps a committed delete, and
//! forgets the selected content hashes.
//! Test: `service::server::remove_files_tests`.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;

use super::state::SearchAppState;
use crate::core::index_roots::IndexRoots;
use crate::core::indexer::{BatchRemoveCounts, BatchRemoveError};
use crate::core::path_selection::PathSelection;
use crate::core::registry::IndexId;

/// Most entries one `paths` list may carry (#10110 Q5).
pub(crate) const MAX_REMOVE_PATHS: usize = 10_000;

/// The body of `search.index.files.remove` (#10110).
///
/// What: exactly one of `path_prefix` and `paths`; `dry_run` (default
/// `false`) reports the counts and changes nothing.
#[derive(Debug, Deserialize)]
pub struct RemoveFilesRequest {
    /// Remove every file under this index-relative or absolute in-root prefix.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Remove exactly these index-relative or absolute in-root files.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Count only; change nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// The body `search.index.files.remove` serves (#10110).
///
/// Why: see the module doc. A removal that failed must say what committed,
/// and nothing may answer success for rows redb kept.
/// What: 400 for a selection that is not exactly one of `path_prefix` /
/// `paths`, an empty list, a list over [`MAX_REMOVE_PATHS`], or a path that
/// is empty, `.`, the root, outside the root, or holds a `..` segment. The
/// dry run answers the counts. The real run answers 503 `remove_files_busy`
/// (`retryable: true`, nothing changed) while a corpus rehydrate or another
/// batch holds the index, 500 `remove_files_failed` with `partial` and the
/// committed counts when a step failed, and 500 `hash_forget_failed` when the
/// rows are gone but the selected content hashes are not. Each 500 is safe to
/// retry. A delete that committed rows stamps `reindexed_unix`; a selection
/// matching nothing writes no mark and no stamp.
/// Test: `batch_remove_fails_closed_on_a_refused_slice_and_a_retry_finishes`,
/// `batch_remove_reports_a_failed_hash_forget_and_a_retry_clears_it`,
/// `a_prefix_matching_nothing_answers_zeros_and_writes_no_mark_or_stamp`,
/// `remove_files_refuses_an_unsafe_selection`,
/// `batch_remove_during_a_rehydrate_is_refused_and_leaves_no_orphan_ids`.
pub(crate) async fn remove_files_report(
    state: &Arc<SearchAppState>,
    id: &str,
    req: RemoveFilesRequest,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    check_shape(&index_id.0, &req)?;
    let handle = super::index_resolve::resolve_or_load_index(state, &index_id)
        .await
        .map_err(|(status, body)| (status, body.0))?;
    let selection = selection_for(&index_id.0, &handle.roots(), &req)?;
    // #3049: a concurrent DELETE must not remove_dir_all mid-removal.
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&index_id).await;
    let indexer = handle.indexer.read().await;
    let body = |counts: BatchRemoveCounts| {
        serde_json::json!({
            "index_id": index_id.0,
            "dry_run": req.dry_run,
            "matched_files": counts.matched_files,
            "matched_chunks": counts.matched_chunks,
            "removed_files": counts.removed_files,
            "removed_chunks": counts.removed_chunks,
        })
    };
    if req.dry_run {
        let counts = indexer
            .count_files_matching(&selection)
            .await
            .map_err(|e| failed(&index_id.0, "remove_files_failed", None, &e))?;
        return Ok(body(counts));
    }
    let counts = match indexer.remove_files_matching(&selection).await {
        Ok(counts) => counts,
        Err(BatchRemoveError::Busy) => return Err(busy(&index_id.0)),
        Err(BatchRemoveError::Failed { counts, source }) => {
            return Err(failed(
                &index_id.0,
                "remove_files_failed",
                Some(counts),
                &source,
            ));
        }
    };
    // #9230: only a delete whose rows left redb moves `reindexed_unix`.
    if counts.removed_chunks > 0 {
        indexer.record_incremental_commit("remove_files").await;
    }
    // #10110 fix-bar 2: forget by selection, so a retry that matches no chunk
    // still clears the hashes a failed forget left behind.
    indexer
        .forget_file_hashes_matching(&index_id, &selection)
        .await
        .map_err(|e| failed(&index_id.0, "hash_forget_failed", Some(counts), &e))?;
    Ok(body(counts))
}

/// 400 unless exactly one non-empty selector within the cap was sent.
fn check_shape(
    index_id: &str,
    req: &RemoveFilesRequest,
) -> Result<(), (StatusCode, serde_json::Value)> {
    let problem = match (&req.path_prefix, &req.paths) {
        (Some(_), Some(_)) => Some("send `path_prefix` or `paths`, not both".to_string()),
        (None, None) => Some("send one of `path_prefix` or `paths`".to_string()),
        (None, Some(paths)) if paths.is_empty() => Some("`paths` is empty".to_string()),
        (None, Some(paths)) if paths.len() > MAX_REMOVE_PATHS => Some(format!(
            "`paths` holds {} entries; the limit is {MAX_REMOVE_PATHS}",
            paths.len()
        )),
        _ => None,
    };
    match problem {
        None => Ok(()),
        Some(message) => Err(bad_request(
            index_id,
            "remove_files_invalid_request",
            message,
        )),
    }
}

/// Map the request's paths to stored keys with the `remove-file` rules.
///
/// What: [`super::remove_path::remove_keys`] refuses an empty path, `.`, a
/// `..` segment, the root, and a path outside every root; a prefix naming a
/// whole additional root (`@root<n>`) is refused here too. An absolute path
/// selects its stored key and its verbatim pre-#9510 key.
fn selection_for(
    index_id: &str,
    roots: &IndexRoots,
    req: &RemoveFilesRequest,
) -> Result<PathSelection, (StatusCode, serde_json::Value)> {
    let keys_of = |path: &str| {
        super::remove_path::remove_keys(index_id, roots, path).map_err(|(_, refused)| {
            let message = refused["message"]
                .as_str()
                .unwrap_or("the path names no file under the index root")
                .replace("remove-file", "remove_files");
            bad_request(index_id, "remove_files_path_outside_root", message)
        })
    };
    if let Some(prefix) = &req.path_prefix {
        let keys = keys_of(prefix)?;
        if keys.iter().any(|k| names_a_whole_root(k)) {
            return Err(bad_request(
                index_id,
                "remove_files_path_outside_root",
                format!("`{prefix}` names a whole index root; remove_files never removes a root"),
            ));
        }
        return Ok(PathSelection::Prefix(keys.into_iter().collect()));
    }
    let mut files = BTreeSet::new();
    for path in req.paths.iter().flatten() {
        files.extend(keys_of(path)?);
    }
    Ok(PathSelection::Files(files))
}

/// True for `@root<n>` or `@root<n>/`, the stored key of an additional root.
fn names_a_whole_root(key: &str) -> bool {
    key.trim_end_matches('/')
        .strip_prefix("@root")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn bad_request(index_id: &str, error: &str, message: String) -> (StatusCode, serde_json::Value) {
    (
        StatusCode::BAD_REQUEST,
        serde_json::json!({
            "error": error,
            "index_id": index_id,
            "removed_chunks": 0,
            "retryable": false,
            "message": message,
        }),
    )
}

fn busy(index_id: &str) -> (StatusCode, serde_json::Value) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({
            "error": "remove_files_busy",
            "index_id": index_id,
            "retryable": true,
            "retry_after_secs": 5,
            "removed_chunks": 0,
            "message": BatchRemoveError::Busy.to_string(),
        }),
    )
}

fn failed(
    index_id: &str,
    error: &str,
    counts: Option<BatchRemoveCounts>,
    e: &dyn std::fmt::Display,
) -> (StatusCode, serde_json::Value) {
    let counts = counts.unwrap_or_default();
    tracing::warn!(index_id, error, "remove_files failed: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({
            "error": error,
            "index_id": index_id,
            "retryable": true,
            "partial": counts.removed_chunks > 0,
            "matched_files": counts.matched_files,
            "matched_chunks": counts.matched_chunks,
            "removed_files": counts.removed_files,
            "removed_chunks": counts.removed_chunks,
            "message": format!("{e:#}"),
        }),
    )
}

impl crate::core::CodeIndexer {
    /// Forget every content hash `sel` names, in process and in redb (#10110).
    ///
    /// Why: a hash left behind makes the next reindex skip a re-admitted
    /// file. Keyed by the selection, not by removed chunks, so a retry after
    /// a failed forget clears what the first call left (fix-bar 2).
    /// What: drops the matching keys from `hashes_for`, then deletes the
    /// matching rows in one transaction. A method so
    /// `scripts/check_teardown_guard.sh` sees its call sites.
    /// Test: `batch_remove_reports_a_failed_hash_forget_and_a_retry_clears_it`.
    pub(crate) async fn forget_file_hashes_matching(
        &self,
        index_id: &IndexId,
        sel: &PathSelection,
    ) -> anyhow::Result<usize> {
        crate::service::reindex::hash::hashes_for(index_id)
            .retain(|key, _| !sel.selects_file(&key.to_string_lossy()));
        let Some(corpus) = self.corpus_store() else {
            return Ok(0);
        };
        #[cfg(test)]
        if crate::service::reindex::hash::TEST_FAIL_FORGET_HASH
            .lock()
            .is_ok_and(|f| f.contains(&index_id.0))
        {
            anyhow::bail!("injected file-hash delete failure (#10110 test seam)");
        }
        let owned = sel.clone();
        tokio::task::spawn_blocking(move || corpus.delete_file_hashes_under(&owned))
            .await
            .map_err(|e| anyhow::anyhow!("file-hash delete task did not complete: {e}"))?
    }
}
