//! Report reporter — model → scope → markdown + JSON, atomic write (M1, #2313).
//!
//! Why: the reporter is the deterministic rendering layer: it maps the resolved
//! [`ReportModel`] onto template placeholders/blocks, renders markdown, and
//! writes a `{slug}.md` / `{slug}.json` pair atomically so a concurrent reader
//! never sees a half-written file.  All fill is deterministic — no LLM (M1).
//! What: [`Reporter`] holds the output directory; `render` builds the [`Scope`]
//! from the model and fills the template; `write` renders and persists both
//! outputs, returning their paths.  Unmapped placeholders fall through to the
//! honesty marker via the fill engine.
//! Test: `reporter_tests.rs` covers scope mapping, markdown substrings, JSON
//! round-trip, and the atomic-write file layout.

use std::path::{Path, PathBuf};

use tracing::info;

use super::error::{ReportError, Result};
use super::fill::{Scope, render};
use super::manifest::slugify;
use super::model::{ReportModel, RepositoryReport};

/// Renders a [`ReportModel`] to markdown + JSON and writes them atomically.
///
/// Why: separating rendering/output from model assembly lets tests render a
/// model without a filesystem and keeps the CLI handler thin.
/// What: `output_dir` is where `{slug}.md` and `{slug}.json` are written.
/// Test: `reporter_tests.rs::{render_contains_expected, write_emits_both}`.
pub struct Reporter {
    output_dir: PathBuf,
}

impl Reporter {
    /// Create a reporter writing to `output_dir`.
    ///
    /// Why: callers choose the output directory (`--out`, default `./reports`).
    /// What: stores the directory; it is created on `write` if absent.
    /// Test: `reporter_tests.rs::write_emits_both`.
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }

    /// Render the model into markdown using the supplied template source.
    ///
    /// Why: exposed separately so tests can assert on rendered markdown without
    /// touching disk.
    /// What: builds the fill [`Scope`] from the model and renders `template`.
    /// Test: `reporter_tests.rs::render_contains_expected`.
    pub fn render(&self, model: &ReportModel, template: &str) -> String {
        let scope = build_scope(model);
        render(template, &scope)
    }

    /// Render and write `{slug}.md` + `{slug}.json` atomically to `output_dir`.
    ///
    /// Why: the CLI's terminal step — persist both the human report and its
    /// machine twin so downstream tooling can consume the JSON.
    /// What: creates `output_dir`, renders markdown, serializes the model to
    /// pretty JSON, and writes each via a temp-file + rename (atomic on the same
    /// filesystem).  Returns the two written paths.
    /// Test: `reporter_tests.rs::write_emits_both`.
    pub fn write(&self, model: &ReportModel, template: &str) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.output_dir).map_err(|source| ReportError::Io {
            path: self.output_dir.clone(),
            source,
        })?;

        let stem = report_stem(model);
        let markdown = self.render(model, template);
        let json = serde_json::to_string_pretty(model).map_err(|source| ReportError::Metrics {
            path: PathBuf::from("<model.json>"),
            source,
        })?;

        let md_path = self.output_dir.join(format!("{stem}.md"));
        let json_path = self.output_dir.join(format!("{stem}.json"));
        atomic_write(&md_path, markdown.as_bytes())?;
        atomic_write(&json_path, json.as_bytes())?;
        info!(md = %md_path.display(), json = %json_path.display(), "report written");

        Ok(vec![md_path, json_path])
    }
}

/// Compute the output file stem for a report: `{date}-{title-slug}`.
///
/// Why: a date-prefixed slug matches the spec's example filenames and keeps
/// repeated runs chronologically ordered.
/// What: joins the generation date with the slugified title.
/// Test: `reporter_tests.rs::stem_is_date_slug`.
fn report_stem(model: &ReportModel) -> String {
    format!("{}-{}", model.generated_date, slugify(&model.title))
}

/// Write `bytes` to `path` atomically via a temp file + rename.
///
/// Why: a reader must never observe a partially written report.
/// What: writes to a temp file in the same directory, then persists (renames)
/// it over `path`; rename is atomic on the same filesystem.
/// Test: `reporter_tests.rs::write_emits_both` (file exists and parses).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| ReportError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    std::fs::write(tmp.path(), bytes).map_err(|source| ReportError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| ReportError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

/// Build the root fill [`Scope`] from a report model.
///
/// Why: this is the single place mapping model fields onto template placeholder
/// names; everything it does not set falls through to the honesty marker.
/// What: sets report-level scalars (codename, dates, analyst, applications list,
/// source provenance) and pushes one `per_application` child scope per repo.
/// Test: `reporter_tests.rs::render_contains_expected`.
fn build_scope(model: &ReportModel) -> Scope {
    let mut root = Scope::new();

    // Report metadata (report-level scalars).
    root.set("target_codename", model.title.clone());
    root.set("report_date", model.report_date.clone());
    root.set("analysis_generated_date", model.generated_date.clone());
    root.set_opt("analyst_name", model.analyst.clone());
    let source_ref = format!("repository inspection (manifest: {})", model.manifest_path);
    root.set("source_document_filename", source_ref.clone());
    root.set("source_document_reference", source_ref);

    let apps: Vec<String> = model.repositories.iter().map(|r| r.name.clone()).collect();
    if !apps.is_empty() {
        root.set("applications_list", apps.join(", "));
    }

    // One per_application block repetition per repository.
    for repo in &model.repositories {
        root.push_block("per_application", per_application_scope(repo));
    }

    root
}

/// Build the per-application child scope for one repository.
///
/// Why: maps a repository's deterministic data (git provenance + metrics) onto
/// the per-application placeholders; git fields are also emitted so a custom
/// template can surface provenance, while the bundled templates carry it in JSON.
/// What: sets app identity, tech stack / LoC / counts from metrics (when
/// present), and git branch/SHA/remote/dirty scalars; leaves scoring/health
/// factors unset (M1 has no scoring) so they render as honesty markers.
/// Test: `reporter_tests.rs::render_contains_expected`.
fn per_application_scope(repo: &RepositoryReport) -> Scope {
    let mut scope = Scope::new();
    scope.set("app_name", repo.name.clone());
    scope.set("app_slug", repo.slug.clone());
    scope.set("app_source", repo.source.clone());
    scope.set("app_source_kind", repo.source_kind.clone());
    scope.set_opt("app_username", repo.username.clone());
    scope.set_opt("app_git_ref", repo.git_ref.clone());

    if let Some(git) = &repo.git_info {
        scope.set("git_branch", git.branch.clone());
        scope.set("git_head_sha", git.head_sha.clone());
        scope.set_opt("git_origin_url", git.origin_url.clone());
        scope.set("git_dirty", if git.dirty { "dirty" } else { "clean" });
    }

    if let Some(metrics) = &repo.metrics {
        let langs = metrics.primary_languages(4);
        if !langs.is_empty() {
            scope.set("app_tech_stack", langs.join(", "));
        }
        if metrics.loc.total > 0 {
            scope.set("app_loc", metrics.loc.total.to_string());
        }
        scope.set(
            "app_file_counts",
            format!(
                "{} files, {} functions",
                metrics.counts.files, metrics.counts.functions
            ),
        );
    }

    scope
}

#[cfg(test)]
#[path = "reporter_tests.rs"]
mod tests;
