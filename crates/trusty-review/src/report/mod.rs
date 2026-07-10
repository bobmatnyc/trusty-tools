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

pub mod error;
pub mod fill;
pub mod git_info;
pub mod manifest;
pub mod metrics;
pub mod model;
pub mod reporter;
pub mod template;

// ── Re-exports for convenience ─────────────────────────────────────────────

pub use error::{ManifestError, ReportError, Result};
pub use fill::{HONESTY_MARKER, Scope, render};
pub use git_info::{GitInfo, gather_git_info};
pub use manifest::{
    Manifest, ReportSection, RepositoryEntry, RepositorySource, load_manifest, parse_manifest,
    slugify,
};
pub use metrics::{AnalyzeMetrics, MetricFinding, Severity, load_metrics};
pub use model::{ReportModel, RepositoryReport};
pub use reporter::Reporter;
pub use template::{DEFAULT_TEMPLATE, TemplateLoader};
