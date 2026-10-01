//! Which content hashes a batch commit records (#8976).
//!
//! Why: a recorded hash makes the next reindex skip the file. Recording one
//! for a file whose chunks did not land left that file unsearchable. Merely
//! not recording it was not enough either: the file's OLD hash stayed in
//! place, so reverting the file to that content skipped it as well.
//! What: per-file chunk ids for a parsed batch, the withhold pass that swaps
//! an unvouched hash for [`WITHHELD_HASH`], and [`record_hashes`], the one
//! writer of the in-memory and redb hash caches for a batch.
//! Test: `service::reindex::hash_withhold_tests`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::core::indexer::ParsedBatch;

use super::batch::BatchCtx;
use super::hash::{shrink_hashes_if_needed, MAX_FILE_HASHES_PER_INDEX};
use super::hash_cache;

/// The hash recorded for a file whose chunks did not all land.
///
/// Why: it must overwrite the previous hash in memory and in redb, and it
/// must never equal a real content hash, so the next reindex re-reads the
/// file whatever its content is. A SHA-256 hex digest is never empty.
pub(super) const WITHHELD_HASH: &str = "";

/// Most file names one summary WARN lists before eliding the rest.
const WARN_SAMPLE: usize = 5;

/// Each file's chunk ids in `parsed` (#8976).
pub(super) fn chunk_ids_by_file(parsed: &ParsedBatch) -> Vec<(String, Vec<String>)> {
    let mut by_file: HashMap<&str, Vec<String>> = HashMap::new();
    for chunk in &parsed.chunks {
        by_file
            .entry(chunk.file.as_str())
            .or_default()
            .push(chunk.id.clone());
    }
    by_file
        .into_iter()
        .map(|(file, ids)| (file.to_string(), ids))
        .collect()
}

/// Replace the hash of every file whose chunks did not all land (#8976).
///
/// Why: a hash with no chunks behind it makes every later reindex skip the
/// file, and dropping the new hash leaves the old one to do the same.
/// What: keeps a hash when its file is in `durable` or in `final_chunkless`
/// (blank content, or JSON above the window ceiling); every other hash
/// becomes [`WITHHELD_HASH`]. Logs one summary WARN per call, not per file.
/// Test: `withheld_hash_overwrites_the_old_one_so_a_revert_reindexes`,
/// `partial_chunk_landing_withholds_the_hash`,
/// `blank_and_too_large_files_keep_their_hash`, and
/// `withheld_hashes_log_one_summary_warn_per_batch`.
pub(super) fn withhold_chunkless_hashes(
    ctx: &BatchCtx,
    new_hashes: Vec<(PathBuf, String)>,
    durable: &HashSet<String>,
    final_chunkless: &[String],
) -> Vec<(PathBuf, String)> {
    let mut withheld: Vec<String> = Vec::new();
    let hashes = new_hashes
        .into_iter()
        .map(|(path, hash)| {
            let rel = path.to_string_lossy().into_owned();
            if durable.contains(&rel) || final_chunkless.contains(&rel) {
                (path, hash)
            } else {
                withheld.push(rel);
                (path, WITHHELD_HASH.to_string())
            }
        })
        .collect();
    if !withheld.is_empty() {
        let more = withheld.len().saturating_sub(WARN_SAMPLE);
        let mut files = withheld[..withheld.len().min(WARN_SAMPLE)].join(", ");
        if more > 0 {
            files.push_str(&format!(" and {more} more"));
        }
        tracing::warn!(
            index_id = %ctx.index_id.0,
            withheld = withheld.len(),
            files = %files,
            "reindex: files in this batch did not land all their chunks; their \
             hashes are cleared, so the next reindex retries them (#8976)"
        );
    }
    hashes
}

/// Write `hashes` to the in-memory cache and mirror them to redb.
///
/// Why: the success and the commit-error arms both record hashes, and the
/// error arm must overwrite the old ones exactly as the success arm does.
/// What: inserts each pair, bounds the cache (#75), then persists the batch
/// to the corpus store (#662).
/// Test: `withheld_hash_overwrites_the_old_one_so_a_revert_reindexes`,
/// `commit_error_clears_the_old_hash`.
pub(super) async fn record_hashes(ctx: &BatchCtx, hashes: &[(PathBuf, String)]) {
    for (path, h) in hashes {
        ctx.hashes.insert(path.clone(), h.clone());
    }
    shrink_hashes_if_needed(&ctx.hashes);
    hash_cache::persist_batch(
        &ctx.handle,
        hashes,
        MAX_FILE_HASHES_PER_INDEX,
        ctx.hashes.len(),
    )
    .await;
}
