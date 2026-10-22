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

use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;

use super::state::SearchAppState;
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
    let _ = (state, req);
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "error": "not_implemented", "index_id": id }),
    ))
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
        anyhow::bail!("#10110: batch removal is not implemented yet")
    }
}
