//! Full-tree reconciliation for a dropped-event (`Flag::Rescan`) notification.
//!
//! Why: when the OS event queue overflows, `notify` reports the loss as a
//! `Flag::Rescan` event and the specific changed paths are gone for good —
//! nothing redelivers them. Without reconciliation the live index silently
//! misses every change in the dropped batch, and a search over those files
//! returns as though the edits never happened, which is indistinguishable from
//! a correct answer.
//!
//! What: [`reconcile_after_rescan`] re-walks the watched root with the same
//! walker the reindex pipeline uses, re-indexes every walked file in bounded
//! batches, drops chunks for tracked files that no longer exist on disk, and
//! rebuilds the symbol graph once at the end. Every failure is returned to the
//! caller rather than logged and swallowed, so the watch loop can re-arm
//! instead of advancing as though the tree were back in sync.
//!
//! Reconciliation scope is the FULL tree, deliberately. `Flag::Rescan`'s own
//! contract is "assume any file or folder might have been modified", so any
//! narrowing has to be justified against that. An mtime watermark is the
//! obvious candidate and is rejected: `rsync --times`, `cp -p`, and `tar -x`
//! all restore a file's old mtime, so an mtime filter can miss exactly the
//! writes this module exists to catch.
//!
//! What the pass DOES narrow is the work it does per file, not which files it
//! looks at (#6570). Every walked file is still read from disk and fingerprinted
//! with the same SHA-256 the reindex pipeline uses, so nothing is trusted from
//! file metadata. A file whose content bytes are byte-identical to what the
//! index already holds skips the tree-sitter parse, the embed, and the redb
//! commit — the pass keeps full-tree coverage and drops the cost that made an
//! overflow expensive. Measured before this: 22 passes in 48h, each reporting
//! `files_reindexed=17284 chunks_indexed=183348` on an unchanged tree.
//!
//! Two limits are worth stating plainly rather than leaving to be discovered.
//! The walk uses [`WalkOptions::default`], so an index registered with
//! `follow_links: true` does not get its symlinked subtrees reconciled here —
//! the watch loop is constructed from a `CodeIndexer`, not an `IndexHandle`,
//! and cannot see that setting. And the deletion sweep only reaches files this
//! process's watcher indexed; a file indexed by a full reindex and deleted
//! during the gap is not in [`IndexedFiles`] and survives until the next
//! reindex prune pass. Both are narrower gaps than the unreconciled state this
//! module replaces, not new ones it introduces.
//!
//! Test: `crate::service::watch_rescan_tests`.
//!
//! [`reconcile_after_rescan`]: crate::service::watch_rescan::reconcile_after_rescan
//! [`WalkOptions::default`]: crate::service::walker::WalkOptions::default

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::RwLock;

use crate::core::chunker::chunk_ast;
use crate::core::registry::IndexId;
use crate::core::CodeIndexer;
use crate::service::indexed_files::IndexedFiles;
use crate::service::walker::walk_source_files;
use crate::service::watch_loop::watcher_relative_path;
use crate::service::watcher::WatchEvent;

/// Files read and committed per batch.
///
/// Why: the reconcile reads file contents into memory before handing them to
/// the indexer. Reading a whole 14k-file tree at once would spike RSS on the
/// exact code path that runs when the machine is already under heavy load.
const RECONCILE_BATCH: usize = 256;

/// First retry delay after a failed reconcile; doubles per consecutive failure.
const RETRY_BASE: Duration = Duration::from_secs(5);

/// Ceiling on the retry backoff. The reconcile is retried forever rather than
/// abandoned — giving up would leave the daemon believing an index is in sync
/// when it is not, which is the failure this whole module exists to prevent.
const RETRY_MAX: Duration = Duration::from_secs(300);

/// What a reconcile pass changed. Reported by the watch loop.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RescanStats {
    /// Files re-read from disk and committed to the index.
    pub files_reindexed: usize,
    /// Chunks added across every batch.
    pub chunks_indexed: usize,
    /// Tracked files removed because they no longer exist on disk.
    pub files_removed: usize,
    /// Files the walk found but could not read, so their current contents are
    /// NOT reflected in the index.
    ///
    /// Why: a per-file read failure must not abort the whole pass — one
    /// permanently unreadable file in a 14k-file tree would otherwise block
    /// every other file from ever being reconciled, and the retry would spin on
    /// it forever. But the pass returning `Ok` while some files were skipped is
    /// exactly the shape this fix exists to eliminate, so the count is carried
    /// out and the watch loop reports a non-zero value at `warn` rather than
    /// letting it disappear into a `debug` line.
    pub files_unreadable: usize,
    /// Files the walk read and fingerprinted, whose content was byte-identical
    /// to what the index already holds, so nothing was re-parsed or committed
    /// for them (#6570).
    ///
    /// Why: this is the number that separates "the overflow found real work" —
    /// what a rescan is for — from "the overflow found nothing", which is what
    /// every one of the 22 observed passes actually found. Reported beside
    /// `files_reindexed` so the log line says which of the two happened.
    pub files_unchanged: usize,
}

