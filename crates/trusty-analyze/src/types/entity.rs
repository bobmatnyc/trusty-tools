//! Entity taxonomy and edge kinds shared between search and analyzer.
//!
//! Why: trusty-search-core, trusty-analyzer-core, and ingest pipelines all
//! consume the same `EntityType` / `EdgeKind` / `RawEntity` shapes. The shared
//! `trusty-symgraph` crate owns the canonical definitions (no tree-sitter
//! dep) and this module simply re-exports them so existing callers continue to
//! work via `crate::types::{EntityType, EdgeKind, RawEntity}`.
//!
//! What: pure re-exports from `trusty_symgraph`. No local definitions.
//!
//! Test: none here; the types are tested where trusty-common defines them.

pub use trusty_common::symgraph::contracts::EdgeKind;
pub use trusty_common::symgraph::{fact_hash_str, EntityType, RawEntity};

/// redb table name constants for entity storage, re-exported from
/// `trusty_common::symgraph::contracts::tables`.
pub mod tables {
    pub use trusty_common::symgraph::contracts::tables::*;
}
