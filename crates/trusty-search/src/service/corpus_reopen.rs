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
//! What: [`try_reopen_quarantined_corpus`] re-opens one index's corpus when its
//! failure kind is transient, wires it through `set_corpus_store` (which lifts
//! the quarantine), reloads the chunks, and re-derives the index's stages. The
//! reindex gate calls it before refusing, and [`spawn_corpus_reopen_sweep`]
//! retries every quarantined index on a fixed interval.
//! Test: `service::corpus_reopen_tests`.

use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::IndexHandle;
use crate::service::storage_layout::{HNSW_FILE, REDB_FILE};
use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

/// How often the background sweep retries quarantined indexes (#8085).
pub(crate) const REOPEN_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// How long one re-open attempt keeps retrying a held lock (#8085).
///
/// Why: short, because the sweep and the reindex gate both retry again later;
/// a long budget would only hold a request open.
pub(crate) const REOPEN_ATTEMPT_BUDGET: Duration = Duration::from_millis(200);

/// What one re-open attempt did (#8085).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReopenOutcome {
    /// The index was not quarantined; nothing to do.
    NotQuarantined,
    /// The failure is not transient (corruption, unclassified); never retried,
    /// because the operator must diagnose it first (#4333).
    NotTransient,
    /// The open failed again; the index stays quarantined.
    StillUnavailable(String),
    /// The corpus is wired, the quarantine lifted, and `chunks` rows reloaded.
    Reopened { chunks: usize },
}

/// Re-open `handle`'s durable corpus if a transient failure quarantined it.
///
/// Why: see the module docs.
/// What: under a read lock, checks the quarantine and its kind and resolves the
/// redb path from the indexer's own storage layout. Opens with a
/// [`REOPEN_ATTEMPT_BUDGET`] retry. On success, takes the write lock, re-checks
/// the quarantine (a concurrent attempt may have won), wires the corpus, and
/// reloads chunks; a reload failure detaches the corpus and re-quarantines the
/// index, so a quarantine is never lifted over an in-memory corpus that does not
/// match the file. Re-derives `handle.stages` as warm boot would, and re-runs a
/// schema chain the boot attempt recorded as failed.
/// Test: `a_reopen_reruns_a_schema_chain_that_failed_for_lack_of_a_corpus`,
/// `a_reindex_after_the_holder_releases_reattaches_the_corpus`,
/// `the_sweep_lifts_a_contention_quarantine_once_the_lock_is_released`,
/// `a_lock_that_never_releases_keeps_the_index_degraded`.
pub(crate) async fn try_reopen_quarantined_corpus(
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
        match indexer
            .storage_layout()
            .storage_dir(&handle.id.0, &handle.root_path)
        {
            Ok(dir) => dir.join(REDB_FILE),
            Err(e) => return ReopenOutcome::StillUnavailable(format!("{e:#}")),
        }
    };
    let corpus = match crate::service::persistence_loader::open_corpus_with_retry_within(
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
    indexer.set_corpus_store(Arc::clone(&corpus));
    let chunks = match indexer.load_chunks_from_redb().await {
        Ok(n) => n,
        Err(e) => {
            indexer.take_corpus_store();
            indexer.quarantine_detached_corpus(
                crate::core::corpus::CorpusOpenFailure::Unclassified,
                "the re-opened corpus could not be read back (#8085)",
            );
            return ReopenOutcome::StillUnavailable(format!("{e:#}"));
        }
    };
    let hnsw_snapshot_ready = !indexer.hnsw_load_failed
        && indexer
            .storage_layout()
            .storage_dir(&handle.id.0, &handle.root_path)
            .map(|d| crate::service::persistence::has_persisted_hnsw(&d.join(HNSW_FILE)))
            .unwrap_or(false);
    let graph_node_count = indexer.snapshot_symbol_graph().await.node_count();
    let vectors = indexer.vector_count().await.unwrap_or(0);
    // The boot-time chain failed writing its version with no corpus wired.
    let rerun_migrations = indexer
        .migration_faults()
        .iter()
        .any(|f| f.stage == crate::core::indexer::MIGRATION_STAGE_SCHEMA_CHAIN);
    drop(indexer);
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
    *handle.stages.write().await = stages;
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
            try_reopen_quarantined_corpus(&handle, REOPEN_ATTEMPT_BUDGET).await
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
