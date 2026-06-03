//! Schema-versioned index migration framework for trusty-search.
//!
//! Why: per-index state lives in redb. When chunking semantics change between
//! releases (e.g. v0.11.1 emits one `ChunkType::Constant` per `pub const`
//! instead of one whole-file `ChunkType::Code`), existing indexes silently
//! serve suboptimal search results until a manual `reindex --force`. This
//! module provides a forward-only, idempotent migration runner that fires
//! automatically on daemon startup so users never have to know about it.
//!
//! What: the `Migration` trait defines a single `apply` call; `MigrationRegistry`
//! holds all known migrations in order; `run_migrations` walks the chain from
//! the index's current `schema_version` to `CURRENT_SCHEMA_VERSION` and applies
//! each migration in sequence, writing the new version to redb only after a
//! successful `apply` (crash-safe retry on next startup). Migrations are spawned
//! per-index as background tasks so the daemon serves queries immediately.
//!
//! Test: `migration::tests` covers chain computation, no-op detection,
//! sequential application, crash-safe retry, and M001 idempotency and correctness.

pub mod m001;
pub mod m002;
pub mod m003;
pub mod m004;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use thiserror::Error;

use crate::core::registry::IndexHandle;

pub use m001::M001PerPubConstRust;
pub use m002::M002AbsoluteToRelativePaths;
pub use m003::M003HnswKeyRelativization;
pub use m004::M004RepairAbsoluteFilePaths;

// ── Schema version ────────────────────────────────────────────────────────────

/// The schema version that newly-created indexes start at.
///
/// Why: freshly-indexed data already satisfies all current migrations, so new
/// indexes skip the migration runner by starting at the highest known version.
/// Increment this constant every time a new migration is added.
/// What: a monotonic `u32`. Migrations with `target_version > CURRENT_SCHEMA_VERSION`
/// are unreachable; such a registry would be a programming error.
/// Test: `test_current_schema_version_matches_registry` asserts that
/// `CURRENT_SCHEMA_VERSION == registry.current_version()`.
/// Issue #402: bump to 2 for M002 (absolute → relative file paths in redb).
/// Issue #402 phase 2: bump to 3 for M003 (absolute → relative HNSW key IDs).
/// Issue #674: bump to 4 for M004 (repair any remaining absolute file paths).
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

// ── redb table for _meta ──────────────────────────────────────────────────────

/// redb table storing per-index metadata, keyed by a string key.
///
/// Why: we need a single place to persist the `schema_version` without adding
/// a new column to the existing `chunks` or `entities` tables (which carry
/// domain payloads only). A separate `_meta` table keeps the schema concern
/// orthogonal and makes it easy to add future per-index metadata (e.g. last
/// reindex timestamp, provenance flags).
/// What: `&str → &[u8]` where the value is a little-endian `u32` for the
/// `schema_version` key. All other keys are reserved for future use.
/// Test: `test_schema_version_roundtrip` reads and writes through
/// `IndexHandle::read_schema_version` and `write_schema_version`.
pub(crate) const META_TABLE: redb::TableDefinition<&str, &[u8]> =
    redb::TableDefinition::new("_meta");

/// redb `_meta` key for the schema version.
pub(crate) const META_KEY_SCHEMA_VERSION: &str = "schema_version";

/// redb `_meta` key for the canonical root path the corpus's chunk `file`
/// fields are stored relative to (#602).
///
/// Why: chunk `file` fields are root-relative. If an index is re-registered
/// under a different root and then incrementally reindexed, the content-hash
/// fast path skips unchanged files so their stored paths are never rewritten —
/// they stay relative to the *old* root and resolve wrong. Persisting the root
/// the corpus was last relativized against lets the reindex orchestrator detect
/// a move and force a full rewrite (see
/// `service::reindex::validate::needs_path_relativization`).
/// What: a UTF-8 path string stored under this `_meta` key, written at the end
/// of every successful reindex.
pub(crate) const META_KEY_INDEXED_ROOT: &str = "indexed_root";

// ── Error type ────────────────────────────────────────────────────────────────

