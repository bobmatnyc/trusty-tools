//! Shared helper that builds a `CodeIndexer`, attempting to restore a
//! previously-persisted HNSW snapshot and chunk corpus from disk.
//!
//! Why (issue #85): both `POST /indexes` and the daemon-startup
//! `restore_indexes` hook need the same logic — construct the indexer, wire
//! the embedder, attempt to load HNSW + chunks, and fall back to an empty
//! index on any failure. Centralising this prevents drift between the two
//! call sites (and the inevitable "the warm-boot path silently runs in
//! BM25-only mode" footgun).
//! What: `build_indexer_with_persisted_state` returns a fully-wired
//! `CodeIndexer`. On a corrupt or missing snapshot it falls back to a fresh
//! empty store + corpus and logs at WARN/INFO so operators can tell which
//! path was taken.
//! Test: covered by integration tests in `tests/integration_tests.rs` that
//! drop a state directory, restart, and assert the corpus is intact.

use std::path::PathBuf;
use std::sync::Arc;

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

/// Build a `CodeIndexer` for `index_id`, restoring HNSW + chunks from disk
/// when a snapshot is present.
///
/// Why: see module docs.
/// What: tries `UsearchStore::load_from` first; falls back to a fresh empty
/// store if the load returns `Ok(None)` (no snapshot) or `Err` (corrupt
/// snapshot — logged at WARN). Then attaches the embedder + store, and
/// finally calls `load_chunks_from_disk` to rehydrate the corpus.
/// Test: see module docs.
pub async fn build_indexer_with_persisted_state(
    index_id: &str,
    root_path: PathBuf,
    embedder: &Arc<dyn Embedder>,
) -> CodeIndexer {
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
/// Why: the caller has a full `PersistedIndex` (from `indexes.toml` or from
/// filesystem discovery), including the `colocated` flag. Using this variant
/// means no flag is lost — colocated indexes open their storage from
/// `<root_path>/.trusty-search/` and legacy indexes from the global data dir.
/// What: resolves paths via `hnsw_path_for_entry` / `corpus_redb_path_for_entry`,
/// then proceeds identically to the original `build_indexer_with_persisted_state`.
/// Test: `colocated_indexer_builds_from_entry` covers the colocated path;
/// the existing warm-boot integration tests cover the legacy path.
pub async fn build_indexer_from_entry(
    entry: &PersistedIndex,
    embedder: &Arc<dyn Embedder>,
) -> CodeIndexer {
    let index_id = &entry.id;
    let root_path = entry.root_path.clone();
    let dim = embedder.dimension();
    let store: Arc<dyn VectorStore> = build_store_for_entry(entry, dim).await;
    let mut indexer =
        CodeIndexer::new(index_id, root_path).with_components(Arc::clone(embedder), store);

    // Issue #28: wire the durable redb corpus store before restoring chunks.
    // A failure to open the redb file is non-fatal — we log and run without a
    // corpus store (the index simply behaves as a pre-#28 in-memory daemon and
    // will be re-persisted to JSON via `spawn_incremental_persist`).
    match persistence::corpus_redb_path_for_entry(entry) {
        Ok(redb_path) => match CorpusStore::open(&redb_path) {
            Ok(corpus) => indexer.set_corpus_store(Arc::new(corpus)),
            Err(e) => tracing::warn!(
                "warm-boot: could not open redb corpus for '{index_id}' at {} ({e}) — \
                 running without durable corpus store",
                redb_path.display()
            ),
        },
        Err(e) => tracing::warn!("cannot resolve redb corpus path for '{index_id}': {e}"),
    }

    restore_corpus_for_entry(&mut indexer, entry).await;
    indexer
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

/// Try to load the HNSW snapshot for `entry`, routing to colocated or legacy
/// storage. On any failure (missing, corrupt, dimension mismatch) returns a
/// fresh empty `UsearchStore`.
///
/// Why: mirrors the original `build_store` but uses `hnsw_path_for_entry` so
/// colocated indexes read from `<root>/.trusty-search/hnsw.usearch`.
/// What: resolves the path, checks for the file, loads, falls back to fresh.
/// Test: covered by the warm-boot integration tests (legacy path) and the
/// colocated integration tests (colocated path).
async fn build_store_for_entry(entry: &PersistedIndex, dim: usize) -> Arc<dyn VectorStore> {
    let index_id = &entry.id;
    let path = match persistence::hnsw_path_for_entry(entry) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("cannot resolve hnsw path for '{index_id}': {e}");
            return fresh_store(dim);
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
                    return Arc::new(store);
                }
                tracing::warn!(
                    "warm-boot: hnsw snapshot for '{}' has dim {} but embedder is {} — starting fresh",
                    index_id,
                    store.dim(),
                    dim
                );
            }
            Ok(None) => {
                // Sidecar missing/corrupt — fall back to fresh.
                tracing::warn!(
                    "warm-boot: hnsw snapshot at {} could not be loaded — starting fresh",
                    path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "warm-boot: error loading hnsw snapshot at {}: {e} — starting fresh",
                    path.display()
                );
            }
        }
    }
    fresh_store(dim)
}

fn fresh_store(dim: usize) -> Arc<dyn VectorStore> {
    // SAFETY (issue #101): `UsearchStore::new` only fails on OOM during the
    // initial HNSW index allocation. There is no meaningful recovery path —
    // the daemon needs an HNSW lane to function, and an OOM at startup would
    // have already torn the process down. We use `.expect` (not `panic!`) so
    // the failure message is uniform and the intent (infallible-modulo-OOM)
    // is documented for the reader.
    let s = UsearchStore::new(dim).unwrap_or_else(|e| {
        tracing::error!(
            "failed to allocate UsearchStore (dim={dim}): {e} — daemon cannot continue"
        );
        // Re-raise as a panic carrying the underlying error: there is no
        // sensible fallback (BM25-only stores are constructed via a different
        // path, not by replacing this Arc<dyn VectorStore>).
        panic!("usearch alloc failure (OOM during HNSW init, dim={dim}): {e}");
    });
    Arc::new(s) as Arc<dyn VectorStore>
}
