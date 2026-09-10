//! Code index storage and trusty-memory RPC contracts (#7360).
//! Explicit fact memory is daemon-owned; legacy local memory files are preserved on disk.
//! Test: `redb_usearch::tests` covers code index persistence; `trusty_client::tests` covers RPC.
pub mod code_store;
pub mod embed;
pub mod redb_recovery;
pub mod redb_usearch;
pub mod scope;
pub mod store;
pub mod trusty_client;
pub use code_store::CodeStore;
pub use embed::{Embedder, FastEmbedder};
pub use redb_usearch::RedbUsearchStore;
// #7443: the per-assistant palace key stays even though the local memory
// subsystem it was written for is gone — `trusty_client` keys its palaces by it.
#[allow(unused_imports)]
pub use scope::{MemoryScope, MemoryScopeError};
pub use store::{MemoryResult, MemoryStore, Segment};
pub use trusty_client::{MemoryBackend, TrustyMemoryClient};
