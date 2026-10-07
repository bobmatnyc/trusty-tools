//! Agent/skill catalog synchronization, and the runtime instructional-content
//! cache (ADR-0064).
//!
//! Why: re-porting ~40 agents and ~25 skills by hand from the claude-mpm
//! repository would immediately diverge; syncing from the remote source of
//! truth keeps the catalog current without manual work. Separately, ADR-0064
//! makes trusty-tools' own content runtime-only, so `tm` installs and pins it.
//! What: re-exports CatalogSync, CatalogError, CatalogSyncResult from
//! catalog_sync; `bundle_cache`, `release_source` and `status` back the
//! `tm content` commands and the doctor `content` row (#8378 PR-C);
//! `first_use` installs the release when tm finds none (#9396).
//! Test: catalog_sync.rs carries unit tests with a FakeGitBackend;
//! `bundle_cache_tests.rs` covers the content cache.

pub mod bundle_cache;
pub mod catalog_sync;
mod catalog_url;
pub mod first_use;
pub mod release_source;
pub mod status;

pub use catalog_sync::{CatalogError, CatalogSync, CatalogSyncResult, catalog_root_for};
