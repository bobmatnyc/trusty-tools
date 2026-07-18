//! Shared helper that builds a `CodeIndexer` by restoring a persisted HNSW
//! snapshot and chunk corpus from disk.
//!
//! Why (issue #85): both `POST /indexes` and `restore_indexes` need the same
//! logic — construct the indexer, wire the embedder, load HNSW + chunks, fall
//! back on any soft failure. Centralising prevents drift between call sites.
//! What: `build_indexer_from_entry` returns `Result<CodeIndexer, anyhow::Error>`.
//! Corrupt/missing snapshots fall back to an empty store (logged at WARN/INFO).
//! OOM allocating the HNSW store surfaces as `Err` so callers can skip the
//! index cleanly instead of panicking (closes #954).
//! Test: integration tests in `tests/integration_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use trusty_common::migrations::{
    file_stamp::{read_version_from_file, write_version_to_file},
    MigrationRunner, SchemaVersion,
};

use crate::core::{
    corpus::CorpusStore,
    embed::Embedder,
    indexer::{migrations::JsonCorpusToRedbMigration, CodeIndexer},
    store::{UsearchStore, VectorStore},
};

use crate::service::persistence::{self, PersistedIndex};

/// Open a `CorpusStore`, retrying once on `DatabaseAlreadyOpen` (issue #840).
///
/// Why: on a fast daemon restart the OS may not have released redb's file lock.
/// A 50 ms async sleep avoids blocking a tokio worker thread during the retry.
/// What: calls `CorpusStore::open`; on `DatabaseError::DatabaseAlreadyOpen`
/// (matched via typed downcast) sleeps 50 ms and retries once. All other
/// errors surface immediately.
/// Test: `corpus_recovery::tests::database_already_open_variant_is_stable`
/// (pinning) + warm-boot tests in this module.
async fn open_corpus_with_retry(path: &Path) -> Result<CorpusStore> {
    match CorpusStore::open(path) {
        Ok(store) => Ok(store),
        Err(e) => {
            // Typed downcast — redb error-message rewording cannot disable retry.
            let is_already_open = e
                .downcast_ref::<redb::DatabaseError>()
                .map(|db_err| matches!(db_err, redb::DatabaseError::DatabaseAlreadyOpen))
                .unwrap_or(false);
            if is_already_open {
                tracing::warn!(
                    "warm-boot: redb corpus at {} is locked (DatabaseAlreadyOpen) — \
                     retrying in 50 ms (refs #840)",
                    path.display()
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                CorpusStore::open(path)
            } else {
                Err(e)
            }
        }
    }
}

/// Build a `CodeIndexer` for `index_id`, restoring HNSW + chunks from disk.
///
/// Why: see module docs. Returns `Err` only on OOM allocating the fresh store.
/// What: tries `UsearchStore::load_from`; falls back to fresh on missing/corrupt
/// snapshot (logged at WARN); attaches embedder + store; rehydrates corpus.
/// Test: see module docs.
pub async fn build_indexer_with_persisted_state(
    index_id: &str,
    root_path: PathBuf,
    embedder: &Arc<dyn Embedder>,
) -> Result<CodeIndexer> {
    // Build a minimal PersistedIndex so we can use the entry-aware path helpers.
    // `colocated` defaults to false (legacy global storage) for backward
    // compatibility — callers that have a full `PersistedIndex` should call
    // `build_indexer_from_entry` instead.
    let entry = PersistedIndex {
        id: index_id.to_string(),
        root_path: root_path.clone(),
        ..Default::default()
    };
    build_indexer_from_entry(&entry, embedder).await
}

/// Build a `CodeIndexer` from a `PersistedIndex`, routing storage to colocated
/// or legacy global paths based on `entry.colocated`.
///
/// Why: preserves the `colocated` flag — colocated indexes open storage from
/// `<root_path>/.trusty-search/`; legacy from the global data dir. Returns
/// `Err` only on OOM; corrupt/missing snapshots fall back to an empty store.
/// What: resolves paths via `hnsw_path_for_entry` / `corpus_redb_path_for_entry`,
/// builds the store (with fallback), attaches the embedder, rehydrates corpus.
/// Test: `colocated_create_path_wires_corpus_store_for_schema_version` and
/// `colocated_create_handler_path_survives_simulated_reload` cover the
/// colocated path; `legacy_non_colocated_path_does_not_panic` covers the
/// legacy path.
pub async fn build_indexer_from_entry(
    entry: &PersistedIndex,
    embedder: &Arc<dyn Embedder>,
) -> Result<CodeIndexer> {
    let index_id = &entry.id;
    let root_path = entry.root_path.clone();
    let dim = embedder.dimension();
    let (store, hnsw_load_failed): (Arc<dyn VectorStore>, bool) =
        build_store_for_entry(entry, dim).await?;
    let mut indexer =
        CodeIndexer::new(index_id, root_path).with_components(Arc::clone(embedder), store);
    // Issue #2922: propagate whether a persisted snapshot existed but failed
    // to load, so warm-boot / lazy-load can fold it into `hnsw_snapshot_ready`
    // instead of trusting bare file existence.
    indexer.hnsw_load_failed = hnsw_load_failed;
    // Issue #313 / #2984 (Phase 0): propagate `skip_kg` onto the indexer
    // itself — not just the `IndexHandle` built later by the caller — so
    // `restore_corpus_for_entry` below (via `load_chunks_from_redb` /
    // `load_chunks_from_disk`) can skip the symbol-graph load/rebuild
    // entirely instead of paying the heap cost the flag was meant to avoid.
    indexer.skip_kg = entry.skip_kg;

    // Issue #28/#840/#1158: wire the durable redb corpus store.  Failure is
    // non-fatal but logged at ERROR (#840) because a missing corpus means the
    // next reindex cold-starts (Skipped 0).  `open_corpus_with_retry` retries
    // once on DatabaseAlreadyOpen (stale file lock from a rapid restart).
    // Issue #1158: set `corpus_open_failed` so the warm-boot stage-classifier
    // can emit `StageStatus::Failed` instead of the misleading `InProgress`.
    match persistence::corpus_redb_path_for_entry(entry) {
        Ok(redb_path) => {
            let open_result = open_corpus_with_retry(&redb_path).await;
            match open_result {
                Ok(corpus) => indexer.set_corpus_store(Arc::new(corpus)),
                Err(e) => {
                    tracing::error!(
                        "warm-boot: FAILED to open redb corpus for '{index_id}' at {} ({e}). \
                         The durable corpus store is unavailable — the next reindex will be a \
                         full cold-start (Skipped 0). Check permissions and whether another \
                         process holds the redb file lock. (refs #840, #1158)",
                        redb_path.display()
                    );
                    // Issue #1158: mark the failure so the stage-classifier
                    // emits Failed instead of the misleading InProgress.
                    indexer.corpus_open_failed = true;
                }
            }
        }
        Err(e) => tracing::warn!("cannot resolve redb corpus path for '{index_id}': {e}"),
    }

    restore_corpus_for_entry(&mut indexer, entry).await;
    Ok(indexer)
}

/// Restore the chunk corpus for `indexer`, preferring the redb store and
/// falling back to the legacy `chunks.json` snapshot via the shared
/// `trusty-common::migrations` runner (issues #28, #179).
///
/// Why: redb is the new source of truth, but daemons upgraded in place have a
/// populated `chunks.json` and an empty `index.redb`. The migration kernel
/// owns the ordering — we always try redb first (the fast path) and only run
/// the legacy-JSON step when redb is empty *and* the on-disk schema stamp
/// hasn't already recorded that the migration ran. The stamp is written
/// after every successful migration step so subsequent boots skip the JSON
/// probe entirely.
/// What: tries `load_chunks_from_redb` first; on a populated redb corpus
/// stamps the schema (so legacy `chunks.json` becomes inert) and returns.
/// On an empty redb the [`MigrationRunner`] dispatches
/// [`JsonCorpusToRedbMigration`], which reads the JSON snapshot and seeds
/// redb. The runner writes the schema stamp after each successful step.
/// Test: covered by the corpus roundtrip + migration integration tests; the
/// runner itself is unit-tested in `trusty-common::migrations`.
async fn restore_corpus_for_entry(indexer: &mut CodeIndexer, entry: &PersistedIndex) {
    let index_id = &entry.id;
    // Primary path: redb durable corpus.
    match indexer.load_chunks_from_redb().await {
        Ok(n) if n > 0 => {
            tracing::info!("warm-boot: restored {n} chunks for index '{index_id}' from redb");
            // Ensure the schema stamp is bumped past the JSON → redb step so
            // a stray `chunks.json` left over from the legacy build is never
            // re-read on the next boot.
            stamp_if_unversioned_for_entry(entry);
            return;
        }
        Ok(_) => {} // empty redb — fall through to the migration runner.
        Err(e) => tracing::warn!(
            "warm-boot: redb corpus load failed for '{index_id}' ({e}) — \
             trying registered migrations"
        ),
    }

    // Migration runner path (issue #179): dispatches the legacy JSON →
    // redb migration when the on-disk schema stamp says it hasn't yet run.
    run_migrations_for_entry(indexer, entry);
}

/// Dispatch the trusty-search migration runner for one index.
///
/// Why: lifted into its own function so the redb-empty branch in
/// [`restore_corpus_for_entry`] and any future "force re-migrate" admin command
/// can share one entry point. Keeps the runner's stamp file path resolution
/// and error logging in one place. Uses `schema_version_path_for_entry` so the
/// stamp lands in the right location for both colocated and legacy indexes.
/// What: reads the current stamp via `read_version_from_file`, instantiates
/// the runner with [`JsonCorpusToRedbMigration`], and runs it against the
/// indexer. Failures are logged but never propagated — a missing
/// `chunks.json` is the genuine first-boot case and yields an empty corpus.
/// Test: covered by the existing migration integration tests.
fn run_migrations_for_entry(indexer: &mut CodeIndexer, entry: &PersistedIndex) {
    let index_id = &entry.id;
    let stamp_path = match persistence::schema_version_path_for_entry(entry) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot resolve schema version path for '{index_id}': {e}");
            return;
        }
    };
    let current = match read_version_from_file(&stamp_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "warm-boot: failed to read schema stamp at {} ({e}) — \
                 treating as UNVERSIONED",
                stamp_path.display()
            );
            SchemaVersion::UNVERSIONED
        }
    };
    let runner = MigrationRunner::new(vec![Box::new(JsonCorpusToRedbMigration)]);
    if let Err(e) = runner.run(indexer, current, |v| write_version_to_file(&stamp_path, v)) {
        tracing::warn!(
            "warm-boot: migration runner failed for '{index_id}' ({e}) — \
             starting with whatever state was restored"
        );
    }
}