impl RescanStats {
    /// Whether this pass left any file's state unknown.
    pub fn is_complete(&self) -> bool {
        self.files_unreadable == 0
    }
}

/// A reconcile pass that could not complete.
///
/// Why: the caller must be able to tell "the tree is reconciled" from "the tree
/// is still unreconciled", and a `Result<(), ()>` or a bare `warn!` cannot. The
/// variants name which half failed so the log line is actionable.
#[derive(Debug, thiserror::Error)]
pub enum RescanError {
    /// A batch of walked files could not be committed to the index.
    #[error("index '{index_id}': could not re-index a batch of {files} file(s) after a dropped-event rescan: {source}")]
    Index {
        /// Index whose reconcile failed.
        index_id: String,
        /// Number of files in the failed batch.
        files: usize,
        /// Underlying indexer error.
        #[source]
        source: anyhow::Error,
    },
    /// Chunks for a file that no longer exists could not be dropped.
    #[error("index '{index_id}': could not drop chunks for deleted file '{path}' after a dropped-event rescan: {source}")]
    Remove {
        /// Index whose reconcile failed.
        index_id: String,
        /// Corpus-relative path whose removal failed.
        path: String,
        /// Underlying indexer error.
        #[source]
        source: anyhow::Error,
    },
}

/// Reconcile the watched tree after the OS dropped an unknown batch of events.
///
/// Why: see the module docs — the dropped paths are unrecoverable, so the only
/// sound response is to re-derive the tree's state from disk.
///
/// What: walks `canonical_root`; for each batch of [`RECONCILE_BATCH`] files
/// reads the content through the office-document-aware extractor, chunks it to
/// learn the chunk ids, commits the batch with `index_files_batch_no_rebuild`,
/// and records the ids in `indexed_files` so a later `Removed` event can still
/// find them. Then removes every tracked file that is absent from disk, and
/// rebuilds the symbol graph once. Unreadable individual files are skipped at
/// `debug`, matching `watch_loop::handle_modified`; indexer failures abort the
/// pass with [`RescanError`].
///
/// The batch path is deliberately not gated on `refuse_incremental_write`: a
/// write-quarantined index holds no `CorpusStore`, so a bulk commit writes
/// nothing durable at all. That is the same reasoning `CodeIndexer::index_file`
/// documents for leaving `index_files_batch*` ungated.
///
/// Test: `rescan_reconcile_indexes_files_written_during_the_gap`,
/// `rescan_reconcile_drops_files_deleted_during_the_gap`,
/// `rescan_reconcile_skips_files_whose_content_did_not_change`.
pub async fn reconcile_after_rescan(
    index_id: &IndexId,
    canonical_root: &Path,
    raw_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
) -> Result<RescanStats, RescanError> {
    let walked = walk_source_files(canonical_root).files;
    let mut stats = RescanStats::default();
    let mut live: HashSet<PathBuf> = HashSet::with_capacity(walked.len());

    // #6570: the same per-index content-hash cache the reindex pipeline skips
    // unchanged files with. Warmed from the durable corpus when this process
    // has not populated it yet, so the FIRST overflow after a daemon restart is
    // already cheap rather than only the second.
    let hashes = crate::service::reindex::hash::hashes_for(index_id);
    warm_hashes_from_corpus(indexer, &hashes).await;

    // #3049: the reconcile is a writer, so it takes this index's teardown-lock
    // read side for the whole pass, exactly as `handle_modified` does.
    let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(index_id).await;

    for batch in walked.chunks(RECONCILE_BATCH) {
        let mut payload: Vec<(String, String)> = Vec::with_capacity(batch.len());
        let mut recorded: Vec<(PathBuf, Vec<String>)> = Vec::with_capacity(batch.len());
        let mut fingerprints: Vec<(PathBuf, String)> = Vec::with_capacity(batch.len());

        for abs in batch {
            let content = match crate::core::extract::read_content(abs).await {
                Ok(content) => content,
                Err(err) => {
                    // Counted into `files_unreadable`, not just logged — this
                    // file's contents are now unknown to the index.
                    stats.files_unreadable += 1;
                    tracing::debug!(%err, ?abs, "rescan reconcile: skip unreadable file");
                    continue;
                }
            };
            // Same relative key `handle_modified` records, so the two paths
            // never disagree about what a file is called in the corpus.
            let rel = watcher_relative_path(canonical_root, raw_root, abs);
            let key = PathBuf::from(&rel);
            // #6570: the file was still read and hashed, so this is a decision
            // about its CONTENT, not about its mtime. `live` is populated either
            // way — a skipped file is present on disk and must never look like a
            // deletion to `sweep_deleted`.
            let fingerprint = crate::service::reindex::hash::hash_content(&content);
            if hashes.get(&key).is_some_and(|h| *h == fingerprint) {
                stats.files_unchanged += 1;
                live.insert(key);
                continue;
            }
            let (chunks, _entities) = chunk_ast(&rel, &content);
            let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
            live.insert(key.clone());
            recorded.push((key.clone(), ids));
            fingerprints.push((key, fingerprint));
            payload.push((rel, content));
        }

        if payload.is_empty() {
            continue;
        }

        let added = {
            let idx = indexer.read().await;
            idx.index_files_batch_no_rebuild(&payload)
                .await
                .map_err(|source| RescanError::Index {
                    index_id: index_id.to_string(),
                    files: payload.len(),
                    source,
                })?
        };

        stats.files_reindexed += payload.len();
        stats.chunks_indexed += added;
        for (key, ids) in recorded {
            indexed_files.record(key, ids).await;
        }
        // #6570: recorded only after the batch committed, so a failed batch
        // cannot leave a hash claiming content the index does not hold.
        for (key, fingerprint) in fingerprints {
            hashes.insert(key, fingerprint);
        }
    }

    stats.files_removed =
        sweep_deleted(index_id, canonical_root, indexer, indexed_files, &live).await?;

    if stats.files_reindexed > 0 || stats.files_removed > 0 {
        // One rebuild for the whole pass — `index_files_batch_no_rebuild` and
        // `remove_file_no_kg_rebuild`'s public sibling both defer it, and the
        // graph is O(N + E) over the entire corpus.
        indexer.read().await.rebuild_symbol_graph_now().await;
    }

    Ok(stats)
}

