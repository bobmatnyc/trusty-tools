//! Analysis primitives for trusty-analyzer.
//!
//! Operates on `crate::types::CodeChunk` corpora fetched from the trusty-search
//! daemon over HTTP. No direct database access — the search daemon is the
//! authoritative source of chunk data.
//!
//! Modules:
//! - [`complexity`]: cyclomatic / cognitive complexity, code smell detection
//! - [`blame`]: temporal decay scoring (the search daemon does the actual
//!   `git log -L`; this crate just consumes the wire-format `ChunkBlame`)
//! - [`concept_cluster`]: k-means clustering helpers (label-only; no embedder
//!   dependency in this crate — callers supply embeddings)
//! - [`facts`]: redb-backed canonical fact store, owned by the analyzer
//! - [`client`]: HTTP client for fetching chunks/index summaries from
//!   trusty-search

pub mod blame;
pub mod call_resolver;
pub mod client;
pub mod complexity;
pub mod complexity_ts;
pub mod concept_cluster;
pub mod explain;
pub mod facts;
pub mod github;
pub mod linker;
pub mod ner;
pub mod quality;
pub mod refactor;
pub mod registry;
pub mod review;
pub mod scip;
pub mod tool_impls;
pub mod tool_registry;
pub mod tools;

pub use client::{IndexSummary, TrustySearchClient};
pub use complexity::compute_complexity_for;
pub use concept_cluster::{bow_embedding, cluster, ClusterResult, ConceptCluster};
pub use explain::{
    build_explain_prompt, deep_analysis, explain_report, render_text as render_deep_analysis_text,
    DeepAnalysisError, DeepAnalysisReport,
};
pub use facts::FactStore;
pub use github::{
    fetch_pr_diff, format_review_as_markdown, post_pr_comment, verify_webhook_signature,
    GithubError, GithubPrRequest,
};
pub use linker::link;
pub use ner::{extract_doc_comments, NerExtractor};
pub use refactor::{analyze as analyze_refactor, RefactorSuggestion, RefactorType, Severity};
pub use registry::{record_frameworks, AnalyzerRegistry};
pub use review::{
    analyze_diff_with_chunks, analyze_diff_with_client, render_text as render_review_text,
    ReviewError, ReviewReport,
};
pub use scip::{extract_kg_from_scip, index_to_graph as scip_index_to_graph, ScipIngestSummary};
pub use tool_registry::{global_registry, ToolRegistry};
pub use tools::{Severity as DiagnosticSeverity, StaticTool, ToolDiagnostic};

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
pub(crate) mod test_utils;
