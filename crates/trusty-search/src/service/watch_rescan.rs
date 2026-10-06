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
use std::sync::atomic::{AtomicBool, Ordering};
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
///
/// #9212: `#[non_exhaustive]`, so a later counter is not a breaking change
/// for an out-of-crate reader; outside the crate, start from `Default`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    /// Files still on disk whose chunks were dropped because the index now
    /// excludes them: the walker's policy no longer admits a tracked file, or
    /// a walked file's content is sops-encrypted (#8922). A file with no
    /// chunks to drop is not counted.
    pub files_excluded: usize,
    /// #9230: tracked files whose removal redb refused. Their chunks and
    /// `IndexedFiles` entry stay, so the retry this forces removes them.
    pub files_failed: usize,
}

impl RescanStats {
    /// Whether this pass left any file's state unknown or unremoved.
    pub fn is_complete(&self) -> bool {
        self.files_unreadable == 0 && self.files_failed == 0
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
    /// The registry no longer holds a handle for this index, so the pass could
    /// not learn which paths the index admits (#7396).
    ///
    /// Why: this used to be a bare `continue` in the watch loop, which
    /// discarded the dropped-event batch outright — no log, no failure count,
    /// no retry. The dropped paths are already unrecoverable at that point, so
    /// an absent handle must re-arm exactly like any other incomplete pass.
    #[error(
        "index '{index_id}': the registry holds no handle for this index, so the admission policy \
         for a dropped-event rescan is unknown"
    )]
    UnregisteredIndex {
        /// Index whose handle could not be resolved.
        index_id: String,
    },
    /// #9059: an exclude glob does not parse, so the pass walks nothing. The
    /// watch loop re-arms it like any failed pass, so the first retry after a
    /// PATCH fixes the globs reconciles what changed during the hold.
    #[error("{message}")]
    Held {
        /// The held index.
        index_id: String,
        /// The hold reason, naming every invalid glob and the fix.
        message: String,
    },
    /// Chunks for a file that no longer exists could not be dropped. Since
    /// #9230 it is logged per file and counted in
    /// [`RescanStats::files_failed`], not returned.
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
/// `debug`, matching `watch_loop::handle_modified`; a batch commit failure
/// aborts the pass with [`RescanError`], and a refused per-file removal is
/// counted in [`RescanStats::files_failed`] (#9230).
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
    reconcile_with_policy(
        index_id,
        canonical_root,
        raw_root,
        indexer,
        indexed_files,
        None,
    )
    .await
}

/// Reconcile against the policy the registry currently holds for this index.
///
/// Why (#7396): the registry lookup belongs INSIDE the pass, not in front of
/// it. When it lived in the watch loop's match arm, a momentarily-absent handle
/// took a bare `continue` and the dropped-event batch was lost with nothing
/// scheduled to recover it — the exact silent data loss the `Flag::Rescan`
/// handling exists to prevent. Returning [`RescanError::UnregisteredIndex`]
/// instead routes the case through the recovery every other incomplete pass
/// already uses: [`rescan_follow_up`] counts it and [`schedule_rescan_retry`]
/// re-arms, so a handle that comes back reconciles the tree.
/// What: `registry` absent means this loop was started without one, which is
/// the pre-#7379 unfiltered behaviour and still reconciles the full root.
/// `registry` present but holding no handle is the failure above.
/// Test: `rescan_without_a_registered_handle_schedules_a_retry`.
pub(crate) async fn reconcile_registered(
    index_id: &IndexId,
    canonical_root: &Path,
    raw_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
    registry: Option<&crate::core::registry::IndexRegistry>,
) -> Result<RescanStats, RescanError> {
    let policy = match registry {
        Some(registry) => match registry.get(index_id) {
            Some(handle) => Some(handle),
            None => {
                return Err(RescanError::UnregisteredIndex {
                    index_id: index_id.to_string(),
                });
            }
        },
        None => None,
    };
    reconcile_with_policy(
        index_id,
        canonical_root,
        raw_root,
        indexer,
        indexed_files,
        policy.as_deref(),
    )
    .await
}