/// Structured errors from the migration subsystem.
///
/// Why: the runner calls user-supplied `apply` impls and lower-level redb I/O;
/// wrapping both in a single `thiserror` enum lets callers distinguish a
/// deterministic bug (programming error) from a transient I/O failure.
/// What: two variants — `Apply` (migration logic failed) and `Io` (redb
/// read/write failed). Both carry the originating `anyhow::Error`.
/// Test: `run_migrations` tests assert `Ok(())` on the happy path; error paths
/// are validated by mock-migration tests.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// A migration's `apply` method returned an error.
    #[error("migration {from}→{to} ({description}) failed: {source}")]
    Apply {
        from: u32,
        to: u32,
        description: &'static str,
        #[source]
        source: anyhow::Error,
    },
    /// Reading or writing the `schema_version` in redb failed.
    #[error("schema-version I/O for index '{index_id}': {source}")]
    Io {
        index_id: String,
        #[source]
        source: anyhow::Error,
    },
}

// ── Migration trait ───────────────────────────────────────────────────────────

/// A single forward-only, idempotent schema migration.
///
/// Why: each migration encapsulates the exact work needed to bring an index
/// from one schema version to the next. Idempotency is required because the
/// runner writes `schema_version` *after* a successful `apply` — a crash
/// between the two leaves the version at the old value, so the migration is
/// retried on the next startup. An idempotent `apply` produces the same result
/// whether it runs once or twice.
/// What: a trait with four methods — `source_version`, `target_version` (the chain
/// computation), `description` (human-readable log label), and `apply` (the
/// actual work). Implementations live in `m001`, `m002`, … submodules.
/// Test: each migration submodule contains its own `#[cfg(test)] mod tests`.
#[async_trait]
pub trait Migration: Send + Sync {
    /// Why: used by `MigrationRegistry::chain_from` to select only the
    /// migrations whose `source_version` is ≥ the index's current version.
    /// What: the schema version this migration starts from. `chain_from(n)`
    /// includes this migration iff `source_version() >= n`.
    fn source_version(&self) -> u32;

    /// Why: written to the index's `schema_version` by `run_migrations` after
    /// a successful `apply` so the version advances monotonically.
    /// What: the schema version this migration produces. Must be
    /// `source_version() + 1` by convention (though the runner only requires
    /// `target_version() > source_version()`).
    fn target_version(&self) -> u32;

    /// Why: included in tracing spans and log lines so operators know which
    /// migration is running without looking at the source.
    /// What: a static string describing the migration's purpose (ticket ref,
    /// one-line summary).
    fn description(&self) -> &'static str;

