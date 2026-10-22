//! Remove every file under a path prefix, or in a file list, in one call (#10110).
//!
//! Why: removing an excluded repository went one file at a time. Each call
//! rehydrated the whole evicted corpus to find one file's chunks, opened its
//! own redb transactions, and stamped its own graph-dirty mark; one 24-chunk
//! removal raised a production daemon by ~1.7 GiB that never came back.
//! What: [`CodeIndexer::remove_files_matching`] reads the selected rows by
//! redb key range (never `ensure_chunks_loaded`), stamps ONE durable dirty
//! mark, then per slice of at most [`REMOVE_SLICE_IDS`] ids deletes the chunk
//! and entity rows in ONE write transaction, drops them from memory, and
//! removes their vectors. Transient memory is O(matched ids + one slice).
//! [`CodeIndexer::count_files_matching`] is the read-only dry run.
//! Test: `indexer::tests::batch_remove_10110`;
//! `service::server::remove_files_tests`.

use anyhow::Result;

use super::CodeIndexer;
use crate::core::path_selection::PathSelection;

/// Chunk ids one redb write transaction deletes, at most. A file's ids never
/// split across slices, so one oversized file is one slice of its own.
pub(crate) const REMOVE_SLICE_IDS: usize = 500;

/// #10110 test seam: for each `(index_id, n)` listed, slice `n` and every
/// later slice of a batch removal is refused before its redb write.
#[cfg(test)]
pub(crate) static TEST_FAIL_SLICE_FROM: std::sync::Mutex<Vec<(String, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// What one batch removal matched and removed (#10110).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BatchRemoveCounts {
    /// Files holding at least one selected chunk row.
    pub(crate) matched_files: usize,
    /// Selected chunk rows.
    pub(crate) matched_chunks: usize,
    /// Files whose rows left redb in a committed slice.
    pub(crate) removed_files: usize,
    /// Chunk rows that left redb in a committed slice.
    pub(crate) removed_chunks: usize,
}

/// Why a batch removal did not finish (#10110).
#[derive(Debug)]
pub(crate) enum BatchRemoveError {
    /// A corpus rehydrate, or another batch removal, holds this index's scan
    /// gate. Nothing was changed; a retry can succeed.
    Busy,
    /// A step failed. `counts` reports what committed before it; a retry
    /// finishes the job.
    Failed {
        counts: BatchRemoveCounts,
        source: anyhow::Error,
    },
}

impl std::fmt::Display for BatchRemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str(
                "a corpus rehydrate or another batch removal is running on this index; retry",
            ),
            Self::Failed { counts, source } => write!(
                f,
                "batch removal stopped after {} chunks of {}: {source:#}",
                counts.removed_chunks, counts.matched_chunks
            ),
        }
    }
}

impl std::error::Error for BatchRemoveError {}

impl CodeIndexer {
    /// #10110 red stub.
    pub(crate) async fn count_files_matching(
        &self,
        _sel: &PathSelection,
    ) -> Result<BatchRemoveCounts> {
        anyhow::bail!("#10110: batch removal is not implemented yet")
    }

    /// #10110 red stub.
    pub(crate) async fn remove_files_matching(
        &self,
        sel: &PathSelection,
    ) -> Result<BatchRemoveCounts, BatchRemoveError> {
        self.remove_files_matching_sliced(sel, REMOVE_SLICE_IDS)
            .await
    }

    /// #10110 red stub.
    pub(crate) async fn remove_files_matching_sliced(
        &self,
        _sel: &PathSelection,
        _slice_ids: usize,
    ) -> Result<BatchRemoveCounts, BatchRemoveError> {
        Err(BatchRemoveError::Failed {
            counts: BatchRemoveCounts::default(),
            source: anyhow::anyhow!("#10110: batch removal is not implemented yet"),
        })
    }
}
