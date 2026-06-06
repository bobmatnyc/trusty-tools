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

    // Issue #28/#840: wire the durable redb corpus store.  Failure is non-fatal
    // but logged at ERROR (#840) because a missing corpus means the next reindex
    // cold-starts (Skipped 0).  `open_corpus_with_retry` retries once on
    // DatabaseAlreadyOpen (stale file lock from a rapid restart).
    match persistence::corpus_redb_path_for_entry(entry) {
        Ok(redb_path) => {
            let open_result = open_corpus_with_retry(&redb_path).await;
            match open_result {
                Ok(corpus) => indexer.set_corpus_store(Arc::new(corpus)),
                Err(e) => tracing::error!(
                    "warm-boot: FAILED to open redb corpus for '{index_id}' at {} ({e}). \
                     The durable corpus store is unavailable — the next reindex will be a \
                     full cold-start (Skipped 0). Check permissions and whether another \
                     process holds the redb file lock. (refs #840)",
                    redb_path.display()
                ),
            }
        }
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
    // SAFETY (#101): `UsearchStore::new` only fails on OOM; an OOM at startup
    // has no sensible recovery path so we panic with a clear message.
    let s = UsearchStore::new(dim).unwrap_or_else(|e| {
        tracing::error!("failed to allocate UsearchStore (dim={dim}): {e}");
        panic!("usearch alloc failure (OOM during HNSW init, dim={dim}): {e}");
    });
    Arc::new(s) as Arc<dyn VectorStore>
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::service::colocated_storage;
    use crate::service::persistence::PersistedIndex;
    use tempfile::tempdir;
    use trusty_common::embedder::MockEmbedder;

    // ── Helper ────────────────────────────────────────────────────────────────

    fn mock_embedder() -> Arc<dyn crate::core::embed::Embedder> {
        Arc::new(MockEmbedder::new(8))
    }

    fn minimal_raw_chunk(id: &str) -> RawChunk {
        RawChunk {
            id: id.to_string(),
            file: "src/lib.rs".to_string(),
            start_line: 1,
            end_line: 5,
            content: "fn hello() {}".to_string(),
            function_name: None,
            language: Some("rust".to_string()),
            chunk_type: ChunkType::Code,
            calls: Vec::new(),
            inherits_from: Vec::new(),
            chunk_depth: 0,
            parent_chunk_id: None,
            child_chunk_ids: Vec::new(),
            nlp_keywords: Vec::new(),
            nlp_code_refs: Vec::new(),
            virtual_terms: Vec::new(),
        }
    }

    // ── Issue #483 regression ─────────────────────────────────────────────────

    /// Why: guards the writer/loader path divergence (#483) where
    /// `create_index_handler` used `colocated: false`, routing the corpus to
    /// app-data, while warm-boot used `colocated: true`, opening an empty
    /// `.trusty-search/` dir.
    /// What: builds a colocated indexer, writes a chunk, drops the handle
    /// (releasing the redb file lock), then reloads via `build_indexer_from_entry`
    /// and asserts the chunk count is > 0.
    /// Test: this IS the test; exercises the exact call path used by the fixed
    /// `create_index_handler`.
    #[tokio::test]
    async fn colocated_create_handler_path_survives_simulated_reload() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let embedder = mock_embedder();

        // --- Phase 1: simulate what the fixed create_index_handler does ---
        let entry = PersistedIndex {
            id: "test-idx-483".to_string(),
            root_path: root.clone(),
            colocated: true,
            ..Default::default()
        };
        // build_indexer_from_entry (the fixed call path): colocated_storage_dir
        // is invoked via corpus_redb_path_for_entry → colocated_redb_path →
        // colocated_storage_dir, creating `.trusty-search/` on disk.
        let indexer = build_indexer_from_entry(&entry, &embedder).await;

        // The corpus store must be wired and pointing into `.trusty-search/`.
        assert!(
            indexer.has_corpus_store(),
            "#483: indexer must have a corpus store after build_indexer_from_entry with colocated=true"
        );
        // `.trusty-search/` must exist so the write-path probes see it.
        assert!(
            colocated_storage::has_colocated_storage(&root),
            "#483: .trusty-search/ must exist after build_indexer_from_entry with colocated=true"
        );

        // Write a chunk to the wired corpus so the reload can verify it.
        // Scope the write so the corpus Arc is dropped before we re-open
        // the same redb file for the simulated restart (redb uses file locking
        // and rejects a second concurrent open of the same database file).
        {
            let corpus = indexer.corpus_store().expect("corpus store must be set");
            corpus
                .upsert_chunks(&[minimal_raw_chunk("src/lib.rs:1:5")])
                .expect("upsert must succeed");
            // corpus Arc dropped here.
        }
        // Drop the whole indexer (and its internal corpus Arc) before reopening.
        drop(indexer);

        // --- Phase 2: simulate daemon restart — reload from the same entry ---
        // This is what `restore_indexes` does on startup.
        let reloaded = build_indexer_from_entry(&entry, &embedder).await;
        assert!(
            reloaded.has_corpus_store(),
            "#483: reloaded indexer must have a corpus store"
        );
        let chunk_count = reloaded
            .corpus_store()
            .expect("corpus store must be set")
            .chunk_count()
            .expect("chunk_count must succeed");
        assert!(
            chunk_count > 0,
            "#483: reloaded index must contain the written chunk (got {chunk_count}); \
             writer/loader paths must agree on the colocated location"
        );
    }

    // ── Issue #485 regression ─────────────────────────────────────────────────

    /// Why: guards the "cannot write schema_version: no durable corpus" error
    /// (#485) that occurred when the corpus store was not wired for a colocated
    /// entry.
    /// What: builds an indexer via the colocated entry path and asserts both
    /// `has_corpus_store` and that the corpus path is inside the project root.
    /// Test: this IS the test.
    #[tokio::test]
    async fn colocated_create_path_wires_corpus_store_for_schema_version() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let embedder = mock_embedder();

        let entry = PersistedIndex {
            id: "test-idx-485".to_string(),
            root_path: root.clone(),
            colocated: true,
            ..Default::default()
        };
        let indexer = build_indexer_from_entry(&entry, &embedder).await;

        // Corpus store must be present — this is the prerequisite that prevents
        // the "cannot write schema_version: no durable corpus" error (#485).
        assert!(
            indexer.has_corpus_store(),
            "#485: indexer built via colocated create path must have corpus store; \
             without it write_schema_version returns 'no durable corpus'"
        );

        // Also confirm the store is backed by the colocated path, not app-data.
        if let Some(corpus) = indexer.corpus_store() {
            let corpus_path = corpus.path().to_path_buf();
            assert!(
                corpus_path.starts_with(&root),
                "#485: corpus store must be inside the project root (colocated); \
                 got {corpus_path:?}"
            );
        }
    }

    // ── Guard: legacy colocated=false path is unchanged ───────────────────────

    /// Why: the fix must not break legacy (`colocated: false`) indexes.
    /// What: builds with `colocated: false`; asserts no panic even when no
    /// app-data corpus exists.
    /// Test: this IS the test.
    #[tokio::test]
    async fn legacy_non_colocated_path_does_not_panic() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let embedder = mock_embedder();

        let entry = PersistedIndex {
            id: "test-idx-legacy".to_string(),
            root_path: root.clone(),
            colocated: false,
            ..Default::default()
        };
        // Must not panic even when no app-data corpus exists.
        let _indexer = build_indexer_from_entry(&entry, &embedder).await;
        // Not asserting has_corpus_store here: app-data path may or may not
        // resolve in a test environment. The key invariant is no panic.
    }
}
