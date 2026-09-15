//! `trusty-search` migration steps, registered against the shared
//! [`trusty_common::migrations`] kernel (issue #179).
//!
//! Why: historically the warm-boot path in `service::persistence_loader`
//! open-coded a "try redb; if empty, read JSON; if non-empty, migrate JSON →
//! redb" cascade. The branching was correct but it was the *only* migration
//! the workspace had, drifting away from any reusable shape. Now that
//! `trusty-common` exposes `Migration` / `MigrationRunner`, this module is
//! the dispatch table — every new schema migration for trusty-search adds a
//! step here, and the runner orchestrates the order + stamps the schema
//! version file.
//!
//! What: defines [`JsonCorpusToRedbMigration`] (UNVERSIONED → v1), which is
//! the canonical entry point for the legacy `chunks.json` → `index.redb`
//! transfer. The migration body is a thin sync wrapper around the existing
//! async [`crate::core::indexer::CodeIndexer::load_chunks_from_disk`] and
//! [`crate::core::indexer::CodeIndexer::migrate_corpus_to_redb`] helpers —
//! the imperative logic is unchanged; only the orchestration moves under the
//! runner.
//!
//! Test: covered indirectly by the existing `service::persistence_loader`
//! integration tests; the runner contract itself is unit-tested in
//! `trusty-common::migrations`.

use anyhow::Result;

use trusty_common::migrations::{Migration, SchemaVersion};

use crate::core::indexer::CodeIndexer;
use crate::service::persistence;

/// Target schema version once every registered trusty-search migration has
/// been applied.
///
/// Why: lets the persistence loader log "current vs. target" and lets tests
/// assert the on-disk stamp converged to the expected value. Bump this
/// whenever a new migration step is added.
/// What: an alias for `SchemaVersion(1)` — Phase 1 introduces exactly one
/// step (JSON → redb).
/// Test: covered by the runner-target assertions in
/// `trusty-common::migrations::tests` (any new migration here should add an
/// equivalent assertion in `core::indexer::tests`).
pub const TRUSTY_SEARCH_SCHEMA_TARGET: SchemaVersion = SchemaVersion(1);

/// One-time migration that copies a legacy `chunks.json` snapshot into the
/// redb corpus store (issue #28, registered with the kernel under #179).
///
/// Why: daemons that booted on a pre-#28 build accumulated a `chunks.json`
/// snapshot next to an empty `index.redb`. The runner makes this the first
/// (and currently only) registered migration — a fresh install lands on the
/// "empty redb" branch directly and stamps schema v1 without doing any work,
/// whereas an upgraded install lands on the "load JSON → seed redb" branch.
/// What: a unit struct whose `apply` body bridges the sync runner into the
/// existing async migration helpers via `tokio::task::block_in_place` +
/// `Handle::current().block_on` — the trusty-search daemon always runs on
/// the multi-threaded tokio runtime (`#[tokio::main]` default) so the
/// bridge is safe. Reads the JSON snapshot through
/// [`CodeIndexer::load_chunks_from_disk`] (which restores BM25 + symbol
/// graph as a side effect) and then seeds redb via
/// [`CodeIndexer::migrate_corpus_to_redb`]. A missing or empty JSON file is
/// the genuine first-boot case and yields `Ok(())` — the stamp still moves
/// to v1 so subsequent boots skip this step entirely. #7923: a corrupt
/// snapshot, no wired corpus store, or a failed or short redb write yields
/// `Err`, so the stamp stays put and the next boot retries.
/// Test: end-to-end coverage lives in
/// `service::persistence_loader`-driven integration tests; the runner-skip
/// semantics are unit-tested in `trusty-common::migrations`.
pub struct JsonCorpusToRedbMigration;

impl Migration<CodeIndexer> for JsonCorpusToRedbMigration {
    fn from_version(&self) -> SchemaVersion {
        SchemaVersion::UNVERSIONED
    }

