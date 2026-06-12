//! Storage backends: vector index (HNSW) + temporal knowledge graph (SQLite).
//!
//! Why: Two complementary data shapes — dense vectors for semantic recall and
//! triples-with-time for relational facts — covered by separate modules so each
//! can evolve independently.
//! What: Re-exports `VectorStore` trait and `KnowledgeGraph` type.
//! Test: See submodule tests.

pub mod chat_sessions;
pub mod concurrent_open;
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
pub mod vector;

pub use chat_sessions::{ChatSession, ChatSessionMeta, ChatSessionStore};
pub use kg::{KnowledgeGraph, Triple};
pub use l1_cache::{L1Cache, L1CacheError};
pub use palace_store::{PalaceStore, PalaceStoreError};
pub use payload_store::{PayloadRow, PayloadStore, PayloadStoreError};
pub use redb_open::{
    INCOMPATIBLE_SUFFIX, backup_incompatible_file, incompatible_backup_path,
    is_incompatible_format, open_or_recreate,
};
pub use vector::{VectorHit, VectorStore};
