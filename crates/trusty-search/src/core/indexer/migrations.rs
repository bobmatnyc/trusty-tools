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
/// to v1 so subsequent boots skip this step entirely.
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
/// install case) and propagates `Err` from the underlying chunk loader.
/// Test: covered indirectly via the persistence_loader integration tests.
async fn run_migration_async(indexer: &CodeIndexer, chunks_path: &std::path::Path) -> Result<()> {
    // Step 1: read the JSON snapshot into memory (best-effort).
    // `load_chunks_from_disk` returns 0 (and `Ok`) for missing / corrupt
    // files — both are the fresh-install case.
    let restored = indexer.load_chunks_from_disk(chunks_path).await?;
    if restored == 0 {
        return Ok(());
    }
    tracing::info!(
        "migrations: '{}' loaded {restored} chunks from legacy {} — seeding redb",
        indexer.index_id,
        chunks_path.display()
    );
    // Step 2: seed redb from the now-live in-memory corpus.
    indexer.migrate_corpus_to_redb().await;
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
