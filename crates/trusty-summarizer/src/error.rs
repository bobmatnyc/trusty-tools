//! The crate's one error type and its record contract.
//!
//! Why: a batch must turn every per-file failure into a JSON error record
//! instead of stopping, so each failure needs a stable, machine-readable kind
//! as well as a human message.

use std::io;
use std::path::PathBuf;

use crate::backend::Usage;

/// Every failure the library reports.
///
/// Why: one enum lets the pipeline convert any failure into an error record
/// with [`Error::kind`], and lets backend and converter implementors report
/// their own failures through [`Error::Backend`] and [`Error::Conversion`].
///
/// What: the contract per variant, and whether it stops a batch:
///
/// - [`Error::Io`]: a path could not be read or listed. Per file.
/// - [`Error::UnsupportedFormat`]: no converter is registered for the
///   detected format; carries the format name. Per file.
/// - [`Error::TooLarge`]: the file is over `max_file_bytes`. Per file.
/// - [`Error::InvalidText`]: a directly read file is not UTF-8. Per file.
/// - [`Error::Conversion`]: a converter failed. Per file.
/// - [`Error::Worker`]: the converter worker process could not be found or
///   started, crashed, timed out, exceeded the output limit or broke the
///   protocol; the [`WorkerFailure`] names which. Per file; the next file
///   gets a fresh worker.
/// - [`Error::NoText`]: a document converted to empty text (a scanned PDF).
///   Per file.
/// - [`Error::Backend`]: an inference or classifier backend failed, or
///   returned a malformed response. Per file.
/// - [`Error::Config`], [`Error::UnknownBackend`]: the configuration is
///   unusable. Fatal: raised before any file is processed.
/// - [`Error::InvalidRange`]: a `--lines` range is malformed or outside the
///   document. Fatal for `read`.
/// - [`Error::Cache`]: the cache directory could not be prepared. Fatal at
///   start; a failed cache write during a batch is a warning, never a record.
///
/// Test: `error_kinds_are_stable`, `bad_file_gives_error_record_and_exit_2`
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem read or directory listing failed.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// No converter is registered for this format.
    #[error("no converter for format `{format}`{}", note.map(|n| format!(": {n}")).unwrap_or_default())]
    UnsupportedFormat {
        /// The detected format name, e.g. `pdf`.
        format: String,
        /// Why the format is off, when that is known.
        note: Option<&'static str>,
    },
    /// The file exceeds the configured size limit.
    #[error("file is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// The file size in bytes.
        size: u64,
        /// The configured limit in bytes.
        limit: u64,
    },
    /// A file read directly as text is not valid UTF-8.
    #[error("not valid UTF-8 text: {0}")]
    InvalidText(String),
    /// A converter failed to produce text.
    #[error("converter `{converter}` failed: {message}")]
    Conversion {
        /// The converter's name.
        converter: String,
        /// What went wrong.
        message: String,
    },
    /// The converter worker process failed.
    #[error("converter worker {}: {message}", failure.code())]
    Worker {
        /// Which failure.
        failure: WorkerFailure,
        /// What happened.
        message: String,
    },
    /// Conversion succeeded but produced no text.
    #[error("no text extracted by `{converter}` (a scanned or image-only document gives this)")]
    NoText {
        /// The converter's name.
        converter: String,
    },
    /// The summary mode answered with an empty or whitespace-only summary,
    /// e.g. an extractive mode on text with no prose sentences.
    #[error(
        "summary mode `{backend}` produced an empty summary (the text has no prose sentences, or the backend answered nothing)"
    )]
    EmptySummary {
        /// The summary mode's configured name.
        backend: String,
    },
    /// An inference or classifier backend failed.
    #[error("backend `{backend}` failed: {message}")]
    Backend {
        /// The backend's configured name.
        backend: String,
        /// What went wrong.
        message: String,
        /// Model usage the failed call(s) reported and the provider bills,
        /// e.g. an answer whose output budget went to reasoning. `None`
        /// when no response reported usage.
        usage: Option<Usage>,
    },
    /// The configuration is invalid.
    #[error("configuration error: {0}")]
    Config(String),
    /// The configuration names a backend that is not registered.
    #[error("unknown {role} backend `{name}` (registered: {known})")]
    UnknownBackend {
        /// `inference` or `classifier`.
        role: &'static str,
        /// The name the configuration asked for.
        name: String,
        /// Comma-separated registered names.
        known: String,
    },
    /// A line range is malformed or out of bounds.
    #[error("invalid line range: {0}")]
    InvalidRange(String),
    /// The cache could not be prepared or read.
    #[error("cache error at {}: {message}", path.display())]
    Cache {
        /// The cache path involved.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
}