    fn label(&self) -> &'static str {
        "chunks.json → index.redb"
    }

    fn apply(&self, indexer: &CodeIndexer) -> Result<()> {
        let index_id = indexer.index_id.clone();
        let chunks_path = match persistence::chunks_path(&index_id) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "migrations: cannot resolve chunks.json path for '{index_id}' ({e}) — \
                     skipping legacy JSON load"
                );
                return Ok(());
            }
        };

        // Bridge the sync runner into the existing async migration helpers.
        // The production daemon runs under the multi-threaded tokio runtime
        // (`#[tokio::main]` default), so `block_in_place` is permitted. Some
        // unit tests, however, spin up a current-thread runtime — for those
        // we spawn a fresh worker thread that hosts its own block_on call so
        // we never deadlock the calling runtime.
        let handle = tokio::runtime::Handle::current();
        match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                // We can't `block_in_place` on a current-thread runtime; the
                // safe pattern is to hand the async work off to a fresh
                // single-thread runtime hosted on a worker thread, so the
                // calling runtime stays free to drive other futures.
                run_migration_off_thread(indexer, &chunks_path)
            }
            _ => tokio::task::block_in_place(|| {
                handle.block_on(run_migration_async(indexer, &chunks_path))
            }),
        }
    }
}

/// Core async migration body: load JSON snapshot, then seed redb.
///
/// Why: extracted into its own function so the two runtime-flavour bridges
/// (`block_in_place` for the multi-threaded path, off-thread runtime for
/// the current-thread path) can share one implementation.
/// What: returns `Ok(())` on success (including the "no JSON file" fresh
/// install case). #7923: returns `Err` — so the runner does not stamp v1 and
/// the next boot retries — when no corpus store is wired, the snapshot is
/// unreadable or corrupt, or the redb write fails or comes up short. The
/// snapshot file is never modified.
/// Test: `tests::json_migration_seeds_every_unique_chunk_and_reports_duplicate_ids`,
/// `json_migration_fails_on_corrupt_snapshot_and_keeps_it`,
/// `json_migration_fails_without_a_corpus_store_and_keeps_snapshot`.
async fn run_migration_async(indexer: &CodeIndexer, chunks_path: &std::path::Path) -> Result<()> {
    // #7923: "success" with no store to seed stamped v1, so the snapshot was
    // never read again once the store did open.
    anyhow::ensure!(
        indexer.has_corpus_store(),
        "index '{}': no durable corpus store is wired (corpus_open_failed={}) — \
         leaving {} for a later boot (#7923)",
        indexer.index_id,
        indexer.corpus_open_failed,
        chunks_path.display()
    );
    // Step 1: read the JSON snapshot. Missing → nothing to do; corrupt → Err.
    let loaded = indexer.restore_chunk_snapshot(chunks_path).await?;
    if loaded.restored == 0 {
        return Ok(());
    }
    tracing::info!(
        "migrations: '{}' loaded {} chunks from legacy {} ({} duplicate-id entries \
         folded) — seeding redb",
        indexer.index_id,
        loaded.restored,
        chunks_path.display(),
        loaded.duplicate_ids
    );
    // Step 2: seed redb; a failed or short write fails the step (#7923).
    indexer.migrate_corpus_to_redb().await?;
    Ok(())
}