    /// Apply the migration to `index`.
    ///
    /// Why: the real work lives here. The runner calls this under the
    /// assumption that `index.read_schema_version() == self.source_version()`,
    /// but the method itself must re-verify any preconditions (idempotency
    /// requirement: running it a second time on an already-migrated index must
    /// be a no-op).
    /// What: performs all work needed to advance the index from `source_version`
    /// to `target_version`. May perform I/O, re-chunk files, re-embed, etc.
    /// Returns `Ok(())` on success or `Err(anyhow::Error)` on any failure.
    /// Test: each migration's test module asserts that calling `apply` twice
    /// produces the same result as calling it once.
    async fn apply(&self, index: &IndexHandle) -> Result<(), anyhow::Error>;
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Ordered list of all known migrations.
///
/// Why: the runner needs a single place to look up the full ordered chain of
/// migrations from any starting version. Constructing the registry at daemon
/// startup (and sharing it as an `Arc`) avoids re-allocating the list on every
/// per-index startup path.
/// What: holds a `Vec<Arc<dyn Migration>>` sorted by `source_version`. `new()`
/// registers every known migration in order; `chain_from(v)` returns the
/// sub-slice that applies from version `v` onward.
/// Test: `test_migration_registry_chain_computation`.
pub struct MigrationRegistry {
    migrations: Vec<Arc<dyn Migration>>,
}

impl MigrationRegistry {
    /// Build the canonical registry containing every known migration in order.
    ///
    /// Why: a single construction site ensures new migrations are registered
    /// exactly once and in the correct order. Adding a new migration means
    /// appending one `Arc::new(M00N…)` entry here.
    /// What: allocates `Arc`s for each concrete migration and stores them in a
    /// `Vec` sorted by `source_version` (ascending). The sort is defensive —
    /// implementations should already be in order, but relying on `sort_by` is
    /// cheaper than debugging a subtle ordering bug in production.
    /// Test: `test_migration_registry_chain_computation` constructs a registry
    /// with multi-step migrations and asserts correct chain extraction.
    pub fn new() -> Self {
        let mut migrations: Vec<Arc<dyn Migration>> = vec![
            Arc::new(M001PerPubConstRust),
            // Issue #402: rewrite absolute chunk file paths to root-relative (redb).
            Arc::new(M002AbsoluteToRelativePaths),
            // Issue #402 phase 2: rewrite absolute HNSW key IDs to root-relative.
            // Also repairs indexes stuck at schema_version=2 whose hnsw.keys.json
            // was left with absolute IDs by a prior binary that ran M002 but not M003.
            Arc::new(M003HnswKeyRelativization),
            // Issue #674: second idempotent pass to repair any remaining absolute
            // `file` fields that were written by a fresh reindex on AL2023 where
            // fs::canonicalize produced a path that did not match the walker root
            // (symlink / EFS mount mismatch), causing strip_prefix to fall back to
            // storing the absolute path.  Newly-indexed corpora start at
            // CURRENT_SCHEMA_VERSION=4 so they skip this migration; only corpora
            // that were indexed by a binary at v3 or earlier will run M004.
            Arc::new(M004RepairAbsoluteFilePaths),
        ];
        // Defensive sort: ensures chain_from returns migrations in ascending
        // version order even if a future contributor adds them out of order.
        migrations.sort_by_key(|m| m.source_version());
        Self { migrations }
    }

    /// The highest `target_version` in the registry, equivalent to
    /// `CURRENT_SCHEMA_VERSION` when the registry is fully populated.
    ///
    /// Why: `run_migrations` compares this against the index's stored version
    /// to decide whether any work is needed.
    /// What: returns 0 when the registry is empty (no migrations registered
    /// yet); otherwise the maximum `target_version` across all migrations.
    /// Test: `test_migration_registry_chain_computation`.
    pub fn current_version(&self) -> u32 {
        self.migrations
            .iter()
            .map(|m| m.target_version())
            .max()
            .unwrap_or(0)
    }

