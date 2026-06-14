//! redb-backed storage engine for the temporal knowledge graph.
//!
//! Why: The KG previously rode on rusqlite + r2d2, which carries a heavy native
//! dependency chain and a 30s default connect timeout that stalls daemon
//! startup when a palace's `kg.db` is corrupt. redb is a pure-Rust embedded
//! transactional k/v store with O(log n) range scans and no native deps.
//! Issue #44 swaps the internals; #47 will retire the sqlite code path.
//! What: `KgStoreRedb` wraps `redb::Database` and implements every method that
//! `KnowledgeGraph` exposes — assert/retract/query/list for triples, and
//! upsert/load/delete for drawers. Composite key encodings, table definitions,
//! and value codecs live in `kg_store.rs`.
//! Test: See `tests` module — round-trips, retract semantics, persistence
//! across reopen, drawer CRUD, count_active.

mod import;
mod read_ops;
mod store;
mod tests;
mod types;
mod write_ops;

pub use store::KgStoreRedb;
pub use types::{BatchOpResult, BatchWriteOp, READ_ONLY_ERROR_MSG};

// Re-export the core domain types so tests (via `use super::*`) and
// downstream callers can access them without reaching through multiple
// module paths.
pub use crate::memory_core::palace::Drawer;
pub use crate::memory_core::store::kg::Triple;