/// Run the async migration body on a fresh single-thread runtime hosted on
/// a brand-new worker thread.
///
/// Why: when the caller is a current-thread tokio runtime,
/// `block_in_place` panics. Spawning a dedicated thread with its own
/// runtime lets the migration run synchronously from the caller's
/// perspective without nesting runtimes. The caller's runtime stays free
/// to drive other futures while this thread blocks.
/// What: spawns an OS thread, builds a `current_thread` runtime inside it,
/// drives [`run_migration_async`] to completion, and joins the thread.
/// Any error from the migration body or the thread join is returned to
/// the caller.
/// Test: exercised by the trusty-search lib tests that build the
/// persistence loader on a current-thread runtime
/// (`create_index_accepts_valid_absolute_root_path` et al.).
fn run_migration_off_thread(indexer: &CodeIndexer, chunks_path: &std::path::Path) -> Result<()> {
    // The migration body needs `&CodeIndexer` and `&Path` — both Send +
    // Sync. We can borrow across the thread boundary via `std::thread::scope`
    // so no `'static` clones are required.
    std::thread::scope(|s| {
        let handle = s.spawn(|| -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)?;
            rt.block_on(run_migration_async(indexer, chunks_path))
        });
        match handle.join() {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("migration worker thread panicked")),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunker::{ChunkType, RawChunk};
    use crate::core::corpus::CorpusStore;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn chunk(id: &str, content: &str) -> RawChunk {
        RawChunk {
            id: id.to_string(),
            file: format!("src/{id}.rs"),
            start_line: 1,
            end_line: 2,
            content: content.to_string(),
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

    fn write_snapshot(path: &std::path::Path, chunks: &[RawChunk]) {
        let json = serde_json::json!({ "version": 1, "chunks": chunks, "entities": [] });
        std::fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
    }

    fn indexer_with_corpus(dir: &std::path::Path) -> (CodeIndexer, Arc<CorpusStore>) {
        let corpus = Arc::new(CorpusStore::open(&dir.join("index.redb")).unwrap());
        let mut indexer = CodeIndexer::new("json-7923", dir);
        indexer.set_corpus_store(Arc::clone(&corpus));
        (indexer, corpus)
    }

    /// #7923: every distinct chunk lands in redb, asserted by count and id set;
    /// a repeated id is reported, not folded away silently.
    #[tokio::test]
    async fn json_migration_seeds_every_unique_chunk_and_reports_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        let chunks = [
            chunk("a", "fn a() {}"),
            chunk("b", "fn b() {}"),
            chunk("c", "fn c() {}"),
            chunk("d", "fn d() {}"),
            chunk("a", "fn a_again() {}"),
        ];
        write_snapshot(&path, &chunks);
        let before = std::fs::read(&path).unwrap();

        let (indexer, corpus) = indexer_with_corpus(dir.path());
        run_migration_async(&indexer, &path)
            .await
            .expect("migration");

        let ids: BTreeSet<String> = corpus
            .load_all_chunks()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        let expected: BTreeSet<String> =
            ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(corpus.chunk_count().unwrap(), 4);
        assert_eq!(ids, expected);
        assert_eq!(std::fs::read(&path).unwrap(), before, "snapshot untouched");

        let probe = CodeIndexer::new("probe-7923", dir.path());
        let report = probe.restore_chunk_snapshot(&path).await.unwrap();
        assert_eq!(
            report,
            crate::core::indexer::SnapshotRestore {
                restored: 4,
                duplicate_ids: 1
            }
        );
    }

    /// Error arm: a corrupt snapshot fails the step (no v1 stamp) and survives.
    #[tokio::test]
    async fn json_migration_fails_on_corrupt_snapshot_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        std::fs::write(&path, br#"{"version":1,"chunks":[{"id":"#).unwrap();
        let before = std::fs::read(&path).unwrap();

        let (indexer, corpus) = indexer_with_corpus(dir.path());
        let result = run_migration_async(&indexer, &path).await;
        assert!(
            result.is_err(),
            "#7923: a corrupt snapshot must not report success"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(corpus.chunk_count().unwrap(), 0);
    }

    /// Error arm: with no store to seed, the step fails instead of stamping v1.
    #[tokio::test]
    async fn json_migration_fails_without_a_corpus_store_and_keeps_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        write_snapshot(&path, &[chunk("a", "fn a() {}")]);
        let before = std::fs::read(&path).unwrap();

        let indexer = CodeIndexer::new("no-corpus-7923", dir.path());
        let result = run_migration_async(&indexer, &path).await;
        assert!(
            result.is_err(),
            "#7923: no corpus store must not report success"
        );
        assert_eq!(
            indexer.chunk_count(),
            0,
            "nothing loaded into a store-less index"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
