//! Canonical KG edge-kind vocabulary (issue #815, ADR-0010 Phase E).
//!
//! Why: extracted from `contracts.rs` to stay under the 500-line cap while
//! adding `Custom(String)` (issue #818), doc notes for `Writes`/`AccessesResource`
//! per the #1111 review, and tag/from_tag helpers that return `Cow<'static, str>`.
//! What: defines `EdgeKind`, `score_multiplier`, `tag`, `from_tag`, and
//! `from_static_tag`. Re-exported via `contracts::EdgeKind`.
//! Test: see `#[cfg(test)]` block at the bottom.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Canonical KG edge-kind vocabulary for the trusty-* toolchain (issue #815, ADR-0010).
///
/// Why: Three formerly diverged enums (contracts, graph, KgEdgeKind) created a
/// maintenance trap. ADR-0010 Option C converges them so there is one vocabulary
/// and one `score_multiplier` table. Phase D adds data-flow variants (#817) and
/// the `Custom(String)` escape hatch (#818).
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
/// **`Copy` removed:** `Custom(String)` holds heap-allocated data and is therefore
/// not `Copy`. Use `.clone()` where a copy was previously implicit.
///
/// Phase A/B/C = original trusty-search KG (16 variants, on-disk tags immutable)
/// Phase D = data-flow (#817): `Reads` 0.80, `Writes` 0.90, `AccessesResource` 0.75
/// Phase E = escape hatch (#818): `Custom(String)`, tag `"custom:<s>"`, multiplier 0.70
/// Phase KG = language-neutral structural (formerly `KgEdgeKind` — 10 variants)
/// Phase SG = symbol-graph coarse (formerly `graph::EdgeKind`; covered by Calls/Imports/Contains)
///
/// When adding a new variant, also add its tag in
/// `trusty_search::core::symbol_graph::edge_tags`.
///
/// Test: `edge_kind_score_multiplier_known_values` (this file);
/// `edge_kind_serde_round_trip`; `edge_kind_union_coverage`;
/// `edge_kind_tag_round_trip` in `trusty_search::core::symbol_graph::tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
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
    /// Source writes to (mutates) shared state.
    ///
    /// Score multiplier: **0.90** (highest of any edge — mutation drives impact
    /// cascades, so "what mutates this?" is the primary impact-analysis query).
    /// **Provisional/heuristic:** this value was chosen to rank above `Implements`
    /// (0.85) per ADR-0010 design intent, but has not yet been validated against
    /// pilot data. Subject to downward tuning once real-world corpora are measured.
    Writes,
    /// Source accesses an external resource: HTTP endpoint, queue topic, DB table
    /// (0.75). Answers "what touches this endpoint/queue/table?".
    ///
    /// **Precedence:** prefer `Reads` or `Writes` when the data-flow direction is
    /// known. Use `AccessesResource` only when direction is unknown or ambiguous
    /// (e.g. a stored-procedure call that both reads and writes). Using this variant
    /// when direction is clear overlaps with `Reads`/`Writes` and dilutes ranking.
    AccessesResource,

    // ── Escape hatch (Phase E — issue #818) ────────────────────────────────
    /// External-extractor relation contributed as data (0.70 conservative default).
    ///
    /// Why: lets external extractors (language adapters, community tools) contribute
    /// relations without requiring a core PR per relation type. Custom relations
    /// earn a named variant here to get a tuned `score_multiplier`; until then the
    /// conservative 0.70 applies.
    /// What: carries the relation label as a `String` (e.g. `"reads_table"`). On
    /// disk the tag is `"custom:<s>"` — the `"custom:"` prefix is permanently
    /// reserved and never collides with future PascalCase named variants.
    /// Test: `edge_kind_custom_tag_round_trip` (this file) and
    /// `edge_kind_custom_survives_warm_boot` in `trusty_search::core::symbol_graph::tests`.
    Custom(String),
}

