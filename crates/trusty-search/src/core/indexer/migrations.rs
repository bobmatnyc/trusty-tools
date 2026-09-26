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

use anyhow::{Context, Result};

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
/// `Err`, so the stamp stays put and the next boot retries. #8134: a durable
/// corpus that already holds rows is never written over, and a snapshot that
/// seeded redb is renamed to `chunks.json.migrated`, so an index emptied later
/// does not re-import it on every boot.
/// Test: `tests/migration_zero_row_8134.rs`
/// (`a_seeded_snapshot_is_retired_so_an_emptied_corpus_stays_empty`,
/// `a_v1_stamp_never_imports_over_rows_the_load_could_not_decode`); the
/// runner-skip semantics are unit-tested in `trusty-common::migrations`.
pub struct JsonCorpusToRedbMigration;

impl Migration<CodeIndexer> for JsonCorpusToRedbMigration {
    fn from_version(&self) -> SchemaVersion {
        SchemaVersion::UNVERSIONED
    }

    fn label(&self) -> &'static str {
        "chunks.json → index.redb"
    }

    fn apply(&self, indexer: &CodeIndexer) -> Result<()> {
        // #8134: read the snapshot where THIS index's artifacts live, not only
        // where the global layout would have put it.
        let Some(chunks_path) = legacy_snapshot_source(indexer) else {
            return Ok(());
        };

        // Bridge the sync runner into the existing async migration helpers.
        // The production daemon runs under the multi-threaded tokio runtime
        // (`#[tokio::main]` default), so `block_in_place` is permitted. Some
        // unit tests, however, spin up a current-thread runtime — for those
        // we spawn a fresh worker thread that hosts its own block_on call so
        // we never deadlock the calling runtime.
        let handle = tokio::runtime::Handle::current();
        let seeded = match handle.runtime_flavor() {
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
        }?;
        // #8134: only a snapshot that seeded redb is retired; every failure
        // above has already returned with the file untouched (#7923).
        if seeded > 0 {
            retire_snapshot(&chunks_path)?;
        }
        Ok(())
    }
}

/// Rename a snapshot that redb supersedes to `<name>.migrated` (#8134).
///
/// Why: nothing refreshes a colocated `chunks.json`. Left in place, an index
/// emptied later by a reindex re-imported the old snapshot on every restart.
/// What: renames in place. Called after the snapshot seeded redb, and at warm
/// boot for a snapshot left beside an already-populated redb. A failed rename
/// is `Err` naming both paths, so the caller records a fault instead of
/// leaving a live snapshot unreported.
/// Test: `a_seeded_snapshot_is_retired_so_an_emptied_corpus_stays_empty`,
/// `a_stale_snapshot_beside_a_populated_corpus_is_retired`,
/// `a_failed_retire_beside_a_populated_corpus_is_a_fault_and_changes_nothing`.
pub(crate) fn retire_snapshot(chunks_path: &std::path::Path) -> Result<()> {
    let mut name = chunks_path.file_name().unwrap_or_default().to_os_string();
    name.push(".migrated");
    let retired = chunks_path.with_file_name(name);
    std::fs::rename(chunks_path, &retired).with_context(|| {
        format!(
            "legacy snapshot {} is superseded by redb but could not be renamed to {}; \
             while it stays, an emptied corpus re-imports it — remove or rename it to clear this \
             fault (#8134)",
            chunks_path.display(),
            retired.display()
        )
    })?;
    tracing::info!(
        "migrations: retired legacy snapshot {} -> {}",
        chunks_path.display(),
        retired.display()
    );
    Ok(())
}