/// Stamp the schema as fully migrated if the on-disk stamp is currently
/// UNVERSIONED.
///
/// Why: the redb-populated branch in [`restore_corpus_for_entry`] short-circuits
/// without invoking the runner. We still want the stamp to advance so a
/// future migration with a higher `from_version` runs cleanly, and so a
/// stray legacy `chunks.json` is never reprocessed. Writing only when the
/// stamp is missing/UNVERSIONED preserves any version that a previous
/// runner write recorded.
/// What: best-effort — read the stamp, and if it is UNVERSIONED, write the
/// current `TRUSTY_SEARCH_SCHEMA_TARGET`. A failure is logged at WARN.
/// Test: covered indirectly via the corpus roundtrip integration tests.
fn stamp_if_unversioned_for_entry(entry: &PersistedIndex) {
    use crate::core::indexer::migrations::TRUSTY_SEARCH_SCHEMA_TARGET;

    let index_id = &entry.id;
    let stamp_path = match persistence::schema_version_path_for_entry(entry) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot resolve schema version path for '{index_id}': {e}");
            return;
        }
    };
    let current = read_version_from_file(&stamp_path).unwrap_or(SchemaVersion::UNVERSIONED);
    if current >= TRUSTY_SEARCH_SCHEMA_TARGET {
        return;
    }
    if let Err(e) = write_version_to_file(&stamp_path, TRUSTY_SEARCH_SCHEMA_TARGET) {
        tracing::warn!(
            "warm-boot: failed to bump schema stamp for '{index_id}' at {} ({e})",
            stamp_path.display()
        );
    }
}

