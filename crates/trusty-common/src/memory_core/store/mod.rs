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
pub mod hnsw_store;
pub mod kg;
pub mod kg_redb;
pub mod kg_store;
pub mod kg_writer;
pub mod kuzu;
pub mod l1_cache;
pub mod palace_store;
pub mod payload_store;
pub mod redb_open;
// ADR-0027 T2: additive, fail-open room backfill run at palace open.
pub mod room_backfill;
// ADR-0027 T10: the read-only plan the backfill executes and `--dry-run` prints.
pub mod room_plan;
// ADR-0027 T1/T4/T6: room record shape, resolve-or-create, and the room surface.
pub mod rooms;
pub mod vector;
// ADR-0027 T9: wing record shape, default-wing seeding, create/rename/list.
pub mod wings;

pub use chat_sessions::{ChatSession, ChatSessionMeta, ChatSessionStore};
pub use concurrent_open::{OpenIntent, OpenMode};
// #4906: the ledger row type; the read/write helpers stay module-qualified so
// call sites read as `embed_ledger::record(..)`.
pub use embed_ledger::EmbedFailure;
pub use kg::{KnowledgeGraph, Triple};
pub use l1_cache::{L1Cache, L1CacheError};
pub use palace_store::{PalaceStore, PalaceStoreError};
pub use payload_store::{PayloadRow, PayloadStore, PayloadStoreError};
pub use redb_open::{
    INCOMPATIBLE_SUFFIX, backup_incompatible_file, incompatible_backup_path,
    is_incompatible_format, open_or_recreate,
};
pub use room_backfill::{BackfillReport, LabelSource, backfill_rooms, backfill_rooms_fail_open};
pub use room_plan::{RoomPlanAction, RoomPlanEntry, plan_rooms};
pub use rooms::{
    RoomRecord, RoomSummary, create_room, list_room_summaries, rename_room, resolve_or_create_room,
    resolve_room_filter_id, resolve_room_selector,
};
pub use vector::{VectorHit, VectorStore};
pub use wings::{
    WingRecord, WingSummary, ensure_default_wing, ensure_default_wing_fail_open, list_wings,
    rename_wing, resolve_or_create_wing, resolve_wing_selector, rooms_in_wing,
};
