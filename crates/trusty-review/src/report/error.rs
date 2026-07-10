//! Error types for the deterministic report-generation pipeline (M1, #2313).
//!
//! Why: report generation spans manifest parsing, template discovery, metrics
//! ingestion, and filesystem output — each with distinct failure modes.  A
//! dedicated `thiserror` enum lets the CLI surface actionable messages (a
//! conflicting source in a manifest vs. a missing template) without leaking
//! serde/toml internals into the public API.
//! What: defines [`ReportError`] (the crate-boundary error) and the [`Result`]
//! alias used throughout `src/report/`.  A separate [`ManifestError`] captures
//! manifest-specific validation failures and is re-wrapped into `ReportError`.
//! Test: variants are exercised by `manifest_tests.rs` (validation),
//! `template.rs` tests (NotFound), and `reporter_tests.rs` (I/O).

use std::path::PathBuf;

use thiserror::Error;

/// Errors produced while loading and validating a report manifest.
///
/// Why: manifest problems are the most common user-facing failure (a hand-typed
/// TOML with both a `path` and a `remote`, or neither); typed variants let the
/// CLI print a precise, correctable message instead of a raw parse dump.
/// What: covers filesystem I/O, TOML parse failures (with the offending path),
/// and the two mutual-exclusion validation cases plus the empty-manifest case.
/// Test: `manifest_tests.rs` exercises every variant.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// The manifest file could not be read from disk.
    #[error("I/O error reading manifest at {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// The manifest exists but is not valid TOML.  `toml::de::Error` carries the
    /// line/column of the offending token.
    #[error("failed to parse manifest TOML at {path}: {source}")]
    Parse {
        /// The manifest path that failed to parse.
        path: PathBuf,
        /// The underlying TOML deserialization error (includes line numbers).
        #[source]
        source: toml::de::Error,
    },

    /// The manifest parsed but declared zero `[[repositories]]` entries.
    #[error("manifest must declare at least one [[repositories]] entry")]
    NoRepositories,

    /// A repository entry declared neither `path` nor `remote`.
    #[error(
        "repository '{name}': exactly one of `path` or `remote` is required, but neither was set"
    )]
    MissingSource {
        /// The offending repository entry's `name`.
        name: String,
    },

    /// A repository entry declared BOTH `path` and `remote`.
    #[error("repository '{name}': `path` and `remote` are mutually exclusive — set exactly one")]
    ConflictingSources {
        /// The offending repository entry's `name`.
        name: String,
    },
}

/// Errors produced by the report-generation pipeline end to end.
///
/// Why: the CLI handler needs a single error type spanning manifest loading,
/// template discovery, metrics parsing, and output writing so `?` composes
/// across the whole `report` subcommand.
/// What: wraps [`ManifestError`], template-not-found, metrics parse failures,
/// and generic I/O; the CLI maps these to `anyhow` at the boundary.
/// Test: `reporter_tests.rs` (I/O), `template.rs` tests (`TemplateNotFound`),
/// `metrics.rs` tests (`Metrics`).
#[derive(Debug, Error)]
pub enum ReportError {
    /// A manifest-level failure (parse or validation).
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// The requested template name resolved to neither an XDG override nor a
    /// bundled default.
    #[error(
        "template '{name}' not found (no XDG override at ~/.config/trusty-review/templates/{name}.md and no bundled default)"
    )]
    TemplateNotFound {
        /// The template name that could not be resolved.
        name: String,
    },

    /// A metrics JSON file exists but could not be parsed against the v0 schema.
    #[error("failed to parse metrics JSON at {path}: {source}")]
    Metrics {
        /// The metrics file path that failed to parse.
        path: PathBuf,
        /// The underlying serde_json error.
        #[source]
        source: serde_json::Error,
    },

    /// A filesystem I/O error while reading templates/metrics or writing output.
    #[error("I/O error in report pipeline at {path}: {source}")]
    Io {
        /// The path involved in the failed operation.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
}

/// Convenience `Result` alias for the report pipeline.
///
/// Why: avoids repeating `Result<T, ReportError>` at every call site.
/// What: aliases `std::result::Result<T, ReportError>`.
/// Test: used transitively by all report-pipeline functions.
pub type Result<T> = std::result::Result<T, ReportError>;