impl Error {
    /// The stable snake_case code written to an error record's `kind` field.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Io { .. } => "io",
            Error::UnsupportedFormat { .. } => "unsupported_format",
            Error::TooLarge { .. } => "too_large",
            Error::InvalidText(_) => "invalid_text",
            Error::Conversion { .. } => "conversion_failed",
            Error::Worker { failure, .. } => failure.code(),
            Error::NoText { .. } => "no_text_extracted",
            Error::EmptySummary { .. } => "empty_summary",
            Error::Backend { .. } => "backend_failed",
            Error::Config(_) => "config",
            Error::UnknownBackend { .. } => "unknown_backend",
            Error::InvalidRange(_) => "invalid_range",
            Error::Cache { .. } => "cache",
        }
    }

    /// The format name, for [`Error::UnsupportedFormat`] only.
    pub fn format(&self) -> Option<&str> {
        match self {
            Error::UnsupportedFormat { format, .. } => Some(format),
            _ => None,
        }
    }

    /// Usage a failed backend call reported, which the provider bills.
    pub fn billed_usage(&self) -> Option<Usage> {
        match self {
            Error::Backend { usage, .. } => *usage,
            _ => None,
        }
    }

    /// Builds an [`Error::Io`] for `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

/// Which way the converter worker failed; each is its own record `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkerFailure {
    /// No worker binary was found.
    Missing,
    /// The worker binary could not be started.
    Spawn,
    /// The worker exited or closed its output mid-request.
    Crashed,
    /// The worker did not answer within `convert.timeout_ms`.
    Timeout,
    /// The answer exceeded `convert.max_output_bytes`.
    OutputTooLarge,
    /// The worker's hello or answer did not match the protocol.
    Protocol,
}

impl WorkerFailure {
    /// The record `kind` for this failure.
    pub fn code(self) -> &'static str {
        match self {
            WorkerFailure::Missing => "worker_missing",
            WorkerFailure::Spawn => "worker_spawn_failed",
            WorkerFailure::Crashed => "worker_crashed",
            WorkerFailure::Timeout => "worker_timeout",
            WorkerFailure::OutputTooLarge => "worker_output_too_large",
            WorkerFailure::Protocol => "worker_protocol",
        }
    }
}

/// The crate's result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kinds_are_stable() {
        let cases = [
            (Error::io("x", io::Error::other("boom")), "io"),
            (
                Error::UnsupportedFormat {
                    format: "pdf".into(),
                    note: None,
                },
                "unsupported_format",
            ),
            (Error::TooLarge { size: 2, limit: 1 }, "too_large"),
            (Error::InvalidText("x".into()), "invalid_text"),
            (Error::Config("x".into()), "config"),
            (Error::InvalidRange("x".into()), "invalid_range"),
            (
                Error::Worker {
                    failure: WorkerFailure::Timeout,
                    message: "x".into(),
                },
                "worker_timeout",
            ),
            (
                Error::NoText {
                    converter: "x".into(),
                },
                "no_text_extracted",
            ),
        ];
        for (err, kind) in cases {
            assert_eq!(err.kind(), kind, "{err}");
        }
        let unsupported = Error::UnsupportedFormat {
            format: "pdf".into(),
            note: None,
        };
        assert_eq!(unsupported.format(), Some("pdf"));
        assert!(unsupported.to_string().contains("`pdf`"));
    }
}