/// Resolve the legacy `chunks.json` that belongs to THIS index (#8134).
///
/// Why: `persistence::chunks_path` names the GLOBAL data dir and nothing else.
/// A colocated index (issue #403) keeps every other artifact — `index.redb`,
/// `hnsw.usearch`, `schema_version.json` — under `<root>/.trusty-search/`, and
/// its `chunks.json` sits there too; reading only the global path stamped v1
/// over a populated snapshot that was then never read again. The probe is
/// gated on the registry layout (#8438): a `DataDir` index sharing a root with
/// a colocated one must never import that index's chunks.
/// What: `Colocated` → `<root>/.trusty-search/chunks.json` when it exists as a
/// file, else the global path; `DataDir` → the global path only. `None` when
/// no candidate exists — the real first boot — or when the global path cannot
/// be resolved.
/// Test: `colocated_snapshot_is_the_resolved_source`,
/// `legacy_global_snapshot_is_still_resolved_and_absence_is_none`,
/// `data_dir_index_ignores_a_foreign_colocated_snapshot`.
pub(crate) fn legacy_snapshot_source(indexer: &CodeIndexer) -> Option<std::path::PathBuf> {
    use crate::service::storage_layout::StorageLayout;
    if indexer.storage_layout() == StorageLayout::Colocated {
        let colocated =
            crate::service::colocated_storage::colocated_chunks_path(&indexer.root_path);
        if colocated.is_file() {
            return Some(colocated);
        }
    }
    match persistence::chunks_path(&indexer.index_id) {
        Ok(p) if p.is_file() => Some(p),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                "migrations: cannot resolve chunks.json path for '{}' ({e}) — \
                 skipping legacy JSON load",
                indexer.index_id
            );
            None
        }
    }
}

