//! Shared types between trusty-search and trusty-analyzer.
//!
//! These are the wire-format types passed across the HTTP/MCP boundary.
//! Kept minimal and dependency-light: only serde. No business
//! logic lives here — that all sits in `trusty-analyzer-core`.
//!
//! Forward-compat: every struct uses `#[serde(default)]` on optional
//! fields and serde's default unknown-field-tolerance, so the trusty-search
//! daemon can add fields without breaking analyzer deserialization.

pub mod blame;
pub mod chunk;
pub mod complexity;
pub mod entity;
pub mod facts;
pub mod graph;

pub use blame::ChunkBlame;
pub use chunk::CodeChunk;
pub use complexity::{CodeSmell, ComplexityGrade, ComplexityMetrics, SmellThresholds};
pub use entity::{fact_hash_str, EdgeKind, EntityType, RawEntity};
pub use facts::FactRecord;
pub use graph::{KgEdge, KgEdgeKind, KgGraph, KgNode, KgNodeKind};
