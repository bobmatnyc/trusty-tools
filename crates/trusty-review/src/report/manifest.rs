//! Report manifest: typed TOML schema, loader, and validation (M1, #2313).
//!
//! Why: report generation is manifest-driven — a single TOML file names the
//! target repositories, their source (a local checkout or a remote repo), and
//! optional per-repo metrics.  Making the schema a typed struct with a
//! `thiserror` loader means every malformed manifest fails loudly with a
//! correctable message instead of silently producing an empty report.
//! What: defines [`Manifest`], [`ReportSection`], [`RepositoryEntry`], and the
//! [`RepositorySource`] enum, plus [`load_manifest`] which reads TOML, validates
//! the exactly-one-of `path`/`remote` rule, and returns a resolved `Manifest`.
//! Test: `manifest_tests.rs` covers both source kinds, mutual-exclusion errors,
//! the empty-manifest error, orphaned-username handling, and slug derivation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::error::ManifestError;

// ─── Resolved (validated) manifest model ────────────────────────────────────

/// A fully validated report manifest ready to drive report generation.
///
/// Why: the resolved model guarantees the invariants the renderer relies on —
/// at least one repository, each with exactly one source — so downstream code
/// never re-checks them.
/// What: holds the `[report]` section and the validated repository list.
/// Test: produced by `load_manifest`; asserted in `manifest_tests.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    /// The top-level `[report]` metadata section.
    pub report: ReportSection,
    /// One or more validated repository entries (never empty after loading).
    pub repositories: Vec<RepositoryEntry>,
}

/// The `[report]` metadata section of a manifest.
///
/// Why: captures report-wide identity (title, chosen template, analyst) that is
/// independent of any single repository.
/// What: `title` is required; `template` and `analyst` are optional.  When
/// `template` is absent the CLI falls back to the default template name.
/// Test: `manifest_tests.rs::parse_local_path_entry`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReportSection {
    /// Human-readable report title (used as the target codename and slug seed).
    pub title: String,
    /// Optional bundled/override template name (e.g. `report-technical-dd-cast`).
    #[serde(default)]
    pub template: Option<String>,
    /// Optional analyst name recorded in the report metadata section.
    #[serde(default)]
    pub analyst: Option<String>,
    /// Optional client / deal-side name recorded in the report metadata section.
    ///
    /// Why: the client is a deal-side fact the tool cannot infer from a repo; when
    /// the manifest declares it the report records it (provenance: declared),
    /// otherwise the field is omitted and listed under Gaps & Caveats rather than
    /// rendered as a "not stated" row.
    #[serde(default)]
    pub client: Option<String>,
    /// Optional path to a free-form analyst instructions markdown file (#2340).
    ///
    /// Why: the analyst brief (focus areas, deal concerns, questions) is recorded
    /// verbatim in the report and injected as synthesis focus directives.  The
    /// `--instructions` CLI flag takes precedence over this key; a relative path
    /// is resolved against the manifest directory.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Optional benchmark corpus directory (M3).  Precedence: `--corpus` CLI flag
    /// wins over this; this wins over the per-user XDG default.  A relative path
    /// is resolved against the manifest directory.
    #[serde(default)]
    pub corpus: Option<String>,
}

/// One validated repository entry mapping to a single per-application block.
///
/// Why: each entry becomes exactly one `per_application` section in the rendered
/// report; the renderer iterates these in declared order.
/// What: `name`/`slug` identify the application; `source` is the validated
/// local-or-remote origin; `username` and `git_ref` are optional access /
/// attribution hints; `metrics` optionally points at a trusty-analyze JSON file.
/// Test: `manifest_tests.rs::parse_local_path_entry`, `parse_remote_entry`.
#[derive(Debug, Clone, Serialize)]
pub struct RepositoryEntry {
    /// Human-readable application name.
    pub name: String,
    /// Filesystem/URL-safe slug (derived from `name` when not supplied).
    pub slug: String,
    /// The validated source: a local checkout or a declared remote.
    pub source: RepositorySource,
    /// Username for remote access/attribution (meaningful only with `remote`).
    pub username: Option<String>,
    /// Branch, tag, or commit to analyze (e.g. `main`, `v1.2.3`, `abc123`).
    pub git_ref: Option<String>,
    /// Optional path to a trusty-analyze metrics JSON file for this repo.
    pub metrics: Option<PathBuf>,
}

/// The origin of a repository — a local checkout or a declared remote.
///
/// Why: local entries are enriched with real git info (branch/SHA/dirty) while
/// remote entries are recorded as-declared (no network fetch in M1); the enum
/// makes that distinction explicit and exhaustive.
/// What: `LocalPath` carries the checkout path; `Remote` carries the declared
/// `owner/repo` slug or full git URL.
/// Test: `manifest_tests.rs::parse_local_path_entry`, `parse_remote_entry`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepositorySource {
    /// A local repository checkout at the given path.
    LocalPath {
        /// The local filesystem path to the checkout.
        path: PathBuf,
    },
    /// A remote repository declared as `owner/repo` or a full git URL.
    Remote {
        /// The declared remote reference (`owner/repo` or a git URL).
        remote: String,
    },
}