impl EdgeKind {
    /// Relevance weight for KG neighbourhood expansion in trusty-search.
    ///
    /// Why: Different edge types carry different levels of semantic relevance.
    /// What: Returns a multiplier in (0, 1] applied to the base relevance score
    /// of a KG neighbour when this edge was traversed to reach it.
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
            // Phase E escape hatch (issue #818): conservative default.
            // Custom relations earn a named variant to get a tuned multiplier.
            EdgeKind::Custom(_) => 0.70,
            // All remaining edges (Phase A/B/C/KG/SG) use the conservative
            // flat multiplier (0.70). This wildcard default is intentional —
            // add an explicit arm only when pilot data justifies a deviation.
            _ => 0.70,
        }
    }

    /// Return the stable on-disk tag for this edge kind.
    ///
    /// Why: funnelling all persistence hops through this helper keeps tags stable
    /// across serde-format changes. Named variants use their PascalCase tag (a
    /// `&'static str`); `Custom(s)` returns an owned `"custom:<s>"` string.
    /// What: returns `Cow<'static, str>` — `Borrowed` for named variants, `Owned`
    /// for `Custom`.
    /// Test: `edge_kind_custom_tag_round_trip` (this file).
    pub fn tag(&self) -> Cow<'static, str> {
        match self {
            EdgeKind::Custom(s) => Cow::Owned(format!("custom:{s}")),
            other => Cow::Borrowed(other.static_tag()),
        }
    }

    /// Parse an on-disk tag back into an `EdgeKind`.
    ///
    /// Why: warm-boot reads tags from redb; this is the single point that maps
    /// string tags to enum variants. `"custom:"`-prefixed tags always parse to
    /// `Custom(s)` (round-trip guaranteed per ADR-0010 Option H). Bare
    /// unrecognized tags return `None` so callers can count drops (issue #816).
    /// What: `Some(Custom(s))` for `"custom:<s>"`, `Some(v)` for known named
    /// tags, `None` for bare unknown tags.
    /// Test: `edge_kind_unknown_bare_tag_returns_none` (this file).
    pub fn from_tag(tag: &str) -> Option<Self> {
        if let Some(suffix) = tag.strip_prefix("custom:") {
            return Some(EdgeKind::Custom(suffix.to_owned()));
        }
        Self::from_static_tag(tag)
    }

    /// Map a PascalCase tag to a known named variant; returns `None` for unknown.
    ///
    /// Why: separates the `Custom` prefix check from the exhaustive match so
    /// both callers (`from_tag` and tests) can reuse the named-variant table.
    /// What: exhaustive match over all named variants; returns `None` on miss.
    /// Test: covered transitively by `edge_kind_tag_round_trip`.
    pub fn from_static_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "CallsFunction" => EdgeKind::CallsFunction,
            "CalledByFunction" => EdgeKind::CalledByFunction,
            "Implements" => EdgeKind::Implements,
            "UsesType" => EdgeKind::UsesType,
            "Derives" => EdgeKind::Derives,
            "ModuleContains" => EdgeKind::ModuleContains,
            "ReExports" => EdgeKind::ReExports,
            "RaisesError" => EdgeKind::RaisesError,
            "Configures" => EdgeKind::Configures,
            "TestedBy" => EdgeKind::TestedBy,
            "TestUsesFixture" => EdgeKind::TestUsesFixture,
            "CoOccursInTest" => EdgeKind::CoOccursInTest,
            "Documents" => EdgeKind::Documents,
            "ReferencesConcept" => EdgeKind::ReferencesConcept,
            "Aliases" => EdgeKind::Aliases,
            "ErrorDescribes" => EdgeKind::ErrorDescribes,
            "Contains" => EdgeKind::Contains,
            "Imports" => EdgeKind::Imports,
            "Exports" => EdgeKind::Exports,
            "Calls" => EdgeKind::Calls,
            "Extends" => EdgeKind::Extends,
            "References" => EdgeKind::References,
            "Tests" => EdgeKind::Tests,
            "DependsOn" => EdgeKind::DependsOn,
            "GeneratedFrom" => EdgeKind::GeneratedFrom,
            "RuntimeObservationFor" => EdgeKind::RuntimeObservationFor,
            "Reads" => EdgeKind::Reads,
            "Writes" => EdgeKind::Writes,
            "AccessesResource" => EdgeKind::AccessesResource,
            _ => return None,
        })
    }

    /// Return the static string slice for a named variant.
    ///
    /// Why: backing helper for `tag()` — avoids allocating a `String` for the
    /// common named-variant case.
    /// What: panics on `Custom(_)` (callers must branch before calling this).
    /// Test: covered by `edge_kind_custom_tag_round_trip`.
    fn static_tag(&self) -> &'static str {
        match self {
            EdgeKind::CallsFunction => "CallsFunction",
            EdgeKind::CalledByFunction => "CalledByFunction",
            EdgeKind::Implements => "Implements",
            EdgeKind::UsesType => "UsesType",
            EdgeKind::Derives => "Derives",
            EdgeKind::ModuleContains => "ModuleContains",
            EdgeKind::ReExports => "ReExports",
            EdgeKind::RaisesError => "RaisesError",
            EdgeKind::Configures => "Configures",
            EdgeKind::TestedBy => "TestedBy",
            EdgeKind::TestUsesFixture => "TestUsesFixture",
            EdgeKind::CoOccursInTest => "CoOccursInTest",
            EdgeKind::Documents => "Documents",
            EdgeKind::ReferencesConcept => "ReferencesConcept",
            EdgeKind::Aliases => "Aliases",
            EdgeKind::ErrorDescribes => "ErrorDescribes",
            EdgeKind::Contains => "Contains",
            EdgeKind::Imports => "Imports",
            EdgeKind::Exports => "Exports",
            EdgeKind::Calls => "Calls",
            EdgeKind::Extends => "Extends",
            EdgeKind::References => "References",
            EdgeKind::Tests => "Tests",
            EdgeKind::DependsOn => "DependsOn",
            EdgeKind::GeneratedFrom => "GeneratedFrom",
            EdgeKind::RuntimeObservationFor => "RuntimeObservationFor",
            EdgeKind::Reads => "Reads",
            EdgeKind::Writes => "Writes",
            EdgeKind::AccessesResource => "AccessesResource",
            EdgeKind::Custom(_) => {
                panic!("static_tag called on Custom variant — use tag() instead")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Phase E escape hatch (issue #818): Custom uses conservative 0.70.
        assert!((EdgeKind::Custom("foo".to_string()).score_multiplier() - 0.70).abs() < 1e-6);
    }

    /// `Custom("foo")` ⇄ `"custom:foo"` round-trip (issue #818).
    #[test]
    fn edge_kind_custom_tag_round_trip() {
        let v = EdgeKind::Custom("foo".to_string());
        let tag = v.tag();
        assert_eq!(tag.as_ref(), "custom:foo");
        let back = EdgeKind::from_tag(&tag).expect("Custom should parse from custom: tag");
        assert_eq!(back, v);
    }

    /// A bare unrecognized PascalCase tag does NOT become Custom (issue #818,
    /// Option H: only `"custom:"`-prefixed tags round-trip).
    #[test]
    fn edge_kind_unknown_bare_tag_returns_none() {
        assert!(EdgeKind::from_tag("UnknownFutureEdge").is_none());
        assert!(EdgeKind::from_tag("SomeTypo").is_none());
    }

    /// Named tags do NOT get confused with the `custom:` prefix.
    #[test]
    fn edge_kind_named_tag_not_treated_as_custom() {
        let tag = EdgeKind::CallsFunction.tag();
        assert_eq!(tag.as_ref(), "CallsFunction");
        let back = EdgeKind::from_tag(&tag).expect("named tag round-trips");
        assert_eq!(back, EdgeKind::CallsFunction);
    }

    /// Every variant round-trips through `serde_json` (guards the on-disk format).
    #[test]
    fn edge_kind_serde_round_trip() {
        let variants = vec![
            // Phase A/B/C
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
            // Language-neutral structural (former KgEdgeKind + graph::EdgeKind)
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
            // Phase D data-flow (issue #817)
            EdgeKind::Reads,
            EdgeKind::Writes,
            EdgeKind::AccessesResource,
            // Phase E escape hatch (issue #818)
            EdgeKind::Custom("reads_table".to_string()),
        ];
        for v in &variants {
            let json = serde_json::to_string(v).expect("serialize EdgeKind");
            let back: EdgeKind = serde_json::from_str(&json).expect("deserialize EdgeKind");
            assert_eq!(*v, back, "round-trip failed for {json}");
        }
    }

    /// Assert that the canonical enum covers the full prior union (issue #815).
    #[test]
    fn edge_kind_union_coverage() {
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
        let _ = EdgeKind::Reads;
        let _ = EdgeKind::Writes;
        let _ = EdgeKind::AccessesResource;
        let _ = EdgeKind::Custom("any".to_string());
    }
}
