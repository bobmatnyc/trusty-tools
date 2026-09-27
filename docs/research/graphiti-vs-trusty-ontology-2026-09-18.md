# Graphiti versus Trusty: practical ontology improvements

Research date: 2026-09-18. Analysis and proposal; no implementation or runtime benchmark.

Tracking: [master epic #8278](https://github.com/bobmatnyc/trusty-tools/issues/8278).

## Recommendation

Extend Trusty's existing KB schema into a versioned, configurable ontology with deterministic validation, stable entity identity, and explicit taxonomy queries. Retain the current storage systems. Borrow Graphiti's schema-driven extraction and entity-resolution interfaces selectively, after measuring extraction quality.

The useful gap is the contract connecting types, relationships, extraction, and queries. A new graph database is not required to address it. Graphiti's ontology is principally a developer-defined extraction schema; its source does not establish a general-purpose taxonomy reasoner.

## Scope and evidence

Trusty source inspected at `b961fd3abb92a29c7dd89f3b0e9af9ae3c55ad49`; the checkout was clean at the start. Graphiti source was cloned for inspection at [`de8eb5b896c05ed1b5b329d4cb52015446d65e21`](https://github.com/getzep/graphiti/tree/de8eb5b896c05ed1b5b329d4cb52015446d65e21). Conclusions below are source-level observations, not claims about installed services or comparative performance. The configured trusty-search index was unavailable, so discovery used direct source inspection.

There is no `trusty-graph` package in this checkout. Three existing systems matter:

| System | Responsibility | Relevant foundation |
|---|---|---|
| `trusty-kb`, exposed through `trusty-agents` OKG | Curated, per-assistant knowledge | Markdown entities, frontmatter types and attributes, relationship vocabulary, inverse relationships, source records |
| `trusty-common::memory_core` / `trusty-memory` | Memory facts and recall | Embedded redb temporal triples, confidence, provenance, history, petgraph traversal |
| `trusty-common::symgraph` / `trusty-search` | Code structure | Symbol identities and code-specific edges; a separate graph contract |

Share schema vocabulary where useful, but preserve these stores' ownership, scope, and lifecycle. The OKG reader explicitly separates assistant knowledge from the memory-palace graph: [OKG reader](../../crates/trusty-agents/src/stores/okg_graph.rs). Code-graph separation is also recorded in [ADR-0038](../adr/0038-kg-stays-additive-recall-gated-on-extraction-quality.md).

## What Graphiti actually offers

1. **Custom entity types and attributes.** Callers supply named Pydantic models. Type descriptions guide classification; fields provide structured attribute-extraction schemas. Nodes have UUID identity, labels, summaries, and attribute dictionaries. See [node operations](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/utils/maintenance/node_operations.py) and [node models](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/nodes.py).

2. **Custom relationship types and endpoint signatures.** `edge_types` and `edge_type_map` describe relationships and applicable source/target labels. These definitions guide extraction and the selection of edge attribute models. They are not a universally enforced database constraint: the extraction prompt explicitly permits deriving a new relation name when no supplied type fits. See [edge operations](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/utils/maintenance/edge_operations.py) and [relation-type prompt rules](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/prompts/extract_edges.py#L163-L166).

3. **Entity resolution during ingestion.** Semantic candidate retrieval is followed by deterministic matching and LLM resolution for unresolved cases. This is richer than string normalization, but still needs evaluation for false merges. See [`resolve_extracted_nodes`](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/utils/maintenance/node_operations.py#L627).

4. **Temporal facts and source lineage.** Relationship records contain episode IDs, `created_at`/`expired_at`, and `valid_at`/`invalid_at`; extraction and reconciliation use episode context. This distinguishes when a fact applies from when the system recorded or expired it. See [edge models](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/edges.py#L263).

5. **Type-aware retrieval.** Search filters expose node labels, relationship types, properties, and temporal fields. See [search filters](https://github.com/getzep/graphiti/blob/de8eb5b896c05ed1b5b329d4cb52015446d65e21/graphiti_core/search/search_filters.py).

Do not equate these features with OWL reasoning, SKOS concept management, inherited subtype queries, or governed automatic ontology evolution. I did not find those capabilities in the inspected core. Pydantic model inheritance alone would not establish graph-level subclass semantics. The allowance for emergent relationship names is useful flexibility, not evidence of a complete learned-taxonomy system.

## Comparison with the current Trusty source

| Capability | Current Trusty | Practical gap |
|---|---|---|
| Typed entities | KB has a required `type`, six default collections, extensible frontmatter, optional `uid`, `same_as`, and aliases | Configurable type definitions, typed domain attributes, and a consistently enforced identity contract |
| Relationship vocabulary | KB has fixed inverse/symmetric pairs and per-collection documented verbs; memory accepts string predicates | One relationship registry with endpoint types, cardinality, inverse rules, and unknown-type policy |
| Validation | KB lint checks parsing, missing type, selected dangling links, slug mismatch, and duplicate aliases | Attribute shapes, allowed types, endpoint compatibility, cardinality, and taxonomy cycles |
| Taxonomy | Collections/types and tags organize knowledge; memory can store `is-a` as a plain predicate | Distinct instance/subclass semantics and bounded subtype-aware queries |
| Extraction | Memory has deterministic phrase/tag extraction; KB ingestion maintains source items and entities | Schema-guided extraction of domain entities, relationships, and attributes |
| Identity | KB has optional identity fields and alias metadata; memory commonly uses string endpoints | Stable IDs across rename, collision-aware resolution, and reviewed merges |
| Time | Memory stores `valid_from`/`valid_to` and closed history | Separate recorded time, evidence-backed corrections, and a defined historical-query contract |
| Provenance | Memory has one optional free-form provenance string; KB has source metadata and ingestion ledgers | Multiple structured evidence links per fact, including exact source revision/span |
| Querying | Memory supports neighbors, paths, reachability, and active facts; OKG exposes definitions and triples | Type/subtype, relationship, attribute, source, and time filters with explicit semantics |

Primary Trusty evidence: [KB profile](../../crates/trusty-kb/src/schema.rs), [KB validator](../../crates/trusty-kb/src/validate.rs), [KB reconciliation](../../crates/trusty-kb/src/reconcile.rs), [memory Triple](../../crates/trusty-common/src/memory_core/store/kg/types.rs), [predicate cardinality](../../crates/trusty-common/src/memory_core/store/kg_store.rs), [graph traversal](../../crates/trusty-common/src/memory_core/store/kg/graph.rs), and [deterministic extractor](../../crates/trusty-memory/src/kg_extract.rs).

### Two concrete consistency issues to resolve first

**Relationship recognition differs across consumers.** The exposed OKG reads any non-envelope frontmatter field containing wiki-links as a relationship. KB reconciliation and frontmatter-link validation recognize fields through the smaller `inverse_edge` table. For example, `founder` appears in the organization profile and can appear in the exposed graph, but has no entry in the inverse table. A relationship can therefore be visible without receiving the same validation coverage. Separate “is a relationship” from “has an inverse”; use one registry in every consumer.

**Identity metadata is ahead of link resolution.** The KB envelope supports aliases and `uid`, but the inspected `resolve_link` implementation searches filenames by slug across collections and returns the first match. It does not consult those identity fields. Alias-aware and collection-aware resolution should precede semantic deduplication. These are source observations, not runtime-reproduced bug reports.

## Proposed design

### 1. Versioned ontology profiles

Evolve `trusty-kb::schema::Profile` rather than introduce a second independent ontology. Initially support a bounded schema language in TOML or YAML:

- Entity types: stable type ID, description, optional parent types, declared attributes, required/optional status, scalar/list shape, and enum values.
- Relationships: stable predicate ID, source and target types, cardinality, optional inverse, symmetry, and explicitly declared transitivity.
- Profile identity and version; strict or permissive unknown-field/type policy.
- A legacy default matching existing behavior. Existing trees must remain readable.

Use the same definitions for validation, tool schema discovery, extraction instructions, and query filtering. Keep the schema model independent of storage adapters; move it into a shared module only when a second consumer needs it. Do not force the code graph or memory graph into the KB lifecycle.

### 2. Small, explicit taxonomy semantics

Distinguish `instance_of(entity, type)` from `subclass_of(child_type, parent_type)`. Add `broader`/`narrower` only for concept organization, with their meaning kept separate from logical subclassing. Aliases identify names for one entity; they must not imply subclassing.

Example: declare `HotelCompany` a subtype of `Organization`, and classify an entity as `HotelCompany`. A query for organizations with `include_subtypes=true` should return it and explain the type path. This is an illustrative domain example, not an assertion about existing stored data.

Reject subclass cycles; bound traversal; invalidate cached closure when a profile changes. Derive inverse and transitive results at query time initially, or mark materialized results explicitly as derived so retraction cannot leave unexplained facts behind. This provides useful taxonomy behavior beyond what was established for Graphiti's core.

### 3. Deterministic validation before semantic extraction

Have `kb_validate` report unknown types/predicates, invalid attribute values, disallowed endpoints, cardinality conflicts, ambiguous identity, and cycles. Reuse that validator on writes. Start in report-only mode for existing trees and enforce strict mode for opted-in profiles.

Make single-valued replacement a declared policy, not an accidental consequence of inserting a new object. In memory, preserve the shipped `FUNCTIONAL_PREDICATES` behavior until a versioned migration explicitly changes it. The redb writer and derived adjacency must consult the same policy.

### 4. Stable identity and structured evidence

Build on existing KB `uid` and `same_as` fields. Use scoped stable IDs for endpoints, with titles and aliases as labels. Resolution order should be exact ID/authority key, unambiguous scoped alias, then optional semantic candidates. Ambiguity must not silently merge records.

Add structured evidence records with source item/drawer ID, revision/hash, optional source span, extraction method, profile version, and confidence. Several sources should support one fact without overwriting each other's provenance. Source withdrawal should remove that support, not automatically erase a fact still supported elsewhere. Preserve assistant and palace boundaries.

### 5. Optional schema-guided enrichment

Keep structured imports and deterministic extraction first. For unresolved prose, an optional background extractor can produce proposed typed facts using the profile. Pass its output through the same validator and retain rejected/candidate results separately from accepted knowledge.

Treat new type or predicate suggestions as proposals requiring explicit promotion into a profile version. Do not let extraction silently expand the accepted vocabulary. Record cost and latency by source and profile. This is the Graphiti feature most worth borrowing after the deterministic foundation is solid.

### 6. Temporal extension only when a use case requires it

Full temporal parity is larger than taxonomy work. Add recorded time separately from valid time only with concrete questions such as “what did we believe on Tuesday about Friday?” Include late-arriving evidence, corrections, overlapping claims, and single-valued versus multi-valued relationships in the contract.

The memory storage format uses positional postcard records. Simply adding optional Rust fields is not a sufficient migration plan. Use versioned records or additive tables, compatibility fixtures, backup/rollback, and rebuildable derived indexes. Do not infer historical recorded timestamps from current validity fields.

## Delivery sequence and acceptance criteria

Sizes below are relative implementation scope, not calendar estimates.

| Phase | Scope | Completion evidence |
|---|---|---|
| A: schema consistency — small/medium | Unify relationship recognition; resolve IDs and ambiguous links consistently | A non-inverse relation gets the same visibility and validation everywhere; alias collisions never select an arbitrary entity |
| B: ontology profile — medium | Versioned type/attribute/relation definitions, schema discovery, optional strict validation | Invalid endpoint/attribute rejected before write; permissive legacy fixtures round-trip unchanged |
| C: taxonomy queries — medium | Subclass DAG, explicit instance typing, subtype filters and explanations | Parent queries return subtype instances; cycles fail; rename preserves edges; derived results retract correctly |
| D: evidence and extraction pilot — medium/large | Structured evidence, source-linked candidate extraction, deterministic-first resolution | Idempotent re-ingestion; multi-source support survives one withdrawal; measured extraction precision and false-merge rate |
| E: temporal parity — large | Separate recorded/valid time and correction history | Late-arriving and contradictory evidence produce correct results for both time dimensions; migration and restart tests pass |

For the pilot, label a representative sample of real sources and queries before tuning. Compare the current approach, deterministic ontology-aware extraction, and optional model enrichment. Measure entity/type precision, relationship precision, false merges, unsupported assertions, retrieval answer quality at a fixed context budget, ingestion cost, and query latency. Schema validation proves structural correctness; it does not prove an extracted claim is true.

## Existing decisions and backlog

[ADR-0038](../adr/0038-kg-stays-additive-recall-gated-on-extraction-quality.md) keeps the memory KG additive in recall and gates expanded KG-derived injection on quality evidence. Ontology validation and explicit KB querying can improve independently. The proposed extraction pilot does not authorize bypassing that recall gate.

Some historical problem statements in that ADR are no longer current: the inspected store already uses `(subject, predicate, object)` keys, and includes active-subject enumeration. Do not re-propose those as missing work or reuse its old corpus percentages as fresh measurements.

Existing open issue [#4283](https://github.com/bobmatnyc/trusty-tools/issues/4283), “OKG entity extraction: pull entities from attached search indexes into the canonical per-assistant OKG store,” already covers OKG extraction, evidence, and retraction. Phase D should integrate and evaluate that work, preserving its existing parent #4007, rather than duplicate it.

Related issue [#766](https://github.com/bobmatnyc/trusty-tools/issues/766), for canonical typed entity extraction during dreaming, is **closed as not planned**, not implemented according to its closure comment. It was closed during the September 2 backlog sweep and explicitly permits reopening if wanted. Its memory-drawer taxonomy is related to phase D, but does not substitute for the KB domain ontology.

Issue [#8252](https://github.com/bobmatnyc/trusty-tools/issues/8252), “feat(trusty-agents): use Oxigraph for OKG storage and queries,” is also **closed as not planned**. That proposal included stable identity and source lifecycle work, but its closure is not evidence of implementation. This analysis keeps database replacement outside the proposed scope.

## Decision

Proceed with A–C as a focused extension of existing Trusty technology. They make curated knowledge more reliable and queryable without requiring model calls. Fund D against measured extraction and retrieval benefit. Defer E until historical queries justify its storage and conflict-resolution complexity. Do not adopt Graphiti wholesale or replace redb to obtain these ontology features.