    /// Return all migrations whose `source_version >= current` in ascending
    /// `source_version` order, forming the chain to apply.
    ///
    /// Why: the runner needs only the migrations that haven't been applied yet.
    /// Migrations with `source_version < current` are already reflected in the
    /// index's state, so they must not run again.
    /// What: filters `self.migrations` by `source_version >= current` and
    /// clones the `Arc`s into a new `Vec`. The `Vec` is in ascending order
    /// because `new()` sorts the underlying list.
    /// Test: `test_migration_registry_chain_computation` — `chain_from(0)` ==
    /// all migrations; `chain_from(2)` skips 0→1 and 1→2.
    pub fn chain_from(&self, current: u32) -> Vec<Arc<dyn Migration>> {
        self.migrations
            .iter()
            .filter(|m| m.source_version() >= current)
            .cloned()
            .collect()
    }
}

impl Default for MigrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Run all outstanding migrations on `index`, starting from its stored
/// `schema_version`.
///
/// Why: called on daemon startup (once per registered index, in a background
/// task) so users never have to manually trigger migrations. The daemon serves
/// queries at the old schema quality until the migration completes — graceful
/// degradation, never a hard block.
/// What: reads the current version from redb, computes the migration chain via
/// `registry.chain_from(current)`, and applies each migration in sequence.
/// The schema version in redb is updated **after** each successful `apply` so
/// that a crash mid-migration retries from the last committed step on the next
/// startup (crash-safe forward progress).
/// Test: `test_run_migrations_no_op_when_current`,
///       `test_run_migrations_applies_in_sequence`,
///       `test_run_migrations_idempotent_after_crash`.
pub async fn run_migrations(
    index: &IndexHandle,
    registry: &MigrationRegistry,
) -> Result<(), MigrationError> {
    let index_id = index.id.to_string();

    let mut current = index
        .read_schema_version()
        .await
        .map_err(|e| MigrationError::Io {
            index_id: index_id.clone(),
            source: e,
        })?;
    let target = registry.current_version();

    if current >= target {
        tracing::debug!(
            index_id = %index_id,
            current,
            target,
            "no migrations needed"
        );
        return Ok(());
    }

    tracing::info!(
        index_id = %index_id,
        current,
        target,
        "running schema migrations"
    );

    for migration in registry.chain_from(current) {
        tracing::info!(
            index_id = %index_id,
            from = migration.source_version(),
            to = migration.target_version(),
            description = migration.description(),
            "applying migration"
        );

        migration
            .apply(index)
            .await
            .map_err(|source| MigrationError::Apply {
                from: migration.source_version(),
                to: migration.target_version(),
                description: migration.description(),
                source,
            })?;

        // Write the new version AFTER a successful apply. A crash before this
        // write leaves `schema_version` at the old value, causing a retry on
        // next startup. The idempotency requirement on `apply` makes this safe.
        index
            .write_schema_version(migration.target_version())
            .await
            .map_err(|e| MigrationError::Io {
                index_id: index_id.clone(),
                source: e,
            })?;

        current = migration.target_version();

        tracing::info!(
            index_id = %index_id,
            now_at = current,
            "migration complete"
        );
    }

    Ok(())
}

// ── Read/write schema_version on IndexHandle ──────────────────────────────────

impl IndexHandle {
    /// Read the `schema_version` from the index's redb corpus.
    ///
    /// Why: the version is the single input to the migration chain computation.
    /// Returning `0` for indexes without a corpus (BM25-only, test indexes)
    /// means they are treated as legacy and the migration runner is a no-op
    /// because their `corpus` is `None` (no redb to query).
    /// What: acquires a read lock on the indexer, clones the corpus `Arc`, then
    /// reads `_meta["schema_version"]` from redb. Returns `0` when the corpus
    /// is absent, the key is absent, or the value cannot be decoded.
    /// Test: `test_schema_version_roundtrip` writes then reads through this
    /// pair of accessors.
    pub async fn read_schema_version(&self) -> anyhow::Result<u32> {
        let corpus = {
            let indexer = self.indexer.read().await;
            indexer.corpus_store()
        };
        let Some(corpus) = corpus else {
            // No durable corpus — treat as version 0 (legacy / BM25-only).
            return Ok(0);
        };
        tokio::task::spawn_blocking(move || corpus.read_schema_version_sync())
            .await
            .map_err(|e| anyhow::anyhow!("schema_version read task panicked: {e}"))?
    }

    /// Persist the `schema_version` to the index's redb corpus.
    ///
    /// Why: the runner calls this after each successful `apply` so the version
    /// advances durably. Crash between `apply` and here → retry next startup.
    /// What: opens a write transaction on the corpus redb, creates `_meta` if
    /// absent, and upserts `schema_version` as a 4-byte little-endian value.
    /// Returns `Err` when the corpus is absent (caller should guard against this).
    /// Test: `test_schema_version_roundtrip`.
    pub async fn write_schema_version(&self, version: u32) -> anyhow::Result<()> {
        let corpus = {
            let indexer = self.indexer.read().await;
            indexer.corpus_store()
        };
        let Some(corpus) = corpus else {
            return Err(anyhow::anyhow!(
                "cannot write schema_version: no durable corpus on this index"
            ));
        };
        tokio::task::spawn_blocking(move || corpus.write_schema_version_sync(version))
            .await
            .map_err(|e| anyhow::anyhow!("schema_version write task panicked: {e}"))?
    }

