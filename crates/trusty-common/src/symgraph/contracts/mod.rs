//! Shared entity types for the trusty-* toolchain (knowledge-graph layer).
//!
//! Why: the knowledge-graph layer (`SymbolGraph`), the analysis sidecar
//! (`trusty-analyzer-core`), and ingest pipelines (SCIP, NER, concept-cluster)
//! all consume the same `EntityType` / `EdgeKind` / `RawEntity` shapes.
//! Co-locating them inside `trusty-symgraph` keeps the entity-graph
//! substrate in one crate while still letting non-tree-sitter consumers
//! import the pure data types.
//!
//! What: pure data definitions — enums, structs, and the `fact_hash_str`
//! helper. No async, no tokio, no tree-sitter. The tree-sitter–based
//! extraction code lives in sibling modules / downstream crates.
//!
//! `EdgeKind` is extracted into `edge_kind.rs` to stay under the 500-line
//! cap (it grew with Phase D/E variants and doc notes). Re-exported here so
//! all existing import paths (`contracts::EdgeKind`) continue to compile.
//!
//! Test: see `#[cfg(test)]` in this file — covers `RawEntity::new` id
//! stability, `fact_hash_str` determinism, and `EntityType::as_str`.
//!
//! ## EdgeKind convergence (issue #815, ADR-0010 Option C / Phase E #818)
//!
//! `contracts::EdgeKind` is the **single canonical enum** for all KG edge
//! kinds across the trusty-* toolchain. Phase E adds `Custom(String)` (#818)
//! and drops `Copy` (String is heap-allocated). See `edge_kind.rs` for the
//! full definition.

mod edge_kind;
pub use edge_kind::{EdgeKind, EdgeKindError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Taxonomy of program entities surfaced from source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EntityType {
    /// Type identifiers (`Arc`, `Vec`, `CodeChunk`, …).
    NamedType,
    /// Trait bound expressions (`Send + Sync`, `Serialize`, …).
    TraitBound,
    /// Module paths (`crate::indexer::CodeIndexer`, `std::sync::Arc`).
    ModulePath,
    /// Error/panic call sites: `bail!`, `anyhow!`, `panic!`, `unwrap`.
    ErrorVariant,
    /// Identifiers referenced from `#[test]` function bodies.
    TestRelation,
    /// Doc-comment derived concept (NLP phrase / keyword).
    DocConcept,
    /// Attribute annotations (`#[derive(...)]`, `#[cfg(...)]`).
    Annotation,
    /// String literals longer than 10 characters.
    LiteralString,
    /// `type Foo = Bar` aliases.
    TypeAlias,
    /// Top-level `const`/`static` symbol.
    ConstantSymbol,
    /// Top-level `use` of a non-stdlib, non-self/super/crate path.
    ExternalCrate,
    /// Cluster of co-occurring concepts (Phase C).
    ConceptCluster,
    /// Free-form natural-language phrase pulled from docs/comments.
    NaturalLanguagePhrase,
}

impl EntityType {
    /// Stable string tag used in `RawEntity::new` id hashing. Changing any of
    /// these strings invalidates previously persisted entity ids.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NamedType => "NamedType",
            Self::TraitBound => "TraitBound",
            Self::ModulePath => "ModulePath",
            Self::ErrorVariant => "ErrorVariant",
            Self::TestRelation => "TestRelation",
            Self::DocConcept => "DocConcept",
            Self::Annotation => "Annotation",
            Self::LiteralString => "LiteralString",
            Self::TypeAlias => "TypeAlias",
            Self::ConstantSymbol => "ConstantSymbol",
            Self::ExternalCrate => "ExternalCrate",
            Self::ConceptCluster => "ConceptCluster",
            Self::NaturalLanguagePhrase => "NaturalLanguagePhrase",
        }
    }
}

/// redb table name constants for entity storage.
pub mod tables {
    /// `entity_id (str) -> RawEntity (bincode/json)`
    pub const ENTITIES: &str = "entities";
    /// `(from_entity_id, edge_kind, to_entity_id) -> ()`
    pub const ENTITY_EDGES: &str = "entity_edges";
    /// `chunk_id -> Vec<entity_id>`
    pub const CHUNK_ENTITIES: &str = "chunk_entities";
    /// `entity_id -> Vec<chunk_id>` (reverse index of `CHUNK_ENTITIES`)
    pub const ENTITY_CHUNKS: &str = "entity_chunks";
}