impl RepositorySource {
    /// A short human string describing the source for report metadata.
    ///
    /// Why: the report metadata section and JSON model surface a compact origin
    /// string regardless of source kind.
    /// What: returns the path display for local, or the remote ref for remote.
    /// Test: `reporter_tests.rs` asserts the local path appears in output.
    pub fn describe(&self) -> String {
        match self {
            RepositorySource::LocalPath { path } => path.display().to_string(),
            RepositorySource::Remote { remote } => remote.clone(),
        }
    }
}

// ─── Raw (pre-validation) TOML shape ────────────────────────────────────────

/// The raw manifest exactly as it appears on disk, before validation.
///
/// Why: TOML cannot express "exactly one of two optional keys"; we deserialize
/// the permissive shape first, then validate into [`Manifest`].
/// What: `path`/`remote` are both optional here and reconciled in `validate`.
/// Test: exercised transitively by every `manifest_tests.rs` case.
#[derive(Debug, Deserialize)]
struct RawManifest {
    report: ReportSection,
    #[serde(default)]
    repositories: Vec<RawRepositoryEntry>,
}

/// One raw repository entry with both source keys optional.
#[derive(Debug, Deserialize)]
struct RawRepositoryEntry {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    username: Option<String>,
    /// `ref` is a Rust keyword, so the TOML `ref` key maps to `git_ref`.
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    metrics: Option<PathBuf>,
}

// ─── Loader ─────────────────────────────────────────────────────────────────

/// Load and validate a report manifest from a TOML file.
///
/// Why: the single entry point the `report` CLI uses to turn a user-authored
/// TOML file into a validated, render-ready [`Manifest`]; centralising I/O +
/// parse + validation keeps the handler thin.
/// What: reads `path`, parses TOML, then validates each entry (exactly one of
/// `path`/`remote`; non-empty repository list).  Warns (does not fail) on an
/// orphaned `username` set alongside a local `path`.
/// Test: `manifest_tests.rs` — all variants via a temp file.
pub fn load_manifest(path: &Path) -> std::result::Result<Manifest, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_manifest(&text, path)
}

/// Parse and validate a manifest from an in-memory TOML string.
///
/// Why: separating parse-from-string from file I/O lets unit tests validate the
/// schema without touching the filesystem.
/// What: deserializes into [`RawManifest`] then delegates to `validate`.  `path`
/// is used only to annotate parse errors.
/// Test: `manifest_tests.rs` calls this directly with literal TOML.
pub fn parse_manifest(text: &str, path: &Path) -> std::result::Result<Manifest, ManifestError> {
    let raw: RawManifest = toml::from_str(text).map_err(|source| ManifestError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    validate(raw)
}

/// Validate a raw manifest into the resolved [`Manifest`] model.
///
/// Why: enforces the invariants the renderer relies on — a non-empty repository
/// list and exactly one source per entry — in one place.
/// What: rejects empty manifests and each malformed entry; derives a slug when
/// absent; warns on an orphaned `username` with a local source.
/// Test: `manifest_tests.rs::{no_repositories_errors, conflicting_sources_error,
/// missing_source_error, orphaned_username_tolerated}`.
fn validate(raw: RawManifest) -> std::result::Result<Manifest, ManifestError> {
    if raw.repositories.is_empty() {
        return Err(ManifestError::NoRepositories);
    }

    let mut repositories = Vec::with_capacity(raw.repositories.len());
    for entry in raw.repositories {
        let source = match (entry.path, entry.remote) {
            (Some(_), Some(_)) => {
                return Err(ManifestError::ConflictingSources { name: entry.name });
            }
            (None, None) => {
                return Err(ManifestError::MissingSource { name: entry.name });
            }
            (Some(path), None) => RepositorySource::LocalPath { path },
            (None, Some(remote)) => RepositorySource::Remote { remote },
        };

        // An orphaned username on a local source has no meaning (attribution and
        // access are only used for remote fetches, deferred to a later
        // milestone).  We keep the entry but warn so the author can clean it up.
        if entry.username.is_some() && matches!(source, RepositorySource::LocalPath { .. }) {
            warn!(
                repository = %entry.name,
                "`username` is set on a local-path repository; it is ignored (only meaningful with `remote`)"
            );
        }

        let slug = entry.slug.unwrap_or_else(|| slugify(&entry.name));
        repositories.push(RepositoryEntry {
            name: entry.name,
            slug,
            source,
            username: entry.username,
            git_ref: entry.git_ref,
            metrics: entry.metrics,
        });
    }

    Ok(Manifest {
        report: raw.report,
        repositories,
    })
}

/// Derive a filesystem/URL-safe slug from a human-readable name.
///
/// Why: output filenames and section anchors need a stable, safe identifier
/// when the author does not supply an explicit slug.
/// What: lowercases, maps any run of non-alphanumeric characters to a single
/// `-`, and trims leading/trailing dashes.  Empty input yields `report`.
/// Test: `manifest_tests.rs::slug_derivation`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "report".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
