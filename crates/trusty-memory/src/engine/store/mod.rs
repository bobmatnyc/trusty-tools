//! Storage backends: vector index (HNSW) + temporal knowledge graph (redb).
//!
//! Why: Two complementary data shapes — dense vectors for semantic recall and
//! triples-with-time for relational facts — covered by separate modules so each
//! can evolve independently.
//! What: Re-exports `VectorStore` trait and `KnowledgeGraph` type.
//! Test: See submodule tests.

pub mod chat_sessions;
pub mod concurrent_open;
// #4906: durable record of drawers whose embed permanently failed.
pub mod embed_ledger;
pub mod format_backup;
// #9274: the palace format N-1 -> N migration (1 -> 2 since #9275), run at a writer-intent open.
pub(crate) mod format_migration;
// #9274: one format migration of a palace at a time, in-process and across processes.
pub(crate) mod format_migration_lock;
// #9274: a format-0 palace written from the old shapes, for consumer tests.
#[cfg(any(test, feature = "memory-core-test-support"))]
pub mod format_v0_fixture;
// #9275: a format-1 palace as 0.33.x writes it, for consumer tests.
#[cfg(any(test, feature = "memory-core-test-support"))]
pub mod format_v1_fixture;
pub mod hnsw_store;
pub mod kg;
pub mod kg_redb;
// See #9278: storage layout, public only for an integration test.
#[doc(hidden)]
pub mod kg_store;
pub(crate) mod kg_writer;
pub(crate) mod l1_cache;
// #9659: the observer open — a palace read through a copy, never the live file.
mod observer_open;
pub mod palace_format;
pub mod palace_store;
pub(crate) mod redb_open;
// ADR-0027 T2: additive, fail-open room backfill run at palace open.
pub(crate) mod room_backfill;
// ADR-0027 T10: the read-only plan the backfill executes and `--dry-run` prints.
pub(crate) mod room_plan;
// ADR-0027 T1/T4/T6: room record shape, resolve-or-create, and the room surface.
pub mod rooms;
pub mod vector;
// #9275: `{uuid}#n` chunk-row keys — one drawer, several vectors.
pub(crate) mod vector_key;
// ADR-0027 T9: wing record shape, default-wing seeding, create/rename/list.
pub(crate) mod wings;
pub mod write_deadline;

// #9278: a store-root re-export stays public only when a caller outside the
// library names it (trusty-mpm, the integration tests); the rest are crate-only.
pub use chat_sessions::ChatSessionStore;
pub use concurrent_open::OpenIntent;
pub use kg::{KnowledgeGraph, Triple};
#[cfg(feature = "server")]
pub(crate) use l1_cache::L1Cache;
pub use palace_store::PalaceStore;
#[cfg(feature = "server")]
pub(crate) use palace_store::PalaceStoreError;
#[cfg(feature = "server")]
pub(crate) use redb_open::INCOMPATIBLE_SUFFIX;
pub(crate) use redb_open::open_or_recreate;
#[cfg(feature = "server")]
pub(crate) use room_backfill::backfill_rooms;
pub(crate) use room_plan::plan_rooms;
pub use vector::VectorStore;