/// Core async migration body: load JSON snapshot, then seed redb.
///
/// Why: extracted into its own function so the two runtime-flavour bridges
/// (`block_in_place` for the multi-threaded path, off-thread runtime for
/// the current-thread path) can share one implementation.
/// What: returns the number of chunks seeded (0 for an empty snapshot). #7923:
/// returns `Err` — so the runner does not stamp v1 and the next boot retries —
/// when no corpus store is wired, the snapshot is unreadable or corrupt, or
/// the redb write fails or comes up short. #8134: also `Err`, before anything
/// is read into memory, when the durable corpus already holds rows or its
/// count cannot be read. The snapshot file is never modified here.
/// Test: `tests::json_migration_seeds_every_unique_chunk_and_reports_duplicate_ids`,
/// `json_migration_fails_on_corrupt_snapshot_and_keeps_it`,
/// `json_migration_fails_without_a_corpus_store_and_keeps_snapshot`,
/// `json_migration_refuses_a_populated_durable_corpus`.
async fn run_migration_async(
    indexer: &CodeIndexer,
    chunks_path: &std::path::Path,
) -> Result<usize> {
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
    // #8134: never import over rows the load did not restore. Checked before
    // the read, so a refusal leaves nothing stale in memory either.
    ensure_durable_corpus_empty(indexer, chunks_path)?;
    // Step 1: read the JSON snapshot. Missing → nothing to do; corrupt → Err.
    let loaded = indexer
        .restore_chunk_snapshot(chunks_path)
        .await
        .with_context(|| {
            format!(
                "legacy snapshot {} could not be imported — remove or rename it to \
                 clear this fault (#8134)",
                chunks_path.display()
            )
        })?;
    if loaded.restored == 0 {
        return Ok(0);
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
    indexer.migrate_corpus_to_redb().await
}

/// Refuse the import unless the durable corpus holds no rows (#8134).
///
/// Why: a corpus whose rows this build cannot decode loads as 0 chunks, and
/// the snapshot's `path:start:end` ids would overwrite those rows.
/// What: `Err` naming the snapshot when the row count is non-zero or cannot be
/// read; `Ok` when the store is empty.
/// Test: `json_migration_refuses_a_populated_durable_corpus`.
fn ensure_durable_corpus_empty(indexer: &CodeIndexer, chunks_path: &std::path::Path) -> Result<()> {
    let corpus = indexer
        .corpus_store()
        .context("no durable corpus store is wired")?;
    let rows = corpus.chunk_count().with_context(|| {
        format!(
            "index '{}': cannot count the durable corpus rows — refusing to import \
             legacy snapshot {} (#8134)",
            indexer.index_id,
            chunks_path.display()
        )
    })?;
    anyhow::ensure!(
        rows == 0,
        "index '{}': the durable corpus holds {rows} chunk rows that the load did not \
         restore — refusing to import legacy snapshot {} over them (#8134). Reindex to \
         rebuild the corpus, or remove or rename the snapshot to clear this fault.",
        indexer.index_id,
        chunks_path.display()
    );
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
fn run_migration_off_thread(indexer: &CodeIndexer, chunks_path: &std::path::Path) -> Result<usize> {
    // The migration body needs `&CodeIndexer` and `&Path` — both Send +
    // Sync. We can borrow across the thread boundary via `std::thread::scope`
    // so no `'static` clones are required.
    std::thread::scope(|s| {
        let handle = s.spawn(|| -> Result<usize> {
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
                duplicate_ids: 1,
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

    /// #7923: `migrate_corpus_to_redb` itself refuses when no store is wired,
    /// independent of the guard `run_migration_async` applies first.
    #[tokio::test]
    async fn migrate_corpus_to_redb_without_a_store_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        write_snapshot(&path, &[chunk("a", "fn a() {}")]);
        let indexer = CodeIndexer::new("no-store-direct-7923", dir.path());
        assert_eq!(indexer.load_chunks_from_disk(&path).await.unwrap(), 1);

        let err = indexer
            .migrate_corpus_to_redb()
            .await
            .expect_err("no store must be an error, not a silent return");
        assert!(
            err.to_string().contains("no durable corpus store"),
            "{err:#}"
        );
        assert_eq!(indexer.chunk_count(), 1, "the in-memory corpus stays live");
    }

    /// Content of the durable row `id`.
    fn durable_content(corpus: &CorpusStore, id: &str) -> Option<String> {
        corpus
            .get_chunks(&[id])
            .unwrap()
            .into_iter()
            .next()
            .map(|c| c.content)
    }

    /// Why (#8134): the snapshot's ids overwrite any durable row they share.
    /// What: a store holding a current `a` and a snapshot carrying a stale `a`
    /// — the import is refused before the read, so nothing reaches memory, the
    /// row keeps its content, and the snapshot is untouched.
    /// Test: this test.
    #[tokio::test]
    async fn json_migration_refuses_a_populated_durable_corpus() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        write_snapshot(&path, &[chunk("a", "fn stale() {}")]);
        let before = std::fs::read(&path).unwrap();
        let (indexer, corpus) = indexer_with_corpus(dir.path());
        corpus
            .upsert_batch(&[chunk("a", "fn current() {}")], &[])
            .unwrap();

        let err = run_migration_async(&indexer, &path)
            .await
            .expect_err("#8134: a populated store must refuse the import");
        assert!(err.to_string().contains("refusing to import"), "{err:#}");
        assert_eq!(indexer.chunk_count(), 0, "nothing stale reaches memory");
        assert_eq!(
            durable_content(&corpus, "a").as_deref(),
            Some("fn current() {}")
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "snapshot untouched");
    }

    /// #8134: `migrate_corpus_to_redb` re-reads the durable count at the write
    /// and refuses to overwrite a populated store, independent of the guard
    /// `run_migration_async` applies before the read.
    #[tokio::test]
    async fn migrate_corpus_to_redb_never_overwrites_a_populated_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.json");
        write_snapshot(&path, &[chunk("a", "fn stale() {}")]);
        let (indexer, corpus) = indexer_with_corpus(dir.path());
        assert_eq!(indexer.load_chunks_from_disk(&path).await.unwrap(), 1);
        corpus
            .upsert_batch(&[chunk("a", "fn current() {}")], &[])
            .unwrap();

        let err = indexer
            .migrate_corpus_to_redb()
            .await
            .expect_err("#8134: a populated store must not be overwritten");
        assert!(format!("{err:#}").contains("refusing to"), "{err:#}");
        assert_eq!(
            durable_content(&corpus, "a").as_deref(),
            Some("fn current() {}")
        );
    }

    // ── #8134: the snapshot source follows the registry layout ───────────────

    /// Why (#8134): a colocated index keeps its `chunks.json` beside
    /// `index.redb`, and the migration only ever looked in the global data dir.
    /// What: with a snapshot under `<root>/.trusty-search/`, that file is the
    /// resolved source.
    /// Test: this test.
    #[test]
    fn colocated_snapshot_is_the_resolved_source() {
        let dir = tempfile::tempdir().unwrap();
        let colocated = dir.path().join(".trusty-search");
        std::fs::create_dir_all(&colocated).unwrap();
        let path = colocated.join("chunks.json");
        write_snapshot(&path, &[chunk("a", "fn a() {}")]);

        let indexer = CodeIndexer::new("colocated-8134", dir.path())
            .with_storage_layout(crate::service::storage_layout::StorageLayout::Colocated);
        assert_eq!(legacy_snapshot_source(&indexer).as_deref(), Some(&*path));
    }

    /// Restore `TRUSTY_DATA_DIR` when the test that pinned it ends.
    ///
    /// Why: the variable is process-global, so a test that leaves it pointing
    /// at a deleted temp dir breaks every later test in this binary. Mirrors
    /// the guard `tests::snapshot_guard_7920` uses for the same variable.
    struct RestoreDataDir(Option<std::ffi::OsString>);

    impl Drop for RestoreDataDir {
        fn drop(&mut self) {
            // SAFETY: dropped inside the same #[serial] span that set it.
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("TRUSTY_DATA_DIR", v) },
                None => unsafe { std::env::remove_var("TRUSTY_DATA_DIR") },
            }
        }
    }

    /// Why (#8134): the colocated probe must not cost a legacy index its own
    /// snapshot — the global path stays the fallback.
    /// What: with no colocated file, resolution falls through to
    /// `persistence::chunks_path`, and `None` only when neither exists.
    /// Test: this test covers both, under a pinned `TRUSTY_DATA_DIR`.
    #[test]
    #[serial_test::serial]
    fn legacy_global_snapshot_is_still_resolved_and_absence_is_none() {
        let data_dir = tempfile::tempdir().unwrap();
        // Declared after `data_dir`, so the variable is restored before the
        // directory it names is removed.
        let _restore = RestoreDataDir(std::env::var_os("TRUSTY_DATA_DIR"));
        // SAFETY: #[serial] excludes every other #[serial] test for this span.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };

        let root = tempfile::tempdir().unwrap();
        let indexer = CodeIndexer::new("global-8134", root.path());
        assert_eq!(
            legacy_snapshot_source(&indexer),
            None,
            "no snapshot in either location resolves to None — the real first boot"
        );

        let global = persistence::chunks_path("global-8134").unwrap();
        write_snapshot(&global, &[chunk("a", "fn a() {}")]);
        assert_eq!(legacy_snapshot_source(&indexer).as_deref(), Some(&*global));
    }

    /// Why (#8134, #8438): the registry decides layout, not the disk. A
    /// `DataDir` index sharing its root with a colocated index must not adopt
    /// that index's `<root>/.trusty-search/chunks.json` — the import would be
    /// stamped v1 and the contamination made permanent.
    /// What: a `DataDir` indexer whose root holds a foreign colocated snapshot
    /// and whose own global path holds nothing resolves to `None`.
    /// Test: this test; the end-to-end "imports nothing" arm is
    /// `data_dir_index_does_not_import_a_colocated_neighbours_snapshot`.
    #[test]
    #[serial_test::serial]
    fn data_dir_index_ignores_a_foreign_colocated_snapshot() {
        let data_dir = tempfile::tempdir().unwrap();
        let _restore = RestoreDataDir(std::env::var_os("TRUSTY_DATA_DIR"));
        // SAFETY: #[serial] excludes every other #[serial] test for this span.
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data_dir.path()) };

        let root = tempfile::tempdir().unwrap();
        let foreign = root.path().join(".trusty-search");
        std::fs::create_dir_all(&foreign).unwrap();
        write_snapshot(&foreign.join("chunks.json"), &[chunk("a", "fn a() {}")]);

        let indexer = CodeIndexer::new("docs-8134", root.path())
            .with_storage_layout(crate::service::storage_layout::StorageLayout::DataDir);
        assert_eq!(
            legacy_snapshot_source(&indexer),
            None,
            "#8438: a DataDir index must never read the colocated neighbour's snapshot"
        );
    }

    /// #7923: a store holding fewer rows than were migrated fails the step.
    #[test]
    fn ensure_all_migrated_rejects_a_short_write() {
        use crate::core::indexer::persist::ensure_all_migrated;
        assert!(ensure_all_migrated("x", 4, 4).is_ok());
        assert!(ensure_all_migrated("x", 4, 5).is_ok());
        let err = ensure_all_migrated("x", 4, 3).expect_err("short write");
        assert!(err.to_string().contains("1 missing"), "{err:#}");
    }
}
