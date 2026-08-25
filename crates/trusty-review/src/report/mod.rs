//! Deterministic technical-DD report generation (M1, epic #2312 / #2313).
//!
//! Why: `trusty-review` expands beyond PR review to generate CAST-style
//! technical due-diligence reports by repository inspection.  M1 is the
//! deterministic foundation: a TOML manifest names the target repositories, the
//! pipeline enriches local checkouts with git provenance, consumes pre-produced
//! trusty-analyze metrics, and fills a bundled template — with a strict honesty
//! rule and NO LLM synthesis (that is M2).
//! What: this module wires together the manifest loader, the git enricher, the
//! v0 metrics schema, the template loader, the deterministic fill engine, the
//! assembled report model, and the reporter (markdown + JSON atomic output).
//! Gated behind the `report` Cargo feature (default-on).
//! Test: each submodule carries its own unit-test section; an end-to-end render
//! lives in `crates/trusty-review/tests/report_e2e.rs`.

pub mod analyze_adapter;
// #6041: per-endpoint paths, request budgets, and partial-fetch vocabulary.
pub mod analyze_endpoints;
// #6082: the per-finding half of the analyze mapping, split out under the cap.
pub(crate) mod analyze_findings;
pub mod analyze_scope;
// #5453/#6004: the tga-authored authorship artifact, receiving half.
pub mod authorship;
pub mod benchmark;
// #6004: exec-summary jump-list — post-render anchor-link injection.
pub mod contents_links;
pub mod error;
pub mod exec_summary;
pub mod figures;
pub mod fill;
pub mod git_info;
pub mod instructions;
pub mod investigate;
pub mod manifest;
pub mod mermaid;
pub mod metrics;
pub mod model;
pub mod polish;
pub mod provenance;
pub mod redact;
pub mod reporter;
// #5453/#6004: Authorship & Key-Person Risk deterministic fill.
pub mod reporter_authorship;
// #6004: Code Quality & Architecture / Security Posture deterministic fill.
pub mod reporter_codesec;
// #6004: engagement-wide Key Facts block.
pub mod reporter_facts;
pub mod reporter_fill;
pub mod reporter_findings;
pub mod reporter_graph_datasets;
// #6004: fixed Performance & Scalability gap text.
pub mod reporter_performance;
pub mod scan;
// #5747: the schema-tag parse both artifact loaders decide compatibility from.
pub(crate) mod schema;
pub mod section_instructions;
// #6046: the code-review / authorship output split.
pub mod split;
pub mod synthesize;
pub mod synthesize_digest;
pub mod synthesize_guard;
// #6082 lap 4: the claim-level companion to `synthesize_guard`'s numeric check —
// prompt hints plus a fail-closed post-check for reachability and load-bearing
// claims. Used only by `synthesize` and `synthesize_prompt`.
pub(crate) mod synthesize_grounding;
mod synthesize_grounding_text;
mod synthesize_grounding_vocab;
mod synthesize_guardrail;
// #6009 shape 3: whitelist-based synonym normalization, used only by
// `synthesize::parse_raw` — not part of the public report API.
pub(crate) mod synthesize_normalize;
pub mod synthesize_prompt;
pub mod template;
// #5405: the board-correlation figures tga hands over beside the manifest.
pub mod ticketing;
/// #6147: the crate graph trusty-audit measures, rendered into the Code Quality
/// & Architecture section and stated to synthesis.
pub mod topology;

// ── Re-exports for convenience ─────────────────────────────────────────────

pub use analyze_adapter::{
    AnalyzeAdapterError, AnalyzeCaveat, AnalyzeFetch, AnalyzeGap, AnalyzeMetricsSource,
    HttpAnalyzeMetricsSource, derive_index_id, enrich_with_analyze, enrich_with_analyze_gaps,
};
pub use benchmark::{
    BenchmarkReport, BenchmarkStatus, CorpusSnapshot, LoadedCorpus, MetricPlacement,
    RepositoryBenchmark, build_benchmark_report, corpus_dir, load_corpus, write_snapshot,
};
pub use error::{ManifestError, ReportError, Result};
pub use exec_summary::{DeterministicRisk, ExecSummary, TopRisks, compose as compose_exec_summary};
pub use fill::{HONESTY_MARKER, Scope, render, strip_leading_comment};
pub use git_info::{GitInfo, gather_git_info};
pub use instructions::{
    Instructions, MANIFEST_INSTRUCTIONS_FILE, discover_manifest_instructions, load_instructions,
};
pub use investigate::{
    Budget, Investigation, InvestigationStatus, RepoInvestigation, run_investigation,
};
pub use manifest::{
    Manifest, ReportSection, RepositoryEntry, RepositorySource, load_manifest, parse_manifest,
    slugify,
};
pub use mermaid::inject as inject_mermaid;
pub use metrics::{AnalyzeMetrics, MetricFinding, Severity, load_metrics};
pub use model::{ReportModel, RepositoryReport};
pub use polish::{polish, polish_with_gaps, strip_template_comments};
pub use provenance::{Provenance, tag};
pub use redact::{report_secrets, scrub_finding, scrub_investigation, scrub_metrics, scrub_prose};
pub use reporter::{RenderedReports, Reporter};
pub use scan::{Framework, RepoScan, scan_repo};
pub use split::{authorship_document, split_authorship};
pub use synthesize::{FindingProse, RiskRow, Synthesis, SynthesisError, Synthesizer};
pub use template::{DEFAULT_TEMPLATE, TemplateLoader, parse_section_instructions};
pub use ticketing::{TicketingSummary, load_ticketing};
