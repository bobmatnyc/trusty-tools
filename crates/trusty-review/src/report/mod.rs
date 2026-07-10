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

pub mod benchmark;
pub mod error;
pub mod fill;
pub mod git_info;
pub mod manifest;
pub mod metrics;
pub mod model;
pub mod reporter;
pub mod synthesize;
pub mod synthesize_guard;
pub mod synthesize_prompt;
pub mod template;

// ── Re-exports for convenience ─────────────────────────────────────────────

pub use benchmark::{
    BenchmarkReport, BenchmarkStatus, CorpusSnapshot, LoadedCorpus, MetricPlacement,
    RepositoryBenchmark, build_benchmark_report, corpus_dir, load_corpus, write_snapshot,
};
pub use error::{ManifestError, ReportError, Result};
pub use fill::{HONESTY_MARKER, Scope, render, strip_leading_comment};
pub use git_info::{GitInfo, gather_git_info};
pub use manifest::{
    Manifest, ReportSection, RepositoryEntry, RepositorySource, load_manifest, parse_manifest,
    slugify,
};
pub use metrics::{AnalyzeMetrics, MetricFinding, Severity, load_metrics};
pub use model::{ReportModel, RepositoryReport};
pub use reporter::Reporter;
pub use synthesize::{FindingProse, RiskRow, Synthesis, SynthesisStatus, Synthesizer};
pub use template::{DEFAULT_TEMPLATE, TemplateLoader};
