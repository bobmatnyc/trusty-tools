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
//! Test: see `#[cfg(test)]` in this file — covers `RawEntity::new` id
//! stability, `EdgeKind::score_multiplier`, and `fact_hash_str` determinism.

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

/// Relationship taxonomy for the trusty-search entity knowledge graph.
///
/// Why: trusty-search's KG layer needs richer semantic fidelity than a plain
/// call-graph — it must express trait/type relationships, test provenance,
/// doc-concept links, and reverse indexes so that KG-expansion scoring can
/// favour high-signal edges via per-variant `score_multiplier` values
/// (issue #18). This enum is the single vocabulary for all those edge types.
///
/// **Intentionally separate from `crate::symgraph::graph::EdgeKind`**
/// (the 3-variant structural enum used by `SymbolGraph`'s petgraph substrate,
/// gated behind the `symgraph-parser` feature). The two enums serve different
/// layers:
///   - `graph::EdgeKind` — petgraph edge weight for the in-memory `SymbolGraph`
///     used by the tree-sitter parser path. Three coarse variants (`Calls`,
///     `Imports`, `Contains`) are sufficient for the local name-resolution
///     queries that path performs.
///   - `contracts::EdgeKind` (this type) — the persisted, scored vocabulary for
///     the trusty-search entity KG. Seventeen variants with per-edge
///     `score_multiplier` values, serialised to stable string tags and stored
///     in the warm-boot index via `edge_kind_tag` / `edge_kind_from_tag` in
///     `trusty_search::core::symbol_graph`.
///
/// **Intentionally separate from `trusty_analyze::KgEdgeKind`** (11 variants).
/// That type is trusty-analyze's independent language-neutral KG for static
/// analysis output (tree-sitter adapters emit into it). It is not connected to
/// the trusty-search KG at runtime and carries a vocabulary suited to
/// whole-codebase structural analysis rather than entity/concept search.
///
/// When adding a new variant here, also add the matching string tag in
/// `trusty_search::core::symbol_graph::edge_kind_tag` /
/// `edge_kind_from_tag` to preserve warm-boot compatibility.
///
/// Phase A = structural (tree-sitter derived)
/// Phase B = test-relation
/// Phase C = doc/concept
///
/// Test: `edge_kind_score_multiplier_known_values` (this file);
/// `edge_kind_serde_round_trip` (this file);
/// `edge_kind_tag_round_trip` in `trusty_search::core::symbol_graph::tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    // Call graph
    /// Caller → callee.
    CallsFunction,
    /// Callee → caller (reverse index of `CallsFunction`).
    CalledByFunction,
    // Phase A — structural
    Implements,
    UsesType,
    Derives,
    ModuleContains,
    ReExports,
    RaisesError,
    Configures,
    // Phase B — test relations
    TestedBy,
    TestUsesFixture,
    CoOccursInTest,
    // Phase C — docs / concepts
    Documents,
    ReferencesConcept,
    Aliases,
    ErrorDescribes,
}

impl EdgeKind {
    /// Relevance weight for KG neighbourhood expansion.
    ///
    /// Why: Different edge types carry different levels of semantic relevance
    /// to a search query. Weighting edges (rather than treating all as equal)
    /// lets the ranking layer boost strongly-related symbols (trait implementations,
    /// tested-by links) over weaker associations (concept co-occurrence).
    /// What: Returns a multiplier in (0, 1] applied to the base relevance
    /// score of a KG neighbour when this edge was traversed to reach it.
    /// Higher values mean the neighbour is ranked more prominently.
    /// Test: `edge_kind_score_multiplier_known_values` in this module.
    pub fn score_multiplier(&self) -> f32 {
        match self {
            EdgeKind::Implements => 0.85,
            EdgeKind::UsesType => 0.75,
            EdgeKind::TestedBy => 0.80,
            EdgeKind::Documents => 0.65,
            EdgeKind::ReferencesConcept => 0.60,
            // Remaining edges use the legacy flat KG-expansion multiplier.
            _ => 0.70,
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

/// Short, stable hex hash of a string. Used by ingest sources (e.g. SCIP) to
/// derive readable, collision-resistant entity IDs from opaque symbol strings.
///
/// Why: SCIP symbol strings (e.g. `"rust-analyzer cargo crate/Foo#"`) are
/// long and noisy. Hashing them produces a compact, stable suffix safe to
/// embed in entity ids and redb keys.
/// What: hashes `s` with `DefaultHasher` and formats as 8-char lowercase hex.
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
    fn edge_kind_score_multiplier_known_values() {
        assert!((EdgeKind::Implements.score_multiplier() - 0.85).abs() < 1e-6);
        assert!((EdgeKind::UsesType.score_multiplier() - 0.75).abs() < 1e-6);
        assert!((EdgeKind::TestedBy.score_multiplier() - 0.80).abs() < 1e-6);
        assert!((EdgeKind::Documents.score_multiplier() - 0.65).abs() < 1e-6);
        assert!((EdgeKind::ReferencesConcept.score_multiplier() - 0.60).abs() < 1e-6);
        // Default branch.
        assert!((EdgeKind::CallsFunction.score_multiplier() - 0.70).abs() < 1e-6);
    }

    /// Verify that every `contracts::EdgeKind` variant round-trips through
    /// `serde_json` without loss. This guards the on-disk KG serialisation
    /// format: a variant added here but missing a `#[serde(rename = "…")]`
    /// annotation would still survive this round-trip (serde uses the variant
    /// name by default), but changing an existing variant name without a rename
    /// would break it.
    #[test]
    fn edge_kind_serde_round_trip() {
        let variants = [
            EdgeKind::CallsFunction,
            EdgeKind::CalledByFunction,
            EdgeKind::Implements,
            EdgeKind::UsesType,
            EdgeKind::Derives,
            EdgeKind::ModuleContains,
            EdgeKind::ReExports,
            EdgeKind::RaisesError,
            EdgeKind::Configures,
            EdgeKind::TestedBy,
            EdgeKind::TestUsesFixture,
            EdgeKind::CoOccursInTest,
            EdgeKind::Documents,
            EdgeKind::ReferencesConcept,
            EdgeKind::Aliases,
            EdgeKind::ErrorDescribes,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).expect("serialize EdgeKind");
            let back: EdgeKind = serde_json::from_str(&json).expect("deserialize EdgeKind");
            assert_eq!(v, back, "round-trip failed for {json}");
        }
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