/// Try to load the HNSW snapshot for `entry`. On soft failure (missing, corrupt,
/// dim mismatch) falls back to a fresh empty store. Returns `Err` only on OOM.
///
/// Why: uses `hnsw_path_for_entry` so colocated indexes read from the project's
/// `.trusty-search/hnsw.usearch`; propagates OOM as `Err` (closes #954).
/// What: resolves path, checks for snapshot, loads (or falls back), returns
/// `(store, load_failed)`. `load_failed` is `true` only when a snapshot file
/// existed but could not be restored (issue #2922) — never for the ordinary
/// "no snapshot yet" first-boot case — so callers can distinguish "never
/// indexed" from "corrupt/truncated on-disk state" when deriving
/// `hnsw_snapshot_ready` for `/health`.
/// Test: warm-boot integration tests (legacy) + colocated integration tests;
/// `tests::build_store_for_entry_flags_load_failure_on_corrupt_snapshot`.
async fn build_store_for_entry(
    entry: &PersistedIndex,
    dim: usize,
) -> Result<(Arc<dyn VectorStore>, bool)> {
    let index_id = &entry.id;
    let path = match persistence::hnsw_path_for_entry(entry) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot resolve hnsw path for '{index_id}': {e}");
            return Ok((fresh_store(dim)?, false));
        }
    };

    if persistence::has_persisted_hnsw(&path) {
        match UsearchStore::load_from(&path).await {
            Ok(Some(store)) => {
                if store.dim() == dim {
                    tracing::info!(
                        "warm-boot: restored HNSW snapshot for '{}' from {}",
                        index_id,
                        path.display()
                    );
                    return Ok((Arc::new(store), false));
                }
                tracing::warn!(
                    "warm-boot: hnsw snapshot for '{}' has dim {} but embedder is {} — starting fresh",
                    index_id,
                    store.dim(),
                    dim
                );
            }
            Ok(None) => {
                // Sidecar missing/corrupt, or the #2922 zero-vector-vs-populated
                // guard tripped — fall back to fresh.
                tracing::warn!(
                    "warm-boot: hnsw snapshot at {} could not be loaded — starting fresh \
                     (semantic search will report not-ready for '{}' until reindexed, issue #2922)",
                    path.display(),
                    index_id,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "warm-boot: error loading hnsw snapshot at {}: {e} — starting fresh \
                     (semantic search will report not-ready for '{}' until reindexed, issue #2922)",
                    path.display(),
                    index_id,
                );
            }
        }
        // A snapshot file existed but we fell through to the fresh-store
        // fallback above — issue #2922: the caller must NOT treat this index
        // as having a ready semantic snapshot.
        return Ok((fresh_store(dim)?, true));
    }
    Ok((fresh_store(dim)?, false))
}

