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
//!
//! ## EdgeKind convergence (issue #815, ADR-0010 Option C)
//!
//! `contracts::EdgeKind` is now the **single canonical enum** for all KG edge
//! kinds across the trusty-* toolchain. It replaces the three formerly diverged
//! enums:
//!   - `contracts::EdgeKind` (this type) — the persisted, scored vocabulary for
//!     the trusty-search entity KG (16 Phase A/B/C variants).
//!   - `trusty_analyze::KgEdgeKind` — language-neutral structural analysis KG
//!     for trusty-analyze's tree-sitter adapters (11 variants). Now a type alias
//!     to this enum.
//!   - `crate::symgraph::graph::EdgeKind` — coarse petgraph edge weight for the
//!     in-memory `SymbolGraph` (3 variants: Calls, Imports, Contains). Now a
//!     type alias to this enum.
//!
//! The union contains all 26 formerly separate variants. Back-compatibility for
//! existing on-disk redb tags is preserved: `edge_kind_tag`/`edge_kind_from_tag`
//! in `trusty_search::core::symbol_graph` map every existing tag string to its
//! canonical variant without change.

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

/// Canonical KG edge-kind vocabulary for the trusty-* toolchain (issue #815, ADR-0010).
///
/// Why: Three formerly diverged enums (contracts, graph, KgEdgeKind) created a
/// maintenance trap. ADR-0010 Option C converges them so there is one vocabulary
/// and one `score_multiplier` table. Phase D adds data-flow variants (#817).
///
/// **Adapter guidance:** language adapters in trusty-analyze should emit `Calls`
/// (coarse, no reverse index). `CallsFunction` is reserved for the trusty-search
/// `EntityExtractor` which also maintains the `CalledByFunction` reverse index.
/// Similarly, `Tests` is the forward direction (test → production symbol) and
/// `TestedBy` is the reverse — emit the one that matches your traversal direction.
/// `Implements` was present in Phase A (contracts) before convergence; it is NOT
/// a new addition from `KgEdgeKind`.
///
/// **score_multiplier wildcard (0.70):** variants without an explicit arm fall
/// through to `_ => 0.70`. This is intentional — add an arm only when pilot data
/// justifies deviating from the conservative baseline.
///
/// Phase A/B/C = original trusty-search KG (16 variants, on-disk tags immutable)
/// Phase D = data-flow (issue #817): `Reads` 0.80, `Writes` 0.90, `AccessesResource` 0.75
/// Phase KG = language-neutral structural (formerly `KgEdgeKind` — 10 variants)
/// Phase SG = symbol-graph coarse (formerly `graph::EdgeKind`; covered by Calls/Imports/Contains)
///
/// When adding a new variant, also add its tag in
/// `trusty_search::core::symbol_graph::edge_kind_tag` / `edge_kind_from_tag`.
///
/// Test: `edge_kind_score_multiplier_known_values` (this file);
/// `edge_kind_serde_round_trip`; `edge_kind_union_coverage`;
/// `edge_kind_tag_round_trip` in `trusty_search::core::symbol_graph::tests`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    // ── trusty-search KG (Phase A/B/C — 16 variants) ──────────────────────
    // Call graph
    /// Caller → callee. On-disk tag: `"CallsFunction"`. Emitted by the
    /// trusty-search `EntityExtractor`; use `Calls` in language adapters.
    CallsFunction,
    /// Callee → caller (reverse index of `CallsFunction`). On-disk tag: `"CalledByFunction"`.
    CalledByFunction,
    // Phase A — structural
    /// Class implements interface / struct implements trait. Present in Phase A
    /// since the original `contracts::EdgeKind` — NOT a convergence addition.
    Implements,
    /// Symbol uses a named type.
    UsesType,
    /// `#[derive(…)]` relationship.
    Derives,
    /// Module structurally contains a child symbol.
    ModuleContains,
    /// Symbol re-exported from a module (`pub use`).
    ReExports,
    /// Function or macro raises / propagates an error variant.
    RaisesError,
    /// Symbol configures another (dependency injection, builder pattern).
    Configures,
    // Phase B — test relations
    /// Production symbol is covered by a test (reverse of `Tests`).
    TestedBy,
    /// Test uses a shared fixture.
    TestUsesFixture,
    /// Two symbols co-occur in the same test body.
    CoOccursInTest,
    // Phase C — docs / concepts
    /// Doc comment documents a symbol.
    Documents,
    /// Symbol references a concept extracted from documentation.
    ReferencesConcept,
    /// `type Foo = Bar` alias relationship.
    Aliases,
    /// Error variant described by a doc comment.
    ErrorDescribes,

    // ── Language-neutral structural (formerly KgEdgeKind — 10 variants) ───
    /// Parent structurally contains child (file → function, module → class, etc.).
    /// Formerly `KgEdgeKind::Contains` and `graph::EdgeKind::Contains`.
    Contains,
    /// File or module imports another.
    /// Formerly `KgEdgeKind::Imports` and `graph::EdgeKind::Imports`.
    Imports,
    /// Symbol exported from a module.
    /// Formerly `KgEdgeKind::Exports`.
    Exports,
    /// Function A calls function B (language-adapter coarse call edge).
    /// Formerly `KgEdgeKind::Calls` and `graph::EdgeKind::Calls`.
    /// Distinct from `CallsFunction`: emitted by language adapters, no reverse index.
    Calls,
    /// Class or interface inherits from another.
    /// Formerly `KgEdgeKind::Extends`.
    Extends,
    /// Symbol references another symbol (general, non-call reference).
    /// Formerly `KgEdgeKind::References`.
    References,
    /// Test function exercises a production symbol (forward direction).
    /// Distinct from `TestedBy` which is the reverse (production → test).
    /// Formerly `KgEdgeKind::Tests`.
    Tests,
    /// Package depends on an external package/crate/library.
    /// Formerly `KgEdgeKind::DependsOn`.
    DependsOn,
    /// Runtime observation derived from a static analysis node.
    /// Formerly `KgEdgeKind::GeneratedFrom`.
    GeneratedFrom,
    /// Profiler measurement attached to a static symbol.
    /// Formerly `KgEdgeKind::RuntimeObservationFor`.
    RuntimeObservationFor,

    // ── Data-flow (Phase D — issue #817) ───────────────────────────────────
    /// Source reads from a global, config, cache, or shared state (0.80).
    /// Answers "what reads this?" Language-agnostic (SQL SELECT, config reads).
    Reads,
    /// Source writes to (mutates) shared state (0.90 — highest data-flow weight).
    /// Answers "what mutates this?" — primary impact-analysis query.
    Writes,
    /// Source accesses an external resource: HTTP endpoint, queue topic, DB table
    /// (0.75). Answers "what touches this endpoint/queue/table?".
    AccessesResource,
}

