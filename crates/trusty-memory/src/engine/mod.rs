//! Memory Palace core types, storage, and retrieval (formerly the
//! `trusty-memory-core` crate, then `trusty_common::memory_core`).
//!
//! Why: Centralises the Memory Palace data model and storage abstractions
//! so every binary (CLI, MCP server, embedded library) reuses the same
//! types. #9271 moved it from trusty-common into trusty-memory, its owner.
//! What: Re-exports the registry and the consolidation, share and identity
//! helpers. The palace hierarchy types (`Palace`, `Drawer`; wings and rooms
//! are palace-store rows) live at `engine::palace` and the retrieval handle at
//! `engine::retrieval::PalaceHandle`, one path per type (#9278). "Closet" is
//! not a level in that hierarchy — it is the keyword -> drawer-ids inverted
//! index on `PalaceHandle` (ADR-0027 D3). Compiled under every trusty-memory
//! feature set (ADR-0066 D1.4).
//! Test: Each submodule keeps its existing unit tests; `cargo test -p
//! trusty-memory` exercises the full surface.

pub mod analytics;
pub mod community;
// #5902: the ONE content-hash entry point for memory bodies, plus the versioned
// normalization contract two machines must agree on to converge.
pub mod content_hash;
pub mod decay;
pub mod dream;
pub mod embed;
pub mod filter;
pub mod git;
// #8733: per-data-root election of the one process that runs maintenance.
pub mod maintenance_lease;
// #8732: durable trail of drawer deletions made by maintenance paths.
pub mod maintenance_log;
#[cfg(test)]
mod maintenance_log_tests;
pub mod palace;
// #9544: the one statement of "this palace is empty" (rename, delete).
pub mod palace_emptiness;
// #9751: the one caller-facing palace-not-found error; it never holds a path.
pub mod palace_not_found;
#[cfg(test)]
pub(crate) mod palace_not_found_tests;
pub mod registry;
// ADR-0027 T1: pure room identity (canonical keys, UUIDv5 minting, the legacy
// fold kept as the migration oracle). No I/O — see `store::rooms` for storage.
pub mod retrieval;
pub mod room_identity;
pub mod semantic_consolidation;
// #5902: palace-targeted JSONL export/import keyed on the content hash — the
// primitive the cross-machine git workflow is built on.
pub mod share;
pub mod store;
pub mod timeouts;
// ADR-0027 T9: pure wing identity (canonical keys, UUIDv5 minting). No I/O —
// see `store::wings` for storage and policy.
pub mod wing_identity;

pub use community::{KnowledgeGap, find_communities};
pub use content_hash::{
    CONTENT_HASH_VERSION, ContentHash, memory_content_hash, normalize_for_hash,
};
pub use maintenance_lease::{LeaseStatus, MaintenanceLease};
// #9278 (ruling 2026-10-14): no root re-export of the palace types or the
// handle, so each has one path before 1.0.
pub use registry::PalaceRegistry;
pub use room_identity::{DEFAULT_WING_ID, canonical_room_key, mint_room_id, room_to_uuid};
pub use semantic_consolidation::{
    ConsolidationAction, ConsolidationProvider, ConsolidationResult, MockInference,
    OllamaInference, OpenRouterInference, SemanticConsolidationConfig, SemanticConsolidator,
    inference_available, resolve_consolidation_provider,
};
pub use share::{
    ImportOutcome, ImportSummary, SHARE_FORMAT_VERSION, SharedMemoryRecord, export_palace_jsonl,
    export_palace_records, import_palace_jsonl, import_palace_records, supersede_drawer,
};
pub use wing_identity::{DEFAULT_WING_LABEL, canonical_wing_key, mint_wing_id};
