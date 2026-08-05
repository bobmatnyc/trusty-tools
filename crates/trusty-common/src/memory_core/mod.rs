//! Memory Palace core types, storage, and retrieval (formerly the
//! `trusty-memory-core` crate).
//!
//! Why: Centralises the Memory Palace data model and storage abstractions
//! so every binary (CLI, MCP server, embedded library) reuses the same
//! types. Absorbed into `trusty-common` (issue #5 phase 2d) so the trusty-*
//! toolchain links a single internal library and we ship one fewer
//! published crate.
//! What: Re-exports the palace hierarchy (`Palace` -> `Wing` -> `Room` ->
//! `Drawer`), the registry, and the retrieval handle. "Closet" is not a level
//! in that hierarchy — it is the keyword -> drawer-ids inverted index on
//! `PalaceHandle` (ADR-0027 D3). Gated behind the
//! `memory-core` feature because it pulls in heavy storage deps
//! (`usearch`, `redb`, `postcard`, `tiktoken-rs`, `git2`).
//! Test: Each submodule keeps its existing unit tests; `cargo test -p
//! trusty-common --features memory-core` exercises the full surface.

pub mod analytics;
pub mod community;
pub mod decay;
pub mod dream;
pub mod embed;
pub mod filter;
pub mod git;
pub mod palace;
pub mod registry;
// ADR-0027 T1: pure room identity (canonical keys, UUIDv5 minting, the legacy
// fold kept as the migration oracle). No I/O — see `store::rooms` for storage.
pub mod retrieval;
pub mod room_identity;
pub mod semantic_consolidation;
pub mod store;
pub mod timeouts;

pub use community::{KnowledgeGap, find_communities};
pub use palace::{Drawer, DrawerType, Palace, PalaceId, Room, RoomType, Wing};
pub use registry::PalaceRegistry;
pub use retrieval::PalaceHandle;
pub use room_identity::{DEFAULT_WING_ID, canonical_room_key, mint_room_id, room_to_uuid};
pub use semantic_consolidation::{
    ConsolidationAction, ConsolidationResult, MockInference, OllamaInference, OpenRouterInference,
    SemanticConsolidationConfig, SemanticConsolidator, inference_available,
};