/// One extracted entity, anchored to a byte span and source line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEntity {
    /// Stable hash of (entity_type, text, file).
    pub id: String,
    pub entity_type: EntityType,
    pub text: String,
    pub span: (usize, usize),
    pub file: String,
    pub line: usize,
}

impl RawEntity {
    /// Construct a `RawEntity` with a deterministic SHA-256 id derived from
    /// `(entity_type, text, file)`. Same inputs always yield the same id, so
    /// re-extraction over identical source produces stable references for the
    /// KG layer.
    ///
    /// Why: stable ids allow incremental re-extraction to reuse existing KG
    /// nodes rather than creating duplicates when span or line changes.
    /// What: hashes `(entity_type, text, file)` with SHA-256; span/line are
    /// stored but excluded from the hash.
    /// Test: `raw_entity_id_is_stable`, `raw_entity_id_changes_with_type`.
    pub fn new(
        entity_type: EntityType,
        text: String,
        span: (usize, usize),
        file: &str,
        line: usize,
    ) -> Self {
        let mut h = Sha256::new();
        h.update(entity_type.as_str().as_bytes());
        h.update(b"\0");
        h.update(text.as_bytes());
        h.update(b"\0");
        h.update(file.as_bytes());
        let id = format!("{:x}", h.finalize());
        Self {
            id,
            entity_type,
            text,
            span,
            file: file.to_string(),
            line,
        }
    }
}

/// Short hex hash of a string. Used by ingest sources (e.g. SCIP) to
/// derive compact, collision-resistant entity IDs from opaque symbol strings.
///
/// Why: SCIP symbol strings (e.g. `"rust-analyzer cargo crate/Foo#"`) are
/// long and noisy. Hashing them produces a compact suffix safe to embed in
/// entity ids and redb keys.
/// What: hashes `s` with `std::collections::hash_map::DefaultHasher` and
/// formats as 8-char (minimum) lowercase hex.
///
/// **Stability caveat:** `DefaultHasher` is NOT guaranteed stable across Rust
/// versions or process restarts (the standard library may change its
/// implementation). The output is deterministic within a single process run
/// but MUST NOT be relied upon for cross-version persistence stability.
/// Tracked as a separate durability issue — do not change the algorithm here
/// without a coordinated migration plan, as existing persisted entity IDs
/// were derived with this hash.
///
/// Test: `fact_hash_str_is_deterministic`.
pub fn fact_hash_str(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_entity_id_is_stable() {
        let a = RawEntity::new(EntityType::NamedType, "Foo".into(), (0, 3), "src/x.rs", 1);
        let b = RawEntity::new(
            EntityType::NamedType,
            "Foo".into(),
            (10, 13),
            "src/x.rs",
            99,
        );
        // Same (type, text, file) → same id even when span/line differ.
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn raw_entity_id_changes_with_type() {
        let a = RawEntity::new(EntityType::NamedType, "Foo".into(), (0, 3), "src/x.rs", 1);
        let b = RawEntity::new(EntityType::ModulePath, "Foo".into(), (0, 3), "src/x.rs", 1);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn fact_hash_str_is_deterministic() {
        let a = fact_hash_str("rust-analyzer cargo crate/Foo#");
        let b = fact_hash_str("rust-analyzer cargo crate/Foo#");
        assert_eq!(a, b);
        // u64 in lowercase hex; `{:08x}` is the *min* width, so output is
        // up to 16 characters (and always at least 8 due to zero-padding).
        assert!(a.len() >= 8 && a.len() <= 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn entity_type_as_str_round_trip() {
        // Just ensure every variant has a non-empty tag.
        let variants = [
            EntityType::NamedType,
            EntityType::TraitBound,
            EntityType::ModulePath,
            EntityType::ErrorVariant,
            EntityType::TestRelation,
            EntityType::DocConcept,
            EntityType::Annotation,
            EntityType::LiteralString,
            EntityType::TypeAlias,
            EntityType::ConstantSymbol,
            EntityType::ExternalCrate,
            EntityType::ConceptCluster,
            EntityType::NaturalLanguagePhrase,
        ];
        for v in variants {
            assert!(!v.as_str().is_empty());
        }
    }
}