/// Reconcile with the current registered admission policy (#7379).
///
/// #8922: a walked file whose content is sops-encrypted has its chunks dropped
/// instead of indexed, and a tracked file the policy now excludes is dropped by
/// the sweep; both are counted in [`RescanStats::files_excluded`].
/// #9059: a held policy refuses with [`RescanError::Held`] before the walk.
/// Test: `rescan_drops_sops_files_and_files_the_policy_now_excludes`,
/// `every_ingest_path_refuses_a_held_index`.
pub(crate) async fn reconcile_with_policy(
    index_id: &IndexId,
    canonical_root: &Path,
    raw_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
    policy: Option<&crate::core::registry::IndexHandle>,
) -> Result<RescanStats, RescanError> {
    if let Some(hold) = policy.and_then(crate::service::exclude_hold::hold) {
        return Err(RescanError::Held {
            message: hold.reason(),
            index_id: hold.index_id,
        });
    }
    let walked = policy
        .map(crate::service::index_admission::walk)
        .unwrap_or_else(|| walk_source_files(canonical_root))
        .files;
    let mut stats = RescanStats::default();
    let mut live: HashSet<PathBuf> = HashSet::with_capacity(walked.len());
    // #9230: a delete whose rows left redb stamps the corpus once, below.
    let mut delete_committed = false;

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
            // #8922: checked before the hash skip — a hash recorded before
            // the content check existed may sit over plaintext chunks. The
            // file stays `live`, so the sweep below never counts it as gone.
            if crate::core::sops::is_sops_encrypted(&content) {
                live.insert(key.clone());
                // #9230: a refused purge keeps the file's chunks and entry.
                let Some((removed, committed)) = drop_file(index_id, indexer, &rel).await else {
                    stats.files_failed += 1;
                    continue;
                };
                delete_committed |= committed;
                if removed > 0 {
                    stats.files_excluded += 1;
                }
                indexed_files.take(&key).await;
                continue;
            }
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

    let swept = sweep_deleted(
        index_id,
        canonical_root,
        indexer,
        indexed_files,
        &live,
        policy,
    )
    .await;
    stats.files_removed = swept.deleted;
    stats.files_excluded += swept.excluded;
    stats.files_failed += swept.failed;

    if stats.files_reindexed > 0 || stats.files_removed > 0 || stats.files_excluded > 0 {
        // One rebuild for the whole pass — `index_files_batch_no_rebuild` and
        // `remove_file_no_kg_rebuild`'s public sibling both defer it, and the
        // graph is O(N + E) over the entire corpus.
        indexer.read().await.rebuild_symbol_graph_now().await;
    }
    if delete_committed || swept.committed {
        let idx = indexer.read().await;
        idx.record_incremental_commit("rescan delete").await;
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

/// Drop one file's chunks and content hash. Returns `(removed, committed)`,
/// or `None` when the fail-closed purge failed: logged at `warn` as
/// [`RescanError::Remove`], with the file's chunks and hash left in place.
///
/// Why (#8922): the hash goes with the chunks, or a re-admitted unchanged file
/// is hash-skipped forever. No per-file graph rebuild: the pass rebuilds once.
/// #9230: `committed` (rows left redb) makes the pass stamp the corpus; a
/// failure is counted per file, never aborting the pass.
/// Caller obligation (#3049): the same as [`sweep_deleted`]'s — the caller
/// holds the teardown guard; declared in `scripts/teardown-guard-manifest.tsv`.
async fn drop_file(
    index_id: &IndexId,
    indexer: &Arc<RwLock<CodeIndexer>>,
    path: &str,
) -> Option<(usize, bool)> {
    let idx = indexer.read().await;
    match idx.purge_file_committed(index_id, path).await {
        Ok(purged) => Some(purged),
        Err(source) => {
            let err = RescanError::Remove {
                index_id: index_id.to_string(),
                path: path.to_string(),
                source,
            };
            tracing::warn!("rescan reconcile: {err:#}; kept for retry (#9230)");
            None
        }
    }
}

/// What [`sweep_deleted`] removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Swept {
    /// Tracked files gone from disk.
    deleted: usize,
    /// Tracked files still on disk that the policy now excludes (#8922).
    excluded: usize,
    /// #9230: some removal's rows left redb, so the pass stamps the corpus.
    committed: bool,
    /// #9230: removals redb refused; each file stays tracked for a retry.
    failed: usize,
}

/// Drop chunks for tracked files that the walk did not find: files gone from
/// disk, and files the index's policy now excludes.
///
/// Why: an overflow drops deletions as readily as writes, and a deletion that
/// is never applied leaves a phantom file answering searches forever. #8922:
/// a file that became excluded — a new `exclude_globs` entry, a narrowed
/// extension list — answers searches forever the same way.
///
/// What: for each tracked path absent from `live`, re-checks the filesystem
/// before removing anything. A file still on disk is removed only when
/// `policy` holds and [`crate::service::index_admission::admits`] answers
/// `Excluded` for it; an `Included` or undetermined answer keeps it (#7396),
/// and with no policy there is nothing to judge it against.
///
/// Caller obligation (#3049): the purge in [`drop_file`] is a durable write
/// and this function does NOT take the teardown guard — [`reconcile_after_rescan`] holds
/// it across the call, and is the only caller. Do not add a second caller
/// without one, and do not "fix" this by acquiring the guard here: that is the
/// read side twice on one task, and once a concurrent DELETE queues for the
/// write side the second read parks behind it while this task still holds the
/// first, deadlocking the pass.
async fn sweep_deleted(
    index_id: &IndexId,
    canonical_root: &Path,
    indexer: &Arc<RwLock<CodeIndexer>>,
    indexed_files: &IndexedFiles,
    live: &HashSet<PathBuf>,
    policy: Option<&crate::core::registry::IndexHandle>,
) -> Swept {
    use crate::service::index_admission::{admits, Admission};
    let mut swept = Swept::default();
    for tracked in indexed_files.paths().await {
        if live.contains(&tracked) {
            continue;
        }
        let abs = canonical_root.join(&tracked);
        let on_disk = abs.exists();
        // #8922: on disk but excluded by the current policy is a removal too.
        let excluded = on_disk && policy.is_some_and(|h| admits(h, &abs) == Admission::Excluded);
        if on_disk && !excluded {
            continue;
        }
        let Some((removed, committed)) =
            drop_file(index_id, indexer, &tracked.display().to_string()).await
        else {
            swept.failed += 1;
            continue;
        };
        swept.committed |= committed;
        indexed_files.take(&tracked).await;
        if excluded {
            // #8922: counted only when chunks actually left.
            if removed > 0 {
                swept.excluded += 1;
            }
        } else {
            swept.deleted += 1;
        }
    }
    swept
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
/// What: spawns a detached timer that sends one `Rescan`. If the watch loop has
/// been torn down the send fails against a closed channel and the timer simply
/// expires.
///
/// Every watch-task caller goes through [`RescanGate`], which is what keeps
/// retries from stacking (#7396). The gate admits two callers and treats them
/// differently: a [`RescanFollowUp::Retry`] arms unconditionally, because its
/// `attempt` carries the backoff and supersedes any coarser timer; a deferred
/// live admission arms only when nothing is already armed. A batch of N
/// undecidable events therefore costs one timer and one full-tree reconcile,
/// not N of each.
///
/// Test: `rescan_partial_pass_schedules_a_retry_that_fires`,
/// `rescan_retry_backoff_grows_and_saturates`,
/// `crate::service::index_admission::tests::three_undecidable_events_schedule_exactly_one_rescan`.
pub fn schedule_rescan_retry(tx: UnboundedSender<WatchEvent>, consecutive_failures: u32) {
    let delay = retry_backoff(consecutive_failures);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tx.send(WatchEvent::Rescan);
    });
}

