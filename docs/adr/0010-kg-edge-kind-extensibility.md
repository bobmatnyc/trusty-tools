# 0010. KG Edge-Kind Extensibility — First-Class Data-Flow Variants + Custom Escape Hatch

- **Status:** Proposed
- **Date:** 2026-06-10
- **Scope:** Workspace-wide (`trusty-common::symgraph::contracts`, `trusty-search` persistence + query surface, `trusty-analyze::types::graph`)
- **Supersedes / Superseded by:** —
- **Decided in:** epic [#814](https://github.com/bobmatnyc/trusty-tools/issues/814), children [#817](https://github.com/bobmatnyc/trusty-tools/issues/817), [#818](https://github.com/bobmatnyc/trusty-tools/issues/818); source Discussion #580
- **Required before:** ADR-0009 (PR #1082) — the vocabulary defined here is the one that ADR-0009's ingest contract references

---

## OPEN QUESTIONS — NEED HUMAN DECISION BEFORE IMPLEMENTATION

> The following questions must be resolved before any child issue is assigned.
> They are listed in priority order; Q1 blocks Q2–Q4.

**Q1 — Convergence scope for the three diverged EdgeKind enums (prerequisite to #817/#818, gates #815):**
The issue inventory found three diverged `EdgeKind` enums at different abstraction levels:
- `trusty-common::symgraph::contracts::EdgeKind` — 17 variants, the load-bearing KG in trusty-search with score multipliers and redb persistence.
- `trusty-analyze::types::graph::KgEdgeKind` — 11 variants, trusty-analyze's independent KG (no traversal, no shared storage); different naming convention (`Calls` vs `CallsFunction`, no `TestedBy` family).
- `trusty-common::symgraph::graph::EdgeKind` — 3 variants (`Calls`/`Imports`/`Contains`), the basic SymbolGraph used for caller/callee queries.

**Option A — Add `Reads`/`Writes`/`AccessesResource` only to `contracts::EdgeKind`** (minimal scope: trusty-search's load-bearing enum is the one that matters for persistence and ADR-0009's ingest surface; postpone convergence with trusty-analyze). Low risk, fast.

**Option B — Extend both `contracts::EdgeKind` and `KgEdgeKind`** (matching variants in trusty-analyze's graph type so future language adapters can emit `Reads`/`Writes` natively). Increases scope of #817 and couples trusty-common and trusty-analyze changes.

**Option C — Full convergence first (#815) then add variants** (one canonical enum, remove the divergence). Cleanest long-term; largest immediate scope.

Decision needed: Which option? The ADR proposes Option A for velocity, with Option B as a documented follow-up, but this is the most consequential choice of the epic.

---

**Q2 — Permissive vs. allowlist for unknown `Custom` tags (gates #818, also fixes #816's warm-boot drop bug):**
When `edge_kind_from_tag` encounters a tag it does not recognize in the redb corpus (version skew from a future release, a typo from a buggy external extractor):

**Option P — Permissive:** any unknown tag → `Custom(tag)`. Fixes the warm-boot drop bug (#816) as a side-effect. Simpler; no configuration. Risk: corrupted/typo'd tags from buggy tools silently survive in the graph.

**Option L — Per-index allowlist:** unknown tags → drop (with counter, surfaced in `/graph/stats`) unless they appear in a per-index `custom_edge_kinds: ["reads_table", ...]` configuration. Version-skew for known future variants still drops, which may surprise operators after a downgrade.

**Option H — Hybrid (proposed):** permissive for `"custom:"`-prefixed tags (the ADR-0009 ingest surface controls these); drop for unrecognized bare tags (version-skew guard stays). This separates extractor-contributed custom kinds from accidental tag corruption.

Decision needed: Which option? The distinction matters for the redb table compatibility section and whether #816 is fully resolved or partially resolved by #818.

---

**Q3 — Long-term relationship between `contracts::EdgeKind` and `KgEdgeKind` (informs Q1 option C):**
Issue #814 asks whether the two enums should converge. Options:

**Option S — Keep separate, document divergence.** `contracts::EdgeKind` is trusty-search's persistence-layer vocabulary; `KgEdgeKind` is trusty-analyze's in-memory analysis vocabulary. They can coexist as long as each has a clear owner and the boundary is documented.

**Option M — Merge into a single canonical enum in `trusty-common`** (behind a feature flag or in the `symgraph` module). Eliminates drift; requires trusty-analyze to take a `symgraph` feature dep on trusty-common. Affects the `KgEdge.kind: KgEdgeKind` field in the public JSON API surface of trusty-analyze.

Decision needed: Guidance on direction helps size #815 and determines whether it is a prerequisite blocker or a nice-to-have cleanup.

---

**Q4 — Accept GrowthCurve community PR for the T-SQL/C# extractor (#814 Q5)?**
The extractor is MIT-licensed, externally maintained, and the emit format will be defined by this ADR and ADR-0009. The question is whether to accept it as a community contribution into this repo or document the wire contract and let it live in a separate repo. No blocking dependency on this answer for the core work.

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

> **This ADR is Proposed. Sections marked "[OPEN QUESTION]" are placeholders
> pending human resolution of Q1–Q4 above before implementation.**

We will make the following changes to establish an extensible edge-kind
vocabulary:

### 1. Add `Reads`, `Writes`, `AccessesResource` as first-class variants in `contracts::EdgeKind`

These three variants are added to `trusty-common::symgraph::contracts::EdgeKind`
unconditionally (no feature flag). **[OPEN QUESTION Q1: whether to also add
matching variants to `KgEdgeKind` in trusty-analyze is deferred — see Q1 options
above.]**

Proposed initial `score_multiplier` values (to be tuned after pilot data):

| Variant | Multiplier | Rationale |
|---|---|---|
| `Writes` | 0.90 | Highest-impact: "what mutates this state?" is the primary impact-analysis query; should rank above `Implements` (0.85) |
| `Reads` | 0.80 | High-value data-flow; matches `TestedBy` multiplier |
| `AccessesResource` | 0.75 | Cross-tier dependency; matches `UsesType` multiplier |

Persistence: `edge_kind_tag()` returns `"Reads"`, `"Writes"`, `"AccessesResource"` (PascalCase, matching existing convention). `edge_kind_from_tag()` recognises all three. Existing indexes with none of these tags are unaffected.

### 2. Add `Custom(String)` escape hatch variant

`contracts::EdgeKind` gains a `Custom(String)` variant.

**Serialization (on-disk and wire):**
- `edge_kind_tag(Custom(s))` returns a `String` with prefix `"custom:"` + `s` (e.g. `"custom:reads_table"`, `"custom:calls_stored_proc"`). The `"custom:"` prefix is a permanent reserved namespace that guarantees no collision with future named variants (which will always be PascalCase without a colon).
- `edge_kind_from_tag(tag)`: if `tag.starts_with("custom:")` → `Custom(tag["custom:".len()..].to_owned())`. **[OPEN QUESTION Q2: handling of unrecognized bare tags — see Q2 options above.]** Under the proposed Option H, bare unrecognized tags are counted and dropped (surfaced in `/graph/stats` as `unknown_edge_kinds_dropped: N`); only `"custom:"`-prefixed tags round-trip as `Custom`.
- `score_multiplier(Custom(_))` = 0.70 (conservative default; custom relations earn a named variant to get a tuned multiplier).
- `Custom(String)` derives `Hash`/`Eq` on the String payload so it works in `HashSet<(String, String, EdgeKind)>` dedup.

**Back-compat for existing indexes:** No existing on-disk tag starts with `"custom:"` (the prefix did not exist before). Existing indexes are unaffected; warm-boot reads them identically. Custom edges written by this release will be preserved across restarts. Downgrade path: a binary that does not know `Custom` will hit the unknown-tag warn-and-drop path for any `"custom:*"` edges (#816's counter, if implemented, makes the drop visible).

**API surface:** `GET /indexes/{id}/graph/stats` groups custom edge kinds by their full string label, e.g. `{ "custom:reads_table": 142, "custom:calls_sproc": 37 }`. Filter parameters in `GET /indexes/{id}/graph` accept the full `"custom:reads_table"` string as an edge-kind filter value.

### 3. Fix warm-boot edge-drop for unrecognized tags (#816)

**[OPEN QUESTION Q2 governs the full fix vs. partial fix.]**

Under the proposed Option H, the fix is:
- `"custom:"`-prefixed unknown tags → `Custom(s)` (fully preserved, round-trips correctly).
- Bare unrecognized tags → drop with `tracing::warn!` AND increment an `unknown_edge_kinds_dropped` counter (per-load, surfaced in `/graph/stats` and `GET /health`). This is a version-skew guard; it also catches typos from buggy external tools.

Under Option P (full permissive), all unknown tags become `Custom(tag)` — simpler, #816 fully resolved.

Under Option L (allowlist), configuration is needed and the fix is conditional on configuration.

### 4. Proposed Rust type design

```rust
// crates/trusty-common/src/symgraph/contracts.rs
// (additions only; existing variants unchanged)

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
        // ... existing 17 variants ...
        "Reads" => Some(EdgeKind::Reads),
        "Writes" => Some(EdgeKind::Writes),
        "AccessesResource" => Some(EdgeKind::AccessesResource),
        s if s.starts_with("custom:") => {
            Some(EdgeKind::Custom(s["custom:".len()..].to_owned()))
        }
        // [OPEN QUESTION Q2]: Option P would add:
        //   unknown => Some(EdgeKind::Custom(unknown.to_owned()))
        // Option H (proposed) keeps the None branch here with a warn + counter.
        _ => None,
    }
}
```

**Impact on `edge_kind_tag` return type:** the current `edge_kind_tag` returns `&'static str`, which is incompatible with `Custom`'s owned string. The return type must change to `Cow<'static, str>`. All call sites that used the returned value as `&str` continue to work via `Deref`; call sites that stored it as `&'static str` (there are none in the current codebase — all callers call `.to_string()`) are unaffected. This is the only non-additive signature change.

**redb persistence impact:** edge rows are stored as `(src_symbol, kind_tag_string, tgt_symbol)`. The `kind_tag_string` column is an untyped `String`; no schema change is needed. A redb migration is NOT required for the additive variants. The `"custom:"` prefix namespace is reserved going forward and should be documented in the migration comments.

**`trusty-analyze::KgEdgeKind` impact:** **[OPEN QUESTION Q1].** Under Option A (proposed default), no changes are made to `KgEdgeKind` in this ADR. Trusty-analyze's in-memory `KgGraph` does not share storage with trusty-search's persisted KG, so there is no compatibility coupling. A follow-up ticket adds `Reads`/`Writes`/`AccessesResource` to `KgEdgeKind` when language adapters are ready to emit them.

---

## Phased Implementation Plan

The implementation sequence respects issue dependencies:

**Phase 0: Convergence decision (prerequisite — Q1 and Q3)**
Issue: #815. Complete the three-enum inventory, document the boundary between
`contracts::EdgeKind` (trusty-search persistence vocabulary), `KgEdgeKind`
(trusty-analyze analysis vocabulary), and `graph::EdgeKind` (basic SymbolGraph).
No code change required under Option A; a clear decision record suffices.
Dependency: none. Unblocks #817 and #818.

**Phase 1: First-class data-flow variants (#817)**
Add `Reads`, `Writes`, `AccessesResource` to `contracts::EdgeKind` with
`score_multiplier`, `edge_kind_tag`, and `edge_kind_from_tag` entries.
Add save→load round-trip test. Update `/graph/stats` to include the new kinds.
Dependency: Phase 0 decision.
Estimated effort: M (1–2 days). Changes: `trusty-common` + `trusty-search`.
Acceptance: three variants present, round-trip test green, `graph/stats` updated,
`cargo test -p trusty-common -p trusty-search` green, clippy clean, line-cap exit 0.

**Phase 2: Custom escape hatch + warm-boot fix (#818, #816)**
Add `Custom(String)` variant; widen `edge_kind_tag` to `Cow<'static, str>`;
implement `"custom:"` prefix serialization; fix (or partially fix, per Q2
resolution) the warm-boot drop bug; add `unknown_edge_kinds_dropped` counter
to `/graph/stats`. Add round-trip test for `Custom("foo")`.
Dependency: Phase 1 (variant naming convention established).
Estimated effort: M (1–2 days). Changes: `trusty-common` + `trusty-search`.
Acceptance: `Custom("foo")` round-trips; custom kinds appear in `/graph/stats`
by label; Q2 decision reflected in warm-boot behaviour; clippy clean; line-cap
exit 0.

**Phase 3: External-extractor ingest contract (ADR-0009, #819)**
`POST /indexes/{id}/graph` endpoint + MCP tool. Requires Phase 1 and Phase 2
vocabulary to be in place. See ADR-0009 (PR #1082) for the storage design
(contributed overlay tables), identity model, and API schema.
Dependency: Phase 2 complete; ADR-0009 accepted.

**Optional follow-up: `KgEdgeKind` matching variants (depends on Q1/Q3)**
Add `Reads`/`Writes`/`AccessesResource` to `trusty-analyze::types::graph::KgEdgeKind`
so language adapters can emit them natively. Tracked as a new child issue if
Option B or C is chosen for Q1.

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
