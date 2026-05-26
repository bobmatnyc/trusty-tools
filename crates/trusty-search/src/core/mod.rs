pub mod bm25;
pub mod chunker;
pub mod classifier;
#[cfg(feature = "clustering")]
pub mod concept_cluster;
pub mod corpus;
pub mod embed;
pub mod entity;
pub mod git;
pub mod indexer;
pub mod memguard;
pub mod memory_policy;
pub mod mmr;
pub mod ner;
pub mod project_config;
pub mod registry;
pub mod repo_config;
pub mod scip_ingest;
pub mod search;
pub mod store;
pub mod symbol_graph;

pub use chunker::{chunk_ast, chunk_text, ChunkType, RawChunk};
pub use classifier::{QueryClassifier, QueryIntent};
pub use embed::{Embedder, FastEmbedder};
pub use entity::{extract_entities, EdgeKind, EntityExtractor, EntityType, RawEntity};
pub use indexer::{CodeChunk, CodeIndexer};
pub use memory_policy::{
    resolve_coreml_batch_size, resolve_coreml_tripwire_mb, MemoryPolicy, MemoryTier,
};
pub use mmr::{cosine_similarity, mmr_rerank};
pub use registry::{IndexHandle, IndexId, IndexRegistry};
pub use scip_ingest::{CodeEntityIndex, ScipEdge, ScipEntityRef, ScipIndex};
pub use symbol_graph::{SymbolGraph, SymbolNode};