/// The one rescan a watch task may have outstanding, and the channel to arm it.
///
/// Why (#7396): the undecidable-admission path in
/// [`crate::service::index_admission::apply_modified`] asks for a rescan once
/// per delivered event, and a broken mount or a directory the daemon lost
/// access to answers every event in a batch that way. Arming a timer per event
/// would put N detached timers and N full-tree reconciles on one watch task for
/// one cause — and a full-tree reconcile is the most expensive thing this
/// module does.
/// What: one flag per watch task, owned by the watch loop. The loop clears it as
/// it takes a [`WatchEvent::Rescan`] off its channel, so a defer raised while a
/// pass is running can still arm the next one.
/// Test: `crate::service::index_admission::tests::three_undecidable_events_schedule_exactly_one_rescan`.
pub(crate) struct RescanGate {
    tx: UnboundedSender<WatchEvent>,
    armed: AtomicBool,
}

impl RescanGate {
    /// Why: the gate arms on the watch loop's own channel, so it holds a clone.
    pub(crate) fn new(tx: UnboundedSender<WatchEvent>) -> Self {
        Self {
            tx,
            armed: AtomicBool::new(false),
        }
    }

    /// Arm the retry a [`RescanFollowUp::Retry`] decision ordered.
    ///
    /// Why: `attempt` is the consecutive-failure count that drives
    /// [`retry_backoff`], and a failed pass must always be re-armed, so this
    /// caller is never suppressed.
    /// What: schedules, then marks the gate armed so defers raised before it
    /// fires fold into it rather than adding a second timer.
    pub(crate) fn arm_retry(&self, attempt: u32) {
        self.armed.store(true, Ordering::SeqCst);
        schedule_rescan_retry(self.tx.clone(), attempt);
    }

    /// Ask for a rescan on behalf of an admission the filesystem could not decide.
    ///
    /// What: arms at the base delay only when nothing is armed, and reports
    /// whether this call was the one that armed it. A deferred save is a fresh
    /// request rather than a consecutive failure, so the attempt number stays 1;
    /// the backoff counter belongs to the reconcile arm that owns it.
    pub(crate) fn request(&self) -> bool {
        if self.armed.swap(true, Ordering::SeqCst) {
            return false;
        }
        schedule_rescan_retry(self.tx.clone(), 1);
        true
    }

    /// Let the next request arm again. Called as a `Rescan` leaves the channel.
    pub(crate) fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
    }
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
