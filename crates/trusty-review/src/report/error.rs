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

    /// The analyst instructions file (`--instructions` / `[report].instructions`)
    /// could not be read — almost always a mistyped path, which must fail loudly
    /// rather than silently produce a report with no recorded focus.
    #[error("analyst instructions file not found at {path}: {source}")]
    InstructionsNotFound {
        /// The instructions path that failed to read.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: std::io::Error,
    },

    /// A report was handed to the writer with no synthesis attached (#5454).
    ///
    /// Why: inference is required, so a synthesis-free model reaching disk would
    /// be the deterministic-only report the writer exists to prevent. Reaching
    /// this is a caller bug, not an operator mistake — the CLI runs synthesis
    /// unconditionally and propagates its failure.
    #[error(
        "internal: the report model carries no synthesis; report rendering requires a completed inference pass"
    )]
    SynthesisRequired,

    /// A metrics JSON file exists but could not be parsed against the v0 schema.
    #[error("failed to parse metrics JSON at {path}: {source}")]
    Metrics {
        /// The metrics file path that failed to parse.
        path: PathBuf,
        /// The underlying serde_json error.
        #[source]
        source: serde_json::Error,
    },

    /// A declared ticketing artifact exists but could not be parsed (#5405).
    ///
    /// Why: the manifest declared the file, so an unreadable one is a producer
    /// bug — tga writes the key only after writing the file. Degrading to a
    /// report with no board coverage would hide that bug behind a section that
    /// merely looks unpopulated, which is the failure mode #5405 is about.
    #[error("failed to parse ticketing JSON at {path}: {source}")]
    Ticketing {
        /// The ticketing artifact path that failed to parse.
        path: PathBuf,
        /// The underlying serde_json error.
        #[source]
        source: serde_json::Error,
    },

    /// A ticketing artifact parsed, but declares a schema major this build does
    /// not read (#5405).
    ///
    /// Why: tga and trusty-review are versioned and installed independently, so
    /// skew is the normal case, and every field of the artifact carries
    /// `#[serde(default)]`. A renamed count therefore parses to zero and renders
    /// `0 of 412 commit(s) reference tracked work` — confident, wrong, and
    /// silent. Refusing an unrecognised major routes that the same way an
    /// unparseable artifact goes: a hard, named failure.
    /// What: distinct from [`ReportError::Ticketing`] because the remedy
    /// differs — that one is a producer bug, this one is a version mismatch the
    /// operator resolves by aligning the two binaries.
    #[error(
        "ticketing artifact at {path} declares schema_version {found:?}, which this build cannot \
         read; it reads schema major {supported} (an empty value means the artifact carried no \
         schema_version)"
    )]
    TicketingSchema {
        /// The ticketing artifact whose schema tag was refused.
        path: PathBuf,
        /// The `schema_version` the artifact declared; empty when absent.
        found: String,
        /// The schema major this build reads.
        supported: u32,
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
