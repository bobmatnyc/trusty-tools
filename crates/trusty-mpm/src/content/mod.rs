//! Agent/skill catalog synchronization from the claude-mpm repository.
//!
//! Why: re-porting ~40 agents and ~25 skills by hand from the claude-mpm
//! repository would immediately diverge; syncing from the remote source of
//! truth keeps the catalog current without manual work.
//! What: re-exports CatalogSync, CatalogError, CatalogSyncResult from
//! catalog_sync so callers import from one stable path.
//! Test: catalog_sync.rs carries unit tests with a FakeGitBackend.

pub mod catalog_sync;

pub use catalog_sync::{CatalogError, CatalogSync, CatalogSyncResult, catalog_root_for};