/// Allocate a fresh empty `UsearchStore`. Returns `Err` on OOM instead of
/// panicking so callers can skip the index with a diagnostic log (#954).
///
/// Why: the HNSW allocator can fail on memory-constrained hosts at warm-boot;
/// propagating the error lets the daemon keep serving remaining indexes rather
/// than aborting. Systemd `Restart=on-failure` handles persistent OOM.
/// What: calls `UsearchStore::new(dim)`; logs ERROR and returns `Err` on failure.
/// Test: normal allocation exercised by all persistence_loader tests; OOM path
/// is not reproducible in unit tests (requires near-full RAM).
fn fresh_store(dim: usize) -> Result<Arc<dyn VectorStore>> {
    UsearchStore::new(dim)
        .map(|s| Arc::new(s) as Arc<dyn VectorStore>)
        .map_err(|e| {
            tracing::error!(
                "failed to allocate UsearchStore (dim={dim}): {e} — \
                 index will be skipped (OOM, #954)"
            );
            anyhow::anyhow!("usearch alloc failure (OOM, dim={dim}): {e}")
        })
}

// Why (issue #1195 / SLOC cap): `mod.rs` was drifting past the 500-SLOC
// production cap once the #2984 Phase-0 follow-up regression tests were
// added. The test module carries no production logic, so it is split into
// its own file — mirroring the `core::indexer::tests` directory-module
// convention already used elsewhere in this crate.
#[cfg(test)]
mod tests;