/// Populate an empty content-hash cache from the index's durable corpus (#6570).
///
/// Why: the in-process cache is per-daemon-lifetime, and `spawn_reindex` is the
/// only thing that warms it today. A daemon that warm-booted an index and then
/// took an FSEvents overflow before any reindex ran would see an empty cache and
/// re-parse the whole tree — the exact cost this skip exists to remove, paid on
/// the first overflow after every restart.
///
/// What: no-op unless the cache is empty for this index, so a cache the reindex
/// pipeline already populated is never re-read and a cache this pass just filled
/// is never overwritten. An index with no durable corpus (BM25-only, tests) and
/// an unreadable hash table both leave the cache empty, which costs a full pass
/// and is correct — the cache is an optimisation whose miss penalty is the old
/// behaviour.
///
/// Test: `rescan_reconcile_warms_the_hash_cache_from_the_corpus`.
async fn warm_hashes_from_corpus(
    indexer: &Arc<RwLock<CodeIndexer>>,
    hashes: &dashmap::DashMap<PathBuf, String>,
) {
    if !hashes.is_empty() {
        return;
    }
    let Some(corpus) = indexer.read().await.corpus_store() else {
        return;
    };
    match tokio::task::spawn_blocking(move || corpus.load_file_hashes()).await {
        Ok(Ok(entries)) => {
            for (path, hash) in entries {
                hashes.insert(PathBuf::from(path), hash);
            }
        }
        Ok(Err(err)) => {
            tracing::debug!(%err, "rescan reconcile: no persisted file hashes to warm from");
        }
        Err(err) => {
            tracing::debug!(%err, "rescan reconcile: file-hash warm task did not complete");
        }
    }
}

