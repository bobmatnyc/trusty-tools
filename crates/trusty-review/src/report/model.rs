//! Assembled report model — manifest + git info + metrics (M1, #2313).
//!
//! Why: the renderer and the JSON output both consume a single resolved model
//! that has already gathered every deterministic data source (git provenance for
//! local checkouts, pre-produced trusty-analyze metrics per repo).  Building this
//! once keeps the reporter pure (model → strings) and makes the JSON output a
//! faithful, testable record of everything the report was derived from.
//! What: [`ReportModel`] holds report-level metadata and a [`RepositoryReport`]
//! per manifest entry; [`ReportModel::build`] performs the enrichment.  No LLM
//! calls and no network fetch (remote entries are recorded as declared).
//! Test: `reporter_tests.rs` builds a model from a fixture manifest + metrics.

use std::path::Path;

use serde::Serialize;

use super::error::Result;
use super::git_info::{GitInfo, gather_git_info};
use super::manifest::{Manifest, RepositoryEntry, RepositorySource};
use super::metrics::{AnalyzeMetrics, load_metrics};

/// The fully assembled, render-ready report model.
///
/// Why: a single source of truth for both the markdown fill and the JSON output;
/// serializing this struct yields the report's machine-readable twin.
/// What: report-level fields (title, chosen template, analyst, dates, manifest
/// provenance) plus one [`RepositoryReport`] per repository.
/// Test: `reporter_tests.rs::build_model_from_manifest`.
#[derive(Debug, Clone, Serialize)]
pub struct ReportModel {
    /// Report title (used as the target codename and output slug seed).
    pub title: String,
    /// The template name chosen for rendering.
    pub template: String,
    /// Optional analyst name recorded in the metadata section.
    pub analyst: Option<String>,
    /// Report date (ISO-8601, generation day).
    pub report_date: String,
    /// Generation timestamp date (ISO-8601, generation day).
    pub generated_date: String,
    /// The manifest file path this report was generated from.
    pub manifest_path: String,
    /// One report per repository, in manifest order.
    pub repositories: Vec<RepositoryReport>,
    /// Optional M2 LLM synthesis result (present only under `--synthesize`).
    ///
    /// Why: recording the synthesis outcome on the model keeps the JSON twin a
    /// faithful record of what the LLM produced (and what the guardrail
    /// rejected); the reporter reads this to inject narrative prose and to render
    /// the visible `synthesis:` status note.  `None` = deterministic M1 output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<super::synthesize::Synthesis>,
    /// Optional M3 cross-repo benchmark placement (present only under `--benchmark`).
    ///
    /// Why: recording the benchmark outcome on the model keeps the JSON twin a
    /// faithful record of the percentile/quartile placement (and any corpus load
    /// warnings or small-n gating); the reporter reads this to fill the benchmark
    /// table and render the visible `benchmark:` status note.  `None` = no
    /// benchmarking requested (M2-identical output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmark: Option<super::benchmark::BenchmarkReport>,
}

/// Per-repository resolved data mapped to one `per_application` block.
///
/// Why: each repository contributes one application section; bundling its
/// identity, source, git provenance, and metrics keeps the reporter's scope
/// construction a straight field-to-placeholder mapping.
/// What: identity (`name`/`slug`), the declared source, optional git provenance
/// (local checkouts only), the declared username/ref, and optional metrics.
/// Test: `reporter_tests.rs::build_model_from_manifest`.
#[derive(Debug, Clone, Serialize)]
pub struct RepositoryReport {
    /// Human-readable application name.
    pub name: String,
    /// Filesystem/URL-safe slug.
    pub slug: String,
    /// Compact source origin string (path or remote ref).
    pub source: String,
    /// Source kind discriminant (`local_path` or `remote`).
    pub source_kind: String,
    /// Username declared for remote access/attribution, if any.
    pub username: Option<String>,
    /// Declared branch/tag/commit to analyze, if any.
    pub git_ref: Option<String>,
    /// Git provenance for local checkouts (`None` for remote or non-git paths).
    pub git_info: Option<GitInfo>,
    /// Pre-produced trusty-analyze metrics for this repo, if a path was given.
    pub metrics: Option<AnalyzeMetrics>,
}

impl ReportModel {
    /// Build a resolved report model from a validated manifest.
    ///
    /// Why: centralises all deterministic enrichment (git + metrics) so the
    /// reporter stays a pure transform and the CLI handler is a thin wrapper.
    /// What: for each repository — records the source; for local sources gathers
    /// git info; loads metrics from the declared path (resolved relative to the
    /// manifest directory when relative).  `report_date`/`generated_date` are the
    /// current local date.  No network, no LLM.
    /// Test: `reporter_tests.rs::build_model_from_manifest`.
    pub fn build(manifest: &Manifest, manifest_path: &Path, template: &str) -> Result<Self> {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));

        let mut repositories = Vec::with_capacity(manifest.repositories.len());
        for entry in &manifest.repositories {
            repositories.push(build_repository(entry, manifest_dir)?);
        }

        Ok(ReportModel {
            title: manifest.report.title.clone(),
            template: template.to_string(),
            analyst: manifest.report.analyst.clone(),
            report_date: today.clone(),
            generated_date: today,
            manifest_path: manifest_path.display().to_string(),
            repositories,
            synthesis: None,
            benchmark: None,
        })
    }
}

/// Resolve one manifest entry into a [`RepositoryReport`].
///
/// Why: keeps `ReportModel::build` readable by isolating per-entry enrichment.
/// What: records source kind/origin, gathers git info for local checkouts, and
/// loads metrics (relative paths resolved against `manifest_dir`).
/// Test: exercised by `reporter_tests.rs::build_model_from_manifest`.
fn build_repository(entry: &RepositoryEntry, manifest_dir: &Path) -> Result<RepositoryReport> {
    let (source_kind, git_info) = match &entry.source {
        RepositorySource::LocalPath { path } => {
            let resolved = resolve(manifest_dir, path);
            ("local_path", gather_git_info(&resolved))
        }
        RepositorySource::Remote { .. } => ("remote", None),
    };

    let metrics = match &entry.metrics {
        Some(p) => Some(load_metrics(&resolve(manifest_dir, p))?),
        None => None,
    };

    Ok(RepositoryReport {
        name: entry.name.clone(),
        slug: entry.slug.clone(),
        source: entry.source.describe(),
        source_kind: source_kind.to_string(),
        username: entry.username.clone(),
        git_ref: entry.git_ref.clone(),
        git_info,
        metrics,
    })
}

/// Resolve a possibly-relative path against the manifest directory.
///
/// Why: manifest authors write paths relative to the manifest file; absolute
/// paths must pass through unchanged.
/// What: returns `path` as-is when absolute, else `manifest_dir.join(path)`.
/// Test: exercised by `reporter_tests.rs` (metrics loaded via relative path).
fn resolve(manifest_dir: &Path, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest_dir.join(path)
    }
}
