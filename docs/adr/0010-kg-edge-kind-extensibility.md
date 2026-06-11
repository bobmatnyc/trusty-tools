# 0010. KG Edge-Kind Extensibility — First-Class Data-Flow Variants + Custom Escape Hatch

- **Status:** Accepted
- **Date:** 2026-06-10
- **Accepted:** 2026-06-11
- **Decision Log:**
  - **Q1 (convergence scope)** → **Option C (full convergence first).** Converge `contracts::EdgeKind`, `KgEdgeKind`, and `graph::EdgeKind` into a single canonical enum in `trusty-common::symgraph::contracts` BEFORE adding new variants. Phase 0 is a real refactor (#815); Phases 1–3 build on this unified foundation.
  - **Q2 (unknown-tag handling)** → **Option H (hybrid).** `"custom:"`-prefixed tags round-trip as `Custom(s)`; bare unrecognized tags are dropped with counter + observable in `/graph/stats` (version-skew guard). Fully resolves #816 for custom-contributed edges and detects accidental tag corruption.
  - **Q3 (long-term direction)** → **RESOLVED: Yes, converge to single canonical enum.** Q1's Option C commits to exactly this. Q3 is no longer open.
  - **Q4 (community T-SQL/C# extractor)** → **DEFERRED.** Wire contract is defined by this ADR and ADR-0009; accept-vs-document decision deferred for later judgment on maintenance burden.
- **Scope:** Workspace-wide (`trusty-common::symgraph::contracts`, `trusty-search` persistence + query surface, `trusty-analyze::types::graph`)
- **Supersedes / Superseded by:** —
- **Decided in:** epic [#814](https://github.com/bobmatnyc/trusty-tools/issues/814), children [#817](https://github.com/bobmatnyc/trusty-tools/issues/817), [#818](https://github.com/bobmatnyc/trusty-tools/issues/818); source Discussion #580
- **Required before:** ADR-0009 (PR #1082) — the vocabulary defined here is the one that ADR-0009's ingest contract references

---

## DECISIONS MADE (Accepted 2026-06-11)

> These questions were resolved and the decisions are now baked into the ADR scope.

**Q1 — Convergence scope for the three diverged EdgeKind enums (DECIDED: Option C)**

The issue inventory found three diverged `EdgeKind` enums at different abstraction levels:
- `trusty-common::symgraph::contracts::EdgeKind` — 17 variants, the load-bearing KG in trusty-search with score multipliers and redb persistence.
- `trusty-analyze::types::graph::KgEdgeKind` — 11 variants, trusty-analyze's independent KG (no traversal, no shared storage); different naming convention (`Calls` vs `CallsFunction`, no `TestedBy` family).
- `trusty-common::symgraph::graph::EdgeKind` — 3 variants (`Calls`/`Imports`/`Contains`), the basic SymbolGraph used for caller/callee queries.

**Decision: Option C — Full convergence first (#815), then add variants.**
Unify the three enums into a single canonical `enum EdgeKind` in `trusty-common::symgraph::contracts` before adding `Reads`, `Writes`, `AccessesResource`. This eliminates future drift and establishes a stable vocabulary for ADR-0009's ingest contract. Phase 0 is a real refactor; Phases 1–3 build on this unified foundation. Migration/back-compat for existing persisted graphs is documented in Phase 0.

**Q2 — Permissive vs. allowlist for unknown `Custom` tags (DECIDED: Option H)**

When `edge_kind_from_tag` encounters a tag it does not recognize in the redb corpus (version skew from a future release, a typo from a buggy external extractor):

**Decision: Option H — Hybrid unknown-tag handling.**
- `"custom:"`-prefixed tags parse to `Custom(s)` and round-trip perfectly (extractor-contributed edges persist).
- Bare unrecognized tags are dropped with counter and made observable in `/graph/stats` (version-skew guard + typo detection).

This fully resolves #816 for custom-contributed edges and distinguishes intentional custom kinds from accidental corruption.

**Q3 — Long-term relationship between `contracts::EdgeKind` and `KgEdgeKind` (RESOLVED)**

Issue #814 asks whether the two enums should converge.

**Decision: YES, converge to a single canonical enum in `trusty-common`.**
This is exactly what Q1's Option C commits to. The unified enum eliminates drift, provides a stable vocabulary for ADR-0009's ingest contract, and makes trusty-analyze's KG API coherent with trusty-search's persistence layer. Phase 0 implements this convergence.

**Q4 — Accept GrowthCurve community PR for the T-SQL/C# extractor (DEFERRED)**

The extractor is MIT-licensed, externally maintained, and the emit format will be defined by this ADR and ADR-0009. The question of accept-vs-document is deferred for later judgment on maintenance burden and community direction. No blocking dependency on this answer for the core work; the wire contract is defined by ADR-0009.

---

## Context

### Problem

The KG edge vocabulary in `trusty-search` can model call-graph and structural
relationships (17 variants in `contracts::EdgeKind`) but cannot express
data-flow or resource-access dependencies: "which function writes this global /
config key / cache entry?" (the highest-value impact-analysis query in any
language), "which handler reads this database table?", "which functions call
this stored procedure?". These relationships are language-agnostic — they span
SQL, HTTP endpoints, queues, config keys, and blob storage.

Without an extensible vocabulary, every new relationship type requires a core PR
against `trusty-common` plus a release before any external extractor (the
GrowthCurve T-SQL/C# tool, future endpoint/queue scanners) can contribute those
relations as data. This is the principal blocker to making trusty-search a
platform for contributed graph extractors (Discussion #580).

### Current state (ground-truthed at commit `ba0d5c56`)

**Three diverged EdgeKind enums across two crates:**

1. `crates/trusty-common/src/symgraph/contracts.rs:81-104` —
   `contracts::EdgeKind`, 17 variants; the load-bearing enum: wired to redb
   persistence via `edge_kind_tag()` / `edge_kind_from_tag()` in
   `crates/trusty-search/src/core/symbol_graph.rs:403-446`; powers
   `edge_kind_breakdown()` / `GET /indexes/{id}/graph/stats`. Score
   multipliers are NOT flat 0.70: `Implements`=0.85, `UsesType`=0.75,
   `TestedBy`=0.80, `Documents`=0.65, `ReferencesConcept`=0.60; all others
   default to 0.70.

2. `crates/trusty-analyze/src/types/graph.rs:98-113` —
   `KgEdgeKind`, 11 variants; trusty-analyze's independent enum for its
   language-adapter KG (`KgGraph` / `KgEdge`). No shared storage or traversal
   with trusty-search's graph. Already has `Calls`, `Implements`, `Extends`,
   `References`, `Tests`, `DependsOn` — different naming convention from
   `contracts::EdgeKind`.

3. `crates/trusty-common/src/symgraph/graph.rs:39-43` —
   `graph::EdgeKind`, 3 variants (`Calls`/`Imports`/`Contains`); the basic
   `SymbolGraph` used for in-memory caller/callee queries; not persisted in
   redb.

**Persistence path for `contracts::EdgeKind`:** tags are string-encoded via
`edge_kind_tag()` → stored in redb adjacency rows → decoded by
`edge_kind_from_tag()` at warm-boot. An unknown tag currently causes a
`tracing::warn!` and the edge is silently dropped (#816's warm-boot drop bug).
No `Custom` variant exists; there is no escape hatch.

**Missing variants:** `Reads`, `Writes`, `AccessesResource` are absent from
all three enums.

### Why this is architecturally significant and costly to reverse

- The `edge_kind_tag()` / `edge_kind_from_tag()` pair is the **on-disk
  serialization contract** for every edge in every persisted KG. Adding
  variants is additive (safe); renaming or removing requires a migration.
- The `Custom(String)` escape hatch's serialization scheme (`"custom:<s>"`
  prefix vs. bare tag) determines whether old indexes containing custom edges
  remain readable after a downgrade, and whether the warm-boot drop bug is
  fully repaired or only partially.
- The convergence question (Q1/Q3) touches the public JSON API shape of both
  trusty-search's `/graph/stats` and trusty-analyze's KG endpoints. Changing
  those after adoption by external extractors (ADR-0009's ingest contract) has
  breakage costs.

---

## Decision

We establish an extensible edge-kind vocabulary by converging three diverged enums into a single canonical enum in `trusty-common`, then adding new data-flow variants and a custom escape hatch:

### Phase 0: Converge the three EdgeKind enums into a single canonical enum (issue #815)

Issue #815 merges `contracts::EdgeKind`, `KgEdgeKind`, and `graph::EdgeKind` into a unified `enum EdgeKind` in `trusty-common::symgraph::contracts`. This is a prerequisite to Phases 1–3. The new canonical enum:
- Unifies naming conventions (resolve `Calls` vs `CallsFunction` inconsistencies).
- Establishes a stable vocabulary for ADR-0009's ingest contract.
- Eliminates future drift between trusty-search persistence and trusty-analyze analysis.
- Handles back-compat for existing persisted graphs during convergence (redb tag mapping).

Estimated effort: **M** (1–2 days). Acceptance: three former enums consolidated, existing indexes still load (back-compat verified), `cargo test -p trusty-common -p trusty-search -p trusty-analyze` green.

### Phase 1: Add `Reads`, `Writes`, `AccessesResource` as first-class variants (issue #817)

These three variants are added to the unified canonical `enum EdgeKind` unconditionally:

Initial `score_multiplier` values (to be tuned after pilot data):

| Variant | Multiplier | Rationale |
|---|---|---|
| `Writes` | 0.90 | Highest-impact: "what mutates this state?" is the primary impact-analysis query; should rank above `Implements` (0.85) |
| `Reads` | 0.80 | High-value data-flow; matches `TestedBy` multiplier |
| `AccessesResource` | 0.75 | Cross-tier dependency; matches `UsesType` multiplier |

Persistence: `edge_kind_tag()` returns `"Reads"`, `"Writes"`, `"AccessesResource"` (PascalCase, matching existing convention). `edge_kind_from_tag()` recognises all three. Existing indexes with none of these tags are unaffected.

Estimated effort: **M** (1–2 days). Changes: `trusty-common` + `trusty-search` + `trusty-analyze` (to use the unified canonical enum). Acceptance: three variants present, round-trip test green, `/graph/stats` updated, `cargo test -p trusty-common -p trusty-search -p trusty-analyze` green, clippy clean, line-cap exit 0.

### Phase 2: Add `Custom(String)` escape hatch + fix warm-boot edge-drop (issues #818, #816)

`EdgeKind` gains a `Custom(String)` variant to let external extractors contribute relations as data without requiring a core PR per new relation type.

**Serialization (on-disk and wire):**
- `edge_kind_tag(Custom(s))` returns a `String` with prefix `"custom:"` + `s` (e.g. `"custom:reads_table"`, `"custom:calls_stored_proc"`). The `"custom:"` prefix is a permanent reserved namespace that guarantees no collision with future named variants (which will always be PascalCase without a colon).
- `edge_kind_from_tag(tag)`: if `tag.starts_with("custom:")` → `Custom(tag["custom:".len()..].to_owned())`. Under Option H (hybrid), bare unrecognized tags are counted and dropped (surfaced in `/graph/stats` as `unknown_edge_kinds_dropped: N`); only `"custom:"`-prefixed tags round-trip as `Custom`.
- `score_multiplier(Custom(_))` = 0.70 (conservative default; custom relations earn a named variant to get a tuned multiplier).
- `Custom(String)` derives `Hash`/`Eq` on the String payload so it works in `HashSet<(String, String, EdgeKind)>` dedup.

**Back-compat for existing indexes:** No existing on-disk tag starts with `"custom:"` (the prefix did not exist before). Existing indexes are unaffected; warm-boot reads them identically. Custom edges written by this release will be preserved across restarts. Downgrade path: a binary that does not know `Custom` will hit the unknown-tag warn-and-drop path for any `"custom:*"` edges; the counter makes the drop visible.

**API surface:** `GET /indexes/{id}/graph/stats` groups custom edge kinds by their full string label, e.g. `{ "custom:reads_table": 142, "custom:calls_sproc": 37 }`. Filter parameters in `GET /indexes/{id}/graph` accept the full `"custom:reads_table"` string as an edge-kind filter value.

**Warm-boot fix (Option H):**
- `"custom:"`-prefixed unknown tags → `Custom(s)` (fully preserved, round-trips correctly).
- Bare unrecognized tags → drop with `tracing::warn!` AND increment an `unknown_edge_kinds_dropped` counter (per-load, surfaced in `/graph/stats` and `GET /health`). This is a version-skew guard; it also catches typos from buggy external tools. Fully resolves #816 for custom-contributed edges.

Estimated effort: **M** (1–2 days). Changes: `trusty-common` + `trusty-search`. Acceptance: `Custom("foo")` round-trips; custom kinds appear in `/graph/stats` by label; unknown tag counter is observable; clippy clean; line-cap exit 0.

### Rust type design

```rust
// crates/trusty-common/src/symgraph/contracts.rs
// Unified canonical enum: consolidates contracts::EdgeKind, KgEdgeKind (trusty-analyze), and graph::EdgeKind.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    // ... existing 17 variants unchanged ...

    // Data-flow (new, Phase D)
    /// Why: express "this function reads this global / config key / cache entry".
    /// What: directed edge from reader symbol to the resource being read.
    /// Test: edge_kind_tag round-trip + score_multiplier in contracts tests.
    Reads,
    /// Why: top impact-analysis query — "what mutates this state?".
    /// What: directed edge from writer symbol to the resource being written.
    /// Test: edge_kind_tag round-trip + score_multiplier in contracts tests.
    Writes,
    /// Why: cross-tier resource dependency (HTTP endpoint, queue, blob storage).
    /// What: directed edge from caller symbol to the accessed resource node.
    /// Test: edge_kind_tag round-trip + score_multiplier in contracts tests.
    AccessesResource,

    // Escape hatch (new, Phase D)
    /// Why: lets external extractors contribute relations as data without a
    /// core PR per relation type. Custom relations earn a named variant to
    /// get a tuned score_multiplier; this is the conservative fallback.
    /// What: the inner String is the relation label (without the "custom:"
    /// prefix, which is added by edge_kind_tag and stripped by edge_kind_from_tag).
    /// Test: Custom("foo") round-trips through edge_kind_tag/from_tag;
    ///       Hash + Eq on the String payload.
    Custom(String),
}

impl EdgeKind {
    pub fn score_multiplier(&self) -> f32 {
        match self {
            EdgeKind::Writes => 0.90,
            EdgeKind::Implements => 0.85,
            EdgeKind::Reads => 0.80,
            EdgeKind::TestedBy => 0.80,
            EdgeKind::UsesType => 0.75,
            EdgeKind::AccessesResource => 0.75,
            EdgeKind::Documents => 0.65,
            EdgeKind::ReferencesConcept => 0.60,
            // Custom relations default to 0.70; earn a named variant for tuned scoring.
            _ => 0.70,
        }
    }
}

// crates/trusty-search/src/core/symbol_graph.rs
fn edge_kind_tag(kind: &EdgeKind) -> Cow<'static, str> {
    // NOTE: return type must widen to Cow<str> to handle the owned String case
    match kind {
        // ... existing 17 variants return &'static str as before ...
        EdgeKind::Reads => Cow::Borrowed("Reads"),
        EdgeKind::Writes => Cow::Borrowed("Writes"),
        EdgeKind::AccessesResource => Cow::Borrowed("AccessesResource"),
        EdgeKind::Custom(s) => Cow::Owned(format!("custom:{s}")),
    }
}

fn edge_kind_from_tag(tag: &str) -> Option<EdgeKind> {
    match tag {
        // ... existing variants unified from all three former enums ...
        "Reads" => Some(EdgeKind::Reads),
        "Writes" => Some(EdgeKind::Writes),
        "AccessesResource" => Some(EdgeKind::AccessesResource),
        s if s.starts_with("custom:") => {
            Some(EdgeKind::Custom(s["custom:".len()..].to_owned()))
        }
        // Option H (hybrid): bare unrecognized tags are dropped with counter + observable.
        // "custom:"-prefixed tags always round-trip as Custom(s).
        _ => None,
    }
}
```

**Impact on `edge_kind_tag` return type:** the current `edge_kind_tag` returns `&'static str`, which is incompatible with `Custom`'s owned string. The return type must change to `Cow<'static, str>`. All call sites that used the returned value as `&str` continue to work via `Deref`; call sites that stored it as `&'static str` (there are none in the current codebase — all callers call `.to_string()`) are unaffected. This is the only non-additive signature change.

**redb persistence impact:** edge rows are stored as `(src_symbol, kind_tag_string, tgt_symbol)`. The `kind_tag_string` column is an untyped `String`; no schema change is needed. A redb migration is NOT required for the additive variants. The `"custom:"` prefix namespace is reserved going forward and should be documented in the migration comments. During Phase 0 convergence, back-compat for existing persisted graphs is handled via redb tag mapping (existing tags from the three former enums are preserved on load; new unified enum serializes consistently going forward).

**`trusty-analyze` impact:** Phase 0 converges `KgEdgeKind` into the unified canonical enum as well. This makes trusty-analyze's in-memory `KgGraph` share the same vocabulary as trusty-search's persisted KG, eliminating future drift and enabling ADR-0009's ingest contract to work across both crates coherently.

---

## Phased Implementation Plan

The implementation sequence respects issue dependencies and implements decisions Q1=Option C (converge first) + Q2=Option H (hybrid unknown handling) + Q3=converge (unified in trusty-common):

**Phase 0: Enum convergence — unify three EdgeKind enums (issue #815)**

Merge `trusty-common::symgraph::contracts::EdgeKind` (17 variants), `trusty-analyze::types::graph::KgEdgeKind` (11 variants), and `trusty-common::symgraph::graph::EdgeKind` (3 variants) into a single canonical `enum EdgeKind` in `trusty-common::symgraph::contracts`. This consolidation:
- Resolves naming conflicts (e.g., `Calls` vs `CallsFunction`).
- Unifies the vocabulary for ADR-0009's ingest contract.
- Eliminates long-term drift between trusty-search persistence and trusty-analyze analysis.
- Updates trusty-analyze to use the unified enum in its `KgGraph` and JSON API.

**Dependency:** none. **Unblocks:** Phases 1, 2, 3.

**Effort:** M (1–2 days). **Changes:** `trusty-common`, `trusty-search`, `trusty-analyze`.

**Acceptance:** all three former enums consolidated into one; existing indexes still load (back-compat verified); existing persisted tags still parse (redb mapping); `cargo test -p trusty-common -p trusty-search -p trusty-analyze` green; clippy clean; line-cap exit 0.

---

**Phase 1: First-class data-flow variants (issue #817)**

Add `Reads`, `Writes`, `AccessesResource` as first-class variants to the unified canonical `enum EdgeKind` with:
- Tuned `score_multiplier` values (Writes=0.90, Reads=0.80, AccessesResource=0.75).
- `edge_kind_tag` / `edge_kind_from_tag` entries.
- Round-trip save→load tests.
- Updated `/graph/stats` to include new-kind counts.

**Dependency:** Phase 0 complete. **Effort:** M (1–2 days). **Changes:** `trusty-common` + `trusty-search`.

**Acceptance:** three new variants present; round-trip test green; `/graph/stats` reports counts per kind; `cargo test -p trusty-common -p trusty-search` green; clippy clean; line-cap exit 0.

---

**Phase 2: Custom escape hatch + warm-boot edge-drop fix (issues #818, #816)**

Add `Custom(String)` variant to the unified enum; widen `edge_kind_tag` to `Cow<'static, str>`; implement `"custom:"` prefix serialization and Option H hybrid unknown-tag handling:
- `"custom:"`-prefixed tags round-trip as `Custom(s)`.
- Bare unrecognized tags are dropped with counter, made observable in `/graph/stats` and `GET /health` (version-skew guard + typo detection).
- `score_multiplier(Custom(_))` = 0.70 (conservative default).
- Add round-trip test for `Custom("foo")`.

**Dependency:** Phase 1 complete (variant naming convention established). **Effort:** M (1–2 days). **Changes:** `trusty-common` + `trusty-search`.

**Acceptance:** `Custom("foo")` round-trips through persistence; custom kinds appear in `/graph/stats` grouped by label; unknown-tag counter is observable; Option H behaviour confirmed (custom: round-trips, bare unknown dropped); clippy clean; line-cap exit 0.

---

**Phase 3: External-extractor ingest contract (ADR-0009, issue #819)**

Implement `POST /indexes/{id}/graph` endpoint + MCP tool. Requires Phases 1 and 2 vocabulary to be in place. See ADR-0009 (PR #1082) for the storage design (contributed overlay tables), identity model, and API schema.

**Dependency:** Phase 2 complete; ADR-0009 accepted. **Effort:** L (2–3 days). **Changes:** `trusty-search`.

---

## Consequences

**Positive:**
- The `contracts::EdgeKind` enum becomes the stable, versioned vocabulary for
  the full graph-extensibility epic. All three requested variants land as
  first-class with tuned multipliers, not as Custom strings.
- `Custom(String)` turns trusty-search into a platform for extractor-contributed
  relations without requiring a core PR per new relation type. This directly
  unblocks ADR-0009's ingest contract.
- The `"custom:"` prefix namespace separation means future named variants
  (assigned a PascalCase tag) are non-overlapping with extractor-minted custom
  kinds, with no migration required to promote a Custom variant to a named one.
- The warm-boot drop bug (#816) is addressed (fully or partially, per Q2
  resolution); either way, the behaviour becomes observable via the counter.

**Negative:**
- `edge_kind_tag`'s return type changes from `&'static str` to `Cow<'static, str>`.
  This is a non-additive change to a private function in `trusty-search`; it is
  not part of any public API. All current call sites are `.to_string()` consumers
  and are unaffected.
- `Custom(String)` makes `EdgeKind` non-`Copy` (it already derives `Clone` but
  not `Copy`; the String payload rules out `Copy`). The current code does not
  derive or rely on `Copy` for `EdgeKind` so this is not a breaking change.
- Score multiplier for `Custom` is a conservative flat 0.70. Custom relations
  from external extractors will rank below named variants until they are promoted
  to named variants or the flat default is tuned per-query.
- Adding three new named variants is an additive on-disk change. A database
  written by a binary containing these variants and then read by an older binary
  will have edges silently dropped for the three new variant tags. This is the
  pre-existing warm-boot drop behavior (#816); the counter added in Phase 2
  makes the drops visible.

**Neutral / follow-up:**
- `graph::EdgeKind` (3-variant basic SymbolGraph enum) is unchanged. If Q1
  Option C (full convergence) is chosen in a future ADR, it would supersede the
  Option A scope taken here.
- The `"custom:"` prefix is permanently reserved in the on-disk format and
  should be documented in the schema migration comments added in Phase 2.
- Promoting a frequently-used `Custom("reads_table")` kind to a named variant
  (e.g., `ReadsTable`) in a future release is pure-additive: the new tag `"ReadsTable"`
  replaces `"custom:reads_table"` on write. A one-time migration would backfill
  existing custom-tagged edges; alternatively, `edge_kind_from_tag` can retain
  both spellings during a transition window.

---

## References

- Epic [#814](https://github.com/bobmatnyc/trusty-tools/issues/814) (extensible KG relationship model); Discussion #580
- [#817](https://github.com/bobmatnyc/trusty-tools/issues/817) (Reads/Writes/AccessesResource first-class variants)
- [#818](https://github.com/bobmatnyc/trusty-tools/issues/818) (Custom(String) escape hatch)
- [#816](https://github.com/bobmatnyc/trusty-tools/issues/816) (warm-boot edge drop for unrecognized kinds)
- [#815](https://github.com/bobmatnyc/trusty-tools/issues/815) (converge EdgeKind enums)
- [ADR-0009 / PR #1082](https://github.com/bobmatnyc/trusty-tools/pull/1082) (external-extractor ingest contract, durable contributed overlay)
- `crates/trusty-common/src/symgraph/contracts.rs` (current `EdgeKind`, 17 variants, `score_multiplier`)
- `crates/trusty-search/src/core/symbol_graph.rs` (`edge_kind_tag`, `edge_kind_from_tag`, `save_to_corpus`, `load_from_corpus`)
- `crates/trusty-analyze/src/types/graph.rs` (`KgEdgeKind`, 11 variants, no persistence)
- `crates/trusty-common/src/symgraph/graph.rs` (`graph::EdgeKind`, 3 variants, basic SymbolGraph)