/// Drop chunks for tracked files that the walk did not find and that are gone
/// from disk.
///
/// Why: an overflow drops deletions as readily as writes, and a deletion that
/// is never applied leaves a phantom file answering searches forever.
///
/// What: for each tracked path absent from `live`, re-checks the filesystem
/// before removing anything. That guard matters because the walk and the
/// watcher do not use identical filters — a file the walk excluded but which
/// still exists must not be mistaken for a deletion.
///
/// Caller obligation (#3049): `remove_file` is a durable write and this
/// function does NOT take the teardown guard — [`reconcile_after_rescan`] holds
/// it across the call, and is the only caller. Do not add a second caller
/// without one, and do not "fix" this by acquiring the guard here: that is the
/// read side twice on one task, and once a concurrent DELETE queues for the
/// write side the second read parks behind it while this task still holds the
/// first, deadlocking the pass. Declared as `CALLER:reconcile_after_rescan` in
/// `scripts/teardown-guard-manifest.tsv`.
async fn sweep_deleted(
    index_id: &IndexId,
    canonical_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
    live: &HashSet<PathBuf>,
) -> Result<usize, RescanError> {
    let mut removed = 0usize;
    for tracked in indexed_files.paths().await {
        if live.contains(&tracked) || canonical_root.join(&tracked).exists() {
            continue;
        }
        let path = tracked.display().to_string();
        indexer
            .read()
            .await
            .remove_file(&path)
            .await
            .map_err(|source| RescanError::Remove {
                index_id: index_id.to_string(),
                path: path.clone(),
                source,
            })?;
        indexed_files.take(&tracked).await;
        removed += 1;
    }
    Ok(removed)
}

/// What the watch loop owes after a reconcile pass finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RescanFollowUp {
    /// Every walked file is reconciled. Clear the consecutive-failure count.
    InSync,
    /// The tree is still not in sync. Schedule a retry as this attempt number.
    Retry {
        /// 1-based consecutive-failure count, which drives [`retry_backoff`].
        attempt: u32,
    },
}

/// Map a finished reconcile pass to the watch loop's next step.
///
/// Why: a pass falls short in two ways — it returns [`RescanError`], or it
/// returns `Ok` having skipped files it could not read — and the loop answered
/// them differently. The partial case reset the failure count and scheduled
/// nothing, so a transiently unreadable file was reindexed only if some later
/// event happened to touch it. That is the same silent staleness a dropped
/// `Flag::Rescan` produces, reached through this module's own error handling
/// instead of through the lost events, so it gets the same answer: re-arm.
///
/// What: returns [`RescanFollowUp::InSync`] only for an `Ok` whose
/// [`RescanStats::is_complete`] holds; every other outcome is a retry at
/// `consecutive_failures + 1`. Keeping the decision here rather than in the
/// loop's match arms is what makes it one decision instead of three.
///
/// A permanently unreadable file therefore re-walks the tree every
/// [`RETRY_MAX`] forever. That is the cost this module already accepts for a
/// failed pass, and for the same reason: the alternative is a daemon that
/// believes an index is in sync when it is not.
///
/// Test: `rescan_follow_up_clears_the_counter_only_when_the_tree_is_in_sync`,
/// `rescan_partial_pass_schedules_a_retry_that_fires`.
pub(crate) fn rescan_follow_up(
    outcome: Result<&RescanStats, &RescanError>,
    consecutive_failures: u32,
) -> RescanFollowUp {
    if matches!(outcome, Ok(stats) if stats.is_complete()) {
        RescanFollowUp::InSync
    } else {
        RescanFollowUp::Retry {
            attempt: consecutive_failures.saturating_add(1),
        }
    }
}

/// Re-arm a failed reconcile by pushing another [`WatchEvent::Rescan`] onto the
/// watch loop's own channel after a backoff.
///
/// Why: a pass that did not fully reconcile leaves the index out of sync.
/// Returning to the event loop at that point would leave the daemon serving
/// stale results with nothing scheduled to fix them — the same silent miss, one
/// layer up. Re-queueing keeps the loop in the "not yet reconciled" state until
/// a pass actually succeeds.
///
/// What: spawns a detached timer that sends one `Rescan`. Called once per
/// [`RescanFollowUp::Retry`] and never otherwise, so retries cannot stack. If
/// the watch loop has been torn down the send fails against a closed channel
/// and the timer simply expires.
///
/// Test: `rescan_partial_pass_schedules_a_retry_that_fires`,
/// `rescan_retry_backoff_grows_and_saturates`.
pub fn schedule_rescan_retry(tx: UnboundedSender<WatchEvent>, consecutive_failures: u32) {
    let delay = retry_backoff(consecutive_failures);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(WatchEvent::Rescan);
    });
}

/// Exponential backoff for reconcile retries, saturating at [`RETRY_MAX`].
///
/// `consecutive_failures` is 1-based: the first failure waits [`RETRY_BASE`].
pub(crate) fn retry_backoff(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(16);
    RETRY_BASE
        .saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
        .min(RETRY_MAX)
}

#[cfg(test)]
#[path = "watch_rescan_tests.rs"]
mod watch_rescan_tests;
