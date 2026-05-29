# NLP Named Entity Recognition (ER) Indexing for trusty-search Knowledge Graph

*Researched: 2026-05-09 | Status: Canonical Reference*

## Executive Summary

trusty-search's current KG is a `SymbolGraph` (petgraph) capturing `callers_of` / `callees_of` relationships from tree-sitter AST traversal. A richer NLP entity layer — extracting typed entities from identifiers, doc comments, type signatures, error literals, and test annotations — adds five to eight new edge categories and materially improves RRF-fused ranking for Usage, Conceptual, and BugDebt query intents.

This document is the canonical reference for the NLP ER initiative. Implementation is tracked via GitHub issues #16–#25.

---

## 1. Entity Taxonomy

### Tier 1 — High-Value, Low-Cost (tree-sitter text nodes, zero new deps)

| Entity Type | Examples | Extraction Source |
|---|---|---|
| `NamedType` | `Arc<RwLock<T>>`, `Vec<CodeChunk>` | tree-sitter `type_identifier` nodes |
| `TraitBound` | `Send + Sync`, `Serialize + Deserialize` | tree-sitter `trait_bounds` |
| `ModulePath` | `crate::indexer::CodeIndexer` | `use_declaration`, qualified path nodes |
| `ErrorVariant` | `anyhow::Error`, thiserror enum variants | `macro_invocation`, `enum_variant` nodes |
| `TestRelation` | functions referenced in `#[test]` bodies | `attribute` + identifier resolution |
| `DocConcept` | noun phrases in `///` doc comments | `line_comment` / `block_comment` nodes |
| `Annotation` | `#[derive(Debug, Clone)]`, `@Override` | `attribute_item`, `decorator` |
| `LiteralString` | `"usearch reserve failed"`, log strings | `string_literal` nodes (len > 10) |

### Tier 2 — Moderate-Value, Moderate-Cost (pattern matching + heuristics)

| Entity Type | Examples | Extraction Technique |
|---|---|---|
| `ConceptCluster` | "authentication", "caching" inferred topic | TF-IDF + fastembed clustering (linfa k-means) |
| `TypeAlias` | `type IndexId = String` | `type_alias` AST node |
| `ConstantSymbol` | `INITIAL_CAPACITY`, `K=60` | `const_item` + SCREAMING_SNAKE_CASE heuristic |
| `ExternalCrate` | `usearch`, `fastembed`, `petgraph` | `Cargo.toml` + `use` tree top-level |
| `FeatureFlag` | `#[cfg(feature = "...")]` | `cfg_attr` nodes |
| `Lifetime` | `'a`, `'static` | `lifetime` nodes |

### Tier 3 — High-Value, Higher-Cost (ONNX NER or embedding classification)

| Entity Type | Examples | Technique |
|---|---|---|
| `SemanticRole` | "validates input", "persists state" | MiniLM embed + k-NN classification |
| `NaturalLanguagePhrase` | "BM25 algorithm", "cosine distance" | distilbert-NER via ONNX (`ort` crate) |
| `ErrorCategory` | "network error", "capacity exceeded" | fastembed clustering of `LiteralString` |

---

## 2. Proposed KG Edge Types

### Phase A — tree-sitter derivable

| Edge Type | Semantics | Query Intent | Score Multiplier |
|---|---|---|---|
| `Implements` | `struct` → `trait` | Definition, Usage | 0.85 |
| `TypeContains` | type alias expansion | Definition | 0.80 |
| `UsesType` | fn parameter/return type dep | Usage, Definition | 0.75 |
| `Derives` | `#[derive(...)]` | Usage | 0.70 |
| `ModuleContains` | module → symbol membership | Definition | 0.80 |
| `ReExports` | `pub use` origin → target | Definition | 0.75 |
| `RaisesError` | fn → ErrorVariant | BugDebt | 0.85 |
| `Configures` | cfg_attr → symbol | BugDebt | 0.70 |

### Phase B — test relationship edges

| Edge Type | Semantics | Query Intent | Score Multiplier |
|---|---|---|---|
| `TestedBy` | production fn → test fn | BugDebt, Usage | 0.80 |
| `TestUsesFixture` | test fn → fixture/helper | Usage | 0.65 |
| `CoOccursInTest` | fn A ↔ fn B in same test | Conceptual | 0.55 |

### Phase C — doc and semantic edges

| Edge Type | Semantics | Query Intent | Score Multiplier |
|---|---|---|---|
| `Documents` | doc comment → fn/struct | Conceptual | 0.65 |
| `ReferencesConcept` | fn → ConceptCluster | Conceptual | 0.60 |
| `Aliases` | type alias both directions | Definition | 0.80 |
| `ErrorDescribes` | LiteralString → fn/struct | BugDebt | 0.70 |

### Phase D — SCIP/LSP-derived (v2.0)

| Edge Type | Semantics |
|---|---|
| `ResolvesTo` | generic `T` → concrete type |
| `Overrides` | method → parent trait method |
| `CrossFileRef` | import-resolved cross-file reference |

---

## 3. Intent-Gated KG Traversal