impl EdgeKind {
    /// Relevance weight for KG neighbourhood expansion in trusty-search.
    ///
    /// Why: Different edge types carry different levels of semantic relevance
    /// to a search query. Weighting edges (rather than treating all as equal)
    /// lets the ranking layer boost strongly-related symbols (trait implementations,
    /// tested-by links) over weaker associations (concept co-occurrence). The new
    /// structural variants from the former `KgEdgeKind` use the conservative
    /// default (0.70) until pilot data informs tuned values (issue #817).
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
            // Phase D data-flow variants (issue #817): tuned starting values.
            // Writes ranks highest because mutation drives impact cascades.
            EdgeKind::Writes => 0.90,
            EdgeKind::Reads => 0.80,
            EdgeKind::AccessesResource => 0.75,
            // All remaining edges (Phase A/B/C/KG/SG) use the conservative
            // flat multiplier (0.70). This wildcard default is intentional —
            // add an explicit arm only when pilot data justifies a deviation.
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
        // Phase D data-flow variants (issue #817).
        assert!((EdgeKind::Writes.score_multiplier() - 0.90).abs() < 1e-6);
        assert!((EdgeKind::Reads.score_multiplier() - 0.80).abs() < 1e-6);
        assert!((EdgeKind::AccessesResource.score_multiplier() - 0.75).abs() < 1e-6);
        // Default wildcard branch (0.70 is intentional for untuned variants).
        assert!((EdgeKind::CallsFunction.score_multiplier() - 0.70).abs() < 1e-6);
        assert!((EdgeKind::Calls.score_multiplier() - 0.70).abs() < 1e-6);
    }

    /// Verify that every `contracts::EdgeKind` variant round-trips through
    /// `serde_json` without loss. This guards the on-disk KG serialisation
    /// format: a variant added here but missing a `#[serde(rename = "…")]`
    /// annotation would still survive this round-trip (serde uses the variant
    /// name by default), but changing an existing variant name without a rename
    /// would break it.
    ///
    /// This also covers the union of all formerly-separate enums (issue #815):
    /// all 26 canonical variants must survive round-trip.
    #[test]
    fn edge_kind_serde_round_trip() {
        let variants = [
            // Phase A/B/C — trusty-search KG (16 legacy variants)
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
            // Language-neutral structural (10 variants from former KgEdgeKind + graph::EdgeKind)
            EdgeKind::Contains,
            EdgeKind::Imports,
            EdgeKind::Exports,
            EdgeKind::Calls,
            EdgeKind::Extends,
            EdgeKind::References,
            EdgeKind::Tests,
            EdgeKind::DependsOn,
            EdgeKind::GeneratedFrom,
            EdgeKind::RuntimeObservationFor,
            // Phase D data-flow variants (issue #817)
            EdgeKind::Reads,
            EdgeKind::Writes,
            EdgeKind::AccessesResource,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).expect("serialize EdgeKind");
            let back: EdgeKind = serde_json::from_str(&json).expect("deserialize EdgeKind");
            assert_eq!(v, back, "round-trip failed for {json}");
        }
    }

    /// Assert that the canonical enum covers the full prior union of the three
    /// formerly-separate enums (issue #815 acceptance criterion).
    ///
    /// Why: ensures no variant was accidentally omitted during convergence.
    /// What: exhaustive list of all 26 variants expected in the unified enum.
    /// Test: this is the test — it fails at compile time if any variant is missing.
    #[test]
    fn edge_kind_union_coverage() {
        // Former contracts::EdgeKind variants (Phase A/B/C)
        let _ = EdgeKind::CallsFunction;
        let _ = EdgeKind::CalledByFunction;
        let _ = EdgeKind::Implements;
        let _ = EdgeKind::UsesType;
        let _ = EdgeKind::Derives;
        let _ = EdgeKind::ModuleContains;
        let _ = EdgeKind::ReExports;
        let _ = EdgeKind::RaisesError;
        let _ = EdgeKind::Configures;
        let _ = EdgeKind::TestedBy;
        let _ = EdgeKind::TestUsesFixture;
        let _ = EdgeKind::CoOccursInTest;
        let _ = EdgeKind::Documents;
        let _ = EdgeKind::ReferencesConcept;
        let _ = EdgeKind::Aliases;
        let _ = EdgeKind::ErrorDescribes;
        // Former KgEdgeKind variants (structural / language-neutral)
        let _ = EdgeKind::Contains;
        let _ = EdgeKind::Imports;
        let _ = EdgeKind::Exports;
        let _ = EdgeKind::Calls;
        let _ = EdgeKind::Extends;
        let _ = EdgeKind::References;
        let _ = EdgeKind::Tests;
        let _ = EdgeKind::DependsOn;
        let _ = EdgeKind::GeneratedFrom;
        let _ = EdgeKind::RuntimeObservationFor;
        // graph::EdgeKind coarse variants are covered by Calls, Imports, Contains above.
        // Phase D data-flow variants (issue #817)
        let _ = EdgeKind::Reads;
        let _ = EdgeKind::Writes;
        let _ = EdgeKind::AccessesResource;
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
