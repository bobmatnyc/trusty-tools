//! In-process re-open of a durable corpus that failed to open transiently
//! (#8085, #8958).
//!
//! Why: a corpus open that loses to another lock holder, or times out under
//! warm-boot contention, write-quarantines the index (#4122). Only a
//! successful `CorpusStore::open` lifts that quarantine, and before this module
//! nothing in a running daemon re-attempted one. The status text promised the
//! state "self-heals", yet 220 indexes stayed failed for 10 minutes after every
//! lock holder had gone (#8085), and an index whose warm-boot open lost the
//! race stayed without a durable store until restart, so no reindex wrote its
//! content hashes again (#8958).
//! What: [`reopen_excluded`] re-opens one index's corpus when its failure kind
//! is transient, wires it through `CodeIndexer::reattach_corpus` (which lifts
//! the quarantine once the chunks read back), and re-derives the index's
//! stages. It must run under this index's teardown read guard and permit, so a
//! DELETE, a relocate or a reindex cannot interleave. [`reopen_guarded`] takes
//! both for the sweep and `POST /reindex`; the config-release path already
//! holds the teardown guard. [`spawn_corpus_reopen_sweep`] retries every
//! quarantined index on a fixed interval.
//! Test: `service::corpus_reopen_tests`.

use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::{IndexHandle, IndexRegistry};
use crate::service::storage_layout::{HNSW_FILE, REDB_FILE};
use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

/// How often the background sweep retries quarantined indexes (#8085).
pub(crate) const REOPEN_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long one re-open attempt keeps retrying a held lock (#8085).
///
/// Why: short, because the sweep and the reindex gate both retry again later.
/// What: bounds the `DatabaseAlreadyOpen` retry loop only. A single attempt is
/// one `open_serialized` call, which can itself wait up to
/// `TRUSTY_CORPUS_OPEN_TIMEOUT_SECS` (default 30 s) on a slow or blocked
/// volume, so a gate call can take that long. The call is not wrapped in an
/// outer timeout: cancelling it would drop the teardown guard while the
/// detached open task still runs.
pub(crate) const REOPEN_ATTEMPT_BUDGET: Duration = Duration::from_millis(200);

/// What one re-open attempt did (#8085).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReopenOutcome {
    /// The index was not quarantined; nothing to do.
    NotQuarantined,
    /// The failure is not transient (corruption, unclassified); never retried,
    /// because the operator must diagnose it first (#4333).
    NotTransient,
    /// A reindex, relocate or catch-up holds this index's permit; skipped.
    Busy,
    /// The handle is no longer the registered one (deleted, relocated or
    /// parked); the opened corpus was dropped, which closes it.
    Superseded,
    /// The open failed again, or the corpus file is gone; the index stays
    /// quarantined.
    StillUnavailable(String),
    /// The corpus is wired, the quarantine lifted, and `chunks` rows reloaded.
    Reopened { chunks: usize },
}

/// `true` when `handle` is write-quarantined with a transient failure kind.
async fn transiently_quarantined(handle: &IndexHandle) -> bool {
    let indexer = handle.indexer.read().await;
    indexer.is_write_quarantined()
        && indexer
            .corpus_open_failure
            .is_some_and(|k| k.is_transient())
}

/// Take this index's teardown read guard and permit, then re-open (#8085).
///
/// Why: a re-open without them raced DELETE (which could remove the data dir
/// and then see the open recreate it), relocate, and a reindex.
/// What: returns `NotQuarantined`/`NotTransient` without locking for a healthy
/// index, so the sweep costs one indexer read lock per index. Otherwise takes
/// the teardown read guard, then the permit with `try_acquire_owned`
/// (`Busy` when held), and runs [`reopen_excluded`]. A caller must not already
/// hold the teardown guard: a queued DELETE writer would block the second read.
/// Test: `a_delete_during_a_reopen_leaves_no_corpus_behind`,
/// `a_reopen_skips_an_index_whose_permit_is_held`.
pub(crate) async fn reopen_guarded(
    registry: &IndexRegistry,
    handle: &Arc<IndexHandle>,
    budget: Duration,
) -> ReopenOutcome {
    if !transiently_quarantined(handle).await {
        return precheck_outcome(handle).await;
    }
    let _teardown = crate::service::reindex::acquire_index_teardown_read(&handle.id).await;
    reopen_with_teardown_held(registry, handle, budget, false).await
}

/// [`reopen_excluded`] for a caller that already holds the teardown read guard
/// (#8085).
///
/// What: when `permit_held` is false, takes the index permit with
/// `try_acquire_owned` and answers `Busy` when it is held.
/// Test: `a_reopen_skips_an_index_whose_permit_is_held`.
pub(crate) async fn reopen_with_teardown_held(
    registry: &IndexRegistry,
    handle: &Arc<IndexHandle>,
    budget: Duration,
    permit_held: bool,
) -> ReopenOutcome {
    if permit_held {
        return reopen_excluded(registry, handle, budget).await;
    }
    let Ok(_permit) = crate::service::reindex::index_semaphore(&handle.id).try_acquire_owned()
    else {
        return ReopenOutcome::Busy;
    };
    reopen_excluded(registry, handle, budget).await
}

/// The outcome for an index the lock-free precheck rejected.
async fn precheck_outcome(handle: &IndexHandle) -> ReopenOutcome {
    if handle.indexer.read().await.is_write_quarantined() {
        ReopenOutcome::NotTransient
    } else {
        ReopenOutcome::NotQuarantined
    }
}