| Query Intent | Edge Types to Traverse |
|---|---|
| `Definition` | `Implements, TypeContains, Aliases, ModuleContains` |
| `Usage` | `CallersOf, TestedBy, CoOccursInTest, UsesType` |
| `Conceptual` | `ReferencesConcept, Documents` |
| `BugDebt` | `RaisesError, ErrorDescribes, Configures` |
| `Unknown` | `CallersOf, ReferencesConcept` |

---

## 4. Extraction Techniques Ranked by Rust Feasibility

1. **tree-sitter text node extraction** — zero new deps, in-stack. Run `EntityExtractor` on same parse tree as chunker.
2. **Regex heuristics** — `regex` crate already in-stack. Pattern-match doc phrases, error strings, module paths.
3. **fastembed semantic clustering** — `FastEmbedder` already in-stack + `linfa-clustering` (pure Rust). K-means over doc embeddings.
4. **ONNX Runtime NER** — `ort` (transitive dep via fastembed) + `tokenizers = "0.20"`. distilbert-NER INT8 ~17MB. v1.0.
5. **SCIP ingest** — `scip` crate (Sourcegraph protobuf). Offline CI-generated indexes. v2.0.

---

## 5. RRF Fusion Interaction

### Entity-match RRF lane
When query exactly matches `NamedType`/`ModulePath` entity: inject rank-1 entry at `beta * 1.5`. Active for `Definition` and `Unknown` intents only.

### BM25 virtual-term expansion
At index-time, store `virtual_terms: Vec<String>` alongside each chunk. At query-time BM25 construction, concatenate `chunk.content` tokens with `chunk.virtual_terms`. Expands vocabulary without changing RRF logic.

### Edge-type-aware KG scoring
Replace flat 70% KG score multiplier with per-edge-type multipliers (see table above).

---

## 6. Relevant Rust Crates

### Already in-stack

| Crate | Role |
|---|---|
| `tree-sitter` + grammars | Entity extraction from AST |
| `fastembed 5` | ConceptCluster embedding |
| `petgraph 0.6` | SymbolGraph with EdgeKind |
| `redb 2.6` | Entity/edge persistence |
| `regex` | Pattern-based entity extraction |
| `rayon` | Parallel entity extraction |

### Recommended additions

| Crate | Version | Role |
|---|---|---|
| `tokenizers` | `0.20` | HuggingFace BPE for ONNX NER |
| `ort` | `2` | ONNX Runtime (already transitive via fastembed) |
| `linfa` + `linfa-clustering` | `0.7` | K-means for ConceptCluster |
| `toml` | `0.8` | Parse Cargo.toml for ExternalCrate |

---

## 7. Reference System Comparison

### Sourcegraph SCIP
Separates extraction (offline, per-language indexers: scip-rust, scip-python) from search. Defines clean `CodeEntityIndex` trait interface. trusty-search should adopt the same separation: native tree-sitter extractor OR SCIP file reader satisfying the same trait.

### GitHub Copilot code graph
Distinguishes structural edges (call graph, import graph — cheap, precise) from semantic edges (conceptual similarity — model-derived). Vector search handles semantic similarity; KG handles structural. Maps directly to trusty-search's BM25+HNSW+KG design.

### LSP symbol tables
`SymbolKind` enum (25 kinds) is a useful vocabulary for `NamedType`/`ModulePath` entities. LSP `textDocument/references` gives exact usage locations. Limitation: request-response per-symbol makes bulk extraction expensive. SCIP offline approach preferred.

**Key design principle**: All reference systems separate structural entities (deterministic, syntax-derived, cheap) from semantic entities (probabilistic, model-derived, expensive). trusty-search follows the same: Phase A/B = structural (tree-sitter), Phase C/D = semantic (embeddings/ONNX).

---

## 8. Implementation Roadmap

| Phase | Version | Tickets | Dependencies |
|---|---|---|---|
| Schema | v0.2 | #16 (RawEntity/EdgeKind) | None |
| Structural extraction | v0.2 | #17 (EntityExtractor), #18 (SymbolGraph edges) | #16, #4 (tree-sitter chunker) |
| BM25/RRF enrichment | v0.3 | #19 (virtual terms), #20 (entity-match lane), #21 (classifier) | #17 |
| Semantic clustering | v0.4 | #22 (ConceptCluster) | #17 + linfa |
| ONNX NER | v1.0 | #23 (distilbert-NER) | ort + tokenizers |
| SCIP ingest | v2.0 | #24 (SCIP interface) | scip crate |
| Benchmarks | ongoing | #25 (benchmark harness) | all phases |

---

## 9. redb Table Schema

```
TABLE "entities"        : entity_id (String) → RawEntity (bincode)
TABLE "entity_edges"    : (from_id, edge_type, to_id) → score (f32)
TABLE "chunk_entities"  : chunk_id → Vec<entity_id>
TABLE "entity_chunks"   : entity_id → Vec<chunk_id>
```

---

*This document is maintained as the canonical reference for the NLP ER initiative. Update when research findings evolve or implementation reveals new constraints.*