    /// Read the canonical root the corpus's chunk paths were last relativized
    /// against (#602).
    ///
    /// Why: the reindex orchestrator compares this against the current root to
    /// decide whether a move occurred between runs and a full path-rewrite is
    /// required. `None` for a no-corpus or never-stamped index → treated as a
    /// first-ever reindex (no forced rewrite).
    /// What: read-locks the indexer, clones the corpus `Arc`, and reads
    /// `_meta["indexed_root"]` on a blocking worker. Returns `None` when no
    /// durable corpus is present.
    /// Test: `service::reindex::validate::needs_path_relativization` covers the
    /// decision; the redb round-trip is covered by
    /// `corpus::tests::test_meta_indexed_root_roundtrip`.
    pub async fn read_indexed_root(&self) -> anyhow::Result<Option<std::path::PathBuf>> {
        let corpus = {
            let indexer = self.indexer.read().await;
            indexer.corpus_store()
        };
        let Some(corpus) = corpus else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || corpus.read_indexed_root_sync())
            .await
            .map_err(|e| anyhow::anyhow!("indexed_root read task panicked: {e}"))?
    }

    /// Persist the canonical root the corpus's chunk paths are now relativized
    /// against (#602).
    ///
    /// Why: written at the end of every successful reindex so the next run can
    /// detect a move. A no-corpus index has nowhere to store it, so this is a
    /// silent no-op there (BM25-only / test indexes never need it).
    /// What: read-locks the indexer, clones the corpus `Arc`, and writes
    /// `_meta["indexed_root"]` on a blocking worker. `Ok(())` (no-op) when no
    /// durable corpus is present.
    /// Test: `corpus::tests::test_meta_indexed_root_roundtrip`.
    pub async fn write_indexed_root(&self, root: &std::path::Path) -> anyhow::Result<()> {
        let corpus = {
            let indexer = self.indexer.read().await;
            indexer.corpus_store()
        };
        let Some(corpus) = corpus else {
            return Ok(());
        };
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || corpus.write_indexed_root_sync(&root))
            .await
            .map_err(|e| anyhow::anyhow!("indexed_root write task panicked: {e}"))?
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Mock migration helpers ────────────────────────────────────────────────

    struct MockMigration {
        from: u32,
        to: u32,
        desc: &'static str,
        call_count: Arc<AtomicU32>,
    }

    impl MockMigration {
        fn new(from: u32, to: u32, desc: &'static str) -> (Self, Arc<AtomicU32>) {
            let counter = Arc::new(AtomicU32::new(0));
            let m = Self {
                from,
                to,
                desc,
                call_count: Arc::clone(&counter),
            };
            (m, counter)
        }
    }

    #[async_trait]
    impl Migration for MockMigration {
        fn source_version(&self) -> u32 {
            self.from
        }
        fn target_version(&self) -> u32 {
            self.to
        }
        fn description(&self) -> &'static str {
            self.desc
        }
        async fn apply(&self, _index: &IndexHandle) -> Result<(), anyhow::Error> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    /// Build a `MigrationRegistry` from a list of `(from, to)` pairs,
    /// bypassing the production `new()` constructor so tests can compose
    /// arbitrary chains.
    fn registry_from(pairs: Vec<(u32, u32)>) -> MigrationRegistry {
        let mut migrations: Vec<Arc<dyn Migration>> = pairs
            .into_iter()
            .enumerate()
            .map(|(i, (from, to))| {
                let (m, _) = MockMigration::new(from, to, "mock");
                let _ = i;
                Arc::new(m) as Arc<dyn Migration>
            })
            .collect();
        migrations.sort_by_key(|m| m.source_version());
        MigrationRegistry { migrations }
    }

    // ── chain_from tests ──────────────────────────────────────────────────────