/// Re-open `handle`'s durable corpus if a transient failure quarantined it.
///
/// Why: see the module docs.
/// What: the caller holds this index's teardown read guard and permit. Under a
/// read lock, checks the quarantine and its kind and resolves the redb path
/// from the indexer's own storage layout; a missing file is
/// `StillUnavailable`, and the open never creates one. Opens with a `budget`
/// retry. Under the write lock, re-checks the quarantine (a concurrent attempt
/// may have won) and that `handle` is still the registered handle (`Superseded`
/// otherwise), then reattaches the corpus. Computes and writes the stages
/// before releasing the write lock, and re-runs a schema chain the boot attempt
/// recorded as failed.
/// Test: `a_reopen_reruns_a_schema_chain_that_failed_for_lack_of_a_corpus`,
/// `a_reindex_after_the_holder_releases_reattaches_the_corpus`,
/// `the_sweep_lifts_a_contention_quarantine_once_the_lock_is_released`,
/// `a_lock_that_never_releases_keeps_the_index_degraded`,
/// `a_corpus_that_cannot_be_read_back_stays_quarantined`,
/// `a_reopen_never_creates_a_missing_corpus`,
/// `a_swapped_handle_is_not_reattached`.
async fn reopen_excluded(
    registry: &IndexRegistry,
    handle: &Arc<IndexHandle>,
    budget: Duration,
) -> ReopenOutcome {
    let path = {
        let indexer = handle.indexer.read().await;
        if !indexer.is_write_quarantined() {
            return ReopenOutcome::NotQuarantined;
        }
        if !indexer
            .corpus_open_failure
            .is_some_and(|k| k.is_transient())
        {
            return ReopenOutcome::NotTransient;
        }
        // #8085: probe, never create — `storage_dir` creates the directory
        // and `CorpusStore::open` the file.
        match indexer
            .storage_layout()
            .existing_file(&handle.id.0, &handle.root_path, REDB_FILE)
        {
            Ok(Some(path)) => path,
            Ok(None) => {
                return ReopenOutcome::StillUnavailable("the corpus file does not exist".into())
            }
            Err(e) => return ReopenOutcome::StillUnavailable(format!("{e:#}")),
        }
    };
    let corpus = match crate::service::persistence_loader::reopen_existing_corpus_within(
        &path, budget,
    )
    .await
    {
        Ok(c) => Arc::new(c),
        Err(e) => return ReopenOutcome::StillUnavailable(format!("{e:#}")),
    };

    let mut indexer = handle.indexer.write().await;
    if !indexer.is_write_quarantined() {
        // Another attempt re-attached first; dropping ours releases the lock.
        return ReopenOutcome::NotQuarantined;
    }
    // #8085: a handle swapped out of the registry must not hold the file open.
    if !registry
        .get(&handle.id)
        .is_some_and(|live| Arc::ptr_eq(&live, handle))
    {
        return ReopenOutcome::Superseded;
    }
    let chunks = match indexer.reattach_corpus(corpus).await {
        Ok(n) => n,
        Err(e) => return ReopenOutcome::StillUnavailable(format!("{e:#}")),
    };
    let hnsw_snapshot_ready = !indexer.hnsw_load_failed
        && indexer
            .storage_layout()
            .existing_file(&handle.id.0, &handle.root_path, HNSW_FILE)
            .is_ok_and(|f| f.is_some());
    let graph_node_count = indexer.snapshot_symbol_graph().await.node_count();
    let vectors = indexer.vector_count().await.unwrap_or(0);
    // The boot-time chain failed writing its version with no corpus wired.
    let rerun_migrations = indexer
        .migration_faults()
        .iter()
        .any(|f| f.stage == crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN);
    let mut stages = derive_warm_boot_stages(WarmBootInputs {
        chunk_count: chunks,
        hnsw_snapshot_ready,
        graph_node_count,
        lexical_only: handle.lexical_only,
        skip_kg: handle.skip_kg,
        skip_vector: handle.skip_vector,
        corpus_open_failure: None,
    });
    crate::service::warm_boot::fail_semantic_over_empty_corpus(&mut stages, chunks, vectors);
    // #8085: stages land before the write lock drops, so no reader sees the
    // quarantine lifted beside the stale failed stages.
    *handle.stages.write().await = stages;
    drop(indexer);
    if rerun_migrations {
        crate::core::migration::spawn_one_index_migration(
            Arc::clone(handle),
            Arc::new(crate::core::migration::MigrationRegistry::new()),
        );
    }
    tracing::warn!(
        index_id = %handle.id,
        chunks,
        "index '{}': durable corpus re-opened in process after a transient open failure; \
         the write quarantine is lifted (#8085, #8958)",
        handle.id
    );
    ReopenOutcome::Reopened { chunks }
}

/// Retry every registered index whose corpus is transiently quarantined, once
/// (#8085). Returns how many re-opened.
///
/// Test: `the_sweep_lifts_a_contention_quarantine_once_the_lock_is_released`.
pub(crate) async fn reopen_sweep_once(state: &crate::service::SearchAppState) -> usize {
    let mut reopened = 0;
    for id in state.registry.list() {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        if let ReopenOutcome::Reopened { .. } =
            reopen_guarded(&state.registry, &handle, REOPEN_ATTEMPT_BUDGET).await
        {
            reopened += 1;
        }
    }
    reopened
}

/// Spawn the background re-open sweep (#8085).
///
/// Why: the reindex gate only helps an index someone reindexes; a quarantined
/// index that is only searched must recover too, as its status text says.
/// What: every [`REOPEN_SWEEP_INTERVAL`], runs [`reopen_sweep_once`]. A healthy
/// daemon pays one read lock per index per tick.
/// Test: the per-tick body is `reopen_sweep_once`, tested directly.
pub fn spawn_corpus_reopen_sweep(state: crate::service::SearchAppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REOPEN_SWEEP_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            reopen_sweep_once(&state).await;
        }
    });
}
