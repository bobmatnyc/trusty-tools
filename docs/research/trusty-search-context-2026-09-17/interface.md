# Trusty Search experiment interface

Design only. Source: `/tmp/trusty-search-experiment-research.md`. Frozen repository: `90b6aeb94`. Owner: experiment implementer. No production installation.

## Boundary and existing components

Use functions and the existing Embedder injection boundary; no new service abstraction or persistence schema. Keep `RawChunk.virtual_terms` as the persisted lexical context. Use the real HTTP router for measurements. Optional card output belongs to the experimental example/API adapter so existing HTTP and MCP response contracts remain intact.

Existing entry points: `core/indexer/helpers.rs::populate_virtual_terms`, `core/indexer/ingest/mod.rs` incremental and parallel bulk parse sites, `core/chunker/walk.rs::split_oversized`, `core/indexer/search/mod.rs` query/refinement embedding guards, `service/server/state_impl.rs::SearchAppState::{new,with_registry_path,with_allowlist_paths,with_embedder}`, and `service/server/mod.rs::build_router`.

## Frozen configuration

Proposed new module: `core/experiment.rs` (or equivalent small module selected by implementer).

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ExperimentConfig {
    pub context_words: usize,
    pub subchunk_window: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ExperimentConfigError {
    InvalidContextWords(String),
    InvalidSubchunkWindow(String),
}

pub fn parse_experiment_config(
    context_words: Option<&str>,
    subchunk_window: Option<&str>,
) -> Result<ExperimentConfig, ExperimentConfigError>;
pub fn experiment_config() -> &'static ExperimentConfig;
```

Environment names: `TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS` accepts only 0, 64, 128; default 0. `TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW` accepts only 100, 64; default 100. Resolve exactly once before serving requests or indexing. Invalid explicit values fail experiment startup. No mutation after resolution. Each treatment uses a fresh corpus/index and process. Defaults reproduce baseline source and window behavior. Tests inject explicit configurations into pure helpers rather than mutating process environment concurrently.

## Deterministic lexical context

```rust
pub fn enrich_chunk_context(
    chunks: &mut [RawChunk],
    file_content: &str,
    config: &ExperimentConfig,
);
```

Preconditions: chunks describe one file; ranges are one-based inclusive; existing entity virtual_terms have already been populated. Paths supplied to this helper are repository-relative. Context budget zero is a strict no-op. Append bounded source-derived context to existing terms without erasing them. Budget covers new context only. Stable ordering and deduplication are required. Chunk source, ranges, IDs, types, calls, entity graph, and parent/child links remain unchanged. Extracted documentation and immediate parent symbol/signature may contribute; no generated semantic claims. Missing documentation or parent yields less context, not an error. No whole-file TOC appended to every chunk. Bulk and incremental ingestion use this same helper. Rebuilding BM25 from persisted chunks must preserve terms and rankings.

## Chunk windows

```rust
pub(super) fn split_oversized_with_config(
    chunks: Vec<RawChunk>,
    config: &ExperimentConfig,
) -> Vec<RawChunk>;
```

Keep the existing maximum-parent threshold 200 lines. Baseline window/stride remain 100/50. Experimental window/stride are 64/32. Keep umbrella parent, make_sub ID grammar, line mapping, and child relationship contracts. Small chunks stay identical across configurations. The existing split_oversized signature may remain a wrapper using frozen config. Window tuning is independent of context_words, including the 0/64 cell. Do not treat renamed window IDs as comparable relevance labels; evaluate source range/symbol.

## Navigation cards

Suggested public Rust helper can live in the experiment module; it is consumed only by the example daemon adapter and tests.

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkLandmark {
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct NavigationCard {
    pub index_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub description: String,
    pub landmarks: Vec<ChunkLandmark>,
    pub score: f32,
    pub match_reason: String,
}

pub fn navigation_card(index_id: &str, chunk: &CodeChunk) -> NavigationCard;
```

Card construction does not rank, retrieve, alter scores, or read mutable source files. Description is extracted from returned source, bounded to 40 whitespace-delimited words. Landmarks are at most four actual nonblank source lines, each at most 120 Unicode characters. Prefer declarations/structural lines; fall back to signature or first nonblank line. Do not fabricate child boundaries unavailable in CodeChunk. Every landmark maps to its true absolute source line inside the chunk. An empty chunk produces an empty description and landmarks. Locator is index_id plus relative path and range. Null symbol is allowed. Keep raw/full retrieval available through the normal router. UTF-8 truncation never panics.

## Zero-vector behavior

No new public API required. In `core/indexer/search/mod.rs`, both initial query embedding and refine_query embedding must respect `self.skip_vector`. Existing lexical stage already skips embedding; extend that contract to graph queries on vector-disabled indexes. Indexing, query, and refinement must invoke neither Embedder method when skip_vector is true. Existing vector-enabled behavior remains unchanged. Tests cover graph query with and without refine_query, bulk indexing, and incremental replacement with an embedder that fails if called.

## Standalone HTTP runner

New example: `crates/trusty-search/examples/context_experiment_daemon.rs`. Compile separately as an example; do not modify or invoke production main startup.

```rust
struct RejectingEmbedder {
    calls: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[async_trait::async_trait]
impl Embedder for RejectingEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}

#[derive(Clone, serde::Serialize)]
struct ExperimentEvidence {
    config: ExperimentConfig,
    embedding_calls: u64,
    source_revision: String,
    data_dir: std::path::PathBuf,
    corpus_root: std::path::PathBuf,
}

async fn experiment_evidence(
    state: axum::extract::State<ExperimentRunnerState>,
) -> axum::Json<ExperimentEvidence>;

async fn main() -> anyhow::Result<()>;
```

`ExperimentRunnerState` is a small adapter state holding frozen config, rejecting-embedder counter, immutable source revision, data_dir and corpus_root. No additional trait is needed.

Both Embedder methods increment the counter then return an explicit embedding-forbidden error. Dimension is a valid nonzero store dimension; it does not initialize a model. Endpoint `GET /experiment/evidence` returns the counter and frozen configuration; completion requires zero calls. This is an experiment endpoint, not a production API change.

Runner configuration requires explicit marked disposable data directory and corpus copy, and an explicit non-default loopback port. Use local AllowlistPaths files and `with_registry_path` before passing SearchAppState to build_router. Attach only RejectingEmbedder, never a model or worker pool. No auto-discovery, production registry read, global allowlist write, or production daemon lifecycle operation. Register only the frozen copy with skip_vector=true and skip_kg=false. Expose normal real-router index and search endpoints. Publish its dedicated http_addr for existing benchmark guard validation. Reject invalid/mismatched roots rather than silently falling back.

Card output can be a standalone adapter route which invokes the same real search result and applies navigation_card; route shape is implementation choice but must preserve normal search endpoints. Evaluation may alternatively apply the same helper to saved real HTTP results via an example command, provided measured bytes are labelled as adapter output.

Startup errors use anyhow context for invalid config, non-disposable paths, non-loopback address, fixture I/O, and bind failure. No silent fallback to normal startup. Runtime rejected embedding must propagate as a failed measurement and nonzero counter.

## Acceptance and scope

Baseline default produces unchanged chunk source/IDs and existing ranking; cards alone preserve result order and scores. Candidate context persists across restart and matches bulk/incremental behavior. Valid raw source ranges and deterministic metadata hold for Unicode, docs-only, unsupported syntax, multiline signatures, empty source, and oversized functions. Frozen corpus/manifest and treatment configuration accompany every result. Evaluate 0/64/128 context budgets crossed with 100/64 windows, using lexical and graph queries with zero embedding calls. Tune on a fixed training set; select a configuration before held-out scoring.

This experiment does not add embeddings, LLM summaries, a production schema migration, new production search defaults, or deployment. It does not claim optimality beyond the measured grid and repository snapshot. Parent owns daemon isolation and benchmark labels; implementation agent owns code and validation under this interface.

## Evaluator interface (parent-owned Python driver)

```python
def evaluate(base_url: str, index: str, queries: list[dict], split: str, repeats: int) -> dict: ...
```

Returns JSON-serializable metrics, raw per-query outcomes, timing samples, readiness/configuration evidence, and embedding_calls. Query records carry id, text, expected relative files/symbols or ranges, split, mode (lexical/graph), and optional kg_seed_query. Lexical mode uses the real lexical stage. Graph mode resolves kg_seed_query by lexical search first, then sends graph text equal to the selected chunk_id, following the existing benchmark convention. Record the chosen seed and distinguish failed seed retrieval from graph expansion failure. Compare both end-to-end graph success and conditional-on-correct-seed results. Include seed lookup in end-to-end timing/tokens. Fixed seeds may be a separately labelled graph-only ablation, never substituted silently. repeats must be positive; fixture readiness and zero embedding calls are prerequisites. Parent owns this driver and orchestration; engineer owns Rust modules and example daemon.