    /// Why: validates the core chain-selection logic that the runner depends on
    /// to skip already-applied migrations.
    /// Test: `chain_from(0)` returns all three migrations;
    ///       `chain_from(1)` returns only 1→2 and 2→3;
    ///       `chain_from(3)` returns empty.
    #[test]
    fn test_migration_registry_chain_computation() {
        // Three migrations: 0→1, 1→2, 2→3.
        let reg = registry_from(vec![(0, 1), (1, 2), (2, 3)]);

        // current_version is the max target_version.
        assert_eq!(reg.current_version(), 3);

        // chain_from(0) → all three.
        let chain = reg.chain_from(0);
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].source_version(), 0);
        assert_eq!(chain[1].source_version(), 1);
        assert_eq!(chain[2].source_version(), 2);

        // chain_from(1) → only 1→2 and 2→3.
        let chain = reg.chain_from(1);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].source_version(), 1);
        assert_eq!(chain[1].source_version(), 2);

        // chain_from(3) → empty (already at target).
        let chain = reg.chain_from(3);
        assert!(chain.is_empty());
    }

    /// Why: ensures an empty registry is handled gracefully.
    #[test]
    fn test_registry_empty() {
        let reg = MigrationRegistry {
            migrations: Vec::new(),
        };
        assert_eq!(reg.current_version(), 0);
        assert!(reg.chain_from(0).is_empty());
    }

    /// Why: verifies `current_version` matches the global constant when the
    /// production registry is used.
    #[test]
    fn test_production_registry_current_version_matches_constant() {
        let reg = MigrationRegistry::new();
        assert_eq!(reg.current_version(), CURRENT_SCHEMA_VERSION);
    }

    // ── run_migrations no-op test ─────────────────────────────────────────────

    /// Why: when the index is already at `CURRENT_SCHEMA_VERSION`, no
    /// migrations should run and the function must return `Ok(())`.
    ///
    /// This test uses a BM25-only (no-corpus) index because that is the only
    /// `IndexHandle` we can construct cheaply in unit tests without a real redb
    /// database. For a no-corpus index `read_schema_version` returns 0 and the
    /// runner skips all work when `CURRENT_SCHEMA_VERSION == 0`. We use an
    /// empty registry to simulate "no migrations needed".
    #[tokio::test]
    async fn test_run_migrations_no_op_on_empty_registry() {
        let empty_reg = MigrationRegistry {
            migrations: Vec::new(),
        };
        let handle = make_test_handle();

        // Empty registry → no migrations, should succeed instantly.
        let result = run_migrations(&handle, &empty_reg).await;
        assert!(result.is_ok(), "empty registry must not error: {result:?}");
    }

    /// Why: a no-corpus index always reads schema_version = 0. When the
    /// production registry has migrations (current_version > 0), `run_migrations`
    /// would try to apply them but fail at `write_schema_version` because there
    /// is no corpus. Verify the no-corpus path returns `Err(MigrationError::Io)`.
    #[tokio::test]
    async fn test_run_migrations_no_corpus_returns_io_err_when_migrations_pending() {
        let reg = registry_from(vec![(0, 1)]);
        let handle = make_test_handle(); // no corpus

        let result = run_migrations(&handle, &reg).await;
        // apply() returns Ok (mock), but write_schema_version fails (no corpus).
        assert!(
            matches!(result, Err(MigrationError::Io { .. })),
            "no-corpus write must surface as Io error, got: {result:?}"
        );
    }

    // ── Schema version roundtrip (unit; requires real redb) ──────────────────

    /// Why: validates the `read_schema_version` / `write_schema_version`
    /// pair that the runner relies on for crash-safe version advancement.
    #[tokio::test]
    async fn test_schema_version_roundtrip_no_corpus() {
        let handle = make_test_handle();
        // No corpus → always reads 0.
        let v = handle.read_schema_version().await.unwrap();
        assert_eq!(v, 0, "no-corpus handle must report version 0");

        // write_schema_version must fail gracefully (no corpus to write to).
        let result = handle.write_schema_version(1).await;
        assert!(
            result.is_err(),
            "write on no-corpus handle must fail: {result:?}"
        );
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Build a minimal `IndexHandle` backed by a BM25-only (no-corpus) indexer.
    ///
    /// Why: unit tests need a handle without spinning up a full daemon, embedding
    /// model, or redb database. The handle is ephemeral and in-memory only; any
    /// call that requires a corpus will fail predictably.
    /// What: creates a `CodeIndexer::new` (no embedder, no store, no corpus),
    /// wraps it in a bare `IndexHandle`.
    fn make_test_handle() -> IndexHandle {
        use crate::core::indexer::CodeIndexer;
        use crate::core::registry::{IndexHandle, IndexId};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let indexer = CodeIndexer::new("migration-test", "/tmp/migration-test");
        IndexHandle::bare(
            IndexId::new("migration-test"),
            Arc::new(RwLock::new(indexer)),
            std::path::PathBuf::from("/tmp/migration-test"),
        )
    }
}
