//! Error type for the `trusty-progress` crate.

use std::io;

/// Why: Library crates in this workspace must surface structured errors via
/// `thiserror` (never `anyhow`, never `unwrap`) so callers can match on the
/// failure mode. The only fallible path in this crate is writing rendered
/// output to the underlying sink, so we model that single failure.
/// What: Enumerates the error conditions a progress sink can raise.
/// Test: none of its own; the `Display` text and the `From<io::Error>`
/// conversion are `thiserror` derive output.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProgressError {
    /// Why: Writing a narrator line or a component row to the configured sink
    /// (stderr, a file, or a captured buffer) can fail with an I/O error; the
    /// caller needs that surfaced rather than silently dropped.
    /// What: Wraps the underlying [`std::io::Error`] from a failed write/flush.
    /// Test: derive output; no test of its own.
    #[error("failed to write progress output: {0}")]
    Io(#[from] io::Error),
}

/// Why: Saves every public function from repeating the full `Result<T,
/// ProgressError>` spelling and gives callers a single canonical alias.
/// What: Crate-local `Result` specialised to [`ProgressError`].
/// Test: Used as the return type throughout the crate; compilation is the test.
pub type Result<T> = std::result::Result<T, ProgressError>;
